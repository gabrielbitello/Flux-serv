# Operations

Running the platform day to day: the VM, the tunnel, the background loops
that act as watchdogs, and what happens when things break.

## The VM

Debian 12 on the local hypervisor, host-only network `192.168.140.0/24`
(host `192.168.140.1`, VM `192.168.140.2`, user `flux`). Inside: Docker,
Caddy (TLS), `fluxd` under systemd, mutagen agent (injected by mutagen via
SSH — nothing pre-installed), `/srv/flux/` holding `projects/`, `state/`
(SQLite), `secrets/` and `releases/`.

The VM is reached over SSH with the `192.168.140.2` entry in
`~/.ssh/config` (`IdentitiesOnly`):

```
flux vm ssh            # interactive shell on the VM
ssh flux@192.168.140.2 # same thing, by hand
```

`flux vm status/start/stop/checkpoint/upgrade` are defined in the CLI but
**not implemented yet** (they refuse with the target phase) — VM power
operations are done in the hypervisor for now; everything else (projects,
DB, deploys) is managed through `flux`.

## The tunnel

`cloudflared` runs as a **Windows service**, keeping an outbound connection
to Cloudflare's edge. A wildcard CNAME `*.bitello.xyz` points at the tunnel,
so every managed host (`{name}.bitello.xyz`, `{name}-d.bitello.xyz`,
`flux.bitello.xyz`) resolves publicly with zero inbound ports on the
network. Caddy terminates TLS on the VM and proxies everything to fluxd's
router on `127.0.0.1:8080`.

Known pitfalls from the live setup (documented so they are not rediscovered):
the Windows service install silently failed on a non-ASCII install script
and on an orphaned EventLog key from an old installation — both fixed; and
the Cloudflare dashboard does **not** create wildcard DNS records — the
`*` CNAME must be pointed at the tunnel by hand.

`flux tunnel status` is not implemented yet; the service's health is visible
with `sc query cloudflared` and in Cloudflare's dashboard.

## Watchdogs (fluxd background loops)

fluxd runs three loops that continuously reconcile reality
(`crates/fluxd/src/lib.rs`, `serve`):

| Loop | Period | What it does |
|---|---|---|
| Sleep sweep | **60 s** | Re-reads container truth from Docker (internal state is never authoritative), stops dev projects idle past `idle_minutes` (default 15), pauses prod past `prod_idle_minutes` (default 30); 5-minute anti-flapping cooldown after each wake. In the same tick: expiry check for temporary dev projects. |
| Temporary teardown | same 60 s tick | `[temporary] enabled = true` projects with no interaction for `ttl_hours` (default **6 h**) are removed from the VM — compose project, `/srv/flux/projects/{name}*`, SQLite trail (deleted **last**, so a half-removed project stays visible for investigation). The Windows repo is never touched. |
| Observer sampler | **30 s** | CPU/RAM per flux-managed container → 1-minute rollups in SQLite, **24 h** retention; peaks (CPU > 80% or memory > 85% of limit) captured with a full sibling snapshot, max one per container per 300 s. |

Keep-awake signals (any of them resets the idle timer): a proxied request,
an open CLI/panel lease (renewed at half-TTL, hard cap 6 h), CPU above
`cpu_percent_teto`, and the **browser beat** — a one-line JS overlay injected
into eligible HTML pages that pings `POST /__flux/beat` on user interaction
(throttled to 30 s, plus a 60 s interval beat). Waking a paused prod
container is measured at ~1.1 s.

## Disaster recovery

What exists today:

- **State safety**: the SQLite state file is opened with a versioned schema
  (`schema_version` in `settings`); a corrupted file is **refused** with a
  message pointing at backups — fluxd never recreates state silently
  (`crates/fluxd/src/state.rs`).
- **Immutable releases**: prod releases are checked out to
  `/srv/flux/releases/{name}/{sha}` via `git worktree` and never mutated —
  rollback is a switch back to an existing, untouched release
  ([deploy.md](deploy.md)).
- **Repo safety as DR**: since sync is one-way (Windows → VM) and the repo
  plus `.env` drafts live on Windows, a lost VM costs at most uncommitted
  VM-side state (`storage/`, database data) — the source of truth never
  lived on the VM.

Pending phases (stubs refuse honestly today): `flux vm upgrade`
(checkpoint + fluxd upgrade with automatic rollback), `flux export` /
`flux import` (passphrase-encrypted zip of config+state+secrets and
re-erection on a fresh VM), `flux db restore`, prod database backups
(48 h/7 d retention), VM save-after-idle and the central warden that wakes
the VM on demand.

## Historical rationale

VM provisioning, hibernation design and the DR plan:
[`doc-old/specs/01-vm-hyperv.md`](../doc-old/specs/01-vm-hyperv.md),
[`doc-old/specs/12-vm-hibernacao-porteiro-central.md`](../doc-old/specs/12-vm-hibernacao-porteiro-central.md)
and [`doc-old/specs/14-ciclo-vida-recuperacao.md`](../doc-old/specs/14-ciclo-vida-recuperacao.md).

## Changelog

| Date | Change |
|---|---|
| 2026-10-14 | Initial English documentation |
