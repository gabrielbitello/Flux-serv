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
| R10 | Prod sem arquivos no Windows: mora 100% na VM, publicado por GitHub Actions com template | 10 |
| R11 | Arquivos de prod imutáveis/estáticos: mais rápido, consumo igual ou menor | 10 |
| R12 | Troca de versão invisível (estilo k8s) sem perder job de background em andamento | 10 |
| R13 | Limites de recursos: cota GLOBAL somada para devs, limite INDIVIDUAL para prod | 11 |
| R14 | Prod hiberna sem o usuário perceber; tarefas agendadas (e-mail, notificação, API) continuam rodando | 11 |
| R15 | URL de dev abre a branch atual do Windows; `{branch}-{name}-d.bitello.xyz` abre branch específica baixada localmente | 02, 03 |
| R16 | Gerenciador de banco (phpmyadmin ou mais leve) | 05 |
| R17 | Toolchain (node, rust, composer…) rodando na VM sob demanda, sem instalar no PC | 06 |
| R18 | `.env` de produção armazenado cifrado localmente; painel NÃO edita dados de prod, só vê | 09, 05 |
| R19 | VM principal dorme (Hyper-V Saved) após ociosidade longa (30 dias sem acesso real) | 12 |
| R20 | Porteiro central acorda a VM e entrega a rota pedida (dev OU prod, API incluída) | 12 |
| R21 | API acorda sem quebrar app (sem resposta que buga parser, sem timeout) e prod acorda mais rápido que dev | 12 |
| R22 | Homologação (`-d`) escondida de bots: `robots.txt` `Disallow: /` sempre em dev + `X-Robots-Tag: noindex` em toda resposta de dev | 03 |
| R23 | `robots.txt`/`sitemap.xml` gerados automaticamente para prod quando o projeto não traz os arquivos; arquivo preexistente NUNCA é sobrescrito | 03, 07, 10 |

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
| `fluxhost` (serviço Windows) | Windows | **Porteiro central** (12): gerencia Start/Save da VM sem UAC + listener `:8443` que segura o tunnel quando a VM dorme e a acorda sob demanda |
| mutagen | Windows (daemon) + agente injetado na VM via SSH | Sync contínuo unidirecional Windows → VM |
| `fluxd serve` | VM | Router HTTP, wake/sleep, painel, API de controle (inclui endpoint de deploy do CI), pipeline de migrations, guard de imagens, blue-green de prod |
| `fluxd ctl` | VM (chamado por SSH) | Subcomandos de baixo nível para o CLI |
| GitHub Actions | infra do GitHub | Dispara deploy de prod por push (template na spec 10) |
| Adminer | container na VM | Gerenciador do banco único, LAN-only, linkado do painel (05) |
| sidecar de toolchain | container efêmero na VM | node/php/composer/rust sob demanda via `flux tool` (06) |
| Caddy | VM (systemd) | TLS local + catch-all → `fluxd`. Configuração estática, nunca editada por projeto |
| `cloudflared` | Windows (serviço, 12) | Tunnel `*.bitello.xyz` → `fluxhost :8443` — fica no HOST para servir mesmo com a VM dormindo |
| `flux-db` | container na VM | Única MariaDB/MySQL de todos os projetos. Nunca dorme |
| Docker Engine | VM | Roda os containers dos projetos |

## Modos dev e prod

Todo projeto tem no máximo dois ambientes, com containers, bancos e URLs separados:

| | dev | prod |
|---|---|---|
| URL | `{name}-d.bitello.xyz` (branch atual) e `{branch}-{name}-d.bitello.xyz` | `{name}.bitello.xyz` |
| Código | sync contínuo do Windows (o que está no editor, por worktree/branch) | clone git na VM, 100%; releases imutáveis; entra por GitHub Actions (10) |
| Sono | `docker stop` após ociosidade (padrão 15 min), acorda com página de espera | `docker pause` após 30 min, acorda em < 1 s, invisível (11); worker/scheduler 24/7 |
| Banco | `{name}_dev`, reconstrutível (`migrate:fresh`) | `{name}_prod`, só `migrate`, mutação de migration = erro; painel read-only |
| Recursos | cota GLOBAL somada (slice cgroup `flux-dev.slice`) | limite INDIVIDUAL por container (11) |
| Comando principal | `flux up` | `git push` (GitHub Actions → 10); rollback: `flux prod rollback` |

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
- **Nunca** editar arquivo de release de prod (imutável — mudança = commit + deploy).
- **Nunca** pausar/hibernar o worker de prod (só o HTTP); job em andamento nunca morre no deploy.
- **Nunca** deixar a soma dos devs passar da cota global (kernel garante, não contabilidade do fluxd).
- **Nunca** deixar o painel ou Adminer escrever em dados de prod (read-only por grant SQL).
- **Nunca** parar o `flux-db` (sono é só de container de app).
- **Nunca** colocar segredo em `flux.toml` nem em arquivo sincronizado.
- **Nunca** deixar host de dev indexável — em dev o `robots.txt` do projeto é
  IGNORADO, vale sempre `Disallow: /` + `X-Robots-Tag: noindex` (03).
