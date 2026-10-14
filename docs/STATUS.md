# STATUS — funcionalidades em escopo e implementação

Matriz viva do Flux: cada funcionalidade em escopo com status de implementação.
**SIM** = entregue e verificado. **PARCIAL** = núcleo existe, automação ou
integração falta. **NÃO** = só na spec.

Atualizar no fim de CADA sessão que mexer em código (mesma cadência do
[PROGRESSO](../PROGRESSO.md)). Última atualização: **14/10/2026 (madrugada)** — PRIMEIRO PROJETO REAL: presente-digital e djm VIVOS pela internet.

Placar atual: **11 SIM · 23 PARCIAL · 12 NÃO** (46 itens — entrou o modo dev temporário).

## Base de plataforma

| Item | Status |
|---|---|
| Workspace Rust 3 crates + CI (fmt, clippy `-D warnings`, test) | **SIM** |
| Estado SQLite (schema da 07, WAL, `schema_version`, recusa corrupção) | **SIM** |
| Router catch-all: decisão `Host → Ação` (matriz da 03) | **SIM** |
| Proxy L7 streaming (sem buffering, hop-by-hop fora) | **SIM** |
| Páginas família Flux (espera 200, "falhou", 404 brandiado, 503 JSON) | **SIM** |
| Intercept de robots/sitemap (R22/R23) | **SIM** |
| API de controle `:7420` (register/status/lease/schema, token sha256 fail-closed) | **SIM** (endpoints reais + auth testados; SSE de logs na F6+) |
| Módulo Docker (bollard): estado/alvo/wake/pause/stop por labels `flux-{name}-{env}` | **PARCIAL** — testado em engine real; sem fluxo end-to-end na VM |
| CLI `flux`: árvore completa da spec 06 | **PARCIAL** — ciclo dev (up/down/status/logs/exec…) real via API com lease + forward SSH automático + token do config; `flux token generate` real (hash na VM + config local); vm/tunnel/panel/shield etc. continuam stubs |
| Docker + Caddy na VM operacionais (Debian 12, fluxd systemd, TLS catch-all) | **SIM** — boot validado, host-only 192.168.140.1/2, router proxyando container real PELO TLS do Caddy (64 ms) |
| flux-db (mariadb:11) + endpoint `/up` com override + fila de sync no `flux up` | **SIM** — banco healthy em flux-net, DBs por projeto; compose com guard re-varrido, `ports !reset`, `DB_HOST=flux-db` |
| Modo dev temporário (14): teardown após TTL sem interação | **PARCIAL** — implementado (sweep 60 s, relógio interação→created_at, purga compose+dirs+SQLite, 7 testes); expiração REAL ainda não presenciada (primeiros TTLs correm) |

## Requisitos R1–R34

