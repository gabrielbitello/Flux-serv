# Inventário — 53 itens in-scope

Linhas medidas por leitura direta no lote de auditoria (podem divergir ±1 de `wc -l` por convenção de newline final). "Lentes" = auditores que cobriram o arquivo. "Decisão" = síntese do veredito estrutural; o detalhe está em [file-audit.md](file-audit.md) (âncoras `FA-xx`).

## crates/flux-cli (binário `flux`, só `[[bin]`, sem `[lib]`)

| # | Caminho | Linhas | Responsabilidade | Lentes | Decisão | Achado |
|---|---|---:|---|---|---|---|
| 1 | `crates/flux-cli/src/main.rs` | 2575 | Árvore clap + dispatch + TODOS os handlers de comando (up/sync/build/watch/env/live/tokens) | auditor-cli; foco-cli-main (integral) | **Dividir** — 9 domínios de comando em `src/comandos/`; parsing/dispatch ficam | FA-01 |
| 2 | `crates/flux-cli/src/sync.rs` | 1287 | Orquestração mutagen (spec, exec, ciclo de vida, parse de status) + estabilidade de árvore git | auditor-cli; foco-cli-sync (integral) | **Dividir parcialmente** — `sync/{mod,spec,estabilidade}.rs`; ponte mutagen fica inteira no `mod.rs` | FA-02 |
| 3 | `crates/flux-cli/src/api.rs` | 1225 | Cliente HTTP da API de controle (:7420), DTOs espelho, SSE, LeaseGuard, mock axum de teste | auditor-cli; foco-cli-api (integral) | **Coeso** — só extrair bloco de testes para `api/tests.rs` | FA-03 |
| 4 | `crates/flux-cli/tests/sync_tests.rs` | 670 | 24 testes da sync (21 puros + 3 gated `MUTAGEN_BIN`), ponte `#[path]` permanente | auditor-testes | **Permanece** — corrigir doc do cabeçalho (ponte é permanente) | FA-04 |
| 5 | `crates/flux-cli/src/tunel.rs` | 240 | Túnel SSH `ssh -L` sob demanda + seção `[api]` do config global do usuário | auditor-cli | **São** — vira fonte única de `VM_HOST`/`VM_USER` | FA-05 |
| 6 | `crates/flux-cli/src/wizard.rs` | 190 | Wizard `flux up -m`: perguntas stdin → `flux.toml`, recusa sobrescrever | auditor-cli | **São com buraco** — extrai montagem pura do TOML (health cru quebra o parse) | FA-06 |

## crates/fluxd (daemon; lib + bin)

