# PROGRESSO — Flux

Diário de status do projeto. Entradas acumuladas por sessão, mais recente no topo.

## 13/10/2026 (parte 3) — F3 adiantado: módulo Docker do fluxd com teste em engine real

**Intenção**: usar a janela em que o VM work está travado no reboot do host
(HVCI) para adiantar o F3 — o módulo Docker do `fluxd` — testando contra um
engine REAL (docker do WSL exposto em `tcp://127.0.0.1:2375`, temporário).

**Feito** (commits `c7947a2`, `f00a15c`):

- **`docker.rs` no fluxd** (bollard 0.21): `DockerSource` implementa o mesmo
  `HostSource` da decisão — estado do container via inspect (`Running`/`Paused`
  /`Starting`/`Sleeping`), alvo de proxy via IP na rede do compose + porta TCP
  exposta (default 80). Convenção de procura por LABELS (`com.docker.compose.
  project=flux-{name}-{env}` + service `app`), nunca por nome de container.
- **Ações**: `ensure_awake` (unpause/start antes do proxy — prod pausado
  invisível < 1 s, 11), `wake` (página de espera dispara em background, 03),
  `pause`/`unpause`/`sleep` (para o sleep manager F4).
- **Degrad honesto**: sem Docker (`FLUX_DOCKER_URL` ausente + socket ausente),
  router cai no `SqliteSource` com warn — o Windows de dev continua rodando a
  suíte inteira sem engine.
- **Teste gated com Docker real**: ciclo `Unknown→Starting→Awake→Paused→
  Awake→Sleeping` + alvo de proxy, validado contra o engine do WSL em 13 s.
  Roda sempre no CI que tiver engine; sem a env `FLUX_DOCKER_URL`, pula.
- **Pós-reboot pronto**: `scripts/host/install-fluxd.sh` (unit systemd
  `After=docker.service`, user `flux` + grupo `docker`, layout `/srv/flux` da
  spec 01) e seed v2 com `network-config` (hostonly `192.168.140.2` estático —
  nomes `enp0s3`/`enp0s8` A VALIDAR no primeiro boot). Binário `fluxd` Linux
  (bollard incluso, 9,1 MB release) já compilado e esperando em
  `E:\flux\fluxd-linux`.
- Trapaças aprendidas do bollard 0.21: options em `query_parameters` com
  builders (`ListContainersOptionsBuilder.filters(&HashMap)`), corpo de create
  é `models::ContainerCreateBody`, `start_container` sem turbofish, pull de
  imagem é stream (`futures_util`).

**FATO do WSL**: distro hiberna em segundos sem processo ativo — docker no
`tcp://2375` morre junto. Para testes longos, manter um `wsl sleep` vivo.
O tcp 2375 é DEV/TESTE temporário: pós-reboot o WSL nem sobe (toggle HVCI);
remover o override quando o fluxd passar a rodar dentro da VM.

## 13/10/2026 (parte 2) — Specs das decisões + VM VirtualBox provisionada até o limiar do host-only

**Intenção**: aplicar as decisões #1/#3/#5 nas specs, reescrever a 01 para o
hypervisor real, e provisionar a VM Debian de verdade no VirtualBox.

**Descoberta central (FATO)**: o host é **Windows 11 Home** — Hyper-V nem
existe nele (`Microsoft-Hyper-V-All` desconhecido). Spec 01 reescrita para
**VirtualBox 7.2.20** (GPL, checksum `a81777d2…` conferido). E o Windows tem
**Memória de Integridade (HVCI) ligada**, que mantém o hypervisor carregado e
empurra o VBox para o modo **NEM Snail** (lento/instável): primeira tentativa
com controlador SATA nem achava a raiz (genericcloud não tem driver AHCI —
`PARTUUID does not exist`), e com VirtIO a VM chegou a **desligar sozinha**.

**Feito**:

- Specs: [00](docs/specs/00-visao-geral.md) perguntas #1/#3/#5 RESOLVIDAS;
  [05](docs/specs/05-banco-unico-migrations.md) imagem `mariadb:11` + WSL sem
  nada a manter; [09](docs/specs/09-seguranca-config.md) migração do WSL
  cancelada; [01](docs/specs/01-vm-hyperv.md) reescrita para VirtualBox com
  equivalências `VBoxManage` e as duas lições duras (VirtIO SCSI, hypervisor
  off). Commit `a379390`.
