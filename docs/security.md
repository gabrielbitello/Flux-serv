# Security

The threat model is "the whole internet can reach three hostnames; a
single developer's laptop holds the keys". Every decision below serves that.

## Secrets: where each one lives

| Secret | Clear value stored | VM stores |
|---|---|---|
| Control API token | `%USERPROFILE%\.flux\config.toml` `[api] token` (user-file ACL) | sha256 in SQLite `settings.api_token_hash` |
| Deploy tokens (per project) | GitHub Secrets only — printed **once** by `flux project token` | sha256 in SQLite `deploy_tokens` |
| Panel password | Nowhere — only you know it | Argon2id hash at `/srv/flux/secrets/panel.argon2` |
| Project `.env` | VM only (`/srv/flux/projects/{name}/.env`, `chmod 600`); local copies are gitignored drafts | n/a — it *is* the VM's |
| SSH key | Windows, referenced by the `192.168.140.2` entry in `~/.ssh/config` (`IdentitiesOnly` — the key never leaves the machine) | authorized_keys for user `flux` |

`flux.toml` never contains secrets (validation and convention both forbid
it). Tokens shown once; regenerating revokes the previous hash.

## Network surface

- The VM answers **only** on SSH (22) and Caddy (80/443). Everything else
  binds `127.0.0.1` inside the VM: fluxd's router (:8080 — only Caddy
  reaches it), control API (:7420) and the debug panel (:7421). From your
  PC, the control API is reachable exclusively through the CLI's on-demand
  SSH forward (`ssh -L 127.0.0.1:7420:127.0.0.1:7420`).
- Adminer binds the LAN address `192.168.140.2:8081` only.
- The deploy route is **off by default** (`[deploy] public_route = false` ⇒
  404) and, when enabled, carries its own rate limit and per-project auth —
  see [deploy.md](deploy.md).

## Control API gate (`:7420`)

Fail-closed by construction (`crates/fluxd/src/api.rs`):

- No `api_token_hash` in state ⇒ **503** on every route except `/healthz`.
  The API is never born open.
- Missing/malformed/mismatched Bearer ⇒ **401**, compared in **constant
  time** (XOR-accumulated fold, no early exit).
- Every refusal is written to the `events` trail — brute force against the
  API is visible in the panel, not just in logs.
- Unknown routes behind the gate also require the token (a probe without
  credentials learns nothing).

## Edge admission (R30)

Before any decision, per host (`crates/fluxd/src/edge.rs`):

- **Body cap**: `Content-Length` above `limits.max_body_bytes` (default
  **100 MB**) ⇒ `413` immediately. Chunked bodies are streamed through a
  limiter that aborts at the cap — a 10 GB upload cannot fill VM RAM.
- **Rate limit**: token bucket, **300 req/min, burst 100** per host ⇒ `429`
  + `Retry-After`. `[limits] rate` in `flux.toml` overrides per project.
- **Concurrency**: **100** in-flight requests per host; overflow waits in a
  **2 s** short queue, then `429`.

Cloudflare absorbs the volumetric layer; this kills the fine abuse that
reaches the VM.

## WAF (R32)

A tower layer after edge admission (`crates/fluxd/src/waf.rs`), running in
**enforce mode** (403) — a deliberate step above the spec's shadow-mode-first
plan:

- SQLi (OWASP subset) + path-scan patterns (`../`, `/etc/passwd`, `/.env`,
  `/.git`, WordPress/phpMyAdmin probes) matched case-insensitively on the
  **decoded** URI (percent-decoding once, `+` → space in query —
  `union+select` does not escape).
- **LAN is never blocked**: `192.168.140.0/24` + loopback are allowlisted
  before pattern matching. Verified from both sides: internet request ⇒ 403,
  LAN request ⇒ 200.
- Real client IP resolution order: `CF-Connecting-IP` > first
  `X-Forwarded-For` > socket info.
- Pending: security headers and bad-bot handling (a later phase); the
  `[shield]` config (shadow days, allowlist, brute-force rule text) exists
  for that phase.

## Image guard

Project compose files and Dockerfiles are scanned before any pull; database
and cache images (`mysql`, `mariadb`, `postgres`, `mongo`, `redis`,
`valkey`, `memcached`) fail the up with file/line/image named — no project
sneaks in its own database ([database.md](database.md)).

## Nothing silent

Auth refusals, WAF blocks, deploy steps, rescan requests, migration
failures — all land in the `events` trail shown in the panel. An operation
without a trail is treated as a bug. Error messages always carry their
origin; a swallowed error is a violation of the house rule, in docs and in
code alike.

## Historical rationale

[`doc-old/specs/09-seguranca-config.md`](../doc-old/specs/09-seguranca-config.md)
and [`doc-old/specs/16-seguranca-propria.md`](../doc-old/specs/16-seguranca-propria.md).

## Changelog

| Date | Change |
|---|---|
| 2026-10-14 | Initial English documentation |
