# Architecture

The five moving parts, where each one runs, and the exact path a request and
a file take through the system.

## Components

| Component | Runs on | What it does |
|---|---|---|
| **`flux` CLI** | Windows | The only surface you touch. Talks HTTP to the control API through an SSH port forward; never runs Docker, never runs project code. (`crates/flux-cli`) |
| **mutagen** | Windows (daemon) + agent injected in the VM via SSH | Continuous file sync, `one-way-replica`: Windows is **alpha** (truth), VM is **beta** (exact replica). (`crates/flux-cli/src/sync.rs`) |
| **cloudflared** | Windows (service) | Cloudflare tunnel: publishes the VM's domains without any inbound port on the network. |
| **Caddy** | VM | TLS termination and catch-all: every `*.bitello.xyz` host is proxied to fluxd's router on `127.0.0.1:8080`. |
| **fluxd** | VM (`systemd`) | The daemon that owns all intelligence: router, control API, web panel, sleep manager, compose overrides, migrations, deploys, metrics, temporary-project teardown. (`crates/fluxd`) |
| **flux-db** | VM (container on `flux-net`) | The single MariaDB 11 for every project. Projects never run their own database container — the image guard blocks it. |
| **Project containers** | VM (Compose projects `flux-{name}-{env}`) | The app itself. Service `app` is the convention; all services join `flux-net` with **no published ports**. |

## Ports and binds

| Port | Bind | Purpose |
|---|---|---|
| 443 | VM, Caddy (public) | TLS for all managed hosts |
| 8080 | VM, `127.0.0.1`, fluxd router | Catch-all router — only Caddy reaches it (`crates/fluxd/src/lib.rs`, `serve`) |
| 7420 | VM, `127.0.0.1`, fluxd control API | Token-gated API; the CLI forwards it over SSH (`ssh -L 127.0.0.1:7420:127.0.0.1:7420`, spawned detached and reused — `crates/flux-cli/src/tunel.rs`) |
| 7421 | VM, `127.0.0.1`, fluxd panel | Local debug instance of the panel; the public panel host is served through the router with session auth |
| 8081 | VM, `192.168.140.2` (LAN only) | Adminer, linked in the panel nav — unreachable from the internet |

Nothing but SSH (22) and Caddy (80/443) answers from outside the VM. The
control API is never exposed: without a port forward you cannot even probe
it.

## Request path (public)

```
browser
  → Cloudflare edge
  → cloudflared (Windows service, outbound tunnel)
  → Caddy :443 (TLS, wildcard *.bitello.xyz)
  → fluxd router :8080
      1. Edge admission: 413 body cap (100 MB default), 429 rate limit
         (300 req/min burst 100 per host), 100 concurrent connections per
         host with a 2 s short queue            (crates/fluxd/src/edge.rs)
      2. WAF: SQLi + path-scan patterns on the decoded URI → 403 in enforce
         mode; LAN (192.168.140.0/24 + loopback) is always allowed
                                              (crates/fluxd/src/waf.rs)
      3. Decision: (host, path, container state) → action — pure function
         over Docker/SQLite truth             (crates/fluxd/src/decision.rs)
      4. Proxy: L7 streaming, no buffering (SSE, uploads, websockets pass
         from the first byte). Sole exception: eligible HTML responses
         (200, text/html, < 256 KB, no content-encoding) get the sleep
         overlay injected before </body>      (crates/fluxd/src/proxy.rs)
  → project container (service `app`, internal network)
```

A sleeping dev project is not an error: the decision returns the **waiting
page** (HTTP 200, `meta refresh` 3 s, Flux-styled) while fluxd wakes the
container — about 1.1 s measured from pause (`docker pause`) states. API
clients (JSON `Accept`) get `503` + `Retry-After` instead of HTML. Unknown
hosts get a branded 404 (~99 ms measured through the full internet chain).

## File path (dev)

```
editor on Windows (alpha)
  → mutagen one-way-replica session flux-{name}
  → /srv/flux/projects/{name} on the VM (beta)
  → mounted into the `app` container by the project's own docker-compose.yml
```

Typical latency under 1 s on LAN, worst case under 5 s. The `.env`,
`vendor/`, `node_modules/`, `storage/` and build outputs never cross — see
[sync.md](sync.md).

## The single database

One `flux-db` container (MariaDB 11) on the external Docker network
`flux-net`. fluxd's compose override forces `DB_HOST=flux-db`, `DB_PORT=3306`
and `ports: !reset null` on every service of every project, so a project's
own compose file cannot publish ports or point elsewhere
(`crates/fluxd/src/compose.rs`). Per-project databases live inside
`flux-db`; migrations run via `docker exec` in the `app` container — see
[database.md](database.md).

## fluxd internals (one page)

- **State**: one SQLite in WAL mode at `/srv/flux/state/flux.sqlite3` with a
  versioned schema; a corrupted file is **refused**, never silently recreated
  (`crates/fluxd/src/state.rs`).
- **Control API** (`:7420`, token-gated, fail-closed): register, status,
  lease, schema, up, switch, fresh, down, logs (SSE), exec, containers
  (`crates/fluxd/src/api.rs`). Unknown routes behind the gate answer 501 —
  future phases — but still require the token.
- **Panel** (public via router host `flux.bitello.xyz`, session auth):
  dashboard, project detail, events, `/observe` metrics — see
  [panel.md](panel.md).
- **Sleep manager**: sweeps every 60 s; re-syncs truth from Docker every
  sweep (internal state is never authoritative); dev stops after
  `idle_minutes` (default 15), prod pauses; 5-minute anti-flapping cooldown
  after waking (`crates/fluxd/src/sleep.rs`).
- **Temporary dev projects**: `[temporary] enabled = true` self-destructs the
  project from the VM after 6 h (default) without any interaction — the
  Windows repo is untouched (`crates/fluxd/src/temporary.rs`).
- **Observer**: samples CPU/RAM per flux-managed container every 30 s into
  1-minute rollups, 24 h retention, with peak forensics snapshots
  (`crates/fluxd/src/observe.rs`).
- **Deploys**: public route on the panel host, per-project Bearer tokens,
  immutable release dirs via `git worktree` — see [deploy.md](deploy.md).

## Historical rationale

The host/VM split, the single-DB rule and the no-published-ports rule are
decided and justified in [`doc-old/specs/00-visao-geral.md`](../doc-old/specs/00-visao-geral.md),
[`doc-old/specs/03-roteamento-dominios.md`](../doc-old/specs/03-roteamento-dominios.md)
and [`doc-old/specs/05-banco-unico-migrations.md`](../doc-old/specs/05-banco-unico-migrations.md).

## Changelog

| Date | Change |
|---|---|
| 2026-10-14 | Initial English documentation |
