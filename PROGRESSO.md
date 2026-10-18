# PROGRESSO — Flux

Diário de status do projeto. Entradas acumuladas por sessão, mais recente no topo.

## 18/10/2026 — zcode-api no flux (LAN-only, sem Cloudflare) + RAM 6,5 GB + hold de API

**Pedido**: subir o zcode-proxy (`TriDefender/zcode-api`) DENTRO do flux na VM,
exposto pro PC só na LAN; dormir podendo, mas dormir = demorar pra responder,
nunca recusar (IDEs quebram); acordar rápido; responder no formato Anthropic;
+500 MB de RAM; spec atualizada depois da migrate; após isso, VM congelada
até ordem.

**Entregue**:

- **Patch `feat(fluxd): [sleep] api_hold`** (default OFF — nenhum projeto
  muda sem a flag): projeto dormindo/subindo com `api_hold = true` no TOML NÃO
  devolve página de espera/503; o router (`router.rs`) segura a request,
  emite `docker start`, polla estado + porta TCP (mesmo probe do live) e
  re-decide; teto de 90 s e fallback honesto. 34 (common) + 222 (fluxd, 2
  testes novos) verdes no host.
- **Projeto `zcode-api`** (`Documents/Projetos/zcode-api`, repo próprio:
  `33a9b15`+`fd92398`): imagem `ghcr.io/tridefender/zcode-proxy:latest`,
  `provider: zai`, `plan: coding-plan` (expõe `/v1/messages` formato
  Anthropic → `api.z.ai/api/anthropic`), `[sleep] idle_minutes = 10,
  api_hold = true`, health `/health`, sem `ZCODE_PROXY_API_KEY` (LAN-only —
  chave quebraria o health probe do flux, ver NUNCA fazer). Secret de
  credenciais em `.env` (gitignored, `flux env push`).
- **RAM 6144 → 6644 MB** (`modifyvm` com a VM desligada; poweroff→modifyvm→
  start). Spec 01 atualizada.
- **Caddy**: bloco `http://zcode-api-d.bitello.xyz → 127.0.0.1:8080`
  (HTTP puro, LAN-only; `cloudflared` segue inativo). Wildcard HTTPS e os
  demais hosts intocados (`vippers-d` continua 308 → HTTPS, controle OK).
- **fluxd novo deployado** na VM (41 s de build; backup
  `/usr/local/bin/fluxd.bak-20261018-preapihold`) e `flux.exe` rebuilt no
  host (precisa parsear `api_hold`).
- **Hold provado em runtime**: request com o projeto em crash-loop segurou
  exatamente os 90 s do teto e caiu no fallback (200 página de espera — o
  cliente curl era browser). Journal: `hold: porta de zcode-api
  (172.21.0.2:8080) não abriu em tempo — segue fluxo normal`.

**Resolvido na sequência (mesmo dia)**: OAuth `auth login zai` concluído;
credenciais em `/srv/flux/secrets/zcode-api/credentials.json` — **fora da
árvore de sync**: arquivos que nasciam no bind `./dados/credenciais` sumiam
em segundos (3 probes: escreve no container → some do host; sessão mutagen
sadia e sem propagar; causa raiz não perseguida até o fim — suspeito
mutagen, FATO = dir sincronizado não é storage de segredo). **Wake medido
com o hold real**: `docker stop` → request pelo chain Caddy→fluxd→proxy →
HTTP 200 em **0,85 s (frio) / 0,84 s (morno)**; acordado, `/health` = 34 ms.
`POST /v1/messages` respondeu **200 em formato Anthropic** (`type: message`,
thinking, usage) com `glm-5.3-flash` em 3,4 s; `/v1/models` lista 11 GLM.
Uso pelo IDE: **Claude Code CLI conectado e provado** — hosts do PC com
`192.168.140.2 zcode-api-d.bitello.xyz` (entrada elevada, UAC) e
`~/.claude/settings.json` com `env.ANTHROPIC_BASE_URL=http://zcode-api-d.bitello.xyz`,
`ANTHROPIC_AUTH_TOKEN=zcode-lan-local` (proxy não valida), main
`ANTHROPIC_MODEL=glm-5.3` (sonnet/opus mapeados idem) e haiku/small =
`glm-5.3-flash`; `claude -p "responda apenas: ok"` → **`ok`** (aviso
cosmético de contexto desconhecido — assume 200k). Nota: `claim` automático
do proxy falha preview com `(3001) parameter error` e re-tenta em 600 s —
erro do endpoint de promoção upstream, sem impacto no proxy.

**Janela de contexto configurada (19/10)**: docs do Claude Code
(`code.claude.com/docs/en/model-config`) + docs do z.ai: GLM-5.3 = **1M de
contexto / 128K de saída**; glm-5.3-flash = 300K. Fix: env
`CLAUDE_CODE_MAX_CONTEXT_TOKENS=1000000` no `settings.json` (vale pra IDs
custom, que não começam com `claude-`) — o aviso de "assumes 200k" sumiu;
resta só a tag `[claude-code:unrecognized_model]` (reconhecimento, sem efeito).
Sufixo `glm-5.3[1m]` também funciona ponta a ponta (testado).

**Verificação a pedido do Gabriel (19/10) — teto real medido na API**: a
doc do z.ai lida DIRETO (não snippet) confirma 1M/128K pro 5.3; e o próprio
upstream DECLARA o teto no erro 1210: `max_tokens ... 限制数值范围[1,131072]`
— **131072 aceito e 200000 rejeitado, no glm-5.3 E no glm-5.3-flash**
(128K da doc = 131072 binário). Com o cap do flash conhecido e IGUAL,
`CLAUDE_CODE_MAX_OUTPUT_TOKENS=131072` setado e testado — ganho real pro
5.3 always-thinking (thinking conta no `max_tokens`; o 1º teste cortou no
`max_tokens=16`). Contexto de 1M NÃO foi testado de verdade (1M de input
queima cota) — é claim da doc. Se o Gabriel viu 300K, era o CONTEXTO do
flash; 200K é o default do Claude Code pra modelo desconhecido.

**O que NUNCA fazer**: confiar em diretório vazio na sync — mutagen não cria
dir sem arquivo, e bind mount de dir inexistente falha com `OCI runtime
create failed` (por isso o `.gitkeep` em `dados/credenciais/`);
`systemctl reload caddy` com `admin off` no Caddyfile (reload usa a API
`:2019`, que está morta — usar `restart`, ~1 s de interrupção); subir
`ZCODE_PROXY_API_KEY` sem antes resolver o `/health` que o fluxd sonda sem
credencial (401 → projeto eternamente "falho" no painel); `flux env push`
antes do diretório do projeto existir na VM (o push não cria pasta);
**guardar segredo dentro da árvore sincronizada** (arquivos novos sumiram —
segredo morre em `/srv/flux/secrets/`, fora do sync).

## 18/10/2026 (parte 7) — redirects estáticos de host: entregues e DESLIGADOS no mesmo dia

**Intenção** (pedido do Gabriel): DNS de `bitello.xyz` e `www.bitello.xyz`
apontados pro túnel — o flux responderia 302 pro GitHub dele
(`https://github.com/gabrielbitello`) em vez do 404 brandiado.

**O que foi construído e revertido**: commits `cc791b1` (feat) + `aa01c61`
(diário), revertidos por `9517331`/`b99e104`. `[redirects.hosts]` no config
global: map host → destino, consultado pelo router ANTES da decisão de
projeto, `302` + `Location` com `Cache-Control: no-store` (302 de propósito —
trocar destino não fica preso em cache de browser). 4 testes novos, 34
(flux-common) + 208 (fluxd) verdes, deployado, www provado pela internet.
O código continua no histórico (`cc791b1`) — religar é revert do revert +
seção no `/etc/fluxd/config.toml` + restart.

