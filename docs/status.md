# Implementation Status

Live matrix of every feature in scope. Vocabulary:

- **SIM** — delivered and verified.
- **PARCIAL** — the core exists; automation or integration is missing.
- **NÃO** — specified only (not implemented yet).

Baseline: the matrix in
[`doc-old/STATUS.md`](../doc-old/STATUS.md) (snapshot 14/10/2026 morning:
11 SIM · 24 PARCIAL · 11 NÃO), updated with the real repository state and
this session's deliveries. Scoreboard for **this** matrix:
**19 SIM · 17 PARCIAL · 11 NÃO** (47 items) → **21 SIM · 15 PARCIAL · 11 NÃO** after 2026-10-18/19 (R21 and R4 moved) → **22 SIM · 15 PARCIAL · 11 NÃO** after 2026-10-21 (redroom).

## Platform base

| Item | Status |
|---|---|
| Rust workspace, 3 crates + CI (fmt, clippy `-D warnings`, test) | **SIM** |
| SQLite state (versioned schema, WAL, corruption refused) | **SIM** |
| Catch-all router: `Host → action` decision machine | **SIM** |
| Redroom global containment (`flux redroom on\|off\|status`; static 503 page behind the WHOLE router — CF and LAN, managed or unknown hosts, public panel and deploy route; sole exception = control-plane project `zcode-api` per AGENTS.md "never kill"; toggle = `POST /redroom` behind token, event trail; flag survives fluxd restart via `settings`) | **SIM** — delivered and ACTIVATED 21/10/2026 (CVE-2026-109618/109619 "php2shell" panic switch); proven: CF edge 503 page (djm-d, www), LAN 503 page, `zcode-api-d` still answering (401 gate at Caddy, real proxy body past the router), `/__flux/beat` 204 |
| L7 streaming proxy (no buffering; sleep-overlay injection for eligible HTML) | **SIM** |
| Flux pages family (waiting page 200, branded 404/503, JSON for API clients) | **SIM** |
| robots/sitemap intercept (dev hidden, prod autogen) | **SIM** |
| Control API `:7420` (register/status/lease/schema/up/switch/fresh/down/logs/exec/containers; sha256 fail-closed; logs SSE real) | **SIM** |
| Docker module (bollard; wake/pause/stop by `flux-{name}-{env}` labels) | **PARCIAL** — tested on a real engine and running real projects; broader engine-failure paths still to exercise |
| CLI `flux` (full spec tree) | **PARCIAL** — complete dev cycle (up/down/restart*/status/ps/logs/exec/shell/build/live), `env pull/push/edit`, `sync status/pause/resume/reset`, `watch`, tokens, `vm ssh`; `vm/tunnel/panel/shield/maintenance/tool/export/import/self-update` remain stubs that refuse with the target phase (*restart answers 501 until the backend phase) |
| VM operational (Debian 12, Docker, Caddy TLS catch-all, fluxd systemd; host-only 192.168.140.1/2) | **SIM** |
| Adminer LAN-only + panel with password (Argon2id, login proven; `viewer` SELECT-only user) | **SIM** |
| Single `flux-db` (mariadb:11) + `/up` health + compose overrides (`ports !reset`, `DB_HOST=flux-db`, image guard rescan) | **SIM** |
| Temporary dev mode (teardown after TTL without interaction; 60 s sweep) | **PARCIAL** — implemented and tested (7 tests); a real expiry has not been witnessed live yet |

## Requirements R1–R34

