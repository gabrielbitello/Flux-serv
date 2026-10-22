# Database

One MariaDB for everything, migrations as a diff pipeline, and the guard
that keeps databases out of project compose files.

## The single `flux-db`

A single container `flux-db` (image `mariadb:11`) runs on the external
Docker network `flux-net`. Every project database is a database **inside**
it. fluxd's compose override enforces, at every `up`:

- `DB_HOST=flux-db` and `DB_PORT=3306` injected into the environment
  (overriding stale `.env` values);
- `ports: !reset null` on **all** services — no project container ever
  publishes a port;
- every service joins `flux-net`.

The original `docker-compose.yml` of the repo is never modified; overrides
are generated per project at `/srv/flux/state/compose/{name}-{env}.override.yml`
and passed via `-f` (`crates/fluxd/src/compose.rs`).

**Adminer** (per-project DB manager) runs LAN-only at
`http://192.168.140.2:8081`, pinned to `flux-db` via
`ADMINER_DEFAULT_SERVER`, linked from the panel nav — unreachable from the
internet. For an **own-db** project, type the dedicated container name
(`flux-db-vippers`) as the server in the Adminer form.

## Own project database (`[db] own = true`, 21/10/2026)

A project can own its SQL engine instead of using the global MariaDB — the
MariaDB model, per project, with the compose kept CLEAN:

```
[db]
own = true            # flux materializes flux-db-{grupo} by itself
image = "mysql:9.3"   # optional; default mysql:8.4 (platform default)
# grupo = "vippers"   # default: the project name; worktrees point at the main project
# ttl_days = 7        # idle DEV databases are dropped (dump first); prod never expires
```

**The project compose does NOT need a db service** (Gabriel, 21/10 v2: "não
é para ter mysql no compose — ele baixa sozinho"). When the rendered compose
has no mysql/mariadb service, fluxd builds the definition from `[db] image`
with credentials GENERATED once per group into the state SQLite
(`db-root/db-user/db-pass/{grupo}`, 48-char hex via `randomblob`) and
synchronizes the MySQL user on every up (`CREATE USER IF NOT EXISTS` +
`ALTER USER IDENTIFIED BY` — covers containers that already existed with
older credentials). The full `DB_*` package injected into the app beats the
`.env`, so generated credentials are invisible to the project. If the
compose DOES declare the service, its capture wins (image + credentials
from the compose — v1 path, still valid). `own = true` with neither a
service nor anything to generate is an ERROR with the cause.

Mechanism (`crates/fluxd/src/banco.rs`):

1. **One container per group** — fluxd writes
   `/srv/flux/state/db/{grupo}/compose.yml` (healthcheck `mysqladmin ping`,
   named volume `flux-db-{grupo}-data`, `restart: "no"`, **no published
   ports**) and runs it as compose project `flux-db-{grupo}` with
   `container_name flux-db-{grupo}`.
2. **Databases are `{name}_{env}`** (`vippers_dev`, `vippers-agenda_dev`,
   …) created idempotently with `GRANT ALL` for the group user; SQL runs
   via `docker exec` with the SQL on **stdin** and the root password taken
   from the STATE (never the container env — a recreated container skips
   init, so its env can diverge from the datadir's real password; caught
   live in the v2 cutover).
3. **Sleeps and wakes with the project** — `up` guarantees container
   healthy + database BEFORE the app's `compose up` (health cap 300 s;
   cold init ≈ 40 s with image pull, warm wake `start → healthy` ≈ 5 s);
   wake (`docker.rs`) starts the DB before the app containers; sleep
   (`stop` actions) and `down` stop the DB only when the LAST instance of
   the group stops (`dormir_se_ninguem`; a listing failure never puts the
   DB to sleep). Prod `pause` never touches the DB.
4. **TTL (dev only)** — the 60 s sweep drops dev databases idle past
   `ttl_days` (default 7): `mysqldump --single-transaction` (64 MiB cap —
   exceeded aborts without dropping) to `/srv/flux/state/db-backups/
   {db}-{epoch}.sql`, then `DROP`, with a warn event trail. No clock
   (never interacted) = never expires; prod never expires; databases
   outside the `{name}_{env}` pattern are listed as "não gerenciado" and
   never touched. Temporary-dev teardown drops the instance database at
   teardown time (with dump).

