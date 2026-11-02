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
| `/` | **Static shell** (`31/10–02/11` rewrite): Alpine fetches `/api/dashboard` (projects with sleep state + source, migrations count, sync with `rescan_requested`, hosts, last events, watcher anomalies, 24 h event curve as a client-drawn sparkline) and pushes over SSE `/api/dashboard/follow` — **only when the snapshot hash changes**, `: ping` between changes. Health dot in the header pulls `/api/saude` (the `flux doctor` machine) once a minute. Card button **acordar** → `POST /api/projects/{name}/acordar` (session-only; wake = the same machinery a request uses). Rescan confirmations stay query flags (`/?rescan=1`, failure = generic `/?rescan=falha` — cause lives in the events, never in the URL). |
| `/projects/{name}` | **Static shell** (fatia 6): `/api/projects/{name}` (environments with state/source and the FULL migration list with hashes, sync, hosts, events mentioning the project, REAL live status = hot file + vite port) + SSE `/follow` (push on change). **Tail de logs ao vivo**: `/api/projects/{name}/logs` (the same handler as the :7420 API — text tail or SSE follow), lines rendered as text. **Ações** (fatia 6b): `POST /api/projects/{name}/acoes` runs migrate / optimize:clear / shield:generate / build / seed / migrate:fresh / fresh+seed / live ON/OFF inside the daemon with a **live terminal** (`/acoes/stream` SSE, line by line). Matrix **prod ⊆ dev**: PROD never runs fresh/seed/build (the safe exception is shield — Filament permissions, non-destructive); one action per project (409); destructive ones are dev-only and need **two clicks**; action state is in-memory on purpose (a daemon restart loses a running action and the page says so). |
| `/projects/{name}/rescan` (POST) | Queues a full rescan of the project's sync session — `flux watch` consumes it. Post/redirect/get: success returns `?rescan=1`; failure now redirects `?rescan=falha` with the REAL cause in the event trail (the shell doesn't render server-side messages anymore). |
| `/events` | **Static shell** (fatia 4): data 100% from `/api/events` (same server-side filters, JSON with stable `id`) + live stream `/api/events/follow` (SSE). No more innerHTML swap — new events append at the top **with scroll compensation** (nothing slides while you read; an expanded `<details>` NEVER closes), identical consecutive events collapse into a ×N counter with expandable timestamps, infinite scroll, live search (300 ms debounce), remembered filters (localStorage), LOCAL browser time (UTC in the raw detail), a live traffic meter (req/min · 429 · 5xx over 5 min), pause/resume with a held-events counter, and browser notifications for warn/error while the tab is hidden. |
| `/observe` | **Static shell** (fatia 7): `/api/observe` JSON (SAME queries as the old SVG render — `observe::serie`/`series_por_container`/`peaks`; `observe_page.rs` was deleted) + SSE `/follow` (push on change — a new 30 s sample IS a change). Charts in **vendored Chart.js 4.4.7** (MIT): main curve with the watcher's peaks MARKED on it (red scatter), per-container mini-curves, colors read from the style guide (theme-aware, redraws on `prefers-color-scheme` change), zoom **1 h/6 h/24 h** (same data, windows on screen). |

**Architecture (31/10–02/11, scoped with Gabriel in question rounds):** pages are
STATIC shells (no data in HTML, `include_str!`), styled by the approved style
guide (`/assets/estilo.css` — semantic color only, IBM Plex vendored, OFL) and
fed by the panel's own JSON API + SSE. Alpine.js 3.17.5 is vendored (MIT; the
client registers components via `Alpine.data` — CSP-build-ready) and `painel.js`
NEVER injects HTML from server data (text/textContent only — a Rust test bans
the injection API). Push model: each page has ONE SSE stream and events push
**only when data actually changes** (snapshot sha256), with `: ping` keepalives.
Empty states stay honest ("sem dados ainda") — inventing a zero is a bug.

## Public panel API (`/api/*` on the panel host)

Read-only JSON + SSE for agents (LAN) and for the panel pages themselves
(scope closed with Gabriel 31/10–02/11). Auth, in ONE middleware layer:

1. **Bearer read-only token** — `settings.eventos_token_hash`; auto-generated
   on first boot (clear value in `/srv/flux/secrets/eventos.token`, chmod 600;
   hash BEFORE/AFTER order makes orphan files self-heal on next boot).
   `flux token eventos` prints it; `--gerar` rotates (hash + file via SSH).
   LAN-ONLY (hosts file pointing the VM — via Cloudflare every IP arrives
   external) and **GET only**: any non-GET with the token = 403.
2. **Panel session** — anywhere; OUTSIDE the LAN it requires `Origin`/`Referer`
   pointing at the panel host (the "Here Maps" rule): a bare `curl` with a
   stolen cookie gets 403.
3. Otherwise 401. Refusals always log an `auth` event with the real IP.

Read endpoints: `/api/events` (+ `/follow` SSE), `/api/dashboard` (+ `/follow`),
`/api/saude` (doctor JSON), `/api/projects/{name}` (+ `/follow`, `/logs`,
`/acoes` POST, `/acoes/atual`, `/acoes/stream`). Write endpoints are
session-only (token is refused): `/acordar`, `/acoes`.

## Login hardening (fatia 8)

- **Global lockout**: 3 wrong passwords within 90 s from OUTSIDE the LAN lock
  EVERYONE out for 5 min — except LAN, immune on both sides (its failures
  don't even count). In-memory on purpose (a daemon restart clears it; the
  per-IP rate limit stays up).
- **Cloudflare Turnstile** on the login page whenever configured — the SAME
  keys/widget as the dev gate (`flux gate turnstile`); fail-closed; a failed
  captcha counts toward the lockout.
- **"Manter conectado"**: checkbox → 30-day session cookie; default stays 12 h.
- **"Sair de todos"**: logout with `todos=1` revokes ALL sessions.
- Login errors travel as query flags (`?erro=1|captcha|lock`) — no form data
  ever echoes into the URL.

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
