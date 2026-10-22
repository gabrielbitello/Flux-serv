---
name: flux
description: Plataforma Flux — dev/prod servers em VM Debian (192.168.140.2) gerenciados pelo CLI `flux` e pelas tools MCP flux_*. Ler antes de operar qualquer projeto gerenciado pelo flux (up, sync, build, env, DB, domínios) ou antes de rodar qualquer comando docker/npm/composer/artisan relacionado a esses projetos.
---

# Plataforma flux (dev servers)

O `flux` gerencia dev e prod server de todo projeto. Roda Docker dentro da VM Debian 12 (host-only `192.168.140.2`, user `flux`); nada roda no Windows além do CLI. **Docker não existe no host** — comando `docker` no terminal é bloqueado pelo guard do plugin. Túnel Cloudflare (`*.bitello.xyz`): prod `https://{name}.bitello.xyz`, dev `https://{name}-d.bitello.xyz`, painel `https://flux.bitello.xyz`. Docs completas do repo: `C:\Users\ieubi\Documents\Projetos\Flux\docs\` (getting-started, cli, configuration, sync, build, database, operations, deploy, security).

## Preferir MCP a comando (22/10)

O servidor MCP `flux` roda DENTRO da VM (HTTP LAN-only, `http://192.168.140.2:7430/mcp` — registrado na config do zcode com o token da API; nada do flux roda no Windows). Com ele ligado, use as tools `flux_*` em vez de montar comando no terminal — resposta estruturada, sem pipe, sem parse de texto:

| Tool | O que faz |
|---|---|
| `flux_projetos` | Projetos registrados (name, framework, hosts). Comece por aqui. |
| `flux_status` | Estado do projeto: containers, sono, migrations, hosts. |
| `flux_ps` | Todos os containers, agrupados (ACORDADO/PARCIAL/DORMINDO). |
| `flux_sync_status` | Sync Windows↔VM pela visão do fluxd (a sessão mutagen local só é visível pelo `flux sync status` no Windows) |
| `flux_db_status` | Banco próprio: container, databases, TTL. |
| `flux_logs` | Cauda de logs (1–1000 linhas). |
| `flux_acordar` | Acorda o dev e espera `running`. |
| `flux_db_query` | SQL de LEITURA no banco próprio (SELECT/SHOW/DESCRIBE/EXPLAIN; escrita o fluxd recusa). |
| `flux_tool` | `npm`, `npx`, `composer`, `php artisan` dentro do container de dev (whitelist, teto 120 s). |

Sem MCP no ar, os comandos CLI abaixo cobrem o mesmo chão. Ferramentas de terminal (`docker`, `ssh` para docker, `npm` local) NUNCA substituem o flux.

## Regras de ouro (violar qualquer uma = incidente)

1. **Build/assets NUNCA na mão no host.** `vendor/`, `node_modules/`, `public/build/` existem SÓ na VM e são produzidos lá (`flux build` ou tool MCP `flux_tool` com npm). Host = editor puro.
2. **NÃO rodar `npm run dev` no host** — cria `public/hot` que envenena o site fora do PC. HMR live = `flux live` (toggle sem down).
3. **Toda operação dentro da VM passa pelo flux** (`flux exec [env] -- <cmd>`, `flux shell`). `flux exec` tem cap de 120 s — comando pesado, usar sessão SSH dedicada.
4. **`.env` vive SÓ na VM** (`/srv/flux/projects/{name}/.env`). Nunca commitar, nunca copiar na mão: `flux env pull` (rascunho gitignored), `flux env push`, `flux env edit`.
5. **Repo Windows = fonte da verdade, nunca tocado.** Sync é one-way Windows→VM. Nunca escrever na pasta do projeto na VM por fora; nunca rsync por cima de sessão ativa; apagou pasta na VM → `flux sync reset`.
6. **1 build pesado por vez na VM** — `flux build` usa flock global (`/srv/flux/build.lock`) e espera janela de silêncio. Nunca disparar 2º build paralelo.
7. **Nunca sincronizar** `storage/`, `vendor/`, `node_modules/` (ignores globais, inegociáveis; `[sync] extra_ignore` só adiciona).
8. **`flux db fresh` só em dev, só com `--yes`**; prod nunca é rebuildado. Leitura de banco = tool MCP `flux_db_query` (escrita recusada de fábrica).

## Ciclo diário (CLI)

```
flux up [-m] [--seed|--no-seed] [env]  # ciclo completo: health→register→lease→sync→compose up→migrations
flux status                            # sync, sono+lease, hosts, migrations
flux ps                                # compose ps espelhado (ACORDADO/PARCIAL/DORMINDO)
flux logs -f [--tail N]                # segue logs (SSE; pega lease keep-awake)
flux exec dev -- php artisan tinker    # docker exec no service `app`; exit code repassado
flux shell                             # = flux exec dev -- bash
flux build [env]                       # build frontend na VM (silêncio 180 s + flock)
flux live [env]                        # toggle HMR live sem down
flux down [env]                        # para containers do projeto (DB único fica de pé)
flux sync status|pause|resume|reset    # reset = terminate+create com rescan total
flux db fresh --yes [--seed|--no-seed] # rebuild dev-only, recusa sem --yes
flux db status                         # banco próprio: container, health, TTL por database
flux watch                             # serviço leve: branches/worktrees + fila de rescan
flux vm ssh                            # SSH interativo na VM
```