**Por que desligou**: o apex `bitello.xyz` responde **200 com 0 bytes e sem
Content-Type em qualquer path** e o journal do fluxd prova que a request
NUNCA chega na VM (probe + `journalctl` no mesmo minuto, zero registro).
As entradas do túnel estão iguais às do wildcard (`https://192.168.140.2:443`
— Gabriel conferiu no painel), então o record DNS do apex aponta pra OUTRA
coisa (túnel velho com `http_status:200` ou Worker route — decidir exige o
painel CF, e ele não podia mexer na hora). Com só o www redirecionando, o
estado ficava inconsistente; pedido dele: desfazer. VM restaurada dos
backups (`fluxd.bak-20261018-redirects` volta a ser o binário vivo; binário
com a feature salvo em `~/fluxd.com-redirects-20261018`; config
`config.toml.bak-20261018` restaurado). Smoke pós-revert: www 404,
vippers-d 200.

**O que NUNCA fazer**: assumir que "apontar o DNS no painel" ligou o caminho
até o fluxd — o record do apex era coisa distinta do túnel editado; provar
sempre com probe externo + journal no mesmo instante. E `cp` em binário em
uso é `Text file busy`: deploy de fluxd é `mv` (inode troca, processo vivo
continua no arquivo velho).

## 18/10/2026 — fix da coleta de migrations (estado não mente mais sobre arquivo fora da sync) + reparo real da homolog vippers

**Intenção** (pedido do Gabriel): durante o reparo da homolog do vippers
(CMS) ele aprovou o fix do buraco apontado na investigação — o `[6/6]` do
`flux up` coletava `database/migrations/*.php` do disco LOCAL sem filtrar
`extra_ignore`, e o fluxd gravava hash de migration "aplicada" que o
`php artisan migrate` nunca via (o arquivo não sincroniza).

**Resultado real**: commit `44f7373`. `coletar_migrations` recebe agora o
mesmo `extra_sync` da janela de estabilidade e filtra com o matcher que já
existia (`caminho_ignorado`, promoted a `pub(crate)`); helper puro
`filtra_migrations_ignoradas` para testar sem chdir. 85 testes do
`flux-cli` verdes (teste novo: `coletor_filtra_migrations_fora_da_sync`),
clippy limpo. Efeito colateral esperado: projeto que JÁ tinha hash fantasma
gravado passa a ver o aviso único "migrations apagadas do disco (estado
mantém hash)" no próximo `up` — o estado mantém o registro, só deixa de
mentir para o plano.

**Contexto do reparo** (ops na VM, sem mudança no repo): homolog vippers
voltou com `DB_CONNECTION=mariadb` (o `.env` da VM tinha `mysql` e as
migrations espaciais caíam no ramo MySQL-only), database/user `vippers`
criados no `flux-db` na mão, `storage/framework` recriado (GLOBAL_IGNORES
nunca sincroniza), 3 migrations de view `professional_lives` com
`LEFT JOIN LATERAL` impossíveis em MariaDB (registradas sem executar —
decisão pendente: flux-db MySQL 8.4 ou reescrever as views), seed completo
com `memory_limit=512M`, `npm ci` + build (44,7 s) e sessão de sync
recriada com os 15 ignores. O `flux.exe` do host estava 3 dias velho
(14/10) — rebuilt; shim `~/bin/flux` aponta `target/release`.

**O que NUNCA fazer** (reforçado por esta sessão): confiar em
`mutagen sync list` sem `-l` para verificar ignores (o formato curto omite
a seção `Ignores:` inteira); editar `vite.config.js` no Windows esperando
que chegue à VM (o overlay é cópia manual — os overlays do vippers e do
vipa foram reescritos em 18/10 para o design same-origin, com hostname
próprio de cada projeto); testar arquivos descartáveis dentro de
`/srv/flux/projects/{name}` FORA dos ignores — a sessão one-way-replica
DELETA arquivo que existe só na réplica (comeu 3 probes de vite até
entender; o lugar seguro é dentro de `node_modules/`).

## 18/10/2026 (parte 2) — live same-origin no host -d: proxy de paths do vite + túnel websocket do HMR

**Intenção** (pedido do Gabriel): matar o desenho cross-origin
(`vite-{name}-d.bitello.xyz`, `hmr: false`, cors por projeto) — ele pediu
"o mesmo método do -d dos devservers": vite exposto no MESMO domínio, com
HMR de verdade. Sintoma que motivava: live ligado = CSS parcial +
Alpine/Livewire quebrado (os dropdowns do Filament perdiam posicionamento);
desligar = voltava.

**Resultado real**: commit `5993487`, deployado na VM (backup
`fluxd.bak-20261018`). No router, quando `public/hot` existe (contrato do
Vite: nasce com o dev server, o wrapper do compose apaga na morte), os
prefixos `/@vite /@id /@fs /node_modules /resources` do host `-d` vão para
a :5173 do MESMO container; rotas do Laravel, Livewire e `public/` seguem
no frankenphp. Pedido com `Upgrade` usa `forward_ws`: headers de upgrade
preservados, 101 do upstream devolvido ao servidor e túnel
`copy_bidirectional` em task própria — o client legacy do hyper-util já
conecta com `with_upgrades`, nenhuma flag nova. 204 testes do fluxd
verdes (2 novos). Overlays do vippers e do vipa reescritos: `origin` = o
próprio host -d, `allowedHosts: true` (o proxy reescreve Host; o gate
público é o fluxd+WAF), `hmr.path = /@vite/ws`.

**Verificado ao vivo**: hot file = `https://vippers-d.bitello.xyz`; HTML
emite URLs same-origin (`/@vite/client`, `/resources/**`); `/@vite/client`
200 pela Cloudflare; handshake ws **101 ponta a ponta** (CF → Caddy →
fluxd → vite) com o subprotocolo `vite-hmr`; `GET /` continua no
frankenphp. O que o smoke NÃO cobre: HMR dentro do browser logado no
Filament — validação visual do Gabriel.

**Armadilha do caminho**: o `ws` do vite recusa handshake SEM
`Sec-WebSocket-Protocol: vite-hmr` DESTRUINDO o socket em silêncio (curl
sem o header = pendência etária, sem resposta) — qualquer teste de
handshake precisa do header. E o `mutagen` one-way-replica come arquivo
novo na réplica (ver "o que NUNCA fazer" acima).

**Pendências**: 1) testar HMR real no browser (hot update, Livewire
morph); 2) host legado `vite-{name}-d` segue existindo apontando :5173 —
candidato a remover num futuro commit (nada depende mais dele); 3) o
`serve` do fluxd usa `axum::serve` (com upgrades) — se um dia trocar por
conn manual, lembrar do `.with_upgrades()`.

## 18/10/2026 (parte 3) — public/{css,js,fonts} volta a sincronizar: ignorado ≠ commitado

**Intenção** (pedido do Gabriel): "deixa só o public/build/, todo o resto
do public vai" — endossando o diagnóstico do bug do Filament (404 em
`/js/filament/**`): os 60 assets publicados commitados no repo
(`filament:assets` + `js/app/money-script.js`) moravam em diretórios da
`GLOBAL_IGNORES` e nunca chegavam à árvore nova da VM. A referência dele
para o desenho certo = deploy hml do vippers: imagem com `COPY . .`, repo
inteiro baked — nada de sync ignorando diretório commitado.

**Resultado real**: commit `f29ebfc`. `GLOBAL_IGNORES` perde
`public/fonts/`, `public/css/`, `public/js/`; fica `public/build/` para o
artefato do vite (mesmo contrato da spec 02: nunca sincronizar build).
Teste verbatim atualizado; 85 testes do flux-cli verdes; sessão do
vippers recreada (`mutagen sync terminate` + `flux up`) — `mutagen sync
list -l` confirma 7 globais + 5 extras, `app.js` 200 pelo domínio público.
Lição registrada na spec 02: **ignorado ≠ commitado — repo trackeado é
fonte da verdade e sincroniza; gerado em `public/build/` fica fora**.
Sessões de sync vivas só pegam a lista nova recriadas (congela na
criação) — vale para presente-digital e demais projetos.

