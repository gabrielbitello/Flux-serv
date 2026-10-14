# 10 — Deploy de prod (GitHub Actions) + troca de versão invisível

## Objetivo

Prod nunca é servido de arquivos do Windows: o código de produção mora **100% na
VM** (clone git) e entra por **GitHub Actions**, com template pronto no Flux. A
troca de versão é invisível para o usuário (blue-green) e não perde jobs de
background em andamento. Arquivos de prod são imutáveis e servidos no perfil
"estático": mais rápido e igual ou menos consumo.

## Mecanismo

### Origem do código de prod

- `flux deploy` via Windows **não existe mais** — deploy só vem do CI. O clone
  de prod vive em `/srv/flux/projects/{name}-prod` na VM (bare worktree por
  release, ver abaixo).
- `flux init ci` escreve o workflow no projeto a partir do template
  [`docs/templates/deploy-workflow.yml`](../templates/deploy-workflow.yml):
  `on push` em `main` (ou tag `v*`, escolha no template) → um `curl` notificando
  o `fluxd` com o SHA do commit. **Nada de SSH da action para a VM** — a VM não
  expõe SSH na internet; o endpoint de deploy do `fluxd` é HTTPS pelo próprio
  tunnel, autenticado por token por projeto (`FLUX_DEPLOY_TOKEN` em GitHub
  Secrets, gerado por `flux project token`).
- Repos privados: `flux repo add {url}` grava deploy key/token de leitura na VM.
  A VM faz `git fetch`; a action não manda código, manda **intenção**.

### Domínio e túnel no deploy — descoberta com fallback (pedido do Gabriel, 14/10)

O deploy NUNCA bloqueia por túnel. A descoberta do domínio público é assim:

1. Candidato: `{name}.bitello.xyz` (derivado do nome — o que o Flux daria
   de qualquer jeito).
2. Túnel do host vivo E nome disponível (sem outro projeto dono)?
   **Assume** — esse é o domínio público dele, público de verdade.
3. Não achou/indisponível? **Ignora e segue**: sobe pelo caminho padrão do
   Flux (Caddy → fluxd → container), respondendo em
   `{name}.bitello.xyz` pela cadeia normal. Sem deadlock, sem abortar.

Túnel/cloudflared no compose do projeto: guard por camada da 05 — no
service principal (`app`) = NADA sobe; em secundário = sobe tudo menos ele.
Nunca connector de projeto disputando com o do host (03/12).

### Pipeline do deploy (roda no `fluxd`, passo a passo)

1. **Fetch**: `git fetch origin` no clone de prod; checkout do SHA em worktree
   novo `/srv/flux/releases/{name}/{sha}` — cada release é um diretório
   imutável; N versions ficam em disco (rotação 5).
2. **Guard**: análise de compose/Dockerfile (05) — continua valendo.
3. **Build**: imagem `{name}-prod:{sha}` com perfil de produção (abaixo),
   incluindo assets; sidecar node roda `npm ci && npm run build` montando o
   release; artefatos ficam no release.
4. **Release imutável**: o diretório do release é montado no container
   **read-only**; só `storage/` (dados de runtime) é volume rw próprio do
   ambiente. Nada de edição in-place: "corrigir prod" = novo commit + novo
   deploy. `flux exec` em prod é **read-only por padrão** (comando de escrita
   exige `--writable` e fica auditado no painel). No mesmo passo o `fluxd`
   registra no SQLite `has_robots`/`has_sitemap` (existência de
   `public/robots.txt` e `public/sitemap.xml` no worktree do release) — o
   router usa os flags para servir robots/sitemap gerados quando faltam (03).
5. **Migrations**: pipeline de prod (05) — só `migrate`, mutação bloqueia.
   Há pendência? Snapshot pré-migrate ANTES do `migrate --force` (14): dump
   `--single-transaction` registrado no `deploy_log`; dump falhou = deploy
   para com erro, banco intacto.
6. **Blue-green** (abaixo): sobe cor nova, health, troca, drena cor velha.
7. **Pós**: URL final no log da action; painel registra release no histórico.

### Zero downtime (blue-green)

- Duas famílias de containers por projeto prod: `{name}-prod-blue` e
  `{name}-prod-green` (HTTP + worker cada). O `fluxd` guarda ponteiro
  `ativa = blue|green` no SQLite; o router proxya para a cor ativa.
- Deploy sobe a cor **inativa** com o release novo → healthcheck (path do
  projeto, timeout 90 s) → **troca atômica** do ponteiro (requests novos já
  nascem na cor nova; requests em voo na cor velha terminam nela) → drenagem
  da cor velha.
- **Drenagem**: cor velha recebe `SIGTERM`; containers HTTP param de aceitar
  e esperam conexões ativas acabarem (timeout 60 s); **workers da cor velha
  terminam o job atual antes de morrer** (`queue:restart` semântica Laravel +
  `--timeout` do worker; teto 300 s). Só depois do drain a cor velha é
  removida. Job que estava rodando **nunca é morto no meio** — ou termina na
  versão velha, ou (job enfileirado ainda não iniciado) é pego pela versão
  nova.
