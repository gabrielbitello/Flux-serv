# 05 — Banco único (MariaDB) + migrations automáticas

## Objetivo

**Um único servidor de banco** na VM alimenta todos os serviços docker — paridade
com o WSL atual, onde o container `mariadb` (imagem `mysql:8.4`) serve tudo.
Projetos **não podem** baixar imagem de banco própria (bloqueio automático de
Dockerfile/compose). Migrations são detectadas e aplicadas automaticamente, com
regras diferentes para dev (banco descartável) e prod (banco sagrado).

## Mecanismo

### O servidor único: `flux-db`

| Aspecto | Valor |
|---|---|
| Container | `flux-db`, gerido pelo `fluxd` (compose próprio, não é de projeto) |
| Imagem | `mysql:8.4` — FATO: paridade com o WSL atual (`MYSQL_VERSION=8.4.11`) |
| Rede | `flux-net` (bridge docker), hostname `flux-db`; porta **não publicada** |
| Volume | `flux-db-data` (dados) + `/srv/flux/db/init` montado ro em `/docker-entrypoint-initdb.d` (mesmo desenho do atual `/opt/mariadb/init`) |
| TZ | `America/Sao_Paulo` (paridade com o atual) |
| Credenciais | root em `/srv/flux/secrets/db-root.env` (600); por projeto: user `{name}` com senha gerada no cadastro, grants só nos bancos dele; user read-only `flux_panel_ro` por projeto com **apenas `SELECT` em prod** (R18) |
| Sono | NUNCA dorme. Healthcheck `mysqladmin ping` a cada 10 s |
| Admin | **Adminer** (container único, ~40 MB, sem dependências) — mais leve que phpmyadmin. Acesso **LAN-only**: o Caddy expõe `db.bitello.xyz` apenas no server block interno (192.168.140.2), nunca pelo tunnel público; painel linka. Credenciais são as dos users de projeto — o que pode ver/editar é o grant do user, não a UI |

FATO (inventário do WSL Ubuntu, 09/10/2026, `docker ps -a` + `docker images`): o
estado atual NUNCA foi "um banco só". Além do `mariadb` compartilhado
(`mysql:8.4`, Up), existem legados por-dev hoje `Exited`: `vippers-dev-mysql` e
`pedi-dev-mysql` (ambos `mysql:9.3`), e 5 imagens de banco mortas em disco
(`mysql:8.0`, `mysql:9.3`, `mariadb:11.4`, `mariadb:11.8` + a 8.4 em uso) —
sobras da era dokploy (`vippers-dev` com imagem local `vippers-vippers-dev`).
O guard + `flux-db` encerram essa era: um motor só. Migração dos dados dos
legados: `flux migrate-from-wsl` (09), descarte dos containers/imagens só com
ok explícito do Gabriel.

FATO (09/10/2026, `SHOW DATABASES` no `mariadb` compartilhado): os bancos REAIS
do vippers moram NELE — `vippers`, `vippers_afiliados`, `base-laravel` e as
DBs de teste `vippers_test_fix_g1..g8` (PHPUnit paralelo). Os `mysql:9.3`
legados NÃO alimentam nada hoje. Banco correto do vippers = `mysql:8.4`
compartilhado; `flux migrate-from-wsl` traz exatamente esses schemas para o
`flux-db`. DBs `*_test_*` viram `{name}_test` gerenciado (criadas/destruídas
por rodada de teste, fora do pipeline de migrations).

Um banco por projeto por ambiente: `{name}_dev` e `{name}_prod` (worktrees de
branch: `{name}_{branch-slug}_dev`, 02). O app recebe as credenciais via `.env`
mantido no lado VM (injetado pelo `flux env` no cadastro: `DB_HOST=flux-db`,
`DB_DATABASE={name}_{env}`, `DB_USERNAME={name}`, senha gerada). Painel e
Adminer em prod: **visualizar só** (R18) — nenhum UPDATE/INSERT/DROP chega ao
banco de prod pela UI; a garantia é o grant SQL, não o botão.

### Guard de imagens de banco (proibição)

No `flux up`/`deploy`/`build`, o `fluxd` analisa **antes** de qualquer pull:

- `compose.yaml`/`docker-compose.yml`/`dev-instance.yml`: campo `image:` de
  cada service.
- `Dockerfile`/`Dockerfile.*`: linhas `FROM` (incluindo estágios).

Contra a lista (configurável no config global, default):

```
mysql, mariadb, mariadb-*, mysql-*, postgres, postgresql*, mongo, mongo*,
mssql, azure-sql*, oracle*, percona*, clickhouse, cockroachdb*, redis, valkey, memcached
```

Match por prefixo/regex do repositório da imagem (ex.: `mysql:8.4`,
`bitnami/mariadb`, `library/postgres`). Pegou = **falha com erro explícito**:
arquivo, linha, imagem e a instrução "remova o service e aponte para
`flux-db`". Build/up não roda. Log no painel. Sem override.

### Pipeline de migrations

Estado por projeto+ambiente no SQLite do `fluxd` (tabela `migration_state`):
`arquivo → sha256 → batch → aplicado_em`. Adapters por framework: `laravel`
(v1 — detecta `artisan` na raiz; `database/migrations/*.php` é a fonte de
arquivos). O runner roda `php artisan ...` **dentro do container** via `docker exec`.