## 18/10/2026 (parte 4) — vendor/ ancorado na raiz: o tema blade do vippers nunca tinha sincronizado

**Intenção** (pedido do Gabriel): "-d ainda diferente da prod — faltam
botão de estatísticas, 'Last day', e os erros do F12 não existem na
prod". Dica dele fechou o caso: a diferença NÃO era banco seed (1 vs 15
afiliados) nem idioma (EN dev vs PT hml = dados), era o TEMA inteiro.

**Causa raiz**: padrão `vendor/` solto na `GLOBAL_IGNORES` casa o
segmento em QUALQUER nível (semântica gitignore) — engoliu
`resources/views/vendor/filament-tables/` (override de blade Laravel
OFICIAL: o tema custom do painel, 4 arquivos trackeados). O painel dev
caía na toolbar default do Filament: sem dropdown de período
("Last day"), sem painel de indicadores, sem stats. Probe reproduziu:
sync não replica nada sob `resources/views/vendor/`. E os
ReferenceErrors do F12 que ele viu no dev (zero na prod) caíram junto —
pós-fix, página com **0 erros**.

**Resultado real**: commits `ba83621` + `9faf85c`. `GLOBAL_IGNORES` com
`/vendor/`, `/node_modules/`, `/storage/` ANCORADOS na raiz (dir gerado é
o da raiz; aninhado é código). Bug no caminho: `padrao_casa` fazia
`strip_suffix('/')` antes da âncora — `/vendor/` virava o segmento
literal `"/vendor"` e não filtrava NADA (apanhado pelo
`hash_da_arvore_ignora_globais_e_muda_com_conteudo`, que o pipeline com
grep engoliu e commitou vermelho — violação da regra "pipeline nunca
termina em grep"; consertado no commit seguinte com a suíte inteira sob
`pipefail`, 86 verdes). Sessão recriada, blades na VM, Playwright:
toolbar completa ("Last day" + toggle de tema), botão marrom
`rgb(104, 26, 9)` = tema aplicado, 0 erros. Nota: o `<link>` do tema
aponta pra URL que o vite dev serve como módulo JS (tag morta) — o CSS
chega pelo pipeline de módulos; inofensivo.

**O que NUNCA fazer**: pipeline de verificação terminando em `grep` sem
`pipefail` (engole exit code de teste falho e commita vermelho); confiar
que "não aparece na VM" = "não está no repo" — diff `git ls-files` vs
árvore da VM antes de culpar a sync.

## 18/10/2026 (parte 5) — live idempotente: fim do dev server duplicado e do hot órfão que apagava o CSS

**Intenção** (ok do Gabriel sobre a proposta do incidente): o dev server
do vite morreu silenciosamente no meio da tarde (SIGKILL sem rastro —
sem OOM no dmesg, container sem restart; dado não encontrado), o hot
file ficou órfão e o `/@vite` devolveu página failed em HTML — MIME
text/html derrubou CSS e Alpine de uma vez. E cada `flux up` re-disparava
`npm run dev` SEM dedup: segundo nascia de "Port in use" na 5174,
invisível ao roteamento.

**Resultado real**: commits `523e656` (feat) + `10a008e` (clippy). No up,
`decidir_live` (puro, testado) decide sobre DUAS fontes — hot file +
TCP connect na :5173 do container: ambas vivas = evento "live server já
ativo" e NADA é re-disparado; qualquer caída = remove hot órfão, mata
zumbis de vite (loop `/proc` sem depender de ps/pkill — o sh/npm do
wrapper sobrevive e completa o `rm -f public/hot` do contrato) e sobe de
novo. No router, hot órfão EM TEMPO REAL: path do vite com live_ativo
mas :5173 morta limpa o hot e cai pro FRANKENPHP (build) — página failed
num path de módulo nunca mais. 205 testes do fluxd verdes (1 novo),
clippy -D warnings limpo.

**Validado na VM com o ambiente real**: up duplo = UM dev server (3
listeners, sem 5174); kill -9 no vite → `/@vite/client` 404 do
frankenphp + home apontando `/build/` (site vivo com build); `flux up`
seguinte ressuscita (hot recriado, client 200 JS). Nota que o fallback
expôs: os assets do build de hoje carregam URL absoluta com IP de
container (`http://172.18.0.4/build/…`) — build gerado dentro do
container sem ASSET_URL correto; latente do modo build, não do fallback.

**O que NUNCA fazer**: confiar no wrapper sozinho para limpar hot —
com SIGKILL no sh o `rm` não roda; a limpeza precisa existir fora do
processo moribundo (up idempotente + fallback por request).

## 18/10/2026 (parte 6) — ASSET_URL injetado no override + zumbi-killer que funcionava errado

**Intenção** (pergunta do Gabriel): "não dá pra fazer igual o db, que
injeta no env os dados corretos?" — sobre o asset do build montado com o
IP interno do container (`http://172.18.0.4/build/…`).

**Causa** (fechada com dado, não suposição): o proxy reescreve o header
`Host` para o IP do container (`proxy.rs` forward) — no modo build o
Laravel monta asset E ROTAS pelo Host percebido. `ASSET_URL` nem existia
no `.env` (verificado). E o mecanismo do DB (`DB_HOST` no override,
vencendo o `.env`) era aplicável: a plataforma injeta infraestrutura, o
`.env` do projeto não manda.

**Resultado real**: commits `095551e` (override injeta `ASSET_URL` —
dev = host `-d`, prod = host raiz, mesmo mecanismo do `DB_HOST`) e `c148850` (pid correto no zumbi-killer). Dose de humildade: o
`matar_vite_zumbis` comitado de dia usava `kill "${p#/proc/}"` —
devolvia `"72/cmdline"` pro kill (EINVAL silencioso, nada morria);
apanhado validando ao vivo contra o live legítimo do próprio Gabriel
(pid 48/72, vivo desde o boot do container). Corrigido com
`basename(dirname())` + redeploy. O projeto precisou também do
`config/vite.php` (não existe no scaffold dele): sem o arquivo o Laravel
nem lê `ASSET_URL` — o flux injeta a env, o config do projeto a lê
(espelho exato de `config/database.php` + `DB_HOST`). Fim a fim
verificado: `env` e `config('vite.asset_url')` = `https://vippers-d.
bitello.xyz` no container; kill real do vite → `/@vite/client` 404 do
frankenphp + hot limpo → `flux up` ressuscita (client 200).

**E o "aberto" acima fechou no mesmo dia** (commit seguinte, deployado):
o lado Laravel já estava pronto — o vippers tem
`trustProxies(at: '*')` no `bootstrap/app.php` — faltava só o fluxd
injetar `X-Forwarded-Host` (host original) + `X-Forwarded-Proto: https`
no forward. Validado: zero `172.18` no HTML, `signin` =
`https://vippers-d.bitello.xyz/signin`. Com isso rotas, assets e Livewire
montam URL pública em QUALQUER modo (live ou build), sem patch por
projeto e sem depender de `ASSET_URL` — que segue de pé como cinto e
suspenders.

## 17/10/2026 (parte 2) — chaves mortas do flux.toml viram função: [domains], [db] rebuild_policy, [health] real, [branding]

**Intenção** (pedido do Gabriel): a auditoria desta sessão mostrou 9 seções do
exemplo TOML citando feature morta ou parcial; ele escolheu 4 para fechar
agora — as 4 viraram função de verdade, cada uma com teste.

**`[db] rebuild_policy` (05)**: `acao_dev(policy, mutadas, novas)` pura em
`fluxd/src/db.rs` decide o braço dev: `never` pula o pipeline com evento
contando as pendências (e diz como aplicar), `always` reconstrói em TODO dev
up mesmo com plano limpo, `auto` é o comportamento histórico. `DbSection::
rebuild_policy()` default `auto` — quem nunca declarou não muda nada.
`migrate_cmd` (node/rust) não é afetado pela policy.

