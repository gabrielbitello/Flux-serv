# 00 — Visão geral

## Objetivo

Flux substitui o arranjo atual da fábrica de dev servers (Docker no WSL2 + porteiro
em Rust + rsync de 4s via devfactory) por uma plataforma única:

- **VM Hyper-V Debian 12 (Bookworm)** como host dos containers, em vez de WSL2.
- **Um binário Rust na VM (`fluxd`)** que faz o papel do porteiro e vai além: roteamento
  por domínio, sono automático por interação, página de espera, gestão de banco única
  e migrations automáticas.
- **Um CLI no Windows (`flux`)** que dá os comandos de docker/compose de dentro do
  repositório, sem prompt de sync.
- **Painel web leve** servido pelo próprio `fluxd`.
- **Domínio real** `bitello.xyz` via Cloudflare Tunnel (fim dos DNS gambiarra `*.dev`).

## Requisitos de origem (rastreabilidade)

| # | Requisito do Gabriel | Spec |
|---|---|---|
| R1 | Rust | 00, 06, 07 |
| R2 | Gerencia containers Docker dentro de Hyper-V Bookworm | 01, 07 |
| R3 | Sono automático por interação direta no site (estudo de caso: porteiro) | 04 |
| R4 | Caddy + Cloudflare Tunnel no domínio, direcionamento automático, nunca 500/404 com container off — página de espera no lugar | 03 |
| R5 | Sync de arquivos Windows ↔ VM em segundos | 02 |
| R6 | 1 MariaDB só, proibido imagem de banco em Dockerfile/compose do projeto, detecção automática de migration, switches de seed/rebuild | 05 |
| R7 | Modo dev (`{name}-d.bitello.xyz`) reconstrói banco se migration editada; modo prod (`{name}.bitello.xyz`) nunca reconstrói | 05 |
| R8 | Comandos tipo docker/compose no Windows (`flux up` num repo), sem prompt de sync | 06 |
| R9 | Painel web profissional e leve | 08 |

## Arquitetura

```mermaid
flowchart LR
    subgraph Windows
        EDITOR[Editor / IDE] --> WT[árvore do repo<br/>C:\...\Projetos\{name}]
        FLUX[flux CLI]
        MUTAGEN[mutagen daemon]
        FHOST[serviço fluxhost<br/>gerencia a VM]
        WT -- sync contínuo --> MUTAGEN
        FLUX -- SSH --> VM
        FLUX --- FHOST
    end

    subgraph "Hyper-V VM (Debian 12 Bookworm)"
        CF[cloudflared] --> CADDY[Caddy :443/:80]
        CADDY -- catch-all proxy --> FLUXD[fluxd :8080<br/>router + wake + painel :7420]
        FLUXD --> APP["container do projeto<br/>({name}-dev / {name}-prod)"]
        FLUXD -- bollard (docker.sock) --> DOCK[Docker Engine]
        FLUXD -- inotify --> CODE[/srv/flux/projects/{name}/]
        APP --- CODE
        APP -- rede flux-net --> DB[(flux-db<br/>MySQL 8.4, única)]
        DOCK --- APP
        DOCK --- DB
    end

    CF -- CNAME * --> EDGE[Cloudflare edge<br/>TLS *.bitello.xyz]
    USER[Navegador] --> EDGE
    USER -. acesso LAN opcional .-> CADDY
    MUTAGEN -- rsync-safe via SSH --> CODE
```

Leitura do fluxo de uma requisição: navegador → Cloudflare edge (TLS) → CNAME
wildcard `*.bitello.xyz` → tunnel → `cloudflared` na VM → Caddy (TLS local, rota
catch-all) → `fluxd` decide por `Host`: container acordado e saudável = proxy
transparente; dormindo = acorda + página de espera (HTTP 200 com auto-refresh);
host desconhecido = página brandiada do Flux.

## Componentes e responsabilidade única

| Componente | Onde roda | Faz |
|---|---|---|
| `flux` | Windows | Comandos no repo, orquestra sync e chama a VM via SSH |
| `fluxhost` (serviço Windows) | Windows | `Start-VM`/`Stop-VM` sem UAC a cada comando; garante VM de pé no boot |
| mutagen | Windows (daemon) + agente injetado na VM via SSH | Sync contínuo unidirecional Windows → VM |
| `fluxd serve` | VM | Router HTTP, wake/sleep, painel, API de controle, pipeline de migrations, guard de imagens |
| `fluxd ctl` | VM (chamado por SSH) | Subcomandos de baixo nível para o CLI |
| Caddy | VM (systemd) | TLS local + catch-all → `fluxd`. Configuração estática, nunca editada por projeto |
| cloudflared | VM (systemd) | Tunnel `*.bitello.xyz` → Caddy |
| `flux-db` | container na VM | Única MariaDB/MySQL de todos os projetos. Nunca dorme |
| Docker Engine | VM | Roda os containers dos projetos |

