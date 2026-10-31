# CLI Reference — `flux`

Derived from the actual command tree (`crates/flux-cli/src/main.rs`, enum
`Cmd`). Context is always the **current directory**: a `flux.toml` there wins
(its `name`, not the folder's), otherwise the folder name is used with a
visible warning — the project context is never guessed silently.

Errors follow one contract: the cause goes to `stderr` with origin and the
exit code is non-zero — a failed command never exits 0, and nothing retries
or swallows.

## Project cycle

| Command | Arguments | Status | Notes |
|---|---|---|---|
| `flux up` | `[-m\|--manifest] [--seed\|--no-seed] [env]` | **Implemented** | Full dev cycle: health → register → lease (900 s, auto-renewed) → sync + overlays → compose up (≤ 600 s wait) → migrations. `-m` opens the wizard (refuses to overwrite). Env defaults to `dev`. |
| `flux down` | `[env]` | **Implemented** | Stops the compose project (`docker compose -p flux-{name}-{env} stop`); the single DB stays up. |
| `flux restart` | `[env]` | Partial | Calls `POST /projects/{name}/restart`; the fluxd backend phase is pending (501 with cause). |
| `flux status` | `[env]` | **Implemented** | Hosts, sleep state + lease expiry, applied migrations (dev/prod). Unknown project ⇒ lists registered projects, exit 1. |
| `flux ps` | `[env]` | **Implemented** | `docker compose ps` mirror grouped by project with aggregate state (ACORDADO/PARCIAL/DORMINDO). Outside a project it lists **all** managed containers. |
| `flux logs` | `[-f] [--tail N] [--since S] [--until S] [env] [service]` | **Implemented** | Default tail 200. `-f` consumes the SSE stream; an engine error arrives as a `[fluxd: erro …]` frame and fails the command (exit ≠ 0). `--since/--until/--service` warn and are **not sent** (historical window is a pending phase). Takes a keep-awake lease. |
| `flux exec` | `[env] -- <command…>` | **Implemented** | `docker exec` in `app`; the command's exit code is forwarded. |
| `flux shell` | `[env]` | **Implemented** | `flux exec [env] -- bash`. |
| `flux build` | — | **Implemented** | No argument: the build always targets the **dev** container. Waits the `[build]` silence window (default 180 s) and runs the build on the VM under the global flock — one build at a time. See [build.md](build.md). |
| `flux live` | `[env]` | **Implemented** | Toggles the HMR live server **without** a down: OFF kills the dev server, clears `public/hot` and saves the command to `.flux/live.bak`; ON restores and re-fires it. |

## Environment file

| Command | Status | Notes |
|---|---|---|
| `flux env pull` | **Implemented** | VM `.env` → local `.env` (warns if not gitignored). |
| `flux env push` | **Implemented** | Local `.env` → VM, `chmod 600`; effective on the next request, no restart needed. |
| `flux env edit` | **Implemented** | Pulls to a temp file **outside** the repo, opens `$EDITOR`/`VISUAL` (default `notepad`), pushes back on clean exit. |

## Sync

| Command | Status | Notes |
|---|---|---|
| `flux sync status` | **Implemented** | Health of the context's single session. Outside a project the folder name becomes the session with a visible warning — there is no "list all" mode (`mutagen sync list` raw output is the tool for that). |
| `flux sync pause` / `flux sync resume` | **Implemented** | Stop/restart replication. |
| `flux sync reset` | **Implemented** | Terminate + recreate with full rescan and live progress — the "sync everything now" command. |

Mechanism: [sync.md](sync.md).

## Watch

| Command | Status | Notes |
|---|---|---|
| `flux watch` | **Implemented** | Lightweight service: monitors branches/worktrees, reports state to fluxd (`POST /sync/report`) and executes the rescan queue (panel's Rescan button included). |

## Database

| Command | Status | Notes |
|---|---|---|
| `flux db fresh [env] --yes [--seed\|--no-seed]` | **Implemented** | Dev-only forced rebuild; refuses without `--yes`; prod refused by fluxd. |
| `flux db status` / `migrate` / `seed` / `console` / `dump` | Not implemented yet | Stub refuses with the target phase (exit 2). |
| `flux db restore <name> <file> --yes` | Not implemented yet | Destructive guard already enforced. |

See [database.md](database.md).

## Tokens and credentials

| Command | Status | Notes |
|---|---|---|
| `flux token generate` | **Implemented** | New 32-byte CSPRNG API token; sha256 written to the VM's SQLite via SSH, clear value printed **once** and saved to `%USERPROFILE%\.flux\config.toml`. Generating a new one revokes the old. |
| `flux project token <name>` | **Implemented** | Per-project deploy token for GitHub Secrets; sha256 in `deploy_tokens`, previous token revoked, shown **once**. |

## VM, tunnel, setup

| Command | Status | Notes |
|---|---|---|
| `flux vm ssh` | **Implemented** | Interactive SSH using the `192.168.140.2` entry in `~/.ssh/config`; exit code forwarded. |
| `flux vm status` / `start` / `stop` / `checkpoint` | Not implemented yet | Stub (target phase: VM lifecycle). |
| `flux vm upgrade` | Not implemented yet | Checkpoint + new fluxd with automatic rollback (pending). |
| `flux tunnel status` | Not implemented yet | Cloudflared health check (pending). |
| `flux setup <host\|vm\|tunnel\|uptime>` | Not implemented yet | Stub. |

## Production and deploy

| Command | Status | Notes |
|---|---|---|
| `flux prod rollback <name>` | Not implemented yet | Rollback is currently the panel/deploy switch path (see [deploy.md](deploy.md)). |
| `flux init ci` | Not implemented yet | Writes the deploy workflow from the template (`doc-old/templates/deploy-workflow.yml`) — pending; the template itself is usable by hand. |
| `flux maintenance <on\|off> <name> [--bypass]` | Not implemented yet | 503 + Retry-After mode with 24 h bypass token (pending phase). |

## Diagnostics

| Command | Status | Notes |
|---|---|---|
| `flux doctor [project]` | **Live** | Validates the WHOLE factory in one shot: fluxd alive, containers (ACORDADO/PARCIAL/DORMINDO), sync (fluxd view — stale reports over 1 h are named as "watch stopped", frozen problems quoted), own-db (state+health; stopped DB is only red when the app is awake), built assets (Laravel manifest), effective hosts, DNS resolution FROM WINDOWS (host-only IP = direct LAN; Cloudflare = green via tunnel; unresolvable = red) and the LOCAL mutagen sessions. Green/red per item, exit 1 with any red, exit 1 with cause when the fluxd is unreachable. NEVER wakes a container. Run this BEFORE hand-debugging any "why is it not working". |
| `flux doorman <host>` | **Live** | Local HTTP tap (31/10): listens on `127.0.0.1` (`--ouvir`, default 7440), prints EVERY request in full to the terminal in real time and forwards to `<host>` (`--porta`, default 80) with the PUBLIC Host preserved — Caddy routing, dev gate and the zcode-api Bearer gate keep working. Flags: `--respostas` (also show responses, SSE streams live), `--so-respostas` (responses only), `--cru` (no JSON pretty-print), `--n N` (stop after N entries), `--janela 30s\|10m\|1h` (time window). Nothing is EVER written to disk (VM or PC) — Ctrl+C ends it. THE CLIENT MUST POINT AT IT (e.g. `ANTHROPIC_BASE_URL=http://127.0.0.1:7440 opencode`) — it is a tap in the path, not a passive sniffer. Plain HTTP only (HTTPS would need MITM TLS — out of scope). Bodies are shown unredacted on purpose; sensitive HEADERS (authorization/x-api-key/cookie) are redacted. |

## Security tools

| Command | Status | Notes |
|---|---|---|
| `flux shield unban <ip>` | Not implemented yet | Unban IPs blocked by the WAF (pending phase). |
| `flux vigia status` | **Live** | Anomaly watchdog thresholds: global default + per-project overrides with the derived limits (reload/hammer/ws). |
| `flux vigia set <project> <limite>` | **Live** | Sets the reload threshold base (recargas/min, 1–600) for a project — pages that self-refresh up to 23×/min pass the 24/min default. Hammer and ws limits DERIVE from the base with floors at the incident-calibrated values. The POST also runs the watchdog cycle immediately — no fluxd restart. |
| `flux vigia reset <project>` | **Live** | Removes the override (back to the global default). |

## Tools and misc

| Command | Status | Notes |
|---|---|---|
| `flux mcp` | **Live** | MCP server (stdio JSON-RPC) so AI agents use the platform through structured tools instead of shell commands. Runs until stdin closes. |
| `flux tool <runtime> -- <cmd…>` | Not implemented yet | On-demand toolchain in an ephemeral VM container (`node\|php\|composer\|rust`). |
| `flux panel` | Not implemented yet | Opens the panel in the browser. |
| `flux export` / `flux import <file>` | Not implemented yet | DR zip (config+state+secrets, passphrase-encrypted) and re-erection on a new VM. |
| `flux self-update` | Not implemented yet | CLI self-update with verified checksum. |

## MCP (AI integration)

Two transports share ONE protocol and tool catalog (`flux-common::mcp` — the
tools table below is identical on both):

- **HTTP from inside the VM (the way it ships, 22/10 night)**: fluxd serves
  `http://192.168.140.2:7430/mcp` — LAN-only on the VirtualBox host-only IP
  (never 0.0.0.0, never Cloudflare), SAME token gate as the control API
  (fail-closed 503 / 401). The AI talks to the platform BY URL; nothing of
  the flux runs on Windows. `flux_sync_status` there is the fluxd view only
  (the local mutagen session is invisible from the VM — `flux sync status`
  on Windows for the full verdict).
- **stdio local (`flux mcp`, the CLI path)**: a local process speaking MCP
  on stdin/stdout; tools call the control API through the automatic SSH
  tunnel (the API itself stays 127.0.0.1-only on the VM). Useful without
  the LAN listener; `flux_sync_status` here includes the local mutagen
  session with the honest verdict.

Every project-scoped tool accepts `projeto` explicitly — no server infers
the project from a directory.

| Tool | Kind | Notes |
|---|---|---|
| `flux_doctor` | read | Whole-factory verdict (containers, sync, own-db, assets, hosts) — green/red per item, NEVER wakes anything. Run FIRST when something "isn't working". Without `projeto` sweeps every registered project; DNS truth lives on Windows (`flux doctor`). |
| `flux_projetos` | read | Registered projects (name, framework, hosts). |
| `flux_status` | read | Project state: containers, sleep (dev/prod), migrations, hosts. |
| `flux_ps` | read | All managed containers grouped with ACORDADO/PARCIAL/DORMINDO. |
| `flux_sync_status` | read | stdio: mutagen session health (Windows↔VM) + fluxd view with an honest `veredito`; HTTP (VM): fluxd view only. |
| `flux_db_status` | read | Own-db container, databases, per-database TTL countdown. |
| `flux_logs` | read | Log tail without follow (1–1000 lines). |
| `flux_acordar` | action | lease + `POST /up`, polls until `running` or `esperar_secs` (default 60, cap 180). |
| `flux_db_query` | action | **Read-only SQL** on the project's own-db: single statement, SELECT/SHOW/DESCRIBE/EXPLAIN only, no locking clauses — enforced by fluxd (`banco::sql_e_leitura`, `POST /projects/{name}/db/query`); output is raw `mysql -N` TSV, 1 MiB cap. |
| `flux_tool` | action | Whitelisted toolchain inside the dev container: `npm`, `npx`, `composer`, `php artisan` only; 120 s exec cap. |

Guard rules have a single owner: the write-guard for SQL lives in fluxd (the
server refuses with 422 and the cause verbatim); the client never re-implements
it. Errors surface as MCP `isError` results with the classified cause — never
a silent failure.

## Historical rationale

The full CLI design (and which phase owns each command):
[`doc-old/specs/06-cli.md`](../doc-old/specs/06-cli.md).

## Changelog

| Date | Change |
|---|---|
| 2026-10-14 | Initial English documentation |
| 2026-10-22 | `flux mcp` → Live (stdio MCP server, 9 tools) + own-db read-only SQL endpoint `POST /projects/{name}/db/query`; zcode plugin under `plugins/flux/` |
| 2026-10-22 (night) | MCP also served BY THE VM over HTTP (`:7430/mcp`, LAN-only, same token gate) — the AI connects by URL, no flux process on Windows; protocol/catalog single-sourced in `flux-common::mcp` |
| 2026-10-26 | `flux vigia status\|set\|reset` → Live — per-project reload threshold for the anomaly watchdog (default raised 8→24/min to tolerate self-refreshing pages); `GET/POST /vigia` on the control API, POST runs the cycle immediately |
| 2026-10-27 | `flux doctor [project]` → Live + MCP tool `flux_doctor` + `GET /doctor` — read-only whole-factory verdict (green/red per item; stale sync reports >1h named as "watch stopped"; never wakes anything). `[db] expose` publishes 192.168.140.2:3306 for Windows PHPUnit against the VM database |