**`[branding] logo_url` (15)**: `BrandingSection::logo_url()` (vazio = None);
`pages::html_page` renderiza `<img>` aplicado nas páginas de espera/falha,
URL escapada; `router.rs` resolve o logo do TOML gravado — parse falho não
derruba a página (só tira a marca). Páginas de plataforma (404, API) ficam
sem logo.

**`[health] path` (06 — F9 fechada)**: probe HTTP REAL em dois pontos.
(1) Gate da troca blue-green (`trocar_cor`): depois de RUNNING, espera o app
da cor nova responder `GET {health.path}` (30×1 s, timeout 2 s, direto no
IP:porta do container pela rede docker — mesmo cliente do proxy). Falha
ABORTA a troca antes do ponteiro se mover. (2) `/status`: container RUNNING
só merece `"awake"` com o path respondendo; senão `"waking"` — o `flux up`
continua esperando em vez de celebrar cedo. `docker::alvo_app` resolve o
alvo; `compose::saude_ok` é o probe unitável.

**`[domains]` (09 — state v4)**: `project_hosts(host, project, dev)` no
SQLite (SCHEMA_VERSION 3→4 com migrador v3; purge apaga a trilha). Registro
valida via `ProjectToml::hosts_custom(&domain)` — host TEM de estar dentro da
zona (o túnel cobre `*.{zone}`; fora dela o erro é explícito na borda), não
pode ser o painel, não pode começar com `vite-` nem o rótulo terminar com
`-d`; colisão entre projetos = 422 citando o dono; re-registro do dono
substitui. Routing: `ManagedHost::Custom { name, dev }` novo, batida EXATA
antes da derivação (`parse_custom` — `parse` puro continua derivado-only,
testes antigos intactos); `decision::decide`, `beat`, `host_env` e
`DockerSource`/`SqliteSource` (`HostSource::host_custom` com default `None`)
respeitam o ambiente do registro. Exibição (register/list/status/panel)
mostra o override com fallback na derivação. Hosts derivados CONTINUAM
routeando — `[domains]` ADICIONA, nunca remove; robots/sitemap seguem na
derivação (sempre válida). Branch de worktree NÃO se aplica a host custom.

**Verificação**: `cargo clippy --workspace --all-targets -- -D warnings`
limpo; `cargo test --workspace` = 336 passed, 0 failed (testes novos:
`acao_dev_segue_a_policy`, logo aplicado/escapado, `hosts_custom_valida_…`,
`host_custom_vence_a_derivacao_…`, `host_custom_do_domains_resolve_…`,
`hosts_custom_grava_resolve_e_purga`, `register_com_domains_persiste_…`).
PENDENTE de smoke na VM real: migrador v3→v4 do fluxd vivo, probe de health
contra container de verdade, registro com `[domains]` + túnel roteando host
custom (DNS `*.{zone}` já cobre).

**Docs**: `configuration.md` (4 linhas de tabela viraram verdade + changelog),
`deploy.md` (health gate antes da troca), `status.md` (R7/R12 + entregas).

**O que NUNCA fazer aqui**: host custom fora da zona (falharia silencioso no
Cloudflare); RUNNING valendo "awake" (já mentiu uma vez); logo em página de
plataforma; host custom com branch de worktree.

## 17/10/2026 — `[sync] use_dockerignore`: opt-in do `.dockerignore` nos ignores da sessão

**Intenção** (pedido do Gabriel): toggle no `flux.toml` para o sync respeitar
o `.dockerignore`, com chave para dizer onde o arquivo está. Auditoria prévia
desta sessão mostrou que a sync lia só `GLOBAL_IGNORES` + `extra_ignore` —
zero menção a `.dockerignore` no código.

**O que entrou**:

- `crates/flux-common/src/config.rs`: `[sync] use_dockerignore` (default
  **false** — nada muda sem decisão explícita) e `[sync] dockerignore_path`
  (default `.dockerignore`, relativo à raiz do projeto), com acessores e
  testes de parse/default.
- `crates/flux-cli/src/sync.rs`: `ignores_do_projeto(dir, sync)` — junta
  `extra_ignore` + padrões do `.dockerignore` (comentários `#` e linhas
  vazias fora, dedup, append-only: nunca re-inclui globais). Toggle ligado e
  arquivo ausente = ERRO com caminho na mensagem. `SyncSpec::for_project`
  integra — então sync start, watch, status/pause/resume/reset e os dois
  pontos de estabilidade (`flux up` em `main.rs`, `flux build`) herdam.
- Testes novos: 4 unitários do helper + 1 de integração do `for_project`
  (globais na frente intocados). Gate local: `cargo clippy --workspace
  --all-targets -- -D warnings` verde, `cargo test --workspace` = 329 passed,
  0 failed.

**Limitação declarada** (doc, não silêncio): os padrões passam VERBATIM ao
mutagen e ao `caminho_ignorado` da plataforma — a semântica exata do
dockerignore (last-match-wins, precedência das exceções `!`) NÃO é
reproduzida. Subconjunto suportado = padrões simples de caminho/glob.

**Docs**: `docs/sync.md` (seção "The `.dockerignore` opt-in"),
`docs/configuration.md` (exemplo + 2 linhas de tabela + changelog),
`docs/STATUS.md` (R5 atualizada + entrega desta sessão), `flux.toml.example`
(seção `[sync]` documentada; teste de parse da raiz verde).

**O que NUNCA fazer aqui**: ler o `.dockerignore` por default (quem nunca
declarou a chave não pode ter o sync mudado); traduzir semântica do Docker
para mutagen em silêncio; deixar arquivo ausente virar fallback calado.

## 14/10/2026 (parte 11) — F2 FECHADA NO USO DIÁRIO: sync/watch/build no CLI, hub de sync no fluxd, card Sync no painel

**Intenção** (pedido do Gabriel): fechar a fase 02 de ponta a ponta — CLI
conversando com o fluxd sobre o estado do mutagen, build de assets com janela
de silêncio, `flux watch` como ponte e o painel mostrando e acionando o sync.
Workflow de 4 frentes paralelas (CLI, fluxd, painel, docs) sobre a árvore
compartilhada + revisão independente com 317 testes. Gate final
`cargo test --workspace` ficou para o orquestrador — NÃO rodei nesta entrada.

**CLI** (commit `89886e0`):

- `flux sync status/pause/resume/reset` ligados ao mutagen real: `status`
  imprime rótulo/pausada/arquivos/pendências alpha-beta/conflitos/last_error
  e `ok · sem pendências` quando `saudavel()`; `reset` = terminate+create
  com poll de status a cada 2 s até ociosa (teto 600 s, erro gritado).
- `flux build` NOVO, em 4 passos (substitui o `Cmd::Build` antigo que batia
  em rota inexistente do fluxd → 501): [1/4] contexto + `sync::create`
  idempotente; [2/4] janela de silêncio da árvore por DUPLA leitura de hash
  (sha256 de caminho+tamanho+mtime, sem ler conteúdo e sem crate de inotify;
  teto de 3 rodadas); [3/4] lease + SSH DIRETO `flock /srv/flux/build.lock
  docker exec <ctr> sh -lc 'cd /var/www && cmd'` com teto próprio de 1800 s
  — direto porque o endpoint de exec do fluxd corta em 120 s server-side
  (`fluxd/src/ciclo.rs:461-463`); [4/4] exit real (124 no teto, estilo GNU
  timeout). Default de comando: só laravel (`npm run build`).
- `flux watch` novo: loop de 5 s, `POST /sync/report` por sessão `flux-*` e
  `GET /sync/status`; consome a fila de rescan reconstruindo a spec da
  sessão viva + `sync::reset` + re-report com `rescan_applied=true`; erro
  sempre logado com origem; contra fluxd morto o ciclo degrada a ~11 s sem
  perder tick (medição instrumentada, revertida depois).
