# Builds (frontend assets)

Why `npm run build` never runs on your PC, and how `flux build` decides when
to fire.

## The rule

Builds and assets are **never** produced by hand and never on the Windows
host. Running `npm run dev`/`npm run build` locally creates
host-specific artifacts (`public/hot`, hashed bundles) that the sync would
then push into the VM — poisoning a environment that must be built from what
the repo actually says. The host stays a pure editor; the VM is the only
place where dependencies are installed and assets are compiled.

## What `flux build` does

```
flux build
```

No environment argument: the build always runs in the **dev** container.

1. **Waits for a quiet window.** Your editor writes files continuously
   (autosave, formatting on save, multiple file renames). Building mid-edit
   compiles a snapshot that is already wrong. So `flux build` waits for the
   sync to be quiet: `[build] silence_window_secs` (default **180 s**) with
   no changes to the synced set before it fires.
2. **Runs the build on the VM**, inside the compose service declared in
   `[build] service` (default `app`), using `[build] command` — e.g.
   `npm run build`. It runs against the synced repo in
   `/srv/flux/projects/{name}`, where `vendor/` and `node_modules/` really
   live ([sync.md](sync.md)).
3. **Global flock — one build at a time.** The build takes the global lock
   `/srv/flux/build.lock` on the VM: a second build (yours or another
   terminal's) waits instead of racing. This flock **is** the concurrency
   control today — there is no platform-wide build queue yet: the
   `[queue] build_concurrency` setting is declared in the config but not
   wired to any scheduler, and there is no prod > provision > dev priority
   (status.md, R27 = **NÃO**).

Output comes back with real exit codes; a failing build is an error on your
terminal (non-zero exit, cause on stderr) — `flux build` does not write to
the panel's event trail today.

## Configuration

```toml
[build]
command             = "npm run build"   # what to run (laravel has a platform default; other stacks must declare it)
service             = "app"             # which compose service runs it (platform convention: app)
silence_window_secs = 180               # quiet window before firing (default 180)
```

The lock is the global file `/srv/flux/build.lock` on the VM, held for the
whole duration — one build at a time across the entire fluxd. Projects
without a `[build]` section are **not** skipped: `laravel` falls back to the
platform default `npm run build`; any other framework without a declared
`command` refuses with an explicit error instead of guessing
(`crates/flux-cli/src/main.rs`, `default_build_command`). The container
image build (`docker compose up --build`) is a different thing and stays
part of `flux up` ([getting-started.md](getting-started.md)).

## Historical rationale

Why assets belong to the VM and never to the host:
[`doc-old/specs/02-sync-arquivos.md`](../doc-old/specs/02-sync-arquivos.md)
("o que NUNCA fazer") and [`doc-old/specs/06-cli.md`](../doc-old/specs/06-cli.md).

## Changelog

| Date | Change |
|---|---|
| 2026-10-14 | Initial English documentation |