## Modos dev e prod

Todo projeto tem no máximo dois ambientes, com containers, bancos e URLs separados:

| | dev | prod |
|---|---|---|
| URL | `{name}-d.bitello.xyz` | `{name}.bitello.xyz` |
| Código | sync contínuo do Windows (o que está no editor) | snapshot no `flux deploy` |
| Sono | liga após ociosidade (padrão 15 min) | desligado por padrão |
| Banco | `{name}_dev`, reconstrutível (`migrate:fresh`) | `{name}_prod`, só `migrate`, mutação de migration = erro |
| Comando principal | `flux up` | `flux deploy` |

## Stack

- Rust 2024 (edition fixada no repo), workspace com 3 crates: `flux-cli`,
  `fluxd`, `flux-common`.
- `fluxd`: `tokio` + `axum` (router/painel/API), `bollard` (Docker API via
  socket), `askama` + `htmx` + CSS próprio embutido via `rust-embed` (painel),
  `rusqlite` (estado local), `serde`/`toml` (config).
- `flux-cli`: SSH via binário `ssh` do Windows (OpenSSH nativo) + `russh` só se
  o binário provar limitação; PowerShell via `powershell.exe -Command` para
  falar com o serviço `fluxhost`.
- Sync: **mutagen** (binário externo OSS, MPL-3.0), controlado pelo CLI. Justificativa
  completa em [02-sync-arquivos.md](02-sync-arquivos.md).
- VM: Debian 12 genericcloud + cloud-init, Docker CE, Caddy e cloudflared via
  apt/repos oficiais.

## O que NUNCA fazer

- **Nunca** publicar porta de container do projeto (`ports:` no compose é removido
  por override) — quebra o roteamento, o sono e expõe serviço sem TLS. Regra de
  ouro herdada do porteiro (`docs` do devfactory).
- **Nunca** publicar a porta 3306 do `flux-db` para fora da rede docker.
- **Nunca** servir 500/404 cru para host gerenciado com container off — página de
  espera/estado, sempre HTTP 200 (ver [03](03-roteamento-dominios.md)).
- **Nunca** rodar build/artisan/composer no Windows — tudo dentro do container na VM.
- **Nunca** sincronizar `.env`, `vendor/`, `node_modules/`, artefatos de build ou
  `storage/` do Windows para a VM.
- **Nunca** reconstruir banco de prod. Sem exceção, sem flag.
- **Nunca** parar o `flux-db` (sono é só de container de app).
- **Nunca** colocar segredo em `flux.toml` nem em arquivo sincronizado.

## Fases de implementação (cada fase fecha com "o que testar e como")

| Fase | Entrega | Aceite resumido |
|---|---|---|
| F1 | `flux setup host` + provisionamento da VM Bookworm (01) | `flux vm status` verde; SSH com chave própria; Docker ativo na VM |
| F2 | Sync mutagen (02) | edição no Windows aparece em `/srv/flux/projects/{name}` em < 5 s; excludes respeitados |
| F3 | `fluxd` router + página de espera + wake (03) | container off → URL mostra página de espera 200 e sobe sozinha |
| F4 | Sono automático (04) | 15 min sem interação → `docker stop`; request → acorda |
| F5 | Banco único + migrations + guard (05) | projeto novo migra sozinho; edição de migration em dev reconstrói; Dockerfile com imagem de banco é bloqueado |
| F6 | CLI completa (06) | `flux up/logs/exec/down` de qualquer repo, sem prompt |
| F7 | Tunnel + domínios (03) | `{name}.bitello.xyz` e `{name}-d.bitello.xyz` resolvem pela internet |
| F8 | Painel web (08) | dashboard com status real, logs ao vivo via SSE |
| F9 | Modo prod completo (05/06) | `flux deploy` com bloqueio de mutação de migration |

## Perguntas em aberto (decisão do Gabriel, com recomendação)

1. **Imagem do banco única.** FATO: o container atual no WSL se chama `mariadb`
   mas roda a imagem `mysql:8.4` (`MYSQL_VERSION=8.4.11`). Recomendação: manter
   `mysql:8.4` por paridade (zero surpresa de collation/auth_plugin em projetos
   Laravel existentes); `mariadb:11` fica como opção em config.
2. **Sync nativo em Rust no futuro?** Recomendação: mutagen v1; reavaliar engine
   nativa só se mutagen virar dor operacional.
3. **Redis.** O guard bloqueia qualquer banco de dados. Redis/cache em memória
   também fica bloqueado por padrão (lista configurável em `flux.toml` global).
   Recomendação: manter bloqueado até existir demanda real.
4. **Suspend da VM inteira.** Sono v1 é por container. Suspender a VM (estado
   "Saved" do Hyper-V) quando 100% dormindo por horas é fase futura, opt-in,
   porque derruba SSH/sync/painel junto.