- Chaves novas em `flux-common/src/config.rs`: `[db].silence_window_secs`
  (default 5), `[build] {command, service, silence_window_secs}` (default
  180, `deny_unknown_fields`); `flux.toml.example` na raiz com teste que
  descomenta e parseia o arquivo contra o struct.

**fluxd** (commit `66a22e5`): `crates/fluxd/src/sync.rs` novo — upsert de
relatos em SQLite (tabela `sync_status`, SCHEMA_VERSION 2→3 com migrador v2
e v1), `POST /sync/report` (204), `POST /sync/rescan` (202; 404 com causa),
`GET /sync/status` — as três atrás do gate Bearer existente (401 testado);
eventos scope `sync` em rescan pedido/aplicado; `purge_project` apaga a
trilha de sync do projeto.

**Painel** (commit `66a22e5`): card Sync no DASHBOARD de cada projeto —
badge colorido (saudável verde / pausada azul / trabalhando amarela / atenção
laranja quando há erro ou pendências, sobrepõe as outras / sem sessão cinza),
arquivos, pendências, updated_at relativo, último erro e badge "rescan
pendente" — mais botão Rescan (`POST /projects/{name}/rescan` → flag no
SQLite consumida pelo `flux watch`; post/redirect/get com aviso "rescan
queued", erro real renderizado).

**Docs**: árvore nova em inglês em `docs/` (13 arquivos) + legado movido
para `doc-old/` (22 renames) — vão no commit deste diário.

**Achado "critical" da revisão NÃO reproduz**: o revisor apontou que a
reconstrução do rescan assumiria `user@host` num campo que o mutagen não
serializa. Na árvore final `endpoint_de` RECOMBINA os campos separados
`user`+`host` (`crates/flux-cli/src/sync.rs:602-608`) e `spec_da_sessao`
separa de novo em '@' com erro explícito quando não fecha
(`crates/flux-cli/src/main.rs:1943`), com teste provando a recombinação
(`sync.rs:925`). Registro para não re-abrir como bug.

**Verificação**: revisor rodou `cargo test -p flux-common -p flux-cli -p
fluxd` = **317 passed, 0 failed**; contrato JSON CLI↔fluxd conferido campo a
campo. Smokes read-only do CLI: `flux sync status` sem flux.toml erra com
"sessão `flux-Flux` não existe" (exit 1); `flux watch` contra fluxd morto
descobriu as sessões vivas reais e seguiu de pé. Caminhos mutadores (`flux
build`, `flux sync reset`, round-trip do Rescan no painel) cobertos por
unit/wiring — ainda sem testemunha ao vivo na VM.

**O que testar**: (1) `flux sync status` e `flux sync reset` num projeto com
sessão real; (2) `flux build` no vipa (assets buildados dentro do container,
exit 0, lock global segurando build paralelo); (3) painel: card Sync verde +
botão Rescan → aviso "rescan queued" → flag consumida pelo watch; (4) aceite
1 da 02 com 10 amostras (arquivo salvo no Windows → visível na VM em < 5 s).

## 14/10/2026 (parte 10) — F4 fechada (rota de deploy + token por projeto) e runner self-hosted buildado na VM

**Intenção** (pedido do Gabriel): rodar o plano orquestrado de runners —
fechar a F4 do fluxd (rota pública de deploy + token por projeto) e colocar a
infra do runner self-hosted de pé: repo próprio, snapshot da VM e imagem
buildada. Escopo do runner foi REDEFINIDO na rodada: em vez da imagem mínima
da F0 do plano, runner estilo tuxon com imagem quente completa (PHP 8.2–8.5,
Node 22/24, gcloud, lastpass-cli) no repo `gabrielbitello/flux-runner`.

**F4 — rota de deploy pública** (2 unidades em paralelo no working tree
compartilhado, nada commitado):

- Unidade a (`deploy.rs`, `router.rs`, `decision.rs`, `lib.rs`,
  `config.rs`): `POST /api/projects/{name}/deploy` nestada SÓ no host do
  painel no router :8080 (teste prova rotas de controle da :7420 inalcançáveis
  pela rota pública), atrás da flag `[deploy].public_route` — default FALSE
  (fechada); flag desligada = rota some em 404 brandiado (rollback = desligar
  flag, testado nos dois caminhos). Handler com ordem de fechamento: método
  ≠ POST → 404 fantasma; rate limit PRÓPRIO por projeto ANTES do auth
  (bucket dedicado 10/min burst 10 — 429 + Retry-After do 11º em rajada;
  borda R30 intocada); auth fail-closed contra `deploy_token_hash` (sem
  Bearer 401 + evento, sem cadastro 503 com comando, token de OUTRO projeto
  403 com causa, leitura falha 503); corpo estrito `{ref,actor?,run?}` com
  `ref` = SHA 40 hex (senão 422); 409 para deploy concorrente (guard em
  memória, solto pelo pipeline em qualquer desfecho); 202 + `tokio::spawn`
  com rastro obrigatório em `deploy_log` + `events` — nada silencioso.
  Pipeline declarado como CENÁRIO B: prepara o release-dir imutável
  `/srv/flux/releases/{name}/{sha}` (`git fetch origin` + guard
  `merge-base --is-ancestor` recusando SHA órfão + `git worktree add
  --detach`), mas build `{name}-prod:{sha}` e subida de container NÃO sobem
  (subida coerente é a blue-green da F10 — subir hoje herdaria
  `restart: "no"` de dev). `api.rs` ganhou APENAS `pub(crate)` em
  `bearer_token`/`sha256_hex`/`hash_confere`; `lib.rs:66` atualizado
  documentando a rota como ÚNICA exceção pública.
- Unidade b (`state.rs`, `flux-cli/main.rs`): SCHEMA_VERSION 1→2 com tabela
  `deploy_tokens`, migrador v1→v2 preservando dados com o guard de adulteração
  intacto; `set_deploy_token_hash`/`deploy_token_hash` fail-closed;
  `flux project token <nome>` real no lugar do stub — token CSPRNG, sha256
  lowercase 64 hex gravado via ssh sqlite3, nome validado contra
  `[a-z0-9-]` antes do ssh, falha de ssh com stderr real e exit ≠ 0, token
  claro impresso UMA vez, regenerar revoga (INSERT OR REPLACE).

**Testes e portões**: 5 cenários novos em 2 arquivos de teste (auth
fail-closed via router HTTP real; rate limit com 11 requests reais → 429 do
11º com Retry-After; 409 concorrente determinístico em runtime
current-thread, estável em 5 execuções; SHA órfão recusado com git real em
tempdir + release imutável; migration v1→v2 em banco real preservando dado,
com adulteração recusada nos dois caminhos do guard). Comando e resultado:
`cargo test -p fluxd` → **195 passed, 0 failed** (178 unit + 4
deploy_integration + 8 integration + 1 schema + 1 sleep + 2
state_migration_integration). Portões: `cargo fmt` + `clippy -D warnings` +
test (fluxd, flux-cli) verdes; sem dependência nova. Restou 1 warning
herdado em `state.rs:527` (arquivo da unidade b), fora dos meus arquivos.

**Runner self-hosted**:

- Repo `github.com/gabrielbitello/flux-runner` publicado PRIVADO (Makefile,
  `image/`, `deploy/`; `deploy/.env` AUSENTE confirmado — só placeholder).
- Snapshot VirtualBox `pre-flux-runner-build` criado ANTES de mutar a VM.
- Preflight na VM Flux: `/dev/sda1` com 61G livres (limite 8G), Docker
  29.9.0. Repo copiado por tar-over-ssh.