| R | Feature | Status |
|---|---|---|
| R1 | Rust | **SIM** |
| R2 | Docker inside a Debian VM | **SIM** — VirtualBox VM operational, router proxying real containers |
| R3 | Interaction-based auto-sleep | **SIM** — slept (15 min), woke (~1.1 s), CLI lease, CPU sweep, and the injected `/__flux/beat` overlay (30 s throttle) proven end to end |
| R4 | Never a raw 500/404: waiting page + wake | **SIM** — full public chain proven through the internet, including the waiting page for a REAL sleeping project captured on 2026-10-19 (`https://djm-d.bitello.xyz/` with djm asleep: 200 + `x-robots-tag: noindex` + branding), plus branded 404 for unknown hosts |
| R5 | Windows↔VM sync (mutagen) | **SIM** — real `one-way-replica` sessions; ignores hold (`.env`/`storage`/`vendor` stay VM-side); extended with `flux sync status/pause/resume/reset` + `flux watch` (commit `89886e0`), rescan queue and live panel state, and the `[sync] use_dockerignore` opt-in (2026-10-17) — unit/wiring-verified; the live `terminate + create` rescan round-trip through the panel button is still to be witnessed |
| R6 | Single DB + migrations + guard | **PARCIAL** — pipeline complete and in the `up` path (real migrations, silence window, guard rescan, seed precedence, never in prod); the collection now filters by the sync ignores (GLOBAL + `[sync]`), so a file outside the sync can no longer be recorded as applied (commit `44f7373`, 2026-10-18 — found during the real vippers homolog repair); `fresh` not yet exercised live; `db status/dump/restore/console` pending |
| R7 | Dev rebuilds / prod never | **PARCIAL** — rule implemented and tested (fresh+seed in dev; mutation blocked in prod); `[db] rebuild_policy` (auto/always/never) now honored in dev; not yet against a large real DB |
| R8 | `flux up/logs/exec/down` on Windows | **SIM** — plus `env pull/push/edit`, `build`, `live` toggle; automatic SSH forward; proven on real projects |
| R9 | Web panel | **PARCIAL** — auth + dashboard/detail/events + `/observe` (24 h, filters, peak forensics) + live sync state with Rescan button (this session); log SSE view and database/sync/domains tabs pending |
| R10 | Prod via GitHub Actions + immutable releases + blue-green | **PARCIAL** — public deploy route (default off) with per-project tokens, own rate limit 10/min burst 10, 409 guard, background pipeline (fetch + ancestry guard + immutable release dir via `git worktree`, 120 s git timeouts, full trail). Gaps: CI step that builds the image and raises the inactive color, commanded rollback (`flux prod rollback`), release rotation |
| R11 | Resource limits + prod hibernation | **NÃO** — F10 |
| R12 | Invisible version switch + drain | **PARCIAL** — mechanism proven live (blue→green switch on the same domain, `stop -t` drain, switch-back rollback) and now gated by the REAL `[health] path` probe (switch aborts if the new color does not answer); the CI that raises the inactive color is pending |
| R13 | Global dev quota / per-project prod limits | **NÃO** — F10 |
| R14 | Prod sleeps, worker 24/7 | **PARCIAL** — pause/unpause in the sleep manager (`prod_mode`); 24/7 worker handling pending |
| R15 | Branch routing `{branch}-{name}-d` | **PARCIAL** — parsing implemented/tested; worktrees/watch integration pending |
| R16 | Database manager (Adminer) | **SIM** — LAN-only container, pinned to `flux-db`, linked in the panel |
| R17 | On-demand toolchain (`flux tool`) | **NÃO** — stub |
| R18 | Encrypted `.env` + panel read-only | **NÃO** — F12 |
| R19 | VM save after 30 days | **NÃO** — `savestate` mapped; F13 |
| R20 | Central warden wakes the VM | **NÃO** — F13 |
| R21 | API wakes without breaking apps | **SIM** — `[sleep] api_hold` (opt-in per project): the router holds the API request while the project wakes, polls the TCP port, re-decides (90 s cap) — never a 503 for opted projects; proven live with zcode-api (wake 0.85 s, `/v1/messages` 200); commit `cbfe02f`. Non-opted projects keep the 503 JSON + `Retry-After` |
| R22 | Staging hidden from bots | **SIM** |
| R23 | robots/sitemap autogen in prod | **SIM** |
| R24 | Metrics + alerts + log rotation | **PARCIAL** — 30 s sampler → 1 min rollup, 24 h retention, peak forensics, `/observe` page; outbound alerts and long curves pending |
| R25 | DR + fluxd upgrade + host boot | **PARCIAL** — schema versioning + corruption refusal; `vm upgrade`, `export/import` pending |
| R26 | Multi-stack (laravel/node/rust/static) | **PARCIAL** — enum + `[health]`/`[db] migrate_cmd` contract; adapters pending |
| R27 | Build queue prioritizing prod | **NÃO** — config exists; F10 (the `flux build` flock is per-host, not the full queue) |
| R28 | Per-project networks (isolation) | **NÃO** — F14 |
| R29 | Runtime guard | **NÃO** — F14 |
| R30 | Edge: rate limit + body cap | **SIM** — 300/min burst 100 per host, 100 connections, early 413, branded 429/413 |
| R31 | Error pages outside dev + maintenance | **PARCIAL** — Flux family ready; maintenance mode + bypass pending |
| R32 | Headers + own WAF | **PARCIAL** — SQLi + path scan in **enforce** 403 with LAN allowlist (proven: internet 403, LAN 200); headers and bad bots pending |
| R33 | Public status page with classified cause | **NÃO** — `Cause` enum exists; F20 |
| R34 | Prod DB backup (48 h / 7 d) | **NÃO** — F16 |

## Delivered 2026-10-18/19