- VM **Flux** criada e provisionada por cloud-init: EFI, 6 vCPUs, 4 GB fixos,
  VDI 80 GB (Debian 12 genericcloud pinado, SHA512 verificado), **VirtIO
  SCSI**, NIC NAT com pf `ssh→2222` e `healthz→7420`, seed NoCloud instalando
  Docker CE 29.9.0 + Caddy. Serial em `.flux\vms\serial.log`.
- cloud-init concluiu ("FLUX PROVISION COMPLETE after 84 s"), SSH respondeu,
  `fluxd` Linux compilado no WSL (rustc 1.99, 7,2 MB, release).
- Toggles do hypervisor aplicados e confirmados no log
  (`HVCI Enabled=0` + `bcdedit hypervisorlaunchtype off`) — **pendem o reboot
  do host** (decisão do Gabriel: ele reinicia quando quiser).
- Referência canônica dos passos + lições: [scripts/host/README.md](scripts/host/README.md).

**Pausado aguardando reboot do host** (aplicar nesta ordem):

1. `VBoxManage startvm Flux --type headless` → SSH `2222` → validar `docker
   version`, `caddy version`, `/var/log/flux-provision-done`.
2. `fluxd` da WSL para a VM (`scp -P 2222`), unit `fluxd.service`
   (After=docker.service), `curl 127.0.0.1:7420/healthz` via pf.
3. Host-only `192.168.140.1/24` + netplan estático `192.168.140.2` + SSH direto.
4. Netplan dentro do seed para os próximos provisionamentos.

**O que NUNCA fazer** (novas lições duras): provisionar genericcloud fora de
VirtIO; provisionar com o hypervisor do Windows carregado (NEM Snail); usar
`--natpf1`/`--nat-dnsproxy1` (sintaxe certa é `--nat-pf1`); `qemu-img resize`
em VDI (resize é do VBox).

## 13/10/2026 — Fundação de código: workspace Rust + núcleo do fluxd + CLI stub

**Intenção**: parar de escrever spec e começar a base de código que comporta
TUDO o que as specs pedem — R22/R23/R24/R30/R31/R32/R33 são todas camadas ou
variantes do MESMO pipeline de requisição do `fluxd`, então a fundação é esse
pipeline (dispatch por `Host` em máquina de estados) + o estado SQLite + os
schemas de configuração.

**Decisões do Gabriel nesta sessão** (fecham 3 das 5 perguntas da
[00-visao-geral](docs/specs/00-visao-geral.md)):

- **#1 banco único = MariaDB**, e **não precisa manter NADA do WSL** — a
  migração `flux migrate-from-wsl` (dump/import dos bancos do vippers) MORREU.
  O Flux nasce do zero; o WSL segue intacto até ele desligar na mão.
- **#3 Redis/valkey/memcached bloqueados por padrão** no guard de imagens —
  materializado no default de `[image_guard].block` do `flux-common`
  (`crates/flux-common/src/config.rs`).
- **#5 gerenciador de banco = Adminer** (LAN-only, linkado do painel, F12).
- Fundação de código (itens 1–4 da proposta) **aprovada e entregue**.

**Entregue** (commits `e9372a2`, `61a07ae`, `3939bb9`, sem push):

- **Workspace de 3 crates** (`crates/flux-common`, `crates/fluxd`,
  `crates/flux-cli`) + CI em `.github/workflows/ci.yml` (fmt + clippy
  `-D warnings` + test). Gates: 53 testes verdes, fmt limpo, clippy limpo.
- **flux-common**: `flux.toml` de projeto e config global com validação
  ESTRITA (`deny_unknown_fields` — chave desconhecida = erro, spec 06); todas
  as seções de fases futuras já declaradas (`[limits]`, `[queue]`, `[shield]`,
  `[status]`, `[tools]`, `[alerts]`…); `ManagedHost` com parse de
  `{name}`/`{name}-d`/`{branch}-{name}-d` por sufixo mais longo (hífen no nome
  do projeto coberto por teste); enum `framework`; `FluxError` com causa
  classificada (R33); tipos do protocolo CLI↔API.