- Tentativa 1 FALHOU no passo gcloud: `E: Package google-cloud-sdk has no
  installation candidate ... However the following packages replace it:
  google-cloud-cli` (pacote renomeado upstream). Fix mínimo no HOST em
  `image/Dockerfile:148` (`google-cloud-sdk` → `google-cloud-cli`, 1 linha,
  NÃO commitado — decisão de commit fica com o Gabriel) e recopiado via scp.
- Tentativa 2 verde com cache: `google-cloud-cli` 589.0.0-0 instalado,
  `lastpass-cli` compilado em `/usr/local/bin/lpass`, zero ERROR no log,
  `make` encerrado limpo. **Imagem `flux-runner:candidate` presente na VM:
  4582 MB** (`docker image inspect` = 4805455756 bytes, bate com
  `make size`). O aviso `AVISO: flux-runner:current ausente no cache local`
  do `make size` é esperado — promote é proibido nesta execução, tag
  `current` não existe.
- Cerca respeitada: nada de `systemctl`, compose, prune ou container tocado
  na VM que serve prod. Tempo total ~12 min (2 tentativas de ~6 min) contra
  janela de 75 min.

**Pendente** (nesta ordem): colar PAT real no `deploy/.env` (runner NÃO
subiu); `make up` (unit systemd) + job de teste executado 5x seguidas; E2E
F5 (comutar pool → push → 202 pela cadeia pública → release na VM). Lacunas
R10 registradas no STATUS: blue-green/drain/lock, perfil estático completo,
rollback comandado, rotação de releases (spec 10:48).

## 14/10/2026 (parte 9) — WORKFLOW DE 7 FRENTES: ciclo CLI, beat, WAF, observe 24h, live+seed

**Intenção** (pedido do Gabriel): fechar o ciclo de uso diário (1: down/logs/exec),
beat JS (2), Adminer (3), SQLi/path scan com 403 e LAN fora do bloqueio,
monitoramento de recursos 24 h MUITO detalhado com filtros + perícia de pico
("gravar o que a imagem e as irmãs faziam no pico"), live server no dev
configurável (prod é só build), migrate automático com seed controlável.
"/workflow com o máximo de agentes assíncronos possíveis".

**Execução**: dwfrun-5788c36a — 7 construtores com arquivos disjuntos (stubs
pré-declarados em lib.rs para ninguém tocar arquivo alheio) + integrador +
portões determinísticos (fmt/clippy/test, 3 rodadas) + revisão de olhos
frescos com confirmador por achado. 2 escaladas resolvidas ao vivo: fallback
da seed = TRUE (spec 05 > frase mal escrita do meu prompt); pagina-observe
entrega com verde do próprio escopo (falhas remanescentes eram da frente
observe, ainda rodando). **242 testes no workspace**; commits `caa6c08`,
`c73929b`.

**Entregas** (todas provadas AO VIVO pós-run):
- `flux down/logs/exec` reais (lease segurando acordado; SSE `data:` por
  linha; exec com exit real — truncado = -1, não falso 0; 409 honesto quando
  o projeto dorme).
- Beat JS: `/__flux/beat` 204 + script injetado em HTML elegível (prova:
  grep no HTML pelo domínio público).
- WAF ENFORCE (decisão do Gabriel, acima do shadow da 16): SQLi + path scan
  → 403; LAN (192.168.140.0/24 + loopback) nunca bloqueada. **Furo achado na
  prova e consertado**: internet via túnel chegava com IP do cloudflared e
  herdava a allowlist — precedência agora é CF-Connecting-IP > XFF >
  ConnectInfo. Prova final: internet 403, LAN 200, /.env 403.
- Observe: sampler 30 s → metrics_1min (retenção 24 h), perícia de pico
  (cpu>80% ou mem>85% do limite) com snapshot JSON da imagem + irmãos
  coletados na hora, anti-spam 300 s; página `/observe` no painel (SVG 24 h,
  filtros geral→projeto→container, peaks expansíveis).
- Live server: `[dev] live` no flux.toml → exec -d no acordar (evento;
  nunca derruba o up) — provado com comando marcador.
- Seed: `[db].seed_on_rebuild` + `flux up --seed/--no-seed`; precedência
  CLI > toml > default da spec (true); NUNCA em prod.

**Adminer (3 do pedido)**: NÃO entregue ainda — ficou de fora do workflow
(sem frente atribuída); pendência honesta.

**Achados da revisão não corrigidos (baixos, registrados)**: peaks sem poda
(576 linhas/dia teórico em pico contínuo); exec com saída >256 KB pode
reportar exit de execução parcial; WAF depende do Caddy setar XFF (sem XFF
parseável = fail para ConnectInfo = loopback); LIKE do filtro ?projeto não
escapa %/_ (superfície autenticada).

**Próximo**: Adminer (F12), watch dos teardowns 6 h, SSE de logs no painel,
F10 (cotas) quando o 3º projeto entrar.

## 14/10/2026 (parte 8) — PRIMEIROS PROJETOS REAIS: presente-digital e djm VIVOS

