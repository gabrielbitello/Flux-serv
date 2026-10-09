# 07 — fluxd (daemon da VM)

## Objetivo

`fluxd` é o único processo dono da inteligência na VM: roteamento HTTP com
wake/sleep, API de controle para o CLI, pipeline de banco, guard de imagens,
painel web e integração Caddy/cloudflared. Binário único Rust, dois modos:
`fluxd serve` (systemd) e `fluxd ctl` (chamado por SSH pelo CLI para tarefas
locais).

## Mecanismo

### Módulos (crates do workspace)

```
flux-common/   tipos compartilhados: flux.toml, protocolo CLI↔fluxd, erros
fluxd/         binário da VM
flux-cli/      binário do Windows
```

`fluxd serve` roda os serviços abaixo em um runtime tokio único:

| Módulo | Função |
|---|---|
| **router** (axum, :8080) | dispatch por `Host`; proxy hyper para o container; injeção do overlay de beat; páginas de espera/estado; catch-all para o painel em `flux.bitello.xyz` |
| **control API** (axum, :7420, bind 127.0.0.1) | REST para o CLI e painel — token obrigatório (09). Endpoints: projetos (register/up/down/status), sync, db, logs (SSE), infra (vm/tunnel/caddy), panel auth |
| **docker** (bollard) | inspect/stats/events/start/stop/exec; listener de `docker events` alimenta o re-sync do sono (04) e o cache de IPs |
| **sleep manager** | sinais de interação, leases, sweep 60 s, cooldown, decisões de stop/start (04) |
| **db manager** | ciclo do `flux-db`, users/DBs por projeto, guard de imagens, runner de migrations com lock e janela de silêncio (05) |
| **sync receiver** | inotify em `/srv/flux/projects/*` (observa migrations para a janela de silêncio e arquivos de compose para invalidar render) — a transferência em si é do mutagen |
| **infra watchers** | systemd (caddy/cloudflared/flux-db) e health; estados expostos na API e no painel |
| **panel** | UI server-side (08) embutida no binário via `rust-embed` |

### Estado (SQLite, `/srv/flux/state/flux.sqlite3`)

```sql
projects(name PK, toml TEXT, toml_hash, compose_hash, created_at)
sleep_state(project, env, last_interaction, sleeping_since, lease_until, cooldown_until)
migration_state(project, env, file, sha256, batch, applied_at, PK(project,env,file))
deploy_log(id, project, env, kind, detail_json, ok, created_at)   -- up/deploy/migrate/rebuild, com motivo
events(id, level, scope, message, created_at)                     -- trilha para o painel
settings(k PK, v)                                                 -- token de API (hash), versão do schema
```

- WAL mode; backup automático diário para `/srv/flux/backups/` (rotação 7).
- NADA de estado de projeto fora daqui + Docker + arquivos — recriar o SQLite =
  re-registro dos projetos (CLI refaz).

### Proxy HTTP (detalhe)

- `hyper` client + `axum` server; streaming bidirecional (request body e
  response body sem buffering — diferença deliberada do porteiro, que lê tudo).
- Upgrade/websocket: repassa headers `Upgrade`/`Connection` e faz pipe TCP.
- Injeção do overlay: apenas respostas `content-type: text/html` com
  `accept-encoding: identity` negociado; acima de 2 MB, passa sem injeção.
- Timeouts: connect 5 s, health-wait 90 s, idle read 300 s (SSE do app
  continua vivo).
- Log de acesso estruturado (host, método, caminho, status, duração) no
  `events` — sem corpo de resposta (o porteiro logava cauda do corpo para
  debug; isso vira modo `--trace` temporário).

### Compose e subprocessos

O `fluxd` chama o plugin `docker compose` via subprocesso para `up`/`render`
(override em `-f`), e `bollard` para o resto. Justificativa: render/merge de
compose é complexo e o plugin é a implementação de referência. Nenhum outro
shell fora daqui (exec de migrations usa `docker exec` via bollard).

### fluxd ctl (modo SSH)

Subcomandos para o CLI sem expor mais superfície HTTP: `fluxd ctl
register|render|tail-logs|dump-state`. O CLI chama via `ssh flux@vm` — canal
único, sem porta de controle aberta para a LAN.

## Decisões

| Decisão | Alternativa descartada | Por quê |
|---|---|---|
| SQLite embutido | MariaDB `flux_control` / etcd / arquivos JSON | Zero dependência de boot (o fluxd sobe ANTES do banco); um arquivo = backup trivial |
| bollard + compose plugin | tudo via shell `docker` | API estruturada (stats/events/exec) sem parse de texto; compose só onde o merge importa |
| axum | actix-web / warp | Ecossistema tokio idiomático, tower para timeouts/limits, menos boilerplate |
| Porta de controle só localhost | API na LAN | O CLI está a um SSH de distância; superfície exposta a menos |

## O que NUNCA fazer

- **Nunca** expor o :7420 fora do localhost da VM.
- **Nunca** montar `/var/run/docker.sock` em container de projeto (só o fluxd
  usa, direto do host).
- **Nunca** bufferizar response inteira no proxy (quebra SSE/upload — erro do
  porteiro que o Flux não repete).
- **Nunca** persistir segredo em texto claro no SQLite (token de API só como
  hash; credenciais de banco ficam nos arquivos de secrets, 09).
- **Nunca** tratar estado interno como verdade sobre o Docker — events + sweep
  re-sincronizam sempre.

## Critérios de aceite

1. `systemctl restart fluxd` não derruba containers nem perde registro de
   projetos (estado persistido).
2. SSE do app (ex.: devtools network) streama sem buffer de fim-de-resposta.
3. Websocket do app funciona através do router (teste com echo server).
4. `docker stop` externo → painel e API refletem em < 5 s.
5. Queda do SQLite (corrompido) → fluxd recusa começar com erro claro apontando
   o backup, nunca recria silencioso.