Inspect it with `flux db status` (container, health, group, TTL countdown
per database) or Adminer. The MariaDB `flux-db` stays the default for
every project without `[db] own`.

## Migration pipeline

Triggered by `flux up` (the CLI collects and posts the set; `flux db
migrate` as a standalone command is still a stub that refuses — see the
commands table below):

1. The **CLI** collects `database/migrations/*.php` from the Windows repo —
   sorted by filename (Laravel's chronological order), each with its sha256 —
   and sends the (name, hash) list to `POST /projects/{name}/schema`
   (`crates/flux-cli/src/main.rs`, `coletar_migrations`).
2. **Quiet window first.** The CLI reads the migration-set hash on the
   Windows side **twice**, `silence_window_secs` apart (default **5 s**,
   `[db] silence_window_secs`): only when both reads match does the pipeline
   fire. This guarantees migrations are not mid-edit without any new
   dependency. It is a conscious deviation from the original spec, which
   asked for an inotify watcher inside the VM — same stability criterion,
   zero new machinery (see
   [`doc-old/specs/05-banco-unico-migrations.md`](../doc-old/specs/05-banco-unico-migrations.md)).
3. **fluxd diffs** the received set against `migration_state` in SQLite:
   - **dev**, set new or changed ⇒ rebuild (`migrate:fresh`) then apply, per
     `[db] rebuild_policy`;
   - **dev**, unchanged ⇒ nothing runs;
   - **prod** ⇒ apply pending only. A **mutated** migration in prod is an
     error that blocks the pipeline — prod is never rebuilt; there is no
     flag for it, by design.
4. Execution is always **inside** the `app` container via `docker exec`.
   For node/rust projects the idempotency belongs to `[db] migrate_cmd`,
   which runs per invocation.

## Seed

Seed exists only on **dev rebuild**: `migrate:fresh` + `db:seed --force` as a
separate exec so the seeder's output is visible in the event trail.
Precedence: CLI flag (`--seed` / `--no-seed`) > `[db] seed_on_rebuild` >
default (`true`). **Prod never seeds** — a `Some(true)` from the CLI never
even reaches the prod code path (`crates/fluxd/src/db.rs`).

## Fresh and restore

```
flux db fresh dev --yes          # forced dev rebuild — destroys dev data
flux db fresh dev --yes --seed   # idem, forcing seed on
```

`--yes` is mandatory (the CLI refuses otherwise, exit 2). Prod is refused by
fluxd. `flux db restore` (prod restore from a pre-migrate dump) still
requires `--yes` and is a pending phase.

## Image guard

Before any `up`, fluxd re-scans the project's compose files and Dockerfiles
for `image:` / `FROM` entries and refuses (with file, line and image
pointed out) anything matching the block list:

```
mysql  mariadb  postgres  mongo  redis  valkey  memcached
```

Matching is by repository basename (`bitnami/mariadb` hits `mariadb`). The
default list blocks cache/queue engines too — Laravel cache and queues run
on the `database` driver, so no project needs Redis. A project that bundled
its own database container is the exact failure mode this kills: the
single-DB rule enforced mechanically (`crates/fluxd/src/guard.rs`).

## Commands

| Command | Status |
|---|---|
| `flux db fresh [env] --yes [--seed/--no-seed]` | implemented (dev only) |
| `flux db status` / `migrate` / `seed` / `console` / `dump` | not implemented yet (stub refuses with the target phase) |
| `flux db restore <name> <file> --yes` | not implemented yet (destructive guard already in place) |

## Historical rationale

Single-database decision, rebuild policies and the prod-never-rebuilds rule:
[`doc-old/specs/05-banco-unico-migrations.md`](../doc-old/specs/05-banco-unico-migrations.md).

## Changelog

| Date | Change |
|---|---|
| 2026-10-14 | Initial English documentation |
