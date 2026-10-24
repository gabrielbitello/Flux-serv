# Incidente 24/10 — "auto-ddos" do vippers-d: WARP matava o websocket do HMR e o client do Vite recarregava a página em loop

**Classificação**: degradação de serviço autogerada (martelada do próprio browser do
operador) + lacuna de observabilidade. **Sem invasão** (forense paralela: zero
indicador nas 6 fontes sobreviventes — ver memória de 24/10 e [[flux-rajada-http-incidente-22-10]]).

## Sintomas

- Página `/cms` do vippers-d recarregando sozinha a cada ~3s (`location.reload()`,
  `Cache-Control: max-age=0` no request do documento).
- Rajada de `POST /livewire/update` (~75/min no pico, ~1s cada, todos 200) +
  reconexões de `/@vite/ws` a cada ciclo — o "Eventos por minuto" do painel
  parecia a rajada de 22/10 ("auto-ddos por causa do vite", diagnóstico do Gabriel).
- Console do browser: `showFailureModal` do Livewire num `__lazyLoad` +
  `InvalidStateError: dialog is already open` (dois modais brigando) — sintoma
  secundário da instabilidade, não causa.

## Causa raiz

**O Cloudflare WARP no Windows do operador fechava o websocket do HMR em ~1–2s.**
O client do Vite, ao perder a conexão, testa o servidor ("polling for restart"),
o encontra vivo e executa `location.reload()` — recarrega, reconecta, o WARP mata
de novo, loop infinito. Com abas múltiplas e os pollers da página, a martelada
virou o "auto-ddos".

**Fatos medidos** (a cadeia que fecha o diagnóstico):

1. O reload é executado pelo client do Vite: HAR `_initiator` dos dois documentos
   = `@vite/client:802` (a linha do `location.reload()`).
2. O servidor Vite NÃO manda recarregar: escuta do ws cru por 10s mostra só
   `{"type":"connected"}` + silêncio; log do Vite sem "full reload" novo;
   nenhum arquivo mudando no disco; sync limpa; container estável (mesmo token).
3. Zero 4xx/5xx no servidor e zero desafios da CF (`cf-mitigated` ausente) na
   janela capturada.
4. Websocket aguenta 40s+ em TODOS os elos (vite direto, fluxd, Caddy, edge CF)
   **quando a origem não é o browser do operador** (curl sem WARP).
5. O tráfego do operador carrega `cf-warp-tag-id` — WARP ativo.
6. Dedução fechada por exclusão: o client do Vite só recarrega recebendo
   `full-reload` (excluído — fato 2) ou perdendo a conexão (só acontece no
   browser com WARP — fatos 4+5).

**Mesma assinatura de 22/10**: as 3.094 tentativas de `@vite/ws` a cada ~4s
contidas pelo redroom eram este mesmo loop.

## Fatores contribuintes (corrigidos no mesmo dia)

| Camada | Problema | Correção |
|---|---|---|
| **CF cache** | `cache_level: aggressive` ignora query string — `livewire.js?id=` e módulos do Vite podiam vir velhos de cache | `basic` + regra de cache: `-d.bitello.xyz`, `/livewire/*`, `/__flux/*` = BYPASS |
| **CF desafios** | `security_level: medium` podia desafiar XHR no meio da rajada | `essentially_off` (proteção real agora: gate + borda + redroom) |
| **CF higiene** | `min_tls_version: 1.0` | `1.2` |
| **vippers** | RangeBoundsInput commitava Livewire a cada pixel do slider (`@input → syncToState`), `->live()` sem debounce, `persistFiltersInSession` ecoava para todas as abas | commit só no `@change` + debounce 800ms (commit `1ba826a0`, branch `cms3`) |
| **fluxd observabilidade** | O 429 da borda morria em silêncio; o access não registrava origem — o loop era invisível e não-atribuível | access com `xff`/`ua`/dump de headers p/ externo; 429 vira evento `warn` escopo `edge` (commit `89ffac4`) |

## Prevenção armada (estado final)

- **Operador**: `bitello.xyz` excluído do split-tunnel do WARP (ou WARP off ao
  usar o HMR).
- **CF**: browser integrity ON; WAF custom bloqueia `/wp-*`, `/phpmyadmin`,
  `/.env`, `/.git`, `/xmlrpc.php` na borda; rate limit 10 POSTs/10s por IP nos
  logins; cache `basic` + bypass de dev; TLS ≥ 1.2; security level
  `essentially_off`; websockets ON; rocket loader OFF; bot fight/Precursor
  OFF de propósito (sem isenção de IP no Free — desafiam automações próprias).
- **fluxd** (commits `21e322f`, `225099f`, `89ffac4`): access 100% auditável;
  gate de senha nos hosts de dev (ban por 5 falhas, confiança 30 dias);
  borda 120/min com 429 logado; redroom como pânico.
- **Identificado e NÃO alterado**: widget `DriverAvailability` polla `10s`
  (só ativo com corrida em andamento — a homolog do cliente, sem o dado de
  seed, não polla); decisão de manter/afrouxar/condicionar é do Gabriel.

## Pendências de segurança

- Rotacionar o token de API da Cloudflare e as chaves R2 (passaram por chat).
- `flux gate senha` (a senha de teste passou por chat) + widget do Turnstile
  no dashboard CF + `flux gate turnstile <site> <secret>`.
- Poda da tabela `events` (A1 do plano v2 de 22/10) e rotação do `APP_KEY` de
  dev — aprovados aguardando execução.

## Lições

1. **WARP + websockets de longa duração não combinam** — qualquer diagnóstico de
   HMR "burro" num browser atrás de WARP começa desligando ele.
2. **Log sem origem atrasou tudo**: de 22/10 a 24/10 o loop foi "invisível e
   não-atribuível". O access auditável (xff/ua/hdrs) fechou o caso em horas.
3. **Uma variável por vez**: as proteções CF só entraram DEPOIS da causa raiz
   fechada — ligar camada nova durante a caça contaminava o diagnóstico.
4. O `429` da borda nunca tinha gerado log (lacuna de 22/10) — agora gera, com
   dedup de 30s para tempestade não encher a tabela.
