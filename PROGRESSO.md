# PROGRESSO — Flux

Diário de status do projeto. Entradas acumuladas por sessão, mais recente no topo.

## 08–09/10/2026 — Specs v0 → v0.2

**Intenção**: definir a arquitetura completa do Flux antes de qualquer código —
gerenciador de containers Docker em VM Hyper-V Bookworm, CLI `flux` no Windows,
daemon `fluxd` na VM, porteiro como estudo de caso.

**Resultado**: 13 specs em `docs/specs/` (índice no [README](docs/specs/README.md))
+ template de CI em `docs/templates/deploy-workflow.yml`.

**v0.2.1 (09/10, terceira rodada)**:

- Pergunta 2 do sync FECHADA: **mutagen** ("usa o pronto" — decisão do
  Gabriel). Reescrever em Rust só se virar dor operacional real.
- R22 novo: homologação (`*-d.bitello.xyz`) invisível a bots — `robots.txt`
  gerado `Disallow: /` SEMPRE em dev (robots do projeto ignorado em dev) +
  header `X-Robots-Tag: noindex, nofollow, noarchive` em toda resposta de dev;
  painel `flux.` idem.
- R23 novo: `robots.txt` + `sitemap.xml` gerados automaticamente para prod
  QUANDO o release não traz `public/robots.txt`/`public/sitemap.xml`;
  preexistente vence, nunca sobrescrito. Detecção no deploy (flags
  `has_robots`/`has_sitemap` no SQLite), não por request.
- Crawler NUNCA acorda a VM em Saved: porteiro central (`fluxhost`) responde
  robots/sitemap localmente (12).
- Honesto: robots/noindex são honorários — bot malicioso ignora; esconder de
  verdade exigiria auth em dev (toggle futuro `[dev] basic_auth`, fora de
  escopo).

**v0.2 (09/10, segunda rodada de observações)**:

- FATO novo: bancos REAIS do vippers (`vippers`, `vippers_afiliados`,
  `vippers_test_fix_g1..g8`, `base-laravel`) moram no `mariadb` compartilhado
  (`mysql:8.4`) — conferido por `SHOW DATABASES`. Os `mysql:9.3` por-dev são
  sobras dokploy Exited, não alimentam nada. WSL segue INTACTO até migração
  assistida (`flux migrate-from-wsl`).
- Spec 12 nova: VM inteira dorme em **Save-VM** (Hyper-V) após 30 dias sem
  acesso REAL (heartbeat/crawler não contam); **porteiro central no host
  Windows** (cloudflared serviço + `fluxhost :8443`) acorda a VM e entrega a
  rota — Alpine descartada com trade-offs registrados (guest Hyper-V não liga
  guest; par nativo custa ~50 MB vs ~256 MB).
- Wake de API: prod pausado responde o request original em < 1 s; prod parado =
  `503` JSON + `Retry-After` (padrão HTTP, nunca HTML, nunca 200 falso) com
  hold opcional até 45 s; fila de wake prioriza prod; concorrência de acordos
  = 2 (anti-explosão de RAM/CPU); `api_always_warm` opt-in por projeto.
- `flux.toml` ganha `[api] hold_secs` e `api_always_warm`; tunnel/token movidos
  da VM para o host (specs 01/09); matriz de respostas da 03 atualizada.

**v0.1 (08/10, observações do Gabriel)**: prod 100% na VM via GitHub Actions
(template pronto), releases imutáveis + perfil estático, blue-green com drain de
jobs e scheduler com lock único, rollback sem build; cota GLOBAL de dev via
slice cgroup com sleep-to-admit, prod com limite individual; prod hiberna em
`docker pause` (< 1 s, invisível) e o worker 24/7 mantém e-mails/notificações;
Adminer (LAN-only) no lugar do phpmyadmin; painel read-only em prod por grant
SQL; `flux tool` (node/php/composer/rust efêmero na VM); envs de prod cifrados
com age + `systemd-creds --tpm2`; branch routing por worktree
(`{branch}-{name}-d.bitello.xyz`).

**Decisões centrais (a validar)**:

- Caddy estático catch-all → `fluxd` decide tudo por `Host` (wake, proxy, páginas).
- Sync = mutagen `one-way-replica` (Windows fonte, VM réplica), `.env` fora do sync.
- Estado do fluxd em SQLite (o daemon sobe antes do banco; zero dependência circular).
- Imagem do banco única: `mysql:8.4` (paridade com o WSL atual — o container
  `mariadb` atual roda MySQL 8.4, não MariaDB).
- Prod (`{name}.bitello.xyz`) nunca reconstrói banco e não dorme; dev
  (`{name}-d.bitello.xyz`) reconstrói em mutação de migration.

**O que NUNCA fazer** (acumulado das specs): publicar `ports:` de container de
projeto ou 3306; servir 500/404 cru para host gerenciado off; sync de
`vendor/`/`node_modules/`/`storage/`/`.env`; reconstruir banco de prod; dormir
`flux-db`; segredo em arquivo versionado/sincronizado.

**Próximo**: revisão do Gabriel nas perguntas em aberto restantes da [00-visao-geral](docs/specs/00-visao-geral.md)
(imagem do banco #1, Redis no guard #3, gerenciador #5) → F1 (setup host + VM).
