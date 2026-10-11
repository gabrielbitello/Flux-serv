# 17 — Status público por site (uptime com causa)

## Objetivo

Cliente pergunta "seu site tá no ar?" — resposta é uma página pública: status
atual + uptime dos últimos 7/30 dias. Por API, JSON; por navegador, página de
verdade. E responde o "por quê": só o SITE caiu, a PLATAFORMA caiu, ou a
INTERNET do servidor caiu.

Requisitos desta spec: R33 (status público com classificação de causa).
Dados vêm do sampler (13) e do heartbeat (13).

## Mecanismo

### Endpoints (sem DNS novo — vive no painel host)

- **Navegador**: `https://flux.bitello.xyz/status/{name}` → página pública
  (sem login): status atual (operacional / degradado / fora), uptime 7 d e
  30 d (%), gráfico SVG dos últimos 7 dias (rollup do 13), últimos incidentes
  (do `events`). `Cache-Control: public, max-age=60` — barato de servir.
- **API**: mesmo path com `Accept: application/json` (ou `/status/{name}.json`)
  → `{"status":"operational","uptime_7d":99.98,"uptime_30d":99.95,
  "cause":null,"checked_at":"…"}`. Navegador nunca recebe JSON, API nunca
  recebe HTML (mesma regra da 12).
- Sem auth, sem dado sensível: só o projeto pedido, nunca lista de todos os
  clientes em uma página.

### Classificação da causa (o pedido do Gabriel)

| Cenário | Quem sabe | Como |
|---|---|---|
| **Só o site X caiu** | `fluxd` | health do container X falha enquanto os outros respondem → status do site = fora, banner "plataforma operacional" |
| **Plataforma/servidor caiu** | `fluxhost` | heartbeat do fluxd (13) quebra com VM Running → o próprio `fluxhost` serve página estática de status ("plataforma em recuperação") em TODOS os `/status/*` |
| **Internet do servidor caiu (ou PC desligado)** | **ninguém por dentro** | impossível medir de dentro do prédio sem luz; exige testemunha EXTERNA (abaixo) |

- **A testemunha externa é obrigatória** para o terceiro cenário — física, não
  escolha de design. V1: 1 monitor gratuito externo (UptimeRobot ou similar)
  batendo no `healthz` (13) a cada 5 min, com alerta no canal. O Flux não
  depende dele para NADA interno — ele só é a prova "a internet me enxerga".
  Alternativa sem terceiros: notebook do Gabriel na LAN rodando sonda
  (`curl healthz` a cada 5 min + alerta) — cobre queda de internet e PC
  desligado enquanto o notebook está em casa.
- A página de status diferencia os 3 estados: banner do topo mostra
  "site / plataforma / conectividade" conforme a causa conhecida; quando a
  testemunha externa alarmar e ninguém interno souber de nada → causa
  "conectividade".

### Incidentes

- Queda abre incidente no SQLite (início, fim, causa classificada, duração);
  a página lista os últimos 10. Uptime % sai dos incidentes + rollup (13) —
  número auditável, não enfeite.

## Decisões

| Decisão | Alternativa descartada | Por quê |
|---|---|---|
| Status no host do painel (`flux.bitello.xyz/status/{name}`) | domínio próprio de status | Um DNS a menos; wildcard já cobre; URL é pública do mesmo jeito |
| Monitor externo só como TESTEMUNHA | depender de serviço externo para monitorar tudo | Toda a inteligência é nossa (13/16); o externo cobre o único ponto cego físico (internet do servidor) |
| `fluxhost` serve página de status estática quando fluxd morre | página de erro de proxy | O cliente vê "estamos resolvido" com causa, não um erro cru — coerente com 15 |

## O que NUNCA fazer

- **Nunca** expor em página pública lista de todos os projetos, IPs, paths de
  health ou qualquer interno — só o status do site pedido.
- **Nunca** prometer uptime 100% auditável sem os dados do sampler — número
  sai do rollup, não é decorado.
- **Nunca** usar a testemunha externa para acordar NADA (13: crawler/monitor
  não acorda VM).
- **Nunca** responder JSON em navegador nem HTML em API.

## Critérios de aceite

1. `curl -H "Accept: application/json" flux.bitello.xyz/status/vippers` → JSON
   com `uptime_7d` real do rollup; browser na mesma URL → página com gráfico.
2. Parar o container de um prod: status DELE vira "fora" com causa "site",
   outros sites seguem "operacional" em ≤ 60 s.
3. `kill -STOP fluxd`: `fluxhost` assume `/status/*` com página
   "plataforma em recuperação" em ≤ 5 min (heartbeat, 13).
4. Cabo de rede fora (simulado): monitor externo alerta; ao voltar, incidente
   classificado como "conectividade".
5. Página pública não vaza nome de outros projetos (auditar HTML/JSON).
