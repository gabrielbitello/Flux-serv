# File Sync (mutagen)

How your editor's keystrokes reach the VM — and what deliberately never
crosses.

## Mechanism

One **mutagen** session per project in `one-way-replica` mode:

- **alpha = your Windows folder** — the only source of truth. Every change
  you save is replicated to the VM and, on divergence, the VM copy is
  overwritten. It is the `rsync --delete` model, but with native filesystem
  watchers on both sides: **typically under 1 s on LAN, worst case under
  5 s**.
- **beta = `/srv/flux/projects/{name}` on the VM** — an exact replica. The
  mutagen agent on the VM side is copied and started by mutagen itself over
  SSH; there is nothing to install on the VM.

Sessions are named `flux-{name}` (and `flux-{name}-{slug}` per worktree
branch, mirroring `/srv/flux/projects/{name}@{slug}`) and carry the label
`flux`, so Flux-owned sessions are selectable with
`--label-selector flux` (`crates/flux-cli/src/sync.rs`).

The CLI never invents mutagen output: structured status comes from
`mutagen sync list --template={{ range . }}{{ json . }}{{ "\n" }}{{ end }}`
(JSON Lines) and is mapped to four health classes — idle (watching),
syncing, attention (disconnected/needs rescan), halted-on-purpose.

## What never crosses (mandatory global ignores)

Hardcoded in `GLOBAL_IGNORES` (`crates/flux-cli/src/sync.rs`) — the module
API offers no way to remove them:

```
.env  .env.*  public/hot  vendor/  node_modules/
public/build/  public/fonts/  public/css/  public/js/  storage/
```

Additionally, mutagen respects the repo's native **`.gitignore`** by default
and always ignores `.git/`. Consequences by design:

- PHP dependencies, npm packages and build assets exist **only** in the VM
  and are produced there (`flux build`, [build.md](build.md)).
- `storage/` state (logs, cache, sessions) is VM-local and survives syncs.
- `public/hot` (Vite HMR marker) never leaves the VM, so your Windows copy
  of the site is never poisoned by it.

`[sync] extra_ignore` in `flux.toml` **only adds** patterns. Creating a
session is idempotent: `flux up` on an existing session reuses it and warns
that new `extra_ignore` entries require `flux sync reset` to take effect —
never a silent no-op.

## The `.dockerignore` opt-in

Sync does **not** read `.dockerignore` by default. Opt in per project:

```toml
[sync]
use_dockerignore    = true              # default false — nothing changes unasked
dockerignore_path   = ".dockerignore"   # repo-relative; default is the Docker convention
```

When enabled, patterns from the file are **added** to `extra_ignore` (never
to the mandatory globals), after stripping comments and blank lines. The
patterns pass through **verbatim** to mutagen (sessions) and to the
platform's stability checker — the exact dockerignore semantics
(last-match-wins, `!` exception precedence) are **not** reproduced, so exotic
Docker patterns may behave differently; plain path/glob patterns are the
supported subset. Toggle on with the file missing is a **hard error**
(`ignores_do_projeto`, `crates/flux-cli/src/sync.rs`) — declared config never
falls back silently. Like `extra_ignore`, changes reach a live session only
via `flux sync reset`.

## The `.env` is exclusive to the VM

`.env` is in the mandatory ignores, so the VM copy is the only live one. The
CLI manages it over SSH (`crates/flux-cli/src/main.rs`, `env_pull` /
`env_push` / `env_edit`):

```
flux env pull    # VM → .env in your folder (a gitignored draft)
flux env push    # .env in your folder → VM (chmod 600; effective on next request)
flux env edit    # round-trip through a temp file OUTSIDE the repo + $EDITOR
```

Content is never printed or logged.

## Commands

```
flux sync status    # health of the current context's session (one session;
                    # outside a project the folder name is used, with a warning)
flux sync pause     # stop replicating (e.g. before a destructive VM operation)
flux sync resume    # back on
flux sync reset     # "sync everything now": terminate + recreate with a full rescan
```

`reset` deserves its name: mutagen's own `sync reset` only clears history,
so Flux implements reset as **terminate + create**, which forces a complete
rescan of both sides and live progress output until convergence. It is the
command you reach for after restoring the VM folder, changing ignores, or
suspecting divergence.

`flux watch` is the lightweight companion service: it monitors
branches/worktrees, reports sync state to fluxd (`POST /sync/report`) and
consumes the **rescan queue** — including requests made by the panel's
Rescan button (`POST /projects/{name}/rescan`, which queues and answers with
"rescan queued"; the panel never pretends it already ran —
`crates/fluxd/src/panel.rs`, `rescan_projeto`). The dashboard card shows the
live sync state (badge, files/problems counts, last error, Rescan button);
the project detail page does not have a sync section.

## Never do

- **Never rsync (or copy by hand) over an active session.** An external
  writer competing with mutagen is how divergences and conflicts are born;
  one-way-replica surfaces foreign beta writes as conflicts for a reason.
- **Never synchronize `storage/`, `vendor/` or `node_modules/`.** Millions of
  small files would make every scan crawl, and host-built artifacts would
  mask build failures that belong to the VM.
- **Never write into the beta** (`/srv/flux/projects/{name}` on the VM) from
  outside the session. The replica would be restored on the next change —
  whatever you wrote silently disappears.
- **Never delete a project folder on the VM without a `flux sync reset`
  afterwards.** A missing beta root halts the session on purpose (halted
  state: root vanished); `flux sync reset` recreates it cleanly.

## Historical rationale

The one-way decision, the ignore list and the worktree model:
[`doc-old/specs/02-sync-arquivos.md`](../doc-old/specs/02-sync-arquivos.md).

## Changelog

| Date | Change |
|---|---|
| 2026-10-14 | Initial English documentation |
