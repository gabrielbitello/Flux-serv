# 06 — CLI `flux` (Windows)

## Objetivo

O Gabriel roda `flux up` dentro de qualquer repositório e o projeto sobe na VM —
build, banco, migrations, sync, domínio — sem prompt de sync e sem decorar nada
de docker. Os comandos básicos do docker/compose existem com prefixo `flux`.

## Mecanismo

### Princípios

- **Diretório = contexto.** O CLI descobre o projeto pela pasta atual:
  `flux.toml` explícito ou detecção (nome da pasta, `compose.yaml`, `artisan`).
- **Zero prompt.** Sync é contínua (02), nunca interativa. Prompts existem só em
  ações destrutivas (`vm --recreate`, `db fresh` em prod é impossível por
  design, então nem prompt tem).
- **Progresso visível.** Comandos longos imprimem etapas (`[2/6] sync inicial…
  412 arquivos`) e terminam com o par URL + status.
- **Erros nunca silenciosos.** Erro de qualquer etapa → mensagem com origem
  (CLI/fluxd/docker) e exit code ≠ 0. Pipe de verificação sempre com exit code
  preservado.

### Comandos

| Comando | Faz |
|---|---|
| `flux up [env]` | ciclo completo do dev (padrão) — ver fluxo abaixo |
| `flux down [env]` | `docker compose stop` do projeto (containers param; banco único segue de pé) |
| `flux restart [env]` | down + up (sem rebuild) |
| `flux status [env]` | sync, containers, health, banco, migrations pendentes, URL, sono |
| `flux ps [env]` | espelho do `docker compose ps` |
| `flux logs [-f] [--since 2h] [--until 1h] [svc] [env]` | logs com lease de sono ativo; janela histórica com `--since/--until` (13) |
| `flux exec <cmd…> [env]` | `docker exec` no container principal |
| `flux shell [env]` | `flux exec bash` |
| `flux build [env]` | build das imagens (sem subir) |
| `flux prod rollback {name}` | reativa release anterior em prod (10), sem build |
| `flux maintenance on\|off {name} [--bypass]` | modo manutenção: 503 brandiado + `Retry-After`, bypass por URL com token 24 h (15) |
| `flux shield unban {ip}` | desbloqueia IP na lista do WAF próprio (16) |
| `flux init ci` | escreve `.github/workflows/deploy.yml` no projeto a partir do template (10) |
| `flux project token {name}` | gera/rotaciona token de deploy do projeto (vai para GitHub Secrets) |
| `flux tool <runtime> [args…]` | roda comando em container efêmero na VM — toolchain sob demanda (abaixo) |
| `flux watch` | serviço leve no Windows: monitora branches/worktrees e reporta ao `fluxd` (02) |
| `flux db status\|migrate\|fresh\|seed\|console\|dump` | banco do projeto (ver 05); `console` = cliente SQL no container `flux-db` |
| `flux sync status\|pause\|resume\|reset` | controle da sync (02) |
| `flux env pull\|push\|edit` | ciclo do `.env` exclusivo da VM (02) |
| `flux vm status\|start\|stop\|ssh\|checkpoint` | ciclo da VM (01) |
| `flux vm upgrade` | checkpoint Hyper-V + novo fluxd com rollback automático (14) |
| `flux self-update` | atualiza o CLI no Windows com checksum verificado (14) |
| `flux export\|import` | DR: zip de config+state+secrets cifrados com passphrase; reergue VM nova (14) |
| `flux db restore {name} {arquivo} --yes` | restaura dump pré-migrate de prod (14) |
| `flux tunnel status` | saúde do cloudflared (03) |
| `flux panel` | abre o painel no browser (login único) |
| `flux setup host\|vm\|tunnel\|uptime` | provisionamento + endpoint `healthz` p/ monitor externo (01/03/09/13) |

`[env]` = `dev` (default) ou `prod`. Sempre -d (detached) por padrão; `-a` anexa.
Deploy de prod não é comando do CLI: é `git push` disparando a GitHub Action
(10). O CLI opera prod para diagnóstico/rollback, nunca para publicar.

### `flux tool` — toolchain sob demanda (R17)

Rodar build/ferramenta SEM instalar nada no PC — o container efêmero sobe na
VM, executa e morre:

```bash
flux tool node -- npm run build          # node:22 (pinado no config global)
flux tool php -- artisan tinker          # php:8.4-cli com os mounts do projeto
flux tool composer -- require foo/bar    # composer:2
flux tool rust -- cargo build --release  # rust:1
```