| R | Funcionalidade | Status |
|---|---|---|
| R1 | Rust | **SIM** |
| R2 | Docker em VM Debian | **SIM** — VM VirtualBox operacional, Docker 29.9, router do fluxd proxyando container real |
| R3 | Sono automático por interação | **PARCIAL** — REAL nos projetos: 15 min sem interação → `docker stop` (presenciado), request → página de espera + wake (1,1 s no 2º request); falta overlay JS `/__flux/beat` (aba parada não segura) |
| R4 | Nunca 500/404: página de espera + wake | **PARCIAL** — CADEIA PÚBLICA COMPLETA pela internet (tunnel + Caddy TLS + fluxd): ghost → 404 brandiado 99 ms, painel → 303 guard, TLS válido; página de espera pela internet falta projeto real |
| R5 | Sync Windows↔VM (mutagen) | **SIM** — sessões one-way-replica reais dos 2 projetos no `flux up`; .env/storage/vendor fora por ignore e persistem na VM |
| R6 | Banco único + migrations + guard | **PARCIAL** — flux-db (mariadb:11) REAL com DBs/users dos 2 projetos, migrations rodaram por docker exec; guard re-varre no up; fresh/seed de dev ainda não exercido |
| R7 | Dev reconstrói / prod nunca | **PARCIAL** — regra implementada e testada (fresh+seed em dev, mutação em prod bloqueia); sem banco real ainda |
| R8 | `flux up/logs/exec/down` no Windows | **SIM** — `flux up` COMPLETO (register → lease → sync → compose up no fluxd com guard/override → poll do ENGINE → migrations), túnel SSH automático, wizard `-m`; provado nos 2 projetos reais |
| R9 | Painel web | **PARCIAL** — dashboard/detalhe/eventos/SVG server-side + AUTH REAL (Argon2id, sessão 12 h, rate limit 5/min, guard) montado no router público; falta SSE de logs e abas de projeto (banco/sync/domínios/deploys) |
| R10 | Prod via GitHub Actions + releases imutáveis + blue-green | **NÃO** — template em `docs/templates/` |
| R11 | Limites de recursos + hibernação de prod | **NÃO** — F10; `[queue]`/limites no schema |
| R12 | Troca de versão invisível + drain de jobs | **NÃO** — F10 |
| R13 | Cota global dev / limite individual prod | **NÃO** — F10 |
| R14 | Prod hiberna, worker 24/7 | **PARCIAL** — pause/unpause no sleep manager (`prod_mode`); worker 24/7 e pressão RAM na F10 |
| R15 | Branch routing `{branch}-{name}-d` | **PARCIAL** — parse implementado/testado; worktrees/watch na F11 |
| R16 | Gerenciador de banco (Adminer) | **NÃO** — decidido; F12 (flux-db real já no ar) |
| R17 | Toolchain sob demanda (`flux tool`) | **NÃO** — stub |
| R18 | `.env` cifrado + painel read-only | **NÃO** — F12 |
| R19 | VM Save após 30 dias | **NÃO** — `savestate` mapeado; F13 |
| R20 | Porteiro central acorda VM | **NÃO** — F13 |
| R21 | API acorda sem quebrar app | **PARCIAL** — 503 JSON + `Retry-After` testado; hold/fila na F13 |
| R22 | Homolog escondida de bots | **SIM** |
| R23 | robots/sitemap autogen em prod | **SIM** |
| R24 | Métricas + alertas + logs com rotação | **PARCIAL** — log de acesso/events no SQLite; F15 |
| R25 | DR + upgrade fluxd + boot do host | **PARCIAL** — versionamento de schema + recusa corrupção; F16 |
| R26 | Multi-stack (laravel/node/rust/static) | **PARCIAL** — enum + `[health]`/`[db] migrate_cmd`; adapters na F17 |
| R27 | Fila de builds priorizando prod | **NÃO** — config existe; F10 |
| R28 | Redes por projeto (isolamento) | **NÃO** — F14 |
| R29 | Guard de runtime | **NÃO** — F14 |
| R30 | Borda: rate limit + teto de body | **SIM** — 300/min burst 100 por host, 100 conexões, 413 cedo, 429/413 brandiados; montada no router (testes unitários; sem tráfego real até a VM) |
| R31 | Páginas de erro fora de dev + manutenção | **PARCIAL** — família Flux pronta; 429/413/manutenção na F18 |
| R32 | Headers + WAF próprio com shadow mode | **NÃO** — F19 |
| R33 | Status público com causa classificada | **NÃO** — enum `Causa` existe no código; F20 |
| R34 | Backup banco prod 48 h/7 d | **NÃO** — F16 |

## Histórico de atualização

- **14/10/2026 (madrugada)** — PRIMEIRO PROJETO REAL: **presente-digital** e
  **djm** vivos pela internet (`*-d.bitello.xyz` 200/302 pela cadeia tunnel →
  Caddy → fluxd → container). flux-db mariadb:11 no ar; composes dos 2 repos
  adaptados ao modelo Flux (service `app`, mysql/ports/tunnel/docker.sock
  fora); `flux up` COMPLETO (sync + compose + migrations); wizard `flux up
  -m`; modo dev temporário 6 h (commits `2f0d9c2`→`24ccd32`). Nos repos:
  presente-digital `a0bbf05`, djm `0b6a6ff`.

- **14/10/2026 (noite)** — TUNNEL PÚBLICO VIVO: cloudflared serviço Windows
  Running (fixes: .ps1 ASCII-only para PS 5.1; chave de EventLog órfã da era
  devfactory derrubava o service install em silencio; CNAME curinga `*`
  apontava pro tunnel velho — dashboard não cria DNS de wildcard, apontado à
  mão pro tunnel novo `c0a19fdd…`). Prova da internet: `zzz.bitello.xyz` →
  404 brandiado (99 ms), `flux.bitello.xyz` → 303 guard + login render +
  robots Disallow, TLS real válido. **F7 FECHADO.** CLI `flux.exe` no PATH
  com FORWARD SSH automático (regra NAT órfã `healthz` 7420 enganava o probe
  — deletada). Commit `e5203a3`. Fix da matriz (commit `333547a`, provado
  pela internet): browser em prod dormindo = página de espera 200 — 503 JSON
  é SÓ pra cliente de API (spec 12); `flux vm ssh` real (`2858dbe`).
- **14/10/2026** — F7: auth real do painel (Argon2id + sessão + rate limit,
  painel montado no router público, binds 127.0.0.1), Caddy estático com
  `default_sni` provado por TLS do host, mutagen 0.18.1 real (sessão a
  374 ms), cloudflared + script prontos pendendo do token do tunnel,
  `passwd -d flux`. Commit `74c8c1e`.
- **13/10/2026** — fundação de código (workspace, router, estado, páginas,
  proxy, CLI stubs), specs das decisões #1/#3/#5 + VirtualBox, VM provisionada,
  módulo Docker testado em engine real; F4 (sleep manager) e F5 núcleo (guard
  de imagens + pipeline de migrations) com testes em engine real; F2 (sync
  mutagen), F6 (API de controle + CLI real), F8 (painel server-side) e borda
  R30 construídos por workflow de 6 agentes, integrados e revisados
  (127 testes; commits `e959857`→`d6c4412`).