- **Nunca** sobrescrever `robots.txt`/`sitemap.xml` que o projeto de prod já
  tem — o gerado existe só na ausência do arquivo preexistente (03/10).

## Fases de implementação (cada fase fecha com "o que testar e como")

| Fase | Entrega | Aceite resumido |
|---|---|---|
| F1 | `flux setup host` + provisionamento da VM Bookworm (01) | `flux vm status` verde; SSH com chave própria; Docker ativo na VM |
| F2 | Sync mutagen (02) | edição no Windows aparece em `/srv/flux/projects/{name}` em < 5 s; excludes respeitados |
| F3 | `fluxd` router + página de espera + wake + bots em dev (03) | container off → URL mostra página de espera 200 e sobe sozinha; `-d` responde `robots.txt` `Disallow` e header `noindex` |
| F4 | Sono automático (04) | 15 min sem interação → `docker stop`; request → acorda |
| F5 | Banco único + migrations + guard (05) | projeto novo migra sozinho; edição de migration em dev reconstrói; Dockerfile com imagem de banco é bloqueado |
| F6 | CLI completa (06) | `flux up/logs/exec/down` de qualquer repo, sem prompt; `flux tool node -- npm run build` roda na VM |
| F7 | Tunnel + domínios (03) | `{name}.bitello.xyz` e `{name}-d.bitello.xyz` resolvem pela internet |
| F8 | Painel web (08) | dashboard com status real, logs ao vivo via SSE; prod tables read-only |
| F9 | Prod: GitHub Actions + blue-green + releases imutáveis + robots/sitemap autogen (10) | deploy por push, 0 request perdido, rollback < 10 s; prod sem `robots.txt` próprio recebe gerados |
| F10 | Limites de recursos + hibernação de prod (11) | cota dev global via slice; prod hiberna em `pause`, worker 24/7 |
| F11 | Branch routing (02/03) | `{branch}-{name}-d.bitello.xyz` serve a branch local; branch só no GitHub → aviso |
| F12 | Envs de prod cifrados + Adminer (09/05) | credencial sem texto claro em disco; gerenciador de DB acessível |
| F13 | Hibernação da VM + porteiro central + wake de API (12) | VM Save/Resume sob demanda; API 503 `Retry-After` sem quebrar app; fila de wake sem explosão |

## Perguntas em aberto (decisão do Gabriel, com recomendação)

1. **Imagem do banco único.** FATO: o container atual no WSL se chama `mariadb`
   e roda a imagem `mysql:8.4` (`MYSQL_VERSION=8.4.11`) — e é NELE que moram os
   bancos reais do vippers (`vippers`, `vippers_afiliados`, `base-laravel`,
   `vippers_test_fix_g1..g8`, conferido por `SHOW DATABASES` em 09/10/2026). Os
   `vippers-dev-mysql`/`pedi-dev-mysql` (`mysql:9.3`, Exited) são sobras dokploy
   que NÃO alimentam nada hoje. Recomendação: `mysql:8.4` (paridade com o que
   realmente roda); `mariadb:11` fica como opção em config. Capacidade: com
   ~20 users/dia somados em todas as prods e ≤ 3 devs, qualquer dos dois motores
   sobra — o gargalo nunca será o motor, será conexões abertas (~50 no pico
   estimado vs `max_connections` 151 do default).
2. **Sync: engine pronta vs escrita em Rust.** RESOLVIDO em 09/10/2026: **mutagen**
   ("usa o pronto", decisão do Gabriel). Escrever sync correto (renames
   simultâneos, escrita pela metade, conflito) é um projeto inteiro e cheio de
   caso raro; reescrever em Rust só se ele virar dor operacional real.
3. **Redis no guard.** O guard barra imagens de banco no Dockerfile/compose do
   projeto. A pergunta é se `redis`/`valkey`/`memcached` (cache/fila em memória)
   também entram na lista de proibidos. Hoje nenhum projeto usa Redis (cache e
   fila Laravel rodam no driver `database`, no próprio `flux-db`); permitir
   Redis por projeto recriaria a era dokploy de N bancos. Recomendação:
   bloqueado por padrão; se um projeto precisar de verdade, avalia-se um Redis
   compartilhado único no estilo do `flux-db`.
4. **Suspend da VM inteira.** RESOLVIDO nesta versão: virou a spec
   [12](12-vm-hibernacao-porteiro-central.md) — Save-VM após 30 dias sem acesso
   real, com porteiro central no host acordando sob demanda.
5. **Gerenciador de banco.** Recomendação: Adminer (single file, ~40 MB, sem
   dependências) no lugar do phpmyadmin, restrito à LAN e linkado do painel.
   phpmyadmin fica como opção se o Gabriel preferir paridade com hoje.