- Mounts idênticos aos do container do projeto (precedente: sidecar node do
  `provision-dev.sh` do porteiro) — `vendor/` e `node_modules/` ficam na VM,
  o Windows nunca baixa dependência.
- Imagens oficiais, versões pinadas no config global (`[tools]`), pull on
  demand com progresso no terminal; container `--rm`, nada fica de pé.
- Sem `flux tool`, projeto Laravel não exige PHP, Node nem Composer no Windows.

### Multi-stack: PHP primeiro, Node e Rust cidadãos de primeira classe (R26)

O suporte MAIOR é Laravel/PHP — é o que os projetos reais usam (vippers, pedi)
e o único caminho 100% automático v1. `framework` é auto-detectado
(`artisan` → laravel; `Cargo.toml` → rust; `package.json` com server → node;
senão static) e cada tipo resolve health, schema e perfil de build:

| | `laravel` (default, primeiro) | `node` | `rust` | `static`/`custom` |
|---|---|---|---|---|
| Health (03) | `/up` (`[health] path`) | `[health] path` OBRIGATÓRIO; sem ele, check TCP da porta do container | idem node | check TCP |
| Schema/migrations (05) | pipeline artisan automático (diff/rebuild) | `[db] migrate_cmd` rodado no container a cada up/deploy (idempotência é do comando; sem hash/mutação) | idem node | nenhum |
| Perfil estático do deploy (10) | completo (composer/opcache/caches) | `npm ci --omit=dev` + build; server node toca o app | `cargo build --release` no sidecar rust | assets servidos pelo fluxd |
| Sono/blue-green/recursos | idênticos | idênticos | idênticos | idênticos |

- Tudo que é INFRA (sync, sono, domínios, banco único, resources, deploy
  blue-green, fila) é independente de linguagem — muda só o adapter.
- `migrate_cmd` sem valor em node/rust: fluxd avisa no `flux up` que o projeto
  não tem pipeline de schema (dado explícito, nunca silêncio).
- Adapter novo (python, go...) = enum + 3 implementações; extensão prevista,
  não prometida para v1.

### Fluxo do `flux up` (dev), passo a passo

1. **Contexto**: lê `flux.toml`/detecta projeto; valida guard de imagens ANTES
   de qualquer rede (05). `flux up -m` (manifesto) abre o WIZARD abaixo antes
   deste passo.
2. **VM**: SSH ping → se falhou, pede start ao serviço `fluxhost` e espera
   (progresso). Verifica `fluxd` saudável.
3. **Registro**: envia `flux.toml` + hash dos arquivos de compose/Dockerfile ao
   `fluxd` (API de controle). Mudou? `fluxd` re-renderiza os overrides.
4. **Sync**: garante sessão mutagen do projeto (cria se falta; mostra progresso
   do full scan inicial quando é a primeira vez — depois disso, é fundo de
   poço, silencioso).
5. **Subida**: `fluxd` aplica overrides de compose (abaixo), build se hash de
   Dockerfile/compose mudou, `compose up -d`, espera health (timeout 90 s).
6. **Banco**: pipeline de migrations (05) — novo/mutado conforme ambiente.
7. **Provision**: hook `on_wake` se marcador de throttle expirou.
8. **Saída**: `https://{name}-d.bitello.xyz · dev acordado · banco em dia · sync ok`.

### Overrides de compose (aplicados pelo `fluxd`, nunca editados no repo do projeto)

- remove `ports:` de todos os services (regra de ouro);
- conecta em `flux-net` (além da rede default do compose);
- renomeia projeto para `flux-{name}-{env}` (containers `flux-{name}-{env}-app-1`…);
- injeta env de banco (`DB_HOST=flux-db` etc.) só se o service não definir;
- `restart: unless-stopping` em prod, `no` em dev (o sono controla o ciclo).

O arquivo original do projeto segue intocado — overrides são gerados em
`/srv/flux/state/compose/{name}-{env}.override.yml` e passados via `-f`.

### Wizard `flux up -m` (o CLI GERA o `flux.toml`)

`-m` / `--manifest` = série de perguntas no terminal; respostas viram um
`flux.toml` na raiz do repo e o `up` segue normal. Existe porque o manifesto
explícito é a regra (deny_unknown_fields) e digitar TOML na mão é atrito
desnecessário para os 3–4 campos que mudam de projeto para projeto.

Perguntas (todas com default no Enter; Ctrl+C aborta sem escrever nada):

