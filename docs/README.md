# Flux Documentation

Flux runs web project dev environments on a Debian VM instead of the Windows
host. One command (`flux up`) syncs the repo into the VM, starts Docker
Compose there, applies database migrations, and serves the app on a public
HTTPS domain through a Cloudflare tunnel. The repo on Windows is never
modified: the Windows folder is the **source** of a one-way file sync, and
nothing (no `npm run dev`, no `public/hot`, no local Docker) ever runs on the
host.

This documentation explains **mechanisms** — how each part works and why —
with exact commands and real numbers. For the historical rationale behind
every decision, read the original specifications preserved in
[`doc-old/specs/`](../doc-old/specs/) (Portuguese).

## Documentation map

| Document | What it covers |
|---|---|
| [getting-started.md](getting-started.md) | Prerequisites, first `flux up`, wizard, daily cycle |
| [architecture.md](architecture.md) | Components: CLI, fluxd, mutagen, single DB, tunnel; request path |
| [configuration.md](configuration.md) | Complete reference of `flux.toml` and the global `config.toml` |
| [sync.md](sync.md) | mutagen one-way-replica sync, ignores, `.env` policy, `flux sync`, `flux watch` |
| [build.md](build.md) | Asset builds on the VM: silence window, global flock, `[build]` section |
| [database.md](database.md) | Single MariaDB, migration pipeline, seed, rebuild policy, image guard |
| [cli.md](cli.md) | Full `flux` command reference (implemented commands and stubs) |
| [panel.md](panel.md) | Web panel: auth, pages, live sync state, rescan button |
| [deploy.md](deploy.md) | Production deploys from GitHub Actions, immutable releases, blue-green |
| [security.md](security.md) | Tokens, binds, edge limits, WAF, secrets policy |
| [operations.md](operations.md) | VM lifecycle, tunnel, watchdog sweeps, disaster recovery |
| [status.md](status.md) | Implementation matrix: SIM / PARCIAL / NÃO per feature |

## The ten-second version

- **Host (Windows):** `flux` CLI + `mutagen` daemon + `cloudflared` service.
  No project code executes here.
- **VM (Debian 12, `192.168.140.2`):** Docker, all project containers, the
  single `flux-db` (MariaDB 11), Caddy (TLS), and `fluxd` — the daemon that
  owns routing, sleeping, builds, migrations and deploys.
- **Sync:** mutagen `one-way-replica`, Windows (alpha) → VM (beta), typically
  under 1 s on LAN.
- **Domains:** `{name}.bitello.xyz` (prod), `{name}-d.bitello.xyz` (dev),
  panel at `flux.bitello.xyz`.
- **Secrets:** `.env` lives only in the VM; tokens are shown once and stored
  hashed (sha256/Argon2id) in the VM's SQLite.

## Source of truth

Docs describe the code in `crates/` (workspace `flux-cli`, `fluxd`,
`flux-common`). When docs and code disagree, code wins — and the doc is the
bug. Each file carries a `## Changelog` section at the end.

## Changelog

| Date | Change |
|---|---|
| 2026-10-14 | Initial English documentation |
