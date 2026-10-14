# 14 — Ciclo de vida e recuperação (snapshot pré-migrate, DR, upgrade)

## Objetivo

Prod migra e "der ruim" sem ponto de volta? Não: dump ANTES de cada migrate de
deploy. VHDX morre? `flux export/import` reergue a plataforma de outro disco —
VM nova de pé e prods servindo em ≤ 30 min (dados: ≤ 60 min). E o próprio Flux
tem procedimento de upgrade sem derrubar nada, com rollback.

Requisitos desta spec: R25 (recuperável: snapshot pré-migrate, DR medido,
upgrade seguro com rollback) e R34 (backup agendado de banco de prod: 48 h,
retenção 7 dias). Boot do host e queda de energia: [01](01-vm-hyperv.md).

## Mecanismo

### Snapshot pré-migrate de prod (R25)

- **Quando**: pipeline de deploy (10), passo migrations, ANTES do primeiro
  `migrate --force` do release — e só se há migration pendente (sem pendência,
  sem dump, sem custo).
- **Como** (roda no `fluxd`, via `docker exec` no `flux-db`):

```bash
docker exec flux-db sh -c \
  'mysqldump -uroot -p"$MYSQL_ROOT_PASSWORD" --single-transaction --quick \
   --routines --triggers {name}_prod' \
  | gzip > /srv/flux/backups/pre-migrate/{name}/{sha_curto}-{ts}.sql.gz
```

  `--single-transaction`: dump consistente de InnoDB SEM lock de escrita — a
  cor velha segue servindo durante o dump (blue-green, 10).
- **Registro**: `deploy_log` ganha entrada com o path e o sha256 do dump — cada
  deploy sabe exatamente qual snapshot o precede.
- **Retenção**: últimos 10 dumps por projeto (prune após deploy bem-sucedido;
  o dump do deploy atual nunca é podado). Laravel médio comprimido: ~2–20 MB —
  orçamento trivial no disco de 80 GB (01).
- **Restore** (manual, nunca automático): `flux db restore {name} {arquivo} --yes`
  — pausa o HTTP da cor ativa (`pause`, < 1 s sem request em voo), importa o
  dump, retoma. Sem `--yes`, o comando só mostra o que faria.

### Backup agendado dos bancos de prod (R34)

Além do pré-migrate (event-driven), roda um agendado de rotina:

- **A cada 48 h** o `fluxd` dumpa TODAS as `{name}_prod` — mesmo comando do
  pré-migrate (`mysqldump --single-transaction --routines --triggers`, sem
  lock), destino `/srv/flux/backups/scheduled/{name}/{ts}.sql.gz`.
- **Retenção 7 dias**: prune diário apaga dump com mais de 7 dias (~3–4 dumps
  por projeto no disco).
- Integridade: `gzip -t` pós-dump; falhou ou não rodou no prazo → **alerta**
  (13). Dump de backup sem verificação é papel.
- Resto de backup (storage/uploads de mídia, VM inteira, offsite): FORA de
  escopo por decisão do Gabriel (10/10/2026) — só banco de prod, 48 h/7 d.

### Disaster recovery: `flux export` / `flux import` (R25)

`flux export` (Windows, puxa via SSH) gera `flux-export-{data}.zip` com:

| Conteúdo | Forma |
|---|---|
| `config.toml` global (sem `api.token`) | texto |
| `flux.toml` de todos os projetos | texto |
| SQLite de estado | SEM tokens (são hash — regenerar); filas/estado descartáveis |
| DBs e users por projeto | lista para recriar users/grants (05) |
| Release ativo por prod (sha) | para re-deployar do GitHub |
| Dados: dumps `.sql.gz` | flag `--with-dumps` (default: traz o pre-migrate mais recente de cada prod) |
| Segredos (`.cred` age, `db-root.env`, `panel.argon2`) | arquivo `secrets.age` cifrado com **passphrase pedida na hora** (`age -p`) — zip vazado sem a passphrase não abre |

A identity age sealed pelo vTPM **não** viaja: no `import`, os `.cred` são
decifrados com a passphrase do export e RE-sealados com age novo sealed pelo
vTPM da VM nova (09).

**Procedimento DR** (meta escrita: infra + prods servindo ≤ 30 min; com dados
≤ 60 min):

1. `flux setup host` + `flux setup vm` — VM nova provisionada (01), ~15 min.
2. `flux import flux-export-{data}.zip` (+ passphrase) → recria state,
   projetos, redes, users/DBs vazios, re-seala os envs.
3. `fluxd` re-deploya cada prod do GitHub no sha exportado — build em fila com
   prioridade prod (11), ~5–10 min para 3–5 prods.
4. `flux db restore` dos dumps quando os dados importam (senão os prods
   vivem do que as migrations criam).
5. Devs: `flux up` em cada repo no Windows (sync recria, 02).
6. GitHub Secrets: re-gerar `FLUX_DEPLOY_TOKEN` por projeto (`flux project
   token`) — tokens de deploy NÃO são exportáveis por desenho.

### Upgrade do fluxd e do `flux` (R25)

- Versionamento: semver + `CHANGELOG.md` no repo. CLI e `fluxd` toleram
  diferença de 1 versão (handshake na API); mismatch maior = erro claro pedindo
  upgrade do outro lado, nunca comportamento estranho.
- **`flux vm upgrade`**:
  1. Checkpoint Hyper-V `pre-upgrade-{v}` (removido automaticamente após 7 dias);
  2. Binário novo vai para `/srv/flux/bin/fluxd.new`; `fluxd --version` + smoke
     interno validam ANTES do swap;
  3. Swap: anterior vira `fluxd.prev`, `systemctl restart fluxd` — shutdown
     graceful (axum espera requests em voo, teto 10 s); estado e fila de wake
     ficam no SQLite; **containers não caem**;
  4. Health interno pós-upgrade; falha em 60 s → rollback automático para
     `fluxd.prev` + alerta (13).
  Janela sem `fluxd` ≤ 5 s: o `fluxhost` segura requests com retry curto
  (2 tentativas, 2 s) — o usuário quase nunca vê página de espera.
