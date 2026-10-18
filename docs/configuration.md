# Configuration

Two files, both strict TOML: the per-project `flux.toml` (versioned, never
contains secrets) and the per-machine global config
(`%USERPROFILE%\.flux\config.toml` on Windows). Both parse with
`deny_unknown_fields`: **an unknown key is a hard error** — a typo never
degrades into a silent default (`crates/flux-common/src/config.rs`).

## `flux.toml` (project root)

Full example:

```toml
name = "vippers"            # optional; defaults to the folder name
framework = "laravel"       # laravel | node | rust | static | custom

[domains]
prod = "vippers.bitello.xyz"   # optional overrides of the derived hosts
dev  = "vippers-d.bitello.xyz"

[db]
rebuild_policy      = "auto"   # auto | always | never (dev only; prod NEVER rebuilds)
seed_on_rebuild     = true     # seed after a dev rebuild
migrate_cmd         = ""       # node/rust: schema command inside the container
silence_window_secs = 5        # quiet window before migrations run (see below)

[build]                        # frontend/assets build run on the VM
command             = "npm run build"
service             = "app"    # compose service where the command runs
silence_window_secs = 180      # quiet window before the build fires

[sleep]
dev_enabled        = true
idle_minutes       = 15
prod_mode          = "pause"   # pause | stop | off
prod_idle_minutes  = 30
cpu_percent_teto   = 5.0       # CPU above this keeps the project awake

[resources]                    # prod-only per-project limits
http_memory_mb   = 512
http_cpus        = 1.0
worker_memory_mb = 256
worker_cpus      = 0.5

[sync]
extra_ignore = ["resources/svg/**"]   # ADDS to the mandatory global ignores
use_dockerignore    = false           # also apply patterns from .dockerignore
dockerignore_path   = ".dockerignore" # where that file lives (repo-relative)

[temporary]                    # self-destructing dev project
enabled   = false              # default false — nothing self-destructs by accident
ttl_hours = 6                  # hours without ANY interaction before teardown

[provision]
on_wake = "bash /provision.sh"   # hook run on wake

[branding]
logo_url = "https://vippers.bitello.xyz/logo.png"   # maintenance page logo

[shield]
auth_paths = ["/login"]        # paths the anti-brute-force watches
tarpit     = false

[health]
path = "/up"                   # default /up; node/rust MUST declare it

[dev]
live     = "npm run dev -- --host 0.0.0.0"  # HMR server run INSIDE the app container after a successful dev up
overlays = true                   # apply .flux/overlays/** over the VM copy on every `flux up`

[limits]
rate = 500                      # per-host rate limit override (edge)
```

### Key-by-key behavior

| Section / key | Default | What does with it |
|---|---|---|
| `name` | folder name | Compose project `flux-{name}-{env}`, sync session `flux-{name}`, hosts `{name}.bitello.xyz` / `{name}-d.bitello.xyz`. `[a-z0-9-]` only at registration. |
| `framework` | `laravel` | Validation of `[health] path` (node/rust must declare) and `[db] migrate_cmd` (empty ⇒ warning, never silence). |
| `domains.prod` / `domains.dev` | derived from global zone | Extra host added to the project (the derived hosts KEEP routing — this adds, never removes). Must stay inside `{zone}` (the tunnel covers `*.{zone}`), cannot collide with the panel host, cannot start with `vite-`, and the label cannot end with the `dev_suffix`. Validated at registration (422 with the key named); a host already owned by another project is refused with its owner. Routing is exact-match: worktree branches do not apply to custom hosts. |
| `db.rebuild_policy` | `auto` | Dev only — prod never rebuilds regardless. `auto`: rebuild when the migration hash set changed; `always`: rebuild on every dev up; `never`: the schema pipeline is skipped with a visible event counting the pending migrations (`flux db fresh` or a policy change applies them). Node/rust `migrate_cmd` is not affected by this policy. |
| `db.seed_on_rebuild` | `true` | CLI flags `--seed`/`--no-seed` win over this; prod never seeds regardless. |
| `db.migrate_cmd` | empty | node/rust schema command executed via `docker exec`; empty means no automatic pipeline (fluxd warns). |
| `db.silence_window_secs` | **5** | Quiet window before the migration pipeline fires: the CLI re-reads the migration-set hash on the Windows side and only proceeds after the window with no change (double-hash read — see [database.md](database.md)). |
| `build.command` / `build.service` / `build.silence_window_secs` | — / `app` / **180** | What `flux build` runs, where, and how long a quiet window it waits for first — see [build.md](build.md). |
| `sleep.*` | dev on, 15 min; prod `pause`, 30 min | Enforced by the sleep manager's 60 s sweep; signals: proxied request, CLI/panel lease, CPU above `cpu_percent_teto`, browser beat. |
| `resources.*` | platform defaults | Prod container limits (dev is unlimited today). |
| `sync.extra_ignore` | — | **Only adds** to the mandatory global ignores; it can never re-include `.env`, `vendor/`, etc. New entries reach a live session only via `flux sync reset`. |
| `sync.use_dockerignore` | `false` | Opt-in: adds the patterns of `.dockerignore` to the session ignores (verbatim, comments stripped; exact Docker semantics not reproduced — see [sync.md](sync.md)). File missing with the toggle on is a hard error. |
| `sync.dockerignore_path` | `.dockerignore` | Where the file lives, repo-relative; only read when `use_dockerignore = true`. |
| `temporary.*` | `false` / 6 h | Teardown removes compose project, `/srv/flux/projects/{name}*` and the SQLite trail — never the Windows repo. |
| `provision.on_wake` | — | Command run on wake. |
| `branding.logo_url` | — | Rendered on the wait/failed pages (the maintenance surfaces) — applied in context, never on platform pages (404, panel). Empty string is treated as absent. |
| `shield.*` | auto-detect | Auth paths for anti-brute-force; `tarpit` toggles the tarpit. |
| `health.path` | `/up` | The REAL health gate (06): the blue-green switch only fires after the new color answers `GET {path}`, and `flux up` reports the container as `awake` only when it answers — a RUNNING container that does not answer reports `waking` and the CLI keeps waiting. |
| `dev.live` | — | Run as `docker compose -p {project} exec -d app sh -c "cd /var/www && {live}"` **only** when `env == dev` and the up succeeded; a live failure is a panel warning and never fails the up. `flux live` toggles it without a down. |
| `dev.overlays` | `false` | With `.flux/overlays/` present, `flux up` tars it over `/srv/flux/projects/{name}/` via SSH before compose/migrations. |
| `limits.rate` | global edge default | Per-host token-bucket override. |

