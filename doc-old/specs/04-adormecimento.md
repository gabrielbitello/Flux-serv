# 04 — Adormecimento automático (estudo de caso: porteiro)

## Objetivo

Container de app dorme (`docker stop`) após ociosidade e acorda na primeira
interação real, sem intervenção — o comportamento do porteiro que já roda em
produção no WSL (`Documents/Projetos/porteiro`), portado para o `fluxd` com as
lições de operação embutidas.

## Estudo de caso — como o porteiro resolve hoje (FATO, `src/main.rs`)

Mecanismo exato do porteiro (linhas do `main.rs` atual):

1. **Detecção de interação humana** (`INJECT_JS`, linhas 16–39): proxy injeta
   `<script src="/__porteiro/inject.js">` antes de `</body>` em respostas
   `text/html` **sem** `content-encoding`. O script cria canvas overlay
   (`pointer-events: none`, `z-index: 2147483647` — zero interferência) e
   escuta `mousemove, mousedown, keydown, wheel, scroll, touchstart`, mandando
   `navigator.sendBeacon('/__porteiro/beat')` **throttle 30 s**, mais beat por
   intervalo a cada 60 s. Qualquer request proxyado também zera o timer
   (linha 578).
2. **Ociosidade com guarda de CPU** (sweeper, linhas 309–352): a cada 60 s,
   `docker stats --no-stream` por container. CPU > teto (5%) = teste/build
   rodando → zera `last_seen`. Abaixo do teto **e** sem request/beat por
   `idle_minutes` (15) → `docker stop`. MySQL nunca dorme.
3. **Re-sync de estado externo** (linhas 322–335): a cada sweep, `docker inspect
   -f {{.State.Running}}` re-sincroniza o estado interno — terceiros podem
   startar/parar containers por fora sem quebrar o porteiro.
4. **Wake** (`iniciar_dev`, linhas 165–209): flag `acordando` anti-reentrada,
   `docker start`, poll de Running por até 10 s (500 ms de passo), depois
   provisionamento em background com throttle de 10 min (`provision-dev.sh`).
5. **Página de espera** (`pagina_espera`, linhas 226–247): HTML 200 com
   `<meta http-equiv="refresh" content="3">`.
6. **Upstream morto no meio** (linhas 471–499): conexão falha → reavalia
   Running → trata como dormir/acordar em vez de 502.

Limitações conhecidas documentadas no README do porteiro (o Flux resolve):
- `npm run build` **no Windows** não segura o dev acordado (container não vê
  CPU nem exec) → Flux: lease de sessão do CLI (abaixo).
- Resposta HTML comprimida não recebe overlay → Flux: proxy força
  `Accept-Encoding: identity` apenas nas respostas HTML pequenas elegíveis a
  injeção (streaming preservado no resto).

## Mecanismo no Flux

### Sinais que mantêm acordado (qualquer um zera o timer)

| Sinal | Origem |
|---|---|
| Request HTTP real proxyada | router do `fluxd` |
| Beat JS (`/__flux/beat`) | overlay injetado, mesmas regras do porteiro (throttle 30 s, eventos idênticos, `pointer-events: none`) |
| CPU acima do teto (5% default) | `bollard` stats no sweep de 60 s |
| **Lease de sessão (novo)** | `flux up/logs/exec/build` e o painel registram lease com TTL renovado enquanto o comando roda — resolve a limitação do porteiro com builds no Windows |

### Ciclo

- **Dormir**: sweep 60 s; sem sinal há `idle_minutes` (dev: 15) → `docker stop`
  no compose do projeto (só containers do app; `flux-db`, phpmyadmin e `fluxd`
  NUNCA dormem). Estado `sleeping_since` gravado no SQLite.
- **Acordar**: request no host → estado dormindo → `docker compose start` +
  página de espera (fase `acordando`) → health OK → proxy normal. Anti-reentrada
  por flag em memória (lição direta do porteiro).
- **Cooldown anti-flapping (novo vs porteiro)**: após acordar, projeto fica
  imune a dormir por 5 min (evita loop acorda-dorme em visits curtos com cache).