- **fluxd núcleo**: SQLite WAL com o schema exato da spec 07 +
  `schema_version` em `settings` (recusa banco de fluxd mais novo — abre
  caminho para o upgrade com rollback da R25; corrompido → erro apontando
  `/srv/flux/backups/`, nunca recria silencioso); `decision.rs` PURA com o
  enum `Action` cobrindo a matriz completa da spec 03 (proxy / página de
  espera 200 / 503 JSON `service_waking` + `Retry-After` / "falhou" /
  404 brandiado / branch ausente / intercept de robots+sitemap R22-R23);
  páginas da família Flux (meta refresh 3 s, `no-store`, noindex em dev);
  proxy hyper STREAMING (hop-by-hop fora, sem buffering); falha de upstream
  vira página de estado — nunca 502 cru; `X-Robots-Tag: noindex` em TODA
  resposta de dev, inclusive proxy; API de controle :7420 bind 127.0.0.1 com
  `/healthz`, resto 501 com causa.
- **flux-cli**: árvore clap COMPLETA da tabela da spec 06 como stubs que
  recusam com fase de destino e exit 2 (a superfície de comandos é design de
  API). `flux db restore` já exige `--yes` antes de existir. `flux up
  homolog` → `ambiente inválido` (validação via `flux-common`).

**O que NUNCA fazer** (acumulado até aqui): ver lista da
[00-visao-geral](docs/specs/00-visao-geral.md) — segue tudo valendo.

**Limitações honestas da fundação** (não são bugs, são fases):

- proxy_target vem de `settings` (`proxy_target/{name}/{env}`) — resolução
  real de IP via bollard é F3.
- websocket upgrade e timeouts de borda (connect 5 s/health 90 s/idle 300 s)
  entram na F3; rate limit/WAF entram como tower layers na frente do
  `decision::decide` (R30/R32).
- token da API de controle nasce junto do primeiro endpoint real (F6).
- `branch_exists` responde sempre true até o `flux watch` existir (F11).
- Painel é placeholder — o real com auth é F8 (painel nunca fica público sem
  auth, spec 03).

**Como testar**: `cargo test --workspace` (53 testes, sem VM e sem Docker —
integração usa upstream dummy na loopback). Smoke do CLI:
`./target/debug/flux --version`, `./target/debug/flux up` (stub exit 2),
`./target/debug/flux db restore x y` (exige `--yes`).

**Próximo**: revisão do Gabriel nas edições propostas das specs (abaixo) →
F1 (`flux setup host` + VM Bookworm, PowerShell/cloud-init) e/ou F3
(resolução de IP via bollard + wake real + timeouts). Specs 00/05/09 ainda
descrevem o estado pré-decisão (#1/#3/#5) — edição proposta e aguardando ok,
não aplicada.

## 08–09/10/2026 — Specs v0 → v0.2

**Intenção**: definir a arquitetura completa do Flux antes de qualquer código —
gerenciador de containers Docker em VM Hyper-V Bookworm, CLI `flux` no Windows,
daemon `fluxd` na VM, porteiro como estudo de caso.

**Resultado**: 18 specs em `docs/specs/` (índice no [README](docs/specs/README.md))
+ template de CI em `docs/templates/deploy-workflow.yml`.

**v0.4 (10/10, quinta rodada — produto "site profissional")**: Gabriel escolheu
itens da lista de recursos de provedor; viraram R31–R34, fases F18–F20 e 3
specs novas.

- Spec 15 nova (R31): FORA de dev nunca página de erro padrão — família visual
  Flux (404 de host, 429, 413, "falhou", manutenção); erro gerado PELO APP
  passa intocado (página própria de prod vence por definição — quem gera o
  erro o desenha); em dev, erros crus. Modo manutenção: `flux maintenance
  on|off` → 503 + `Retry-After` (SEO correto), página com marca do cliente,
  bypass por URL com token 24 h, API recebe JSON, worker segue de pé,
  manutenção e deploy se bloqueiam mutuamente.
- Spec 16 nova (R32, decisão "segurança própria, sem terceiros"): security
  headers em TODA prod por padrão (HSTS, nosniff, Referrer-Policy,
  SAMEORIGIN, Permissions-Policy; server/X-Powered-By removidos); WAF próprio
  no router: padrões OWASP básicos (SQLi/XSS/traversal/cmd injection, corpo
  inspecionado só até 64 KB), paths sensíveis (`/.env`, `/wp-admin`…), bad
  bots por lista curada no repo, anti-brute-force (10 falhas/10 min → ban
  15 min), reputação de IP com pontos e decaimento (ban progressivo 1 h/24 h/
  7 d), tarpit opcional; **shadow mode 7 dias antes de enforce** — falso
  positivo nunca derruba cliente; `flux shield unban`.
- Spec 17 nova (R33): status público `flux.bitello.xyz/status/{name}` —
  navegador vê página (uptime 7/30 d, gráfico SVG do rollup 13, incidentes),
  API vê JSON; causa classificada: só o site caiu (fluxd sabe) / plataforma
  caiu (fluxhost serve status estático quando heartbeat quebra) / internet do
  servidor caiu (impossível ver por dentro — testemunha externa: 1 monitor
  gratuito no `healthz`, ou sonda no notebook; nunca usada para acordar nada).
- Backup (R34): banco de prod dumpado a cada 48 h, retenção 7 dias, `gzip -t`
  + alerta em falha (14). Backup de storage/uploads/VM: FORA por decisão dele.
- Em estudo / em dúvida (00 perguntas 6–7): cache de HTML na edge (Livewire +
  MP3 de rádio mudando quebram a premissa) e e-mail transacional (IP
  residencial não entrega no Gmail; relay free tier é terceiro).

**v0.3 (09/10, quarta rodada — maturidade operacional)**: Gabriel escolheu 13
itens da lista de lacunas; viraram R24–R30, fases F14–F17 e 2 specs novas.

- Spec 13 nova (R24): sampler de métricas no fluxd (rollup 1 min no SQLite,
  retenção 30 d), histórico no painel (SVG server-side, zero lib JS), alertas
  outbound (Telegram/Discord/e-mail com cooldown; gatilhos: crash loop, OOM,
  disco >80/>90%, tunnel down, deploy falho, flux-db/worker parado),
  dead-man's switch (fluxd bate no fluxhost a cada 60 s; 5 min sem = alerta
  PELO HOST), endpoint público `healthz` p/ monitor externo gratuito, logs com
  rotação (`json-file max-size 10m ×3`, journald 500 MB, `flux logs
  --since/--until`).