| # | Pergunta | Default |
|---|---|---|
| 1 | Nome do projeto (domínios derivam dele) | nome da pasta em kebab-case |
| 2 | Framework | laravel (auto-detectado: `artisan` presente → laravel; `package.json` → node; `Cargo.toml` → rust) |
| 3 | Health path | `/up` (laravel) — node/rust exigem resposta |
| 4 | Dev TEMPORÁRIO? (apaga da VM após TTL sem interação, 14) | não |
| 5 | (se temporário) TTL em horas | 6 |
| 6 | `extra_ignore` de sync (vírgula-separado, vazio = nada) | vazio |

- `flux.toml` já existe → wizard RECUSA (nunca sobrescreve silencioso);
  sair editando à mão ou apagar antes.
- Perfil de uso (decisão do Gabriel, 14/10): **99% é dev** — `flux up` é o
  caminho comum; **prod é comando separado e raro** (`flux deploy`, 10) e não
  entra no wizard.
- O `flux.toml` gerado é versionável: revisa no git como qualquer código.

### `flux.toml` (raiz do projeto, versionado)

```toml
name = "vippers"            # default: nome da pasta
framework = "laravel"       # laravel | node | rust | static | custom — auto-detectado (R26, abaixo)

[domains]                   # derivados de name por padrão
# prod = "vippers.bitello.xyz"
# dev  = "vippers-d.bitello.xyz"

[db]
rebuild_policy = "auto"     # auto | always (só dev) | never
seed_on_rebuild = true
# migrate_cmd = ""          # node/rust: comando de schema dentro do container (ex.: "npm run migrate"); vazio = sem pipeline automático
# image_guard = true        # default global true; desligar é proibido

[sleep]
dev_enabled = true
idle_minutes = 15
prod_mode = "pause"        # pause | stop | off (11)
prod_idle_minutes = 30
cpu_percent_teto = 5.0

[resources]                # limites INDIVIDUAIS de prod (11)
http_memory_mb = 512
http_cpus = 1.0
worker_memory_mb = 256
worker_cpus = 0.5

[sync]
extra_ignore = []           # só ADICIONA ao ignore global

[temporary]                 # modo dev temporário (14): apaga da VM sozinho
# enabled = true            # default false — precisa ser dito
# ttl_hours = 6             # sem INTERAÇÃO nenhuma por esse tempo = teardown

[provision]
on_wake = ""                # comando dentro do container, ex.: "bash /provision.sh"

[branding]                  # páginas da plataforma em nome do projeto (15)
# logo_url = ""             # logo na página de manutenção

[shield]                    # WAF próprio (16)
# auth_paths = ["/login"]   # default: detecta login padrão
# tarpit = false            # atrasar scanner reincidente

[health]
path = "/up"                # path de health do app
```

Validação do `flux.toml` é estrita: chave desconhecida = erro (nada de typo
silencioso).

## Decisões

| Decisão | Alternativa descartada | Por quê |
|---|---|---|
| CLI conversa com `fluxd` por SSH+API | docker context remoto (TCP 2375/TLS) | Um caminho autenticado só; o fluxd valida guard/overrides — docker exposto direto furaria as regras |
| Overrides gerados pelo fluxd | pedir compose "correto" no repo do projeto | Projetos existentes entram sem edição; regra central vale para todos |
| `flux up` = dev, `flux deploy` = prod | mesmo comando com flag | Verbos distintos para operações com garantias distintas (05) |

## O que NUNCA fazer

- **Nunca** pedir confirmação de sync no terminal (não existe "sync após
  construir no prompt").
- **Nunca** rodar docker/compose/artisan no Windows — o CLI não exige Docker
  Desktop nem PHP local.
- **Nunca** sair do comando com exit 0 se qualquer etapa falhou.
- **Nunca** imprimir segredo (senha de banco, token) na saída — mostrar caminho
  do arquivo em vez disso.

## Critérios de aceite

1. Repo qualquer com compose: `flux up` do zero (VM ligada) → URL respondendo
   em ≤ 2 min sem input do usuário.
2. `flux logs -f` segue logs ao vivo; Ctrl+C solta; sono só retoma contagem
   depois do fim do comando.
3. `flux exec "php artisan route:list"` funciona no repo Laravel sem PHP local.
4. VM desligada + `flux up` → VM sobe sozinha e o comando completa.
5. Todo erro produzido sai com contexto e exit code correto (testar: compose
   inválido, VM off sem Hyper-V instalado, guard bloqueado).