- **Scheduler único**: `schedule:work` roda em UM worker por vez — lock de
  scheduler no cache do Laravel (driver `database`, banco único). A cor nova
  só assume o lock depois da velha soltar (fim do tick atual). Sem execução
  dupla de tarefa agendada.
- **Rollback**: `flux prod rollback {name}` reativa a cor do release anterior
  (ainda em disco), sem build, em segundos. Migrations revertem NÃO — rollback
  de código assume migrations compatíveis (padrão expand/contract, anotado no
  template).
- Usuário na recepção: zero reload forçado, zero 5xx — o swap é um update de
  upstream no router, ~1 ms.

### Perfil estático de prod (imutável = rápido)

Por que mais rápido gastando igual ou menos: assets não passam pelo PHP e o
PHP roda pré-compilado. No build do release, o `fluxd` aplica:

- `composer install --no-dev --optimize-autoloader --no-interaction`;
- `php artisan config:cache && route:cache && view:cache && event:cache`
  (opcache `validate_timestamps=0` — PHP nunca re-lê arquivo);
- opcache preload quando o projeto declara;
- `APP_ENV=production`, `APP_DEBUG=false` forçados pelo fluxd (não dependem do
  `.env`).

Perfil por `framework` (R26, tabela completa em 06): os comandos acima são do
**laravel** (suporte máximo); `node` = `npm ci --omit=dev` + build;
`rust` = `cargo build --release` no sidecar. Assets pelo fluxd, blue-green,
drain e recursos são idênticos para qualquer stack.

Servir estático: o **router do `fluxd`** intercepta prefixos de asset
(`/build/*`, `/vendor/*`, `favicon`, fonts — lista derivada do release) e
entrega direto do disco do release, sem tocar o container. `/robots.txt` e
`/sitemap.xml` entram no mesmo intercept (03): ausentes no release → gerados;
presentes → proxy normal (preexistente nunca é sobrescrito):

| Arquivo | Cache-Control |
|---|---|
| Fingerprinted (`app-abc123.js`, hash no nome) | `public, max-age=31536000, immutable` |
| Não fingerprinted | `public, max-age=3600` |
| `index.php`/HTML/API | sem cache no edge, vai ao container |

Ganho mensurável: request de asset sai por sendfile do fluxd (sem PHP-FPM, sem
Laravel boot ~30-80 ms por request), CPU do container cai; mesmo tráfego,
menos processos PHP vivos. Medição no aceite.

### Segurança do endpoint de deploy

- `POST /api/projects/{name}/deploy` com `Authorization: Bearer` do token do
  projeto (hash no SQLite, igual ao token de API, 09). Rate limit 10/min.
- Deploy concorrente no mesmo projeto: segundo pedido entra em fila (não
  interrompe o primeiro). Deploy durante rollback: bloqueado, erro claro.

## Decisões

| Decisão | Alternativa descartada | Por quê |
|---|---|---|
| Action notifica via HTTPS (curl) | action SSH na VM / self-hosted runner | VM não expõe SSH público; runner GH é efêmero e barato; um POST = zero infra no CI |
| Build na VM (sidecar node) | build de imagem na action + registry | Sem registry externo para manter; build na VM reusa cache de camadas e a rede do pull é local |
| Blue-green (2 corpos) | rolling restart / swarm | Com 2 cores a garantia de "nenhum request cai" é trivial; rolling exige orquestrador (k8s) para menos |
| Releases imutáveis em disco | rsync por cima do clone | Imutabilidade dá rollback instantâneo e cache de asset sem invalidação |
| Assets pelo fluxd | Caddy servindo direto | Um lugar só com a lógica de release/rotação; Caddy segue estático (03) |

## O que NUNCA fazer

- **Nunca** editar arquivo de release de prod em disco — release é imutável;
  mudança = commit + deploy.
- **Nunca** matar worker com job em andamento — drain é obrigatório (teto 300 s
  antes de forçar).
- **Nunca** rodar dois `schedule:work` no mesmo projeto (lock obrigatório).
- **Nunca** exibir na action informações de segredo (logs redact).
- **Nunca** aceitar deploy de SHA que não é descendente da branch de produção
  configurada (proteção contra deploy de commit órfão).

## Critérios de aceite

1. Push em `main` de projeto com template → deploy completo sem toque manual;
   action loga URL final e versão.
2. Durante deploy ativo, loop de requests (ex.: 100 req/s) → 0 falhas, 0 5xx;
   latência p99 estável na troca.
3. Job longo (ex.: sleep 240 s enfileirado) no momento do deploy → termina
   integro na cor velha; nada re-executa.
4. Tarefa agendada a cada minuto → nenhum minuto pulado nem duplicado na
   janela do deploy.
5. `flux prod rollback` volta a versão anterior em < 10 s, sem build.
6. `curl` num asset fingerprinted: header `immutable`, resposta saída do fluxd
   (log de acesso marca `static`); comparar tempo/CPU antes/depois com
   medição em regime.