- Spec 14 nova (R25): snapshot pré-migrate de prod (`mysqldump
  --single-transaction` antes de TODO migrate de deploy com pendência,
  retenção 10, `flux db restore --yes`); DR `flux export/import` (secrets
  cifrados com passphrase na hora; tokens regenerados; meta ≤ 30 min sem
  dados / ≤ 60 min com); upgrade do fluxd (checkpoint Hyper-V, swap com
  rollback automático, containers não caem) e `flux self-update` (checksum,
  rename dance).
- Boot do host (01, R25): VM é a primeira coisa útil do PC — `vmms` + fluxhost
  Automatic + `AutomaticStartAction Start`; pós-boot prods voltam DE PÉ, devs
  dormindo; Windows Update/queda de luz cobertos; vCPUs da VM = metade do
  host (o PC continua utilizável).
- Host manda (11, R27): fila global de builds com `[queue] build_concurrency`
  default 1 e prioridade deploy prod > provision > build dev; dev builds com
  nice +10/ionice idle; provision on_wake entra na fila.
- Isolamento de rede (05, R28): `flux-{name}-net` por projeto + `flux-db-net`
  — container do projeto A não alcança banco do projeto B.
- Guard de runtime (05, R29): `privileged`, namespaces host, `cap_add`
  crítica, `devices`, docker.sock bloqueados (ou `--ack` com motivo gravado).
- Borda (07/03, R30): rate limit por host (300 req/min, burst 100, 429
  brandiado/JSON), teto 100 conexões, `max_body_bytes` 100 MB com 413 cedo.
- Multi-stack (06, R26): `framework = laravel|node|rust|static|custom` —
  Laravel com suporte máximo; node/rust via `[health] path` +
  `[db] migrate_cmd`; perfil de deploy por stack (10).

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
(imagem do banco #1, Redis no guard #3, gerenciador #5; cache edge #6 e e-mail
#7 em estudo) → F1 (setup host + VM). Itens oferecidos e NÃO escolhidos:
domínio próprio do cliente (Cloudflare for SaaS), redirects na borda,
edge cache, e-mail transacional, domínio do cliente, analytics/heatmap
próprios (factíveis — analytics server-side é fácil, heatmap pede JS injetado),
kit LGPD, domínio de e-mail do cliente (fronteira: nunca).