| # | Caminho | Linhas | Responsabilidade | Lentes | Decisão | Achado |
|---|---|---:|---|---|---|---|
| 7 | `crates/fluxd/src/panel.rs` | 1986 | Painel web: Router + guard, leitura SQL, formatação, render HTML/SVG, 23 testes e2e | auditor-painel-observe; foco-painel (integral) | **Dividir** — `panel/{mod,formato,leitura,paginas,tests}.rs` nas seams existentes | FA-07 |
| 8 | `crates/fluxd/src/api.rs` | 1674 | API de controle :7420: gate Bearer fail-closed + handlers + e2e | auditor-painel-observe; foco-api-fluxd (integral) | **Quase coesa** — extrair cluster de auth (`api/auth.rs`) e política awake/waking (→ docker.rs); NÃO fatiar handlers | FA-08 |
| 9 | `crates/fluxd/src/compose.rs` | 1384 | Override YAML + up em background + live server + túnel + **blue-green de prod** | auditor-dados; foco-compose (integral) | **Dividir** — blue-green sai para `bluegreen.rs`; depois `classificar_de` pura obrigatória; split up/live opcional | FA-09 |
| 10 | `crates/fluxd/src/observe_page.rs` | 1284 | Página /observe: filtros GET, curvas SVG, tabela de picos — consumidor puro de `observe` | auditor-painel-observe; foco-observe-page (integral) | **Permanece** — deduplicar núcleo visual com panel (ui.rs) e decompor `render_monitor` in-file | FA-10 |
| 11 | `crates/fluxd/src/observe.rs` | 1106 | Sampler de recursos: bollard 30 s → rollup 1-min SQLite → picos → consultas do painel | auditor-painel-observe; foco-observe (integral) | **Permanece** (defer) — coeso; gatilhos de reavaliação documentados | FA-11 |
| 12 | `crates/fluxd/src/state.rs` | 1038 | Fachada única do SQLite de estado: schema v1→v4, mutex, 25+ métodos por domínio | auditor-nucleo (leve); auditor-dados; foco-state (integral) | **Permanece** (defer) — inventário schema↔purge é load-bearing no mesmo arquivo | FA-12 |
| 13 | `crates/fluxd/src/ciclo.rs` | 997 | Handlers down/logs/exec do ciclo de dev, contrato CLI↔fluxd documentado | auditor-nucleo | **Permanece** — grande porém coeso; deduplicar helpers `erro`/`garante_projeto` | FA-13 |
| 14 | `crates/fluxd/src/deploy.rs` | 918 | Rota pública de deploy: gate sequencial + pipeline git/worktree + trilha | auditor-painel-observe | **Permanece** — gate e pipeline são fases do mesmo fluxo | FA-14 |
| 15 | `crates/fluxd/src/edge.rs` | 910 | Camada tower de admissão: 413/429/concorrência por host, respostas brandiadas | auditor-rede | **Permanece** — uma preocupação (a borda); CSS deve importar de pages.rs | FA-15 |
| 16 | `crates/fluxd/src/waf.rs` | 905 | WAF local: detecção substring, IP real (cf-connecting-ip→XFF→ConnectInfo), allowlist fail-closed | auditor-rede | **Permanece** — idem CSS; nota de segurança registrada (ver AF-20) | FA-16 |
| 17 | `crates/fluxd/src/decision.rs` | 761 | Máquina de decisão pura do router: Host+path+estado → Action, `HostSource` injetável | auditor-nucleo | **Permanece** — fronteira exemplar; só unificar validador de nome | FA-17 |
| 18 | `crates/fluxd/src/db.rs` | 555 | Pipeline de migrations/seed DENTRO dos containers (docker exec) — nome enganoso | auditor-dados | **Permanece + renomear** — `db.rs` → `migrations.rs`; `exec_in_container` sai para docker.rs | FA-18 |
| 19 | `crates/fluxd/src/proxy.rs` | 537 | Forward L7 streaming + túnel ws + injeção do overlay de sono | auditor-rede | **Permanece** — extrair montagem de request compartilhada (helper privado) | FA-19 |
| 20 | `crates/fluxd/src/router.rs` | 528 | Router catch-all :8080 + despacho único de `decision::Action` | auditor-rede | **Permanece** — despacho único é a matriz da spec 03 | FA-20 |
| 21 | `crates/fluxd/src/docker.rs` | 395 | Adaptador Docker: descoberta por label, ContainerState, ações, DockerSource | auditor-dados | **Reformar por dentro** — centralizar listagem/exec; delegar SQLite ao SqliteSource (erros engolidos hoje) | FA-21 |
| 22 | `crates/fluxd/src/temporary.rs` | 391 | Dev temporário: TTL por ociosidade + teardown ordenado | auditor-nucleo | **Permanece** — só a const de layout sai (ou vira delegação) e a doc se corrige | FA-22 |
| 23 | `crates/fluxd/src/sleep.rs` | 353 | Política de sono: decisão pura + SleepManager com sweep de 60 s | auditor-nucleo | **Permanece** — remover variante morta `Busy` | FA-23 |
| 24 | `crates/fluxd/src/pages.rs` | 338 | Respostas brandiadas: wait/failed/branch/404/503/robots/sitemap | auditor-rede | **Permanece + conserto** — interpolação sem escape (XSS latente, pré-F11); vira dono do croma | FA-24 |
| 25 | `crates/fluxd/src/panel_auth.rs` | 306 | Sessões em memória, rate limit de login, Argon2id, cookie — sem handlers HTTP | auditor-painel-observe | **Permanece** — rebaixar `hash_path` para privado | FA-25 |
| 26 | `crates/fluxd/src/sync.rs` | 306 | Estado de sync (mutagen) no fluxd: upsert, leitura, fila de rescan | auditor-painel-observe | **Permanece** | FA-26 |
| 27 | `crates/fluxd/src/guard.rs` | 262 | Guard de imagens (compose `image:` / Dockerfile `FROM`) vs lista bloqueada | auditor-rede | **Permanece** — puras com testes de caracterização embutidos | FA-27 |
| 28 | `crates/fluxd/src/lib.rs` | 198 | AppState + `serve()`: composition root com loop de sweep embutido | auditor-nucleo | **Extrair loop** — sweep de ciclo de vida para `sweep.rs` (padrão `observe::iniciar`) | FA-28 |
| 29 | `crates/fluxd/src/beat.rs` | 178 | POST /__flux/beat + SCRIPT JS do overlay | auditor-nucleo | **Permanece** | FA-29 |
| 30 | `crates/fluxd/src/sqlite_source.rs` | 113 | `HostSource` sobre o SQLite do estado, com degradação logada | auditor-dados | **Permanece** — vira dono único das queries copiadas em docker.rs | FA-30 |
| 31 | `crates/fluxd/src/main.rs` | 88 | Binário fino: clap + tracing + tokio → `fluxd::serve` | auditor-nucleo | **Permanece** — fino como declara | FA-31 |
| 32 | `crates/fluxd/src/events.rs` | 23 | Trilha de eventos no SQLite, falha visível em tracing::error | auditor-nucleo | **Permanece** | FA-32 |
| 33 | `crates/fluxd/tests/integration.rs` | 227 | 9 testes da cadeia decisão→proxy→páginas sobre HTTP real | auditor-testes | **Permanece** — ganhar asserts de `x-robots-tag` (R22) | FA-33 |
| 34 | `crates/fluxd/tests/deploy_integration.rs` | 313 | 4 testes da rota de deploy: auth, rate, 409, guard de descendência com git real | auditor-testes | **Permanece** | FA-34 |
| 35 | `crates/fluxd/tests/state_migration_integration.rs` | 179 | Migrador de schema: v1 real em arquivo → versão atual; recusa de versão adulterada | auditor-testes | **Permanece** — comentário de "v2" velho num cleanup qualquer | FA-35 |
| 36 | `crates/fluxd/tests/schema_integration.rs` | 115 | `ensure_schema` com engine real (exec no container, prova por arquivo) | auditor-testes | **Permanece** — gated `FLUX_DOCKER_URL`, vazio no CI (ver L0) | FA-36 |
| 37 | `crates/fluxd/tests/sleep_integration.rs` | 83 | Sweep do SleepManager com engine real (cooldown, idle→dorme) | auditor-testes | **Permanece** — gated, vazio no CI (ver L0) | FA-37 |
| 38 | `crates/fluxd/tests/docker_integration.rs` | 78 | Ciclo completo do DockerSource com engine real | auditor-testes | **Permanece** — gated, vazio no CI (ver L0) | FA-38 |

