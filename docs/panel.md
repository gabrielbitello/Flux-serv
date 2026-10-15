# Web Panel

The one place to see every project without a terminal. Served at
`https://flux.bitello.xyz` (the `[domain] panel_host`) through Caddy →
fluxd's router, plus a local-only instance on `127.0.0.1:7421` on the VM for
debugging.

## Auth

Single user, **fail-closed** on every layer (`crates/fluxd/src/panel_auth.rs`):

- Password stored as an **Argon2id** hash at
  `/srv/flux/secrets/panel.argon2`. No hash on disk ⇒ no login at all — the
  login page shows the generation instruction instead of pretending.
- Session = 256-bit token in a cookie with `HttpOnly` + `SameSite=Lax`,
  expiring after **12 h**. Sessions live in memory: restarting fluxd logs
  everyone out (accepted for a single-user panel).
- Login rate limit: **5 attempts per minute per IP**.
- Only `/login` is public; every other route passes the session guard and
  redirects to login without a valid cookie. This gate is what authorizes
  mounting the panel on the public wildcard host — the platform rule is
  *never a panel without auth behind a public host*.
- Every panel response carries `Cache-Control: no-store` and
  `X-Robots-Tag: noindex, nofollow, noarchive`; `robots.txt` is served with
  `Disallow: /` for panel hosts.

## Pages

| Page | What it shows |
|---|---|
| `/` | Dashboard: projects with sleep state, hosts, and the live sync status; confirmations arrive as query flags (`/?rescan=1`). |
| `/projects/{name}` | Project detail: containers, sleep/lease, migrations applied, and events mentioning the project. The sync state and the **Rescan** button live on the dashboard card, not here. |
| `/projects/{name}/rescan` (POST) | Queues a full rescan of the project's sync session — `flux watch` consumes the queue and runs `flux sync reset`-style work. Post/redirect/get: success returns with `?rescan=1` ("rescan queued"); an error re-renders the page with the real message and a 503 — the panel never fakes success. Every outcome is written to the event trail. |
| `/events` | Paginated event trail (auth refusals, deploys, rescan requests, errors) — the "nothing is silent" rule made visible. |
| `/observe` | 24-hour resource monitor: CPU/RAM curves (server-rendered SVG, zero JavaScript), filters from global → project → container, per-container mini-curves, and a **peaks table** where `<details>` opens the forensics snapshot — a JSON of the peaked container **and all its sibling containers at the same instant** (image + state), captured when CPU > 80% or memory > 85% of limit, anti-spam capped at one peak per container per 300 s. |

Pages are generated server-side with no JavaScript requirement: filters are
plain GET forms, meta-refresh handles auto-update, and empty data is stated
honestly ("no data yet" / "no disk truth in this phase") — inventing a zero
is treated as a bug (`crates/fluxd/src/panel.rs` header contract).

## Adminer

The dashboard nav links to **Adminer** at `http://192.168.140.2:8081` —
LAN-only, pinned to the single `flux-db` server. It is unreachable from the
internet (only Caddy's 80/443 and SSH answer externally).

## Historical rationale

Panel design and the honesty contract:
[`doc-old/specs/08-painel-web.md`](../doc-old/specs/08-painel-web.md) and
[`doc-old/specs/13-observabilidade-alertas.md`](../doc-old/specs/13-observabilidade-alertas.md).

## Changelog

| Date | Change |
|---|---|
| 2026-10-14 | Initial English documentation |