Stubs que recusam com a fase alvo (exit 2, nada silencioso): `flux db migrate|seed|console|dump|restore`, `flux vm start|stop|checkpoint|upgrade`, `flux tunnel status`, `flux setup`, `flux panel`, `flux self-update`, `flux tool`.

## flux.toml

Raiz do projeto, versionado, NUNCA com segredo. Wizard: `flux up -m` (recusa sobrescrever); pela IA: comando `/flux:init` deste plugin. Chave desconhecida = parse error (validação estrita). Chaves: `name`, `framework` (laravel|node|rust|static|custom), `[domains]`, `[db]` (`rebuild_policy`, `seed_on_rebuild`, `migrate_cmd`, `image_guard`, `silence_window_secs`, `own`, `grupo`, `ttl_days`), `[build]` (`command`, `service`, `silence_window_secs`), `[sleep]` (`idle_minutes` 15, `prod_mode` pause, `prod_idle_minutes` 30, `cpu_percent_teto`), `[sync] extra_ignore`, `[temporary]` (`enabled`, `ttl_hours` 6 — apaga TUDO na VM sem interação; repo Windows intacto), `[dev]` (`live`, `overlays`), `[resources]`, `[health] path`, `[provision]`, `[shield]`, `[limits]`, `[branding]`. Template completo: `flux.toml.example` no repo do Flux.

## Sync (mutagen)

Uma sessão `one-way-replica` por projeto (`flux-{name}`; worktree = `flux-{name}-{slug}`). Alpha = pasta Windows (fonte da verdade), beta = `/srv/flux/projects/{name}`. LAN <1 s típico, pior caso <5 s. `.git/` e `.gitignore` respeitados; sessão é idempotente — `extra_ignore` novo exige `flux sync reset`. Verificação rápida: tool MCP `flux_sync_status` (veredito ok/atenção) ou `flux sync status`.

## Banco próprio do projeto (own-db)

- `[db] own = true`: o mysql/mariadb do compose do projeto vira **UM container dedicado** `flux-db-{grupo}`. Todas as instâncias do grupo usam o MESMO container, cada uma com database `{nome}_{env}`.
- Dorme/acorda junto: `up`/wake garantem o banco saudável ANTES do app; para quando a ÚLTIMA instância do grupo para.
- TTL: database de dev inativo > `ttl_days` (default 7) é dumpado em `/srv/flux/state/db-backups/` e DROPADO pelo sweep. Prod nunca.
- Leitura SQL pela IA: tool MCP `flux_db_query` (guarda read-only no fluxd; saída TSV do `mysql -N`, teto 1 MiB). `flux db status` mostra container/health/TTL.

## Sono e ciclo de vida

- Dev dorme após `idle_minutes` (15); prod pausa após 30 (wake ~1,1 s). Acordam: request proxyada, lease CLI/painel, CPU > teto, browser beat — ou tool MCP `flux_acordar`.
- VM sem autostart no boot do host — powered off pós-reboot é esperado; ligar no hypervisor.
- Projeto `[temporary]` teardown após `ttl_hours` (6 h) sem interação. Repo Windows NUNCA é tocado.

## Debug rápido (por sintoma)

- **CLI não acha fluxd** → túnel SSH `127.0.0.1:7420` morto: matar o `ssh.exe` do forward e rodar de novo (o CLI reabre).
- **Sync divergiu / pasta recriada na VM / ignore mudou** → `flux sync reset` (terminate+create, progresso ao vivo).
- **Container em estado estranho** → `flux ps`, depois `flux up` de novo (fluxd reconcilia lendo a verdade do Docker a cada 60 s).
- **Build travado esperando** → flock global: outro build em curso na VM. Esperar ou matar o holder em `/srv/flux/build.lock`.
- **Site sem CSS / hot órfão** → hoje impossível por design (wrapper do live limpa `public/hot`); se suspeitar, `flux live` OFF+ON.
- **Inotify ENOSPC no vite** → sysctl já aplicado na VM; só reaplicar se VM recriada.
- **Watch comendo RAM da VM** → watch roda node no host; projeto pesado, manter watch OFF e build manual.

## NÃO confundir

Fábrica WSL antiga (`*.dev`, porteiro, devfactory) foi DESINSTALADA em 14/10/2026 — nada de `https://<nome>.dev` nem rsync de 4 s. Skill avulsa `flux` em `~/.zcode/skills/` foi aposentada em 22/10/2026 (mora em `skills-disabled/`); esta skill do plugin é a única viva.

## Changelog

| Data | Mudança |
|---|---|
| 2026-10-22 | Migrou pro plugin `flux` (skill + /flux:init + docker guard). Seção MCP nova; comando `docker` agora é barrado por hook. |
| 2026-10-22 (noite) | MCP virou HTTP servido DENTRO da VM (:7430, LAN-only, token da API) — registrado na config do zcode; plugin ficou sem MCP embutido (0.1.1). |
| 2026-10-14 | Criada substituindo a skill `dev-servers` (fábrica WSL morta) |
