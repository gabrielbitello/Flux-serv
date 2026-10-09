# 06 — CLI `flux` (Windows)

## Objetivo

O Gabriel roda `flux up` dentro de qualquer repositório e o projeto sobe na VM —
build, banco, migrations, sync, domínio — sem prompt de sync e sem decorar nada
de docker. Os comandos básicos do docker/compose existem com prefixo `flux`.

## Mecanismo

### Princípios

- **Diretório = contexto.** O CLI descobre o projeto pela pasta atual:
  `flux.toml` explícito ou detecção (nome da pasta, `compose.yaml`, `artisan`).
- **Zero prompt.** Sync é contínua (02), nunca interativa. Prompts existem só em
  ações destrutivas (`vm --recreate`, `db fresh` em prod é impossível por
  design, então nem prompt tem).
- **Progresso visível.** Comandos longos imprimem etapas (`[2/6] sync inicial…
  412 arquivos`) e terminam com o par URL + status.
- **Erros nunca silenciosos.** Erro de qualquer etapa → mensagem com origem
  (CLI/fluxd/docker) e exit code ≠ 0. Pipe de verificação sempre com exit code
  preservado.

### Comandos

| Comando | Faz |
|---|---|
| `flux up [env]` | ciclo completo do dev (padrão) — ver fluxo abaixo |
| `flux down [env]` | `docker compose stop` do projeto (containers param; banco único segue de pé) |
| `flux restart [env]` | down + up (sem rebuild) |
| `flux status [env]` | sync, containers, health, banco, migrations pendentes, URL, sono |
| `flux ps [env]` | espelho do `docker compose ps` |
| `flux logs [-f] [svc] [env]` | logs com lease de sono ativo enquanto roda |
| `flux exec <cmd…> [env]` | `docker exec` no container principal |
| `flux shell [env]` | `flux exec bash` |
| `flux build [env]` | build das imagens (sem subir) |
| `flux deploy` | modo prod: snapshot de código + build + migrate (bloqueia mutação) + health |
| `flux db status\|migrate\|fresh\|seed\|console\|dump` | banco do projeto (ver 05); `console` = cliente SQL no container `flux-db` |
| `flux sync status\|pause\|resume\|reset` | controle da sync (02) |
| `flux env pull\|push\|edit` | ciclo do `.env` exclusivo da VM (02) |
| `flux vm status\|start\|stop\|ssh\|checkpoint` | ciclo da VM (01) |
| `flux tunnel status` | saúde do cloudflared (03) |
| `flux panel` | abre o painel no browser (login único) |
| `flux setup host\|vm\|tunnel` | provisionamento (01/03/09) |

`[env]` = `dev` (default) ou `prod`. Sempre -d (detached) por padrão; `-a` anexa.

### Fluxo do `flux up` (dev), passo a passo

1. **Contexto**: lê `flux.toml`/detecta projeto; valida guard de imagens ANTES
   de qualquer rede (05).
2. **VM**: SSH ping → se falhou, pede start ao serviço `fluxhost` e espera
   (progresso). Verifica `fluxd` saudável.
3. **Registro**: envia `flux.toml` + hash dos arquivos de compose/Dockerfile ao
   `fluxd` (API de controle). Mudou? `fluxd` re-renderiza os overrides.
4. **Sync**: garante sessão mutagen do projeto (cria se falta; mostra progresso
   do full scan inicial quando é a primeira vez — depois disso, é fundo de
   poço, silencioso).
5. **Subida**: `fluxd` aplica overrides de compose (abaixo), build se hash de
   Dockerfile/compose mudou, `compose up -d`, espera health (timeout 90 s).
6. **Banco**: pipeline de migrations (05) — novo/mutado conforme ambiente.
7. **Provision**: hook `on_wake` se marcador de throttle expirou.
8. **Saída**: `https://{name}-d.bitello.xyz · dev acordado · banco em dia · sync ok`.

### Overrides de compose (aplicados pelo `fluxd`, nunca editados no repo do projeto)

- remove `ports:` de todos os services (regra de ouro);
- conecta em `flux-net` (além da rede default do compose);
- renomeia projeto para `flux-{name}-{env}` (containers `flux-{name}-{env}-app-1`…);
- injeta env de banco (`DB_HOST=flux-db` etc.) só se o service não definir;
- `restart: unless-stopping` em prod, `no` em dev (o sono controla o ciclo).

O arquivo original do projeto segue intocado — overrides são gerados em
`/srv/flux/state/compose/{name}-{env}.override.yml` e passados via `-f`.

### `flux.toml` (raiz do projeto, versionado)

```toml
name = "vippers"            # default: nome da pasta
framework = "laravel"       # detectado automaticamente; adapter v1

[domains]                   # derivados de name por padrão
# prod = "vippers.bitello.xyz"
# dev  = "vippers-d.bitello.xyz"

[db]
rebuild_policy = "auto"     # auto | always (só dev) | never
seed_on_rebuild = true
# image_guard = true        # default global true; desligar é proibido

[sleep]
dev_enabled = true
idle_minutes = 15
cpu_percent_teto = 5.0

[sync]
extra_ignore = []           # só ADICIONA ao ignore global

[provision]
on_wake = ""                # comando dentro do container, ex.: "bash /provision.sh"

[health]
path = "/up"                # path de health do app
```

Validação do `flux.toml` é estrita: chave desconhecida = erro (nada de typo
silencioso).

## Decisões

| Decisão | Alternativa descartada | Por quê |
|---|---|---|
| CLI conversa com `fluxd` por SSH+API | docker context remoto (TCP 2375/TLS) | Um caminho autenticado só; o fluxd valida guard/overrides — docker exposto direto furaria as regras |
| Overrides gerados pelo fluxd | pedir compose "correto" no repo do projeto | Projetos existentes entram sem edição; regra central vale para todos |
| `flux up` = dev, `flux deploy` = prod | mesmo comando com flag | Verbos distintos para operações com garantias distintas (05) |

## O que NUNCA fazer

- **Nunca** pedir confirmação de sync no terminal (não existe "sync após
  construir no prompt").
- **Nunca** rodar docker/compose/artisan no Windows — o CLI não exige Docker
  Desktop nem PHP local.
- **Nunca** sair do comando com exit 0 se qualquer etapa falhou.
- **Nunca** imprimir segredo (senha de banco, token) na saída — mostrar caminho
  do arquivo em vez disso.

## Critérios de aceite

1. Repo qualquer com compose: `flux up` do zero (VM ligada) → URL respondendo
   em ≤ 2 min sem input do usuário.
2. `flux logs -f` segue logs ao vivo; Ctrl+C solta; sono só retoma contagem
   depois do fim do comando.
3. `flux exec "php artisan route:list"` funciona no repo Laravel sem PHP local.
4. VM desligada + `flux up` → VM sobe sozinha e o comando completa.
5. Todo erro produzido sai com contexto e exit code correto (testar: compose
   inválido, VM off sem Hyper-V instalado, guard bloqueado).