Fluxo a cada `up`/`deploy` (com lock por projeto+ambiente — nunca dois runners):

1. **Scan**: lista atual de arquivos + hash de cada um.
2. **Diff contra o estado**:
   - novos arquivos (não constam no estado) → `migrate` (aplica em batch);
   - arquivos alterados (hash difere do aplicado) → ver regra por ambiente;
   - arquivos apagados → avisar no log/painel (nunca ação automática).
3. **PROD** (`flux deploy` / `{name}.bitello.xyz`):
   - novos → `php artisan migrate --force`;
   - **alterado → ERRO e deploy BLOCAADO**: migration de prod é imutável.
     Resolução: criar migration nova; sair do bloqueio exige restaurar o
     conteúdo original (hash volta a bater) ou `flux db prod-mutated --ack`
     (só grava o novo hash no estado, nunca reconstrói — trilha auditada).
   - **Nunca** `fresh`, **nunca** drop, **nunca** seed automático.
4. **DEV** (`flux up` / `{name}-d.bitello.xyz`):
   - novos → `php artisan migrate --force`;
   - **alterado → reconstrói o banco dev**: `php artisan migrate:fresh` +
     seed conforme switch (abaixo). O banco dev é descartável por definição;
   - **editando agora**: janela de silêncio — o runner só dispara quando o
     arquivo está 5 s sem mudança (inotify) e duas leituras com hash igual;
     enquanto o Gabriel digita, nada roda. Mudanças em rajada (commit de
     branch) consolidam num único rebuild;
   - switches `flux.toml`:
     - `[db] rebuild_policy`: `"auto"` (default: rebuild só quando detecta
       mutação), `"always"` (rebuild a todo `flux up` — banco limpo com seed a
       cada subida), `"never"` (nunca rebuild, até mutação vira aviso);
     - `[db] seed_on_rebuild`: `true`/`false` (default `true`);
   - override de CLI: `flux db fresh --seed` / `--no-seed` força uma vez.
5. **Troca de branch no dev** (caso devfactory atual): sync traz arquivos novos
   e apagados; o diff do passo 2 naturalmente dispara migrate/rebuild —
   paridade com o comportamento atual (`migrate:fresh --seed` quando branch
   muda), sem detecção especial de branch.
6. **Tudo é logado**: `flux db status` e painel mostram por projeto+ambiente:
   pendentes, aplicados (com hash), último rebuild, motivo (mutação de X / policy).

### Ordem de boot dependente

`flux up` espera o `flux-db` saudável antes de subir containers de app
(healthcheck é a barreira — não sleep fixo).

## Decisões

| Decisão | Alternativa descartada | Por quê |
|---|---|---|
| Estado de migrations no SQLite do `fluxd` | tabela `flux_control` dentro do `mysql:8.4` | O `fluxd` precisa de estado ANTES do banco existir/saudável (ele é quem sobe o banco); SQLite remove dependência circular. Backup = um arquivo |
| Bloqueio por análise de compose/Dockerfile | auditoria em runtime de imagens baixadas | Bloqueia ANTES do pull — economia e garantia; runtime só constataria o problema depois |
| `migrate:fresh` no dev com mutação | drop só das tabelas tocadas | `fresh` é o comportamento já aceito no devfactory (dados do dev são descartáveis); simples e determinístico |
| Hash sha256 por arquivo de migration | `migrations` table do Laravel | A table do Laravel não detecta MUTAÇÃO de arquivo já aplicado; o hash existe exatamente para o requisito "alterou migration rodada → reconstrói (dev)" |

## O que NUNCA fazer

- **Nunca** reconstruir banco de prod — nem com flag, nem "só uma vez". A única
  ação de escrita em prod é `migrate` (e o `--ack` auditado de hash).
- **Nunca** deixar o projeto trazer banco próprio (guard sem override).
- **Nunca** publicar a 3306 do `flux-db` para fora da rede docker.
- **Nunca** rodar o runner enquanto a migration está sendo editada (janela de
  silêncio é obrigatória).
- **Nunca** apagar `flux-db-data` em operação normal; reset de dados é
  `migrate:fresh` por projeto, nunca drop do servidor.
- **Nunca** aplicar seed em prod.

## Critérios de aceite

1. Projeto com `FROM mysql:8.4` no Dockerfile → `flux up` falha com arquivo+linha
   apontados, nada é baixado.
2. Projeto novo: `flux up` cria user+DB, roda migrations, app conecta em
   `flux-db` — sem nenhum passo manual.
3. Dev edita migration já aplicada → após ~5 s de silêncio, `migrate:fresh --seed`
   roda sozinho; `flux db status` mostra o rebuild e o motivo.
4. Mesmo cenário em prod → erro claro, deploy bloqueado, banco intacto.
5. `[db] rebuild_policy = "always"` → todo `flux up` faz fresh+seed no dev.
6. Dois projetos simultâneos compartilham o `flux-db` sem conflito (users/DBs
   isolados).
7. Painel conectado como `flux_panel_ro` tenta `UPDATE` em tabela de prod →
   erro de permissão do MySQL (1144); o mesmo comando em dev funciona.