## crates/flux-common (crate base compartilhado)

| # | Caminho | Linhas | Responsabilidade | Lentes | Decisão | Achado |
|---|---|---:|---|---|---|---|
| 39 | `crates/flux-common/src/config.rs` | 965 | Schemas TOML (ProjectToml 16 seções, GlobalConfig 12) + validação de hosts custom | auditor-common | **Permanece** — `hosts_custom` migra para host.rs; `[db].image_guard` sem leitor sai | FA-39 |
| 40 | `crates/flux-common/src/host.rs` | 313 | Modelo ManagedHost + decodificação pura do header Host | auditor-common | **Permanece** — recebe as regras de validação de hosts custom | FA-40 |
| 41 | `crates/flux-common/src/protocol.rs` | 152 | Tipos do protocolo CLI↔API :7420 (Env, ContainerState, corpos fixos) | auditor-common | **Permanece** | FA-41 |
| 42 | `crates/flux-common/src/framework.rs` | 72 | Enum Framework com FromStr/Display | auditor-common | **Permanece** — vira fonte única de validação no wizard | FA-42 |
| 43 | `crates/flux-common/src/error.rs` | 68 | Taxonomia FluxError; `Causa` sem consumidor | auditor-common | **Permanece** — remover (ou marcar) `Causa` | FA-43 |
| 44 | `crates/flux-common/src/lib.rs` | 10 | Raiz do crate: 5 módulos + fronteira documentada | auditor-common; auditor-testes | **Permanece** — dep `sha2` morta sai do manifest | FA-44 |

## Manifestos, CI e scripts (não-.rs in-scope)

| # | Caminho | Linhas | Responsabilidade | Lentes | Decisão | Achado |
|---|---|---:|---|---|---|---|
| 45 | `.github/workflows/ci.yml` | 28 | Pipeline: fmt + clippy `-D warnings` + `cargo test --workspace`, job único ubuntu-latest | auditor-testes; editor-plano (leitura direta desta sessão) | **Ativar gates** — Docker e Mutagen nunca definidos → testes passam vazios (P1) | FA-45 |
| 46 | `Cargo.toml` (raiz) | — | Workspace: 3 members, deps centralizadas em `[workspace.dependencies]` | auditor-testes | **Permanece** — sem `[alias]`, sem `[profile.*]` | FA-46 |
| 47 | `crates/fluxd/Cargo.toml` | — | Manifest do fluxd: lib+bin, runtime + dev-deps | auditor-testes | **Permanece** — 3 dev-deps redundantes (inofensivo, registro) | FA-47 |
| 48 | `crates/flux-cli/Cargo.toml` | — | Manifest do CLI: só `[[bin]]` — torna permanente a ponte `#[path]` do sync_tests | auditor-testes | **Permanece** — criar `[lib]` seria decisão estrutural maior, não proposta | FA-48 |
| 49 | `crates/flux-common/Cargo.toml` | — | Manifest do common: serde/toml/thiserror/**sha2 morto** | auditor-common; auditor-testes | **Remover sha2** (verificado nesta sessão: `git grep` só acha o manifest) | FA-49 |
| 50 | `flux.toml.example` | — | Template user-facing do flux.toml (16 seções) | auditor-common | **Remover bloco image_guard** — documenta chave sem leitor | FA-50 |
| 51 | `scripts/host/README.md` | — | Referência manual da VM (doc com trechos WSL históricos) | auditor-testes | **Permanece** — doc desatualizada registrada | FA-51 |
| 52 | `scripts/host/install-cloudflared.ps1` | — | Instala o serviço cloudflared no host Windows | auditor-testes | **Permanece** — token como arg de CLI registrado, sem proposta | FA-51 |
| 53 | `scripts/host/install-fluxd.sh` | — | Instala fluxd + unit systemd na VM; casa com binds 127.0.0.1 | auditor-testes | **Permanece** — sólido (`set -euo pipefail`, idempotente) | FA-51 |

**Total: 53 itens — 44 `.rs` + 9 não-`.rs`. Não auditado: 0.** Detalhe de cobertura por revisor em [coverage-manifest.md](coverage-manifest.md).