## Global config — `%USERPROFILE%\.flux\config.toml`

Same strictness. The CLI reads `[api]`; fluxd reads the whole file via
`--config` (missing file ⇒ factory defaults **plus a visible warning**).

| Section / key | Default | Consumer |
|---|---|---|
| `api.token` | — | Control API Bearer. Clear value lives here only; the VM stores the sha256 in SQLite (`settings.api_token_hash`). Written by `flux token generate`. |
| `api.base_url` | `http://127.0.0.1:7420` | Where the CLI points (override for tests). |
| `vm.switch` / `vm.host_ip` / `vm.vm_ip` / `vm.user` / `vm.key` / `vm.cpus` / `vm.memory_min_gb` / `vm.memory_max_gb` / `vm.disk_gb` | `192.168.140.2`, user `flux` | VM management (`flux vm …` commands are stubs today; the sync/SSH layers already use the host/user). |
| `domain.zone` | `bitello.xyz` | Host derivation: `{name}.{zone}` / `{name}{dev_suffix}.{zone}`. |
| `domain.dev_suffix` | `-d` | Idem. |
| `domain.panel_host` | `flux.bitello.xyz` | Panel host routing. |
| `defaults.idle_minutes` | **15** | Sleep manager dev idle. |
| `defaults.cpu_percent_teto` | — | Keep-awake CPU ceiling. |
| `defaults.sync_debounce_secs` | — | Sync reporting debounce. |
| `image_guard.block` | `mysql, mariadb, postgres, mongo, redis, valkey, memcached` | Images forbidden in project compose/Dockerfile. The default already blocks cache/queue engines — Laravel cache/queue run on the `database` driver. |
| `limits.max_body_bytes` | **104857600** (100 MB) | Edge 413 admission cap (chunked bodies are aborted mid-stream at the cap). |
| `queue.build_concurrency` | **1** | Build queue width; one build at a time, priority prod > provision > dev. |
| `logs.retention_days` | — | Log window (historical-journal phase). |
| `alerts.channel` / `alerts.cooldown_minutes` | `telegram` | Outbound alerting (telegram \| discord \| email) — not wired yet. |
| `shield.shadow_days` / `shield.allowlist` / `shield.brute_force` | 7 / `192.168.140.1` / text rule | WAF posture. Shadow mode is configured; the deployed WAF already runs **enforce** (403) with LAN allowlist — see [security.md](security.md). |
| `status.external_witness` | — | External uptime monitor probing `/healthz`; never used to wake anything. |
| `tools.images` | — | Pinned images for `flux tool` (not implemented yet). |
| `deploy.public_route` | **false** | `true` makes `POST /api/projects/{name}/deploy` exist on the panel host; absent/false ⇒ the path 404s. Everything is born closed — no new public surface without an explicit decision. |

## Editing rules

- `flux.toml` is versioned with the repo; secrets never go in it (`.env` is
  VM-only; tokens live in the global config or GitHub Secrets).
- A broken `flux.toml` fails loudly at registration with the offending key in
  the message (test `flux_toml_chave_desconhecida_e_erro`,
  `crates/flux-common/src/config.rs`).
- `flux live` edits only the `live = …` line inside `[dev]` of your local
  file; everything else stays byte-identical.

## Historical rationale

Section-by-section decision records:
[`doc-old/specs/09-seguranca-config.md`](../doc-old/specs/09-seguranca-config.md)
and [`doc-old/specs/06-cli.md`](../doc-old/specs/06-cli.md).

## Changelog

| Date | Change |
|---|---|
| 2026-10-14 | Initial English documentation |
| 2026-10-17 | `[sync] use_dockerignore` + `sync.dockerignore_path` (opt-in `.dockerignore` support) |
| 2026-10-17 | Dead keys now real: `[domains]` (exact-match extra host, state v4), `[db] rebuild_policy` (auto/always/never in dev), `[health] path` (real probe in the blue-green gate and in `awake`), `[branding] logo_url` (rendered on wait/failed pages) |
