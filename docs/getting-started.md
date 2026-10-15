# Getting Started

How to take a project from a plain folder to a running dev environment on its
own public domain — and the daily cycle afterwards.

## Prerequisites

- **Windows host** with OpenSSH in `PATH` (Windows `System32` OpenSSH is what
  the CLI shells out to) and the `flux` CLI on `PATH`.
- **`mutagen`** binary: resolved in this order — `MUTAGEN_BIN` env var, then
  `%USERPROFILE%\.flux\bin\mutagen.exe`, then `mutagen` on `PATH`
  (`crates/flux-cli/src/sync.rs`, `mutagen_bin`).
- **The VM** running Debian 12 at `192.168.140.2` (host-only network), with
  Docker, Caddy and `fluxd` up, and an SSH entry for `192.168.140.2` in
  `~/.ssh/config` (user `flux`, `IdentitiesOnly` — the Flux key never leaves
  this machine). VM provisioning is the `flux setup vm` flow (not implemented
  yet — see [operations.md](operations.md)).
- **One-time API token:** `flux token generate` creates a 32-byte CSPRNG
  token, stores its sha256 in the VM's SQLite (`settings.api_token_hash`) and
  prints the clear value **once**; it also writes it to
  `%USERPROFILE%\.flux\config.toml` under `[api] token`. Every CLI call after
  that is authenticated automatically
  (`crates/flux-cli/src/main.rs`, `token_generate`).

Rules the CLI enforces for you: builds/assets are **never** built by hand, and
`npm run dev` is never run on the host (it would create `public/hot`, which
breaks the site outside your PC). Everything happens inside the VM through
`flux`.

## Creating a project manifest

From the project root:

```
flux up -m
```

The wizard asks a short series of questions (every answer has a default on
Enter) and writes a versionable `flux.toml`. It **refuses** to overwrite an
existing manifest, and aborting with Ctrl+C writes nothing
(`crates/flux-cli/src/wizard.rs`). Without `-m`, `flux up` works even without
a `flux.toml`: the project inherits the folder name and platform defaults,
with a visible warning — never a silent guess.

The full `flux.toml` reference lives in [configuration.md](configuration.md).

## First `flux up`

```
flux up          # dev environment (default)
flux up prod     # prod environment
```

What it does, in order (from `crates/flux-cli/src/main.rs`, `up`):

1. **Health** — checks `fluxd` (`GET /healthz` over an automatic SSH port
   forward of `127.0.0.1:7420` — see [architecture.md](architecture.md)).
2. **Register** — sends the project name and the full `flux.toml` to
   `POST /projects`. Strict validation: an unknown key in the manifest is an
   error here, never a silent typo downstream.
3. **Lease** — acquires a 15-minute keep-awake lease
   (`LEASE_UP_SECS = 900`), renewed at half-TTL while the command runs, dying
   by itself afterwards — sleeping is the platform's default state, not an
   exception.
4. **Sync** (dev only) — creates the mutagen session `flux-{name}`
   (`one-way-replica`, Windows → VM, folder
   `/srv/flux/projects/{name}`). The initial full scan runs in the
   background; see [sync.md](sync.md). Prod never syncs from Windows — it
   receives releases via deploy (see [deploy.md](deploy.md)).
   Right after, if `[dev] overlays = true` and `.flux/overlays/` exists, the
   CLI tars that folder over the VM copy via SSH (gitignored local variants
   of committed files; the Windows repo is never touched).
5. **Up** — `POST /projects/{name}/up` makes fluxd run
   `docker compose -p flux-{name}-dev up -d --build` (with its golden-rule
   overrides, see [architecture.md](architecture.md)). The CLI polls the
   container state every 3 s for up to **600 s** (`UP_TIMEOUT_SECS`); a
   first build legitimately takes minutes — beyond that the failure is
   visible in the panel.
6. **Migrations** — the CLI collects `database/migrations/*.php` (name +
   sha256, chronological order) and sends them to
   `POST /projects/{name}/schema`. A dev database that is new or whose
   migration set changed is rebuilt; prod only applies. Seed precedence:
   `--seed`/`--no-seed` flag > `[db] seed_on_rebuild` > default (seed on
   dev rebuild). Seed never runs in prod. See [database.md](database.md).

At the end it prints the consolidated status and the dev URL.

## Daily cycle

| Command | Effect |
|---|---|
| `flux status` | Sync, sleep state, hosts, applied migrations |
| `flux ps` | `docker compose ps` mirror, grouped by project |
| `flux logs -f` | Follow logs (SSE stream; acquires a keep-awake lease) |
| `flux exec dev -- php artisan tinker` | `docker exec` in the `app` container; exit code is forwarded |
| `flux shell` | `flux exec dev -- bash` |
| `flux down` | Stops the project containers (the single DB stays up) |
| `flux restart` | Down + up without rebuild |
| `flux build` | Runs the frontend build on the VM (silence window + flock; [build.md](build.md)) |
| `flux live` | Toggles the HMR live server without a down ([configuration.md](configuration.md), `[dev] live`) |

## The `.env` is exclusive to the VM

The VM copy at `/srv/flux/projects/{name}/.env` is the live one — the sync
never brings it to Windows and the Windows repo never gains a copy. To edit:

```
flux env pull   # VM → local .env (gitignored draft; warns if not gitignored)
flux env push   # local .env → VM (chmod 600; effective on the next request)
flux env edit   # pulls to a temp file OUTSIDE the repo, opens $EDITOR, pushes back
```

Content is never printed or logged (`crates/flux-cli/src/main.rs`, `env_*`).

## Domains you get

With the default global config: `https://{name}.bitello.xyz` (prod) and
`https://{name}-d.bitello.xyz` (dev), panel at `https://flux.bitello.xyz`
(`crates/flux-common/src/config.rs`, `DomainConfig`). The dev sleep default
is 15 minutes idle; any proxied request, open CLI lease, CPU above the
ceiling, or a browser beat (injected JS overlay) wakes it in about a second —
see [operations.md](operations.md).

## Historical rationale

Why one-way sync, why the `.env` stays in the VM, why builds run remotely:
[`doc-old/specs/02-sync-arquivos.md`](../doc-old/specs/02-sync-arquivos.md)
and [`doc-old/specs/06-cli.md`](../doc-old/specs/06-cli.md).

## Changelog

| Date | Change |
|---|---|
| 2026-10-14 | Initial English documentation |