**Intenção** (pedido do Gabriel): dev-only de presente-digital e djm ("sigla
djm"); `flux up -m` como wizard que GERA o `flux.toml`; prod continua comando
separado (99% é dev); modo dev temporário que se apaga da VM após 6 h sem
interação — nunca nada fora da VM.

**Entregas de código** (commits `2f0d9c2`→`24ccd32`): specs 06 (wizard) e 14
(temporário); `[temporary]` no flux-common; `temporary.rs` no fluxd (sweep de
60 s, relógio = interação→created_at, teardown compose down -v → rm dirs →
purge SQLite POR ÚLTIMO, nome inseguro = recusado); wizard `-m` no CLI
(auto-detecta framework, recusa flux.toml existente); **ciclo completo do
`flux up`** — sync mutagen idempotente → `POST /projects/{name}/up` (202 +
background: guard re-varre, override com `ports: !reset`/flux-net/`DB_HOST=
flux-db` gerado dos services REAIS do engine, `compose up -d --build`) →
poll do estado do ENGINE (não da linha de sono — falso positivo do default
'awake' provado no DJM) → migrations.

**Infra**: flux-db (mariadb:11) healthy em `flux-net`; DBs `{name}` +
`{name}_test` com users por projeto; regra NAT órfã `healthz` 7420 deletada.

**Projetos**: composes adaptados nos 2 repos (presente-digital `a0bbf05`,
djm `0b6a6ff`) — service `app`, mysql/phpmyadmin/tunnel/docker.sock FORA
(banco único, tunnel ÚNICO no host — o cloudflared do container do djm
brigaria pelo ingress). `.env` exclusivo da VM (sync ignora, persiste);
vendor/node_modules/storage das massas: composer/npm/migrate RODADOS no
container (sync não carrega artefatos).

**Resultado**: `https://presente-digital-d.bitello.xyz` → **200 em 254 ms**
(landing completa); `https://djm-d.bitello.xyz` → **302 → /entrar → 200**
(6 containers: app/worker/reverb/scheduler/stream/pi; rádio fica no profile
`radio` com credencial Spotify preservada). Robots dev `Disallow` (R22) nos
dois. Painel mostra os 2 projetos com auth.

**Tropeços reais** (registrados como NUNCA): (1) include do compose resolve
caminho pelo CWD — `./serve` do include não é do arquivo incluído; o fluxd
agora passa `-f` do compose real (`escolher_compose`); (2) `trim()` dentro
de `filter` não propaga pro `all()` — stub só-include era escolhido; (3)
pipeline de verificação terminando em pipe engoliu clippy/teste FALHANDO e
commitou quebrado 2× — verificação com exit code visível daqui em diante;
(4) `env_file` do mariadb quer `MARIADB_ROOT_PASSWORD`, não `DB_ROOT_PASSWORD`.

**O que NUNCA fazer** (novo): nunca confiar no `mode` da linha de sono como
estado de container (nasce 'awake'); nunca rodar `flux up` esperando build
síncrono no request HTTP (202 + poll); nunca marcar projeto real como
temporário sem o Gabriel saber — os DOIS entraram temporários (6 h) por
pedido dele: sem interação 6 h, a VM apaga sync+containers (repo Windows
intocado; `flux up` recria tudo).

**Próximo**: assistir os primeiros teardowns de 6 h (validação real do 14);
`flux sync reset`/`logs`/`exec` polidos; F12 Adminer; SSE de logs no painel.

## 14/10/2026 (parte 7) — F7: Caddy TLS + auth do painel + mutagen REAL

**Intenção**: fechar a cadeia de tráfego da 03 dentro da VM (Caddy → fluxd),
destravar o painel para o wildcard público com auth real (o NUNCA da 03/08) e
provar a sync mutagen com binário de verdade (F2).

**Auth real do painel (F8 núcleo, commit `74c8c1e`)**: `panel_auth.rs` —
Argon2id puro-Rust contra `/srv/flux/secrets/panel.argon2` (re-lido a cada
login; ausente = 503 honesto com a instrução de geração, fail-closed), sessão
por cookie `HttpOnly; SameSite=Lax` de 12 h em memória, rate limit 5 logins/min
por IP (antes de tocar no hash), guard em toda rota exceto `/login`. O painel
ENTROU no router público via `Action::Panel` (host `flux.bitello.xyz`) — o
cerco da porta 7421 virou atalho de debug, agora em `127.0.0.1`. Os três
binds do fluxd colaram no critério da 09 (só 22/80/443 respondem de fora).
78 testes no crate; e2e provado pelo TLS: sem sessão → 303 `/login`,
`/login` renderiza, login sem hash → 503 com instrução.

**Caddy estático (03)**: Caddyfile catch-all `*.bitello.xyz` → `tls internal`
→ `127.0.0.1:8080`. Achado real: bloco `:443` nu morria com
`tlsv1 alert internal error` (sem SNI não há cert pré-emitido) — corrigido com
`default_sni *.bitello.xyz` global, spec 03 atualizada. Prova do HOST via TLS
(host-only): proxy 200 nginx em 64 ms, ghost → 404 brandiado, painel → 303,
robots dev → `Disallow: /`.

**Mutagen real (F2 fechado na mecânica)**: `mutagen.exe` 0.18.1 +
`mutagen-agents.tar.gz` (bundle é arquivo SEPARADO no zip — sem ele o agente
não instala na VM) em `%USERPROFILE%\.flux\bin\`. Sessão real one-way-replica
Windows→VM: arquivo apareceu no `/srv/flux/projects/` em **374 ms** (aceite da
02: < 5 s). `~/.ssh/config` ganhou entrada `192.168.140.2` com a chave do Flux
(`IdentitiesOnly yes` — a chave nunca vai para outra máquina, 09).
`mutagen_bin()` do CLI resolve `MUTAGEN_BIN` → `%USERPROFILE%\.flux\bin\` → PATH.

**cloudflared no host (FECHADO à noite)**: binário 2026.10.0, serviço Windows
Running. Três tropeços registrados: (1) `.ps1` UTF-8 sem BOM — PS 5.1 lê como
ANSI e em-dash vira aspa curva (0x94) quebrando strings no parse → scripts
do repo saem ASCII-only; (2) chave de EventLog órfã da era devfactory fazia o
`service install` rolar back em silencio (serviço "installed", nada no SCM) →
script remove a chave e faz throw em exit ≠ 0; (3) o aviso do dashboard que
quase passa batido: **hostname curinga `*` não cria registro DNS** — o CNAME
`*` antigo apontava pro tunnel velho (1033 na internet); apontado à mão pro
tunnel novo. Prova final pela internet: `zzz.bitello.xyz` → 404 brandiado em
99 ms, `flux.bitello.xyz` → 303 guard, login renderiza, robots `Disallow`,
TLS real válido sem `-k`. **F7 FECHADO.**

**Higiene**: `passwd -d flux` — senha temporária do serial removida
(`passwd -S` = NP); SSH sem chave = `Permission denied (publickey)`.

**O que NUNCA fazer** (novo desta fase): nunca montar painel sem o guard de
sessão; nunca apontar o tunnel direto a container (sempre Caddy → fluxd); nunca
deletar `mutagen-agents.tar.gz` do diretório de binários; nunca confiar no
"installed" do cloudflared sem checar o SCM (`Get-Service`).

**Próximo**: primeiro projeto real com `flux up` + sync (página de espera
pela internet); hash da senha do painel (`printf '%s' 'SUA-SENHA' | argon2
"$(openssl rand -hex 16)" -id -e > /srv/flux/secrets/panel.argon2` dentro da
VM); F13 porteiro central (`fluxhost :8443`) na frente do cloudflared.

**CLI no PATH com túnel automático (ainda 14/10, commit `e5203a3`)**:
`flux.exe` release em `%USERPROFILE%\.flux\bin` (dir no PATH de usuário —
mesmo lugar de mutagen/cloudflared). `tunel.rs`: a `:7420` é 127.0.0.1-only
na VM (09), então o CLI sobe `ssh -N -L` SOB DEMANDA — probe TCP, processo
desapegado com CREATE_NO_WINDOW, idempotente. Dois achados reais: (1) SEM
`-f` — o OpenSSH do Windows (System32) não faz fork como o do Git Bash;
(2) regra NAT órfã `healthz` (host 7420 → VM 7420) da sessão anterior fazia
o probe ver porta "aberta" e o request morrer no NAT (fluxd não escuta na
interface) — deletada via `controlvm natpf1 delete`. Config global criado em
`%USERPROFILE%\.flux\config.toml` (token dev). Provado dos dois mundos:
PowerShell (ssh.exe System32) e Git Bash (MSYS ssh) → `flux status`
atravessa o túnel e responde honesto ("projeto não registrado — rode flux
up; registrados: teste").

## 13/10/2026 (parte 6) — FLUXD OPERACIONAL NA VM: router ponta a ponta provado

**Intenção**: pós-reboot do host (HVCI off + WSL/Docker desinstalados), subir a
plataforma de verdade.

**O que a investigação provou** (workflow dwfrun-b92ae519, 3 investigadores
read-only, 26 achados): a stack de rede do VirtualBox estava MEIO-INSTALADA —
driver `VBoxNetLwf` dava "erro interno" 5× e o serviço sumiu; host-only attach
falhava com `VERR_INTNET_FLT_IF_NOT_FOUND`; adapters recriados 3× no dia
(renumeraram para "Ethernet 5"); Flux.vbox divergiu (NIC1 virou e1000 sem
driver no guest, NIC2 NATNetwork). **Resquícios devfactory descartados como
causa** (caddy/coredns/cloudflared não estão no caminho host↔VM) — e foram
REMOVIDOS a pedido do Gabriel (serviço Cloudflared deletado, processos
mortos, tarefas desregistradas, pool UDP revertido, CrossDeviceService sob
demanda, 4 arquivos da sessão fdcd6fea apagados).

**Correção aplicada** (em ordem): `netcfg` registrou `oracle_VBoxNetLwf`
(rodando, bound na host-only), `pnputil` + `netcfg` registraram
`oracle_VBoxNetAdp6`, host-only recriada ("adapter #4", 192.168.140.1/24),
NICs da VM reescritas (virtio NAT + pf 2222/7420; virtio host-only).

**Resultado — plataforma viva**:

- SSH DIRETO `flux@192.168.140.2` em ~20 s pós-boot (nativamente; snail morto
  confirmado).
- `fluxd` compilado DENTRO da VM (rustup 1.99, build-essential, 6 vCPUs) com
  TODAS as fases: F2 sync, F3 router/docker, F4 sono, F5 guard/migrations,
  F6 API+CLI, F8 painel, R30 borda.
- Instalado via systemd (`scripts/host/install-fluxd.sh`, corrigido para criar
  config mínima idempotente): `fluxd.service` active, healthz
  `{"status":"ok","version":"0.1.0"}`.
- **Router ponta a ponta provado com container real** (nginx:alpine com labels
  da convenção): proxy 200 + `x-robots-tag: noindex` (dev), container parado →
  página de espera 200, host fantasma → 404 brandiado, `/robots.txt` →
  `Disallow: /` (R22).

**Estado**: fluxd-linux no E: OBSOLETO (compilação agora é dentro da VM).
`flux.toml` token da API: settings `api_token_hash` no SQLite (sha256), setado
na VM. Pending list honesta: Caddy reverso + tunnel (F7), worker de build no
CI para não depender de rustup na VM, painel na 7421 sem auth (F8 real antes
de expor).

**Próximo**: Caddy na VM servindo `*.bitello.xyz` → fluxd (F7), primeiro
projeto real sincronizado (F2 com mutagen binário), painel com auth.

## 13/10/2026 (parte 5) — F2, F6, F8 e borda R30 por workflow assíncrono (6 agentes)

**Intenção**: "implemente mais 4 fases de forma assíncrona usando /workflow com o
máximo de agentes possível, foco operacional" — com proibição dura de tocar
WSL/Docker/Ubuntu (outras coisas usando; desinstalação é decisão do Gabriel pós-
reboot). Fases escolhidas por serem 100% código e testarem SEM engine: F2, F6,
F8, R30.

**Como rodou**: workflow `dwfrun-aa90c392` — 4 construtores paralelos com
ARQUIVOS DISJUNTOS (nada de conflito), integrador que liga os módulos e caça
compile, portões determinísticos (`cargo clippy -D warnings` + `cargo test
--workspace`, 3 rodadas de correção), revisor de olhos frescos no diff, correção
dos achados, portões de novo. 3 escaladas resolvidas ao vivo (ligação de deps do
workspace nos Cargo.toml, ponte de testes #[path] do sync, 1 linha de `pub mod
edge;` no lib.rs).

**Entregado** (commits `e959857`, `9ab1b0b`, `24b9775`, `795459e`, `d6c4412`):

- **F2** — `crates/flux-cli/src/sync.rs`: orquestração mutagen completa
  (one-way-replica, 10 ignores globais da spec 02, sessão por projeto +
  worktree slug, status via JSON Lines do template nativo, create idempotente
  com aviso de reuso, reset = terminate+create com full rescan). 23 testes.
- **F6** — `crates/fluxd/src/api.rs` reescrito: register/status/lease/schema
  com validação estrita e auth da 09 (token sha256 fail-closed, 401 logado no
  events, sem hash configurado = 503 em tudo exceto /healthz; comparação
  constant-time); CLI real conversando com a API (ciclo dev com lease do sleep
  manager). 
- **F8** — `crates/fluxd/src/panel.rs`: dashboard/detalhe/eventos server-side,
  zero lib JS, curva 24h SVG, X-Robots-Tag, aviso de SEM AUTENTICAÇÃO visível
  (painel real com auth substitui antes de qualquer exposição).
- **R30** — `crates/fluxd/src/edge.rs`: borda tower montada NA FRENTE do
  router — 300 req/min burst 100 por host, 100 conexões, 413 cedo, 429/413
  família Flux vs JSON.
- **Integração**: edge na frente do router, painel na :7421 (porta própria por
  falta de auth), módulos declarados.

**Revisão independente**: 8 achados (1 high "borda não montada", 1 high
"primeiro create de sync quebraria", fallback da API sem gate, panel fora da
build, IPv6 no host_key, reuso silencioso de sessão, unwrap_or(0) do relógio,
comparação de token early-exit) — TODOS corrigidos pelo integrador antes dos
portões fecharem; conferidos um a um no código final por mim. Portões: clippy
`-D warnings` limpo + **127 testes** verdes.

**Estado**: sem WSL/Docker/VM tocados — testes de engine pulam por design.
`fluxd` Linux do E: NÃO inclui F2/F6/F8/R30 ainda — recompilar pós-reboot.

**Próximo**: pós-reboot → VM boot validado → recompilar `fluxd` Linux completo
→ instalar na VM → host-only → primeiro router ponta a ponta (acordar/dormir de
verdade + painel na 7421 + CLI `flux up` de um repo real).

## 13/10/2026 (parte 4) — F4 e F5: sono automático + guard/migrations, tudo provado em engine real

**Intenção**: "implemente os próximos 2" — as duas fases seguintes da ordem das
specs, ambas testáveis contra o docker do WSL enquanto a VM espera o reboot.

**F4 — sono automático** (commit `3212b52`):

- `sleep.rs`: sweep de 60 s ligado no `fluxd serve` (spawn tokio). Decisão
  PURA `should_sleep` (tabela testada): só dorme container `Awake`, ocioso
  além do limite, sem lease e sem cooldown.
- Sinais que zeram o timer: request proxyado (o router toca
  `touch_interaction`), lease de sessão (`set_lease` com TTL — canal do
  `flux logs -f` da F6), CPU acima do teto (snapshot de stats no sweep;
  `[sleep] cpu_percent_teto`).
- Cooldown anti-flapping de 5 min pós-acordar — `wake`/`ensure_awake`
  reportam se EXECUTARAM transição.
- Dev dorme com `stop` (`[sleep] idle_minutes`, default 15), prod hiberna
  com `pause`/`stop`/`off` (`prod_mode`, default pause 30 min) — direção da
  spec 11. `flux-db`/Adminer/`fluxd` nunca entram (sweep itera SÓ projetos).
- Verdade = Docker: sweep re-sincroniza `mode` divergente e grava evento.
- Teste gated em engine real: cooldown segura o recém-acordado; com sinal
  antigo + `idle_minutes = 0`, dorme (`stop`) e registra `sleeping_since`
  (12,5 s).

**F5 — guard + migrations** (commit `20ba7c4`):

- `guard.rs` PURO: varre compose (`image:`) e Dockerfile (`FROM`, multistage,
  `--platform`, `scratch` fora), pulando comentários. Match por BASENAME do
  repo — `bitnami/mariadb` cai em `mariadb`, `library/postgres` em `postgres`;
  coringas `mysql-*`/`postgresql*`; violação carrega arquivo + linha + imagem
  + padrão. Limitação honesta: varredura por linha (parse de YAML estruturado
  entra na F6, quando o fluxd já renderiza overrides). 8 testes.
- `[image_guard].block` default do config global agora é a lista COMPLETA da
  spec 05 (17 padrões).
- `db.rs`: `plan_migrations` diff por hash (novas/mutadas/apagadas) +
  `ensure_schema`:
  - laravel: novas → `php artisan migrate --force`; dev com mutação →
    `migrate:fresh` (+`--seed` conforme `[db] seed_on_rebuild`) + regravação
    do estado; **prod com mutação → ERRO e bloqueio SEM executar nada** (05).
  - node/rust: `[db] migrate_cmd` via `docker exec` a cada ciclo
    (idempotência é do comando); sem cmd → aviso explícito, nunca silêncio.
  - Execução SEMPRE dentro do container (`docker exec` — único shell da
    plataforma, 07). Hashes aplicados em `migration_state`.
- Teste gated: `migrate_cmd` roda dentro do container (marker criado no FS
  dele) e prod mutado bloqueia sem tocar no Docker.

**Estado**: 63 testes na suíte (20 commons + 38 fluxd lib + 5 integração) +
3 gated de engine. `fluxd` Linux recompilado com F4+F5:
`E:\flux\fluxd-linux` (11 MB). Faltam do F4/F5 para "SIM" no STATUS: janela
de silêncio do sync (F2), `flux-db` real, ciclo do `flux up` (F6).

**Próximo**: pós-reboot do host — VM boot validado → `fluxd` com F3+F4+F5
dentro da VM → host-only → router ponta a ponta. Depois F2 (sync mutagen) e
F6 (CLI real: `up/logs/exec/down` com lease de sono).

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