- **`flux self-update`** (Windows): baixa release do repo, valida SHA256 do
  checksum, troca o binário em execução via rename (Windows permite renomear o
  `.exe` rodando: `flux.exe` → `flux.old.exe`, novo entra no lugar; `.old`
  limpo no boot seguinte).

Fora de escopo agora (decisão futura do Gabriel): backup OFFSITE da VM/dumps
para outro dispositivo ou nuvem — hoje todo backup mora no mesmo disco físico.

### Modo dev temporário (`[temporary]`, pedido do Gabriel 14/10)

Projeto de uso curto (experimento, demo, branch descartável) que se apaga
SOZINHO da VM: `[temporary] enabled = true` no `flux.toml` (wizard `-m`
pergunta, 06). Contador: **TTL sem interação NENHUMA** (default **6 h**,
`ttl_hours`) — o mesmo `last_interaction` do sono (04: request proxyado,
beat, CPU acima do teto, lease de CLI/painel renovam). Vale SÓ para **dev**;
`enabled = true` em projeto com prod ativo = recusado no registro.

Teardown (executado pelo sweep do `fluxd`, 60 s):

1. `docker compose -p flux-{name}-dev down -v` (containers + rede + volumes
   nomeados do projeto) e o mesmo para cada worktree `flux-{name}-@{slug}-dev`;
2. `rm -rf /srv/flux/projects/{name}` e `{name}@*`;
3. purge no SQLite: `projects`, `sleep_state`, `migration_state` do nome;
4. evento "temporário expirou" no painel com o motivo ("6 h sem interação")
   e a lista do que foi apagado.

Garantias:

- **Nunca toca nada FORA da VM**: o repo no Windows fica intocado (sync é
  one-way-replica VM←Windows, 02 — apagar o beta não apaga a fonte);
  próxima `flux up` do mesmo repo recria tudo do zero (sync fresca + build).
- Sessão mutagen do projeto fica `halted` no host (beta sumiu) — inofensiva;
  o próximo `flux up` detecta sessão halted/órfã e recria.
- `last_interaction` ausente/ilegível = NÃO expira (sem dado honesto de
  tempo, não há teardown — nunca chutar relógio).
- Teardown com lease ativo não acontece: lease renova interação (04), então
  projeto em uso não chega ao TTL.

## Decisões

| Decisão | Alternativa descartada | Por quê |
|---|---|---|
| Dump antes de TODO migrate de prod | dump diário agendado | Diário pode estar 23 h velho no momento exato do deploy; o snapshot pré-migrate é sempre "o estado de 1 segundo atrás" |
| Export de segredos com passphrase na hora | exportar identity age / nada de segredos | Identity sealed no vTPM não sai (garantia anti-VHDX-roubado, 09); sem segredos o DR perde a metade cara. Passphrase no export = 3ª camada |
| Re-deploy do GitHub em vez de copiar releases | zipar releases no export | GitHub é a fonte da verdade do código de prod (10); copiar release é carregar peso morto e guardar DOIS lugares |
| Checkpoint Hyper-V antes do upgrade | backup só do binário | Checkpoint volta fluxd + estado + sistema inteiro; binário .prev cobre 99% dos casos, checkpoint é o plano B |
| Tokens regenerados no import | exportar tokens | Token vazado no zip = deploy de terceiro na VM; regenerar custa 1 comando |

## O que NUNCA fazer

- **Nunca** rodar o teardown temporário com `last_interaction` ausente ou
  ilegível — sem relógio honesto, não há expiração.
- **Nunca** apagar nada fora de `/srv/flux` nem containers fora do padrão
  `flux-{name}*` no teardown — o repo Windows e projetos vizinhos são
  intocáveis.
- **Nunca** rodar `migrate --force` de deploy sem o snapshot gravado (com
  pendência existente) — se o dump falha, o deploy PARA com erro claro.
- **Nunca** gerar export com `secrets.age` sem passphrase.
- **Nunca** restaurar dump sem confirmação humana explícita (`--yes`) — restore
  sobrescreve dados de prod.
- **Nunca** `flux import` por cima de state existente sem `--force` com backup
  automático antes.
- **Nunca** upgradar `fluxd`/VM durante deploy, rollback ou drain em curso
  (mesmas guardas do Save-VM, 12).
- **Nunca** deixar checkpoint `pre-upgrade-*` acumular (7 dias, prune).

## Critérios de aceite

1. Deploy com 1 migration pendente gera `.sql.gz` registrado no `deploy_log`;
   deploy sem pendência gera NADA (log prova).
2. Restore: COUNT por tabela do pós-restore bate com o pré-incidente; HTTP
   pausado durante o import retoma em < 1 s depois.
3. Ensaio de DR completo cronometrado: VM nova + import + 3 prods servindo
   ≤ 30 min sem dados, ≤ 60 min com dumps (executar 1x após esta fase).
4. Upgrade com loop de 100 req/s: 0 falhas (fluxhost retry cobre a janela);
   rollback automático provado com binário quebrado de propósito.
5. `flux self-update` troca o binário durante um comando em andamento sem
   matar o comando.
6. `flux export` aberto sem a passphrase do `secrets.age` → nada legível.
7. Dump agendado de 48 h roda sozinho em todas as prods; `gzip -t` passa;
   falha simulada → alerta (13); dump com 8 dias some do disco (prune).