| Delivery | Where |
|---|---|
| `[sleep] api_hold` — API requests held while the project wakes (never a 503; 90 s cap) → R21 **SIM** | commit `cbfe02f`; `crates/fluxd/src/router.rs`; [04-adormecimento](../doc-old/specs/04-adormecimento.md) |
| zcode-api deployed on the platform: LAN-only host (no Cloudflare), credentials outside the sync tree (`/srv/flux/secrets/`), wake 0.85 s proven | Projetos/zcode-api; [03-roteamento-dominios](../doc-old/specs/03-roteamento-dominios.md) |
| VM RAM 6144 → 6644 MB (zcode-api headroom) | [01-vm-hyperv](../doc-old/specs/01-vm-hyperv.md) |
| Toolchain pinned (`rust-toolchain.toml`, 1.92.0) — fmt/clippy no longer drift with the host toolchain | branch `audit/modularizacao`; commit from the refactoring batch |
| CI: `test-engine` job (dind + `FLUX_DOCKER_URL`) — the 3 Docker-gated tests now run for real, no longer passing empty | branch `audit/modularizacao`; `.github/workflows/ci.yml` |
| Modularization refactoring EXECUTED IN FULL and RUNTIME-VALIDATED: 41/41 plan steps; complete CLI matrix (up/exec/build/env/live/token/logs SSE) + fluxd deployed on the VM (backup kept) + public-chain regression (waiting page 200 noindex, branded 404, WAF 403, panel login, observe) all green on the new binaries | branch `audit/modularizacao`; [refactoring/execution-plan.md](refactoring/execution-plan.md) |

## Delivered in this session (2026-10-17)

| Delivery | Where |
|---|---|
| `[sync] use_dockerignore` + `sync.dockerignore_path` (opt-in `.dockerignore` in the session ignores; file missing with the toggle on is a hard error) | [sync.md](sync.md); `ignores_do_projeto`, `crates/flux-cli/src/sync.rs` |

## Delivered on 2026-10-14

These entered the matrix above as **SIM**:

| Delivery | Where |
|---|---|
| Real migrations inside `flux up` (CLI collects `database/migrations/*.php` + sha256) | commit `3b96141`; [database.md](database.md) |
| `flux env pull/push/edit` (VM-exclusive `.env`) | commit `3b96141`; [sync.md](sync.md) |
| Automatic infra strip on `up` + per-project service/workdir | commit `7305ffe` |
| Project file overlays (`[dev] overlays`, `.flux/overlays/**` over the VM copy) | commit `97914cb` |
| `vite-{dev}` host routed to the dev server (HMR through the tunnel) | commit `63418aa` |
| Live server toggle without downtime (`flux live`, `.flux/live.bak`) | commit `3d0199b` |
| `[db] silence_window_secs` (default 5 s, double-hash read on the Windows side) | commit `89886e0`; [database.md](database.md) |
| `[build]` section + `flux build` (silence window 180 s, global flock on the VM) | commit `89886e0`; [build.md](build.md) |
| `flux sync status/pause/resume/reset` + `flux watch` (`POST /sync/report`, rescan queue consumer) | commit `89886e0`; [sync.md](sync.md) |
| Panel: live sync state + Rescan button (`POST /projects/{name}/rescan`) | commit `66a22e5`; [panel.md](panel.md) |
| `[sync] use_dockerignore` + `dockerignore_path` (opt-in `.dockerignore` in the session ignores) | [sync.md](sync.md) |
| Dead keys now real: `[domains]` (validated extra host, exact-match routing, state v4), `[db] rebuild_policy` (auto/always/never in dev), `[health] path` (real probe: blue-green gate + `awake` vs `waking` in `flux up`), `[branding] logo_url` (wait/failed pages) | [configuration.md](configuration.md); `acao_dev`/`saude_ok`/`hosts_custom` unit-tested |
| Documentation rebuilt in English (this docs/ tree; legacy preserved in `doc-old/`) | this file's repo area |

Verification level: the three code fronts land with
`cargo test -p flux-common -p flux-cli -p fluxd` = **317 passed, 0 failed**
(independent review run), plus read-only CLI smokes; the mutating paths
(`flux build`, `flux sync reset`, the panel rescan round-trip) are covered by
unit/wiring tests and still await a live exercise against the VM.
`cargo test --workspace` runs afterwards as the final gate.

## How to update this file

After any session that changes code: move rows honestly, add the delivery
to the session list with its commit, recompute the scoreboard. The old
matrix and its full history live in
[`doc-old/STATUS.md`](../doc-old/STATUS.md).

## Changelog

| Date | Change |
|---|---|
| 2026-10-14 | Initial English documentation (matrix rebased on doc-old/STATUS.md + this session's deliveries) |
| 2026-10-14 | Post-session pass: new entries anchored to their commits (`89886e0`, `66a22e5`); verification level recorded |
| 2026-10-18/19 | R21 → SIM (`api_hold`, `cbfe02f`); R4 → SIM (real waiting page captured); zcode-api on the platform (LAN-only, secrets out of the sync tree); refactoring 41/41 executed, master merged in, pushed as PR #1, CI green (test + test-engine), CLI + fluxd runtime-validated end to end |
