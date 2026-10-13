# STATUS — funcionalidades em escopo e implementação

Matriz viva do Flux: cada funcionalidade em escopo com status de implementação.
**SIM** = entregue e verificado. **PARCIAL** = núcleo existe, automação ou
integração falta. **NÃO** = só na spec.

Atualizar no fim de CADA sessão que mexer em código (mesma cadência do
[PROGRESSO](../PROGRESSO.md)). Última atualização: **13/10/2026**.

Placar atual: **8 SIM · 16 PARCIAL · 21 NÃO** (45 itens).

## Base de plataforma

| Item | Status |
|---|---|
| Workspace Rust 3 crates + CI (fmt, clippy `-D warnings`, test) | **SIM** |
| Estado SQLite (schema da 07, WAL, `schema_version`, recusa corrupção) | **SIM** |
| Router catch-all: decisão `Host → Ação` (matriz da 03) | **SIM** |
| Proxy L7 streaming (sem buffering, hop-by-hop fora) | **SIM** |
| Páginas família Flux (espera 200, "falhou", 404 brandiado, 503 JSON) | **SIM** |
| Intercept de robots/sitemap (R22/R23) | **SIM** |
| API de controle `:7420` (`/healthz`; resto 501 com causa) | **PARCIAL** — token + endpoints reais na F6 |
| Módulo Docker (bollard): estado/alvo/wake/pause/stop por labels `flux-{name}-{env}` | **PARCIAL** — testado em engine real; sem fluxo end-to-end na VM |
| CLI `flux`: árvore completa da spec 06 | **PARCIAL** — toda declarada, 100% stubs com fase e exit 2 |
| VM VirtualBox provisionada (Debian 12, Docker CE + Caddy via cloud-init) | **PARCIAL** — provisionada; boot válido pendente do reboot do host (HVCI) |

## Requisitos R1–R34

| R | Funcionalidade | Status |
|---|---|---|
| R1 | Rust | **SIM** |
| R2 | Docker em VM Debian | **PARCIAL** — VM + Docker instalados; boot não validado |
| R3 | Sono automático por interação | **NÃO** — F4 |
| R4 | Nunca 500/404: página de espera + wake | **PARCIAL** — decisão e páginas prontas; Caddy/tunnel não instalados |
| R5 | Sync Windows↔VM (mutagen) | **NÃO** — F2 |
| R6 | Banco único + migrations + guard | **PARCIAL** — lista de guard no config default; runner na F5 |
| R7 | Dev reconstrói / prod nunca | **NÃO** — F5 |
| R8 | `flux up/logs/exec/down` no Windows | **PARCIAL** — stubs com fase + exit 2 |
| R9 | Painel web | **PARCIAL** — placeholder + healthz; real na F8 |
| R10 | Prod via GitHub Actions + releases imutáveis + blue-green | **NÃO** — template em `docs/templates/` |
| R11 | Limites de recursos + hibernação de prod | **NÃO** — F10; `[queue]`/limites no schema |
| R12 | Troca de versão invisível + drain de jobs | **NÃO** — F10 |
| R13 | Cota global dev / limite individual prod | **NÃO** — F10 |
| R14 | Prod hiberna, worker 24/7 | **NÃO** — F10; ações pause/unpause existem |
| R15 | Branch routing `{branch}-{name}-d` | **PARCIAL** — parse implementado/testado; worktrees/watch na F11 |
| R16 | Gerenciador de banco (Adminer) | **NÃO** — decidido; F12 |
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
| R30 | Borda: rate limit + teto de body | **NÃO** — F14 |
| R31 | Páginas de erro fora de dev + manutenção | **PARCIAL** — família Flux pronta; 429/413/manutenção na F18 |
| R32 | Headers + WAF próprio com shadow mode | **NÃO** — F19 |
| R33 | Status público com causa classificada | **NÃO** — enum `Causa` existe no código; F20 |
| R34 | Backup banco prod 48 h/7 d | **NÃO** — F16 |

## Histórico de atualização

- **13/10/2026** — fundação de código (workspace, router, estado, páginas,
  proxy, CLI stubs), specs das decisões #1/#3/#5 + VirtualBox, VM provisionada,
  módulo Docker testado em engine real.