- **Hold de API (`[sleep] api_hold`, 18/10/2026)**: opt-in por projeto
  (default OFF — sem a chave, nada muda). Com `api_hold = true`, request de
  API para projeto `Sleeping`/`Starting` NÃO recebe a página de espera nem o
  503 + Retry-After: o router (`router.rs`) SEGURA a request no ar, emite
  `docker start`, polla o estado do container e a porta TCP (mesmo probe do
  live, 06) e re-decide antes de responder — o cliente de API só vê a espera
  e o proxy normal quando o app responde. Teto de 90 s
  (`WAKE_HOLD_TIMEOUT_SECS`): estourou, cai no fluxo original (página/503
  honestos — falha REAL não vira espera infinita). Motivação: IDEs apontadas
  para uma API quebram com recusa e toleram espera (caso do
  `zcode-api`/zcode-proxy na VM). Sem a flag o comportamento segue a matriz
  da [12](12-vm-hibernacao-porteiro-central.md).
- **Re-sync externo**: além do sweep de 60 s, o `fluxd` escuta `docker events`
  (die/start/stop) e re-sincroniza na hora — o porteiro espera o próximo sweep;
  o Flux reage em segundos.
- **Provisionamento no acordar**: hook por projeto (`[provision] on_wake` no
  `flux.toml`), rodando dentro do container, com marcador de throttle de 10 min
  em `/srv/flux/logs/{name}-provision.marker` e log textuais — mesmo desenho do
  `provision-dev.sh` (composer → filament:assets → build Vite em sidecar node
  montando os mesmos volumes, verificação de integridade de fontes).
- **Dev vs prod (R14)**: sono de `stop` é de dev. **Prod hiberna com `docker
  pause`** (CPU zero, RAM retida, resume < 1 s, invisível — sem página de
  espera) após `prod_idle_minutes` (default 30), e **o worker de prod nunca
  hiberna**: `queue:work` + `schedule:work` rodam 24/7 (tarefa agendada não
  para). Mecanismo completo, sleep-to-admit e política de pressão de RAM na
  spec [11](11-recursos-hibernacao.md). Config: `[sleep] prod_mode =
  "pause"|"stop"|"off"` (default `pause`).

### Página associada

Descrita em [03](03-roteamento-dominios.md) — o sono e a página são o mesmo
fluxo: dormir é barato, acordar é invisível para o visitante.

## Decisões

| Decisão | Alternativa descartada | Por quê |
|---|---|---|
| `docker stop` (recreate no acordar via `start`) | `docker pause` | Pause congela processo mas retém memória; stop devolve RAM à VM — objetivo é economia real (em DEV; em prod a direção se inverte, ver 11) |
| Lease de CLI com TTL | CPU fake / keep-alive HTTP | Explicito e auditável no painel; não depende de truque de CPU |
| `docker events` + sweep | só sweep | Reação em segundos a ações externas (o porteiro leva até 60 s; documentado como fonte de colisão) |
| Beat por `sendBeacon` (mantido) | websocket dedicado | `sendBeacon` sobrevive ao unload da página; websocket vivo mantém o container acordado com aba aberta e macete de "interação" que não existe |

## O que NUNCA fazer

- **Nunca** dormir `flux-db`, Adminer ou o próprio `fluxd`.
- **Nunca** pausar o worker de prod (só o HTTP) nem prod com request HTTP em
  andamento (11).
- **Nunca** dormir projeto em prod por padrão.
- **Nunca** dormir enquanto existe lease de CLI/painel ativo.
- **Nunca** confiar só no estado interno: a verdade é sempre re-sincronizada
  com o Docker (externos mexem nos containers — lição do porteiro).
- **Nunca** injetar o overlay em respostas comprimidas sem descomprimir — ou
  força `identity` ou passa intacta (jamais corromper o corpo).

## Critérios de aceite

1. Dev sem interação 15 min → `docker ps` mostra parado; painel mostra "dormindo
   há X min". `flux-db` continua Up.
2. Acessar a URL dormindo → página de espera → app responde sem reload manual;
   `docker ps` mostra Up.
3. `flux logs -f` segurando o dev acordado: container NÃO dorme enquanto o
   comando roda (lease), dorme 15 min após o fim.
4. `docker stop {name}-dev` manual → `fluxd` re-sincroniza em < 5 s (events),
   sem logs de colisão.
5. Teste/build dentro do container (CPU > 5%) segura acordado mesmo sem request.
