# 15 — Páginas de erro fora de dev + modo manutenção

## Objetivo

FORA de dev, o Flux nunca entrega página de erro padrão/feia: 404, 429, 413,
500 de plataforma — tudo com a família visual do Flux (mesma linha da página
de espera, 03). Se o site de prod tem página de erro PRÓPRIA (do app), a do
app vence — a plataforma não interfere no que é do app. Em dev, o inverso:
erros CRUS (stack, whoops) — dev precisa ver o erro de verdade.

Modo manutenção completa a família: o dono pede, o site mostra página de
"voltamos já" com a marca DELE, e o resto do mundo não vê meio-site quebrado.

Requisitos desta spec: R31 (erros padrão fora de dev + manutenção; página
própria de prod vence).

## Mecanismo

### De quem é cada erro (a regra que resolve "a da prod vence")

A plataforma só desenha o que é DELA. Quando o erro nasce no app, passa
intocado:

| Erro | Quem gera | O que aparece fora de dev |
|---|---|---|
| 404 de rota do app (ex.: Laravel 404) | o APP | a página do app (personalizada vence — naturalmente, é resposta do app) |
| 500 do app | o APP | a página do app (exceção app Throwable) |
| Host não registrado dentro de `*.bitello.xyz` | a PLATAFORMA | página Flux "página não encontrada" (404 brandiado) |
| Rate limit (07) | a PLATAFORMA | página Flux "muitas requisições" (429 brandiado) |
| Body acima do teto (07) | a PLATAFORMA | página Flux "arquivo grande demais" (413 brandiado) |
| Upstream inalcançável/crash (03) | a PLATAFORMA | página Flux de estado "falhou" (já existe) |
| Container off (03) | a PLATAFORMA | página de espera (já existe) |
| Site em manutenção (abaixo) | a PLATAFORMA | página de manutenção (503 brandiado, 15) |
| Painel (08) | a PLATAFORMA | páginas da família do painel |

- Em **dev** (`*-d`): nada disso — erros crus do app e do fluxd, sempre. É
  ferramenta de trabalho, não vitrine.
- A família de páginas da plataforma: mesmo visual da página de espera
  (gradiente escuro, branding Flux OU branding do projeto quando disponível),
  mensagem honesta em português, `Cache-Control: no-store`.
- 404 da plataforma ganha atalho "conheça os sites" (lista de prods públicos
  registrados) — transformar beco em porta.

### Modo manutenção (R31)

- `flux maintenance on {name}` / `off {name}` (CLI) ou botão no painel. Estado
  no SQLite; o router checa ANTES do proxy.
- Resposta em manutenção: `503` + `Retry-After: 3600` + página com o NOME do
  projeto (e logo, se configurado em `flux.toml [branding] logo_url`),
  mensagem "voltamos em breve", `X-Flux-Maintenance: 1`.
- **Bypass do dono**: `flux maintenance on {name} --bypass` gera URL com token
  (válida 24 h) — o cliente navega o site real com cookie de bypass enquanto o
  resto do mundo vê a página. Sem token, sem bypass.
- APIs em manutenção: mesmo 503 em **JSON** (`{"error": "maintenance",
  "retry_after": 3600}`) — coerente com a regra de API (12): nunca HTML em
  rota de API.
- Manutenção NÃO derruba o worker: fila/cron seguem (11) — manutenção é
  vitrine, não desligamento.
- SEO: `Retry-After` + 503 é o sinal correto para buscador reagendar; a página
  NÃO é 200 (200 falso indexa "voltamos em breve" — erro clássico).
- Worker/scheduler do próprio site continua; deploy em curso BLOQUEIA `on`
  (e `on` bloqueia deploy) — estados não se misturam.

## Decisões

| Decisão | Alternativa descartada | Por quê |
|---|---|---|
| Plataforma só desenha erro DELA; app passa intocado | fluxd reescrever 404/500 do app pela página do Flux | A página personalizada do prod VENCE sem detecção nenhuma — quem gerou o erro o desenha; reescrever exigiria interceptar HTML do app (frágil e errado) |
| Manutenção = 503 com Retry-After | página 200 "voltamos logo" | 200 falso indexa página de manutenção; 503+Retry-After é o padrão que buscadores entendem |
| Bypass por URL com token | liberar por IP do cliente | IP muda (4G/residêncial); URL com cookie de 24 h é à prova de rede |
| Manutenção só manual | automático em crash | Crash já tem página de estado (03); manutenção é decisão de negócio, não de saúde |

## O que NUNCA fazer

- **Nunca** entregar página de erro padrão (cinza do browser, HTML do proxy)
  fora de dev — ou é do app, ou é da família Flux.
- **Nunca** responder manutenção com 200.
- **Nunca** mostrar erros crus em prod (stack trace vaza paths/segredos — e
  é feio).
- **Nunca** deixar manutenção ativa atravessar um deploy (um trava o outro).
- **Nunca** pausar worker/scheduler no modo manutenção (só a vitrine para).

## Critérios de aceite

1. `zzz.bitello.xyz` → 404 brandiado da família, nunca página cinza.
2. App de prod com 404 próprio → a página DELE aparece byte a byte (diff vazio
   contra resposta direta ao container).
3. Site laravel SEM 404 própria → 404 default do framework (app também tem
   direito de ser feio — é problema do app, visível, não da plataforma).
4. `flux maintenance on vippers --bypass`: mundo vê 503 brandiado; URL com
   token navega o site real; API recebe JSON; Googlebot recebe 503 +
   `Retry-After`.
5. `flux maintenance on` durante deploy → erro claro, nada muda.
6. Dev `*-d` com erro de PHP → stack cru na tela (comportamento de dev
   preservado).
