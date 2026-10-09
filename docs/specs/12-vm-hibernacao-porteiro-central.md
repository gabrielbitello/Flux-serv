# 12 — Hibernação da VM + porteiro central + acordar API

## Objetivo

Quando NADA é acessado por muito tempo (padrão 30 dias), a VM principal inteira
dorme (Hyper-V **Saved** — RAM zero no host). Mas o domínio continua respondendo:
um **porteiro central** no Windows segura o tunnel e acorda a VM na primeira
batida, devolvendo a rota pedida. API acorda sem quebrar apps e mais rápido em
prod. Nada disso pode gerar explosão momentânea de RAM/CPU.

Requisitos desta spec: R19 (VM dorme após ociosidade longa), R20 (porteiro
central acorda VM e a rota), R21 (API acorda sem quebrar app, prod mais rápido
que dev).

## Mecanismo

### Porteiro central — onde mora (decisão de arquitetura)

O Gabriel propôs uma VM Alpine com um segundo porteiro. Alternativa mais nativa
e mais leve, **recomendada**: nada de VM extra — o porteiro central são dois
processos nativos do Windows, sempre de pé:

| Processo | Papel | Custo em RAM |
|---|---|---|
| `cloudflared` como **serviço Windows** | tunnel `*.bitello.xyz` — único connector, roda no host | ~30 MB |
| `fluxhost` (serviço SYSTEM que já existe, 01) com listener HTTP `:8443` | recebe o request do tunnel, decide VM acordada/dormindo, acorda a VM, proxy | ~20 MB |

Cadeia completa quando a VM está dormindo:

```
browser/API → Cloudflare edge → cloudflared (host)
  → fluxhost :8443 (porteiro central)
      → VM Saved? → Resume-VM → espera fluxd :8080 (timeout 90 s)
      → proxy para Caddy da VM → fluxd → acorda container/rota pedida (03)
```

Quando a VM está acordada, o mesmo caminho serve em ~1 ms extra (proxy local),
sem troca de connector, sem dois cloudflared brigando pelo mesmo tunnel
(ingress de tunnel é global — connector duplo não permite rota diferente por
réplica; um caminho só elimina esse estado).

Alternativa Alpine (opção B, descartada mas registrada): VM de ~256 MB RAM com
cloudflared + mini-porteiro. Contra: kernel/distro extra para manter, mais RAM
que os ~50 MB do par nativo, e ainda precisaria de canal para chamar o
`fluxhost` de dentro (Hyper-V não deixa guest ligar guest). Se um dia o host
Windows sair da jogada (ex.: mover tudo para VPS Linux), a opção B volta a
fazer sentido lá.

### Hibernação da VM (Saved state)

- **Save-VM** (nativo Hyper-V): escreve a RAM da VM num arquivo `.vmrs`, desliga
  CPU, host libera toda a memória. Resume em ~10–20 s (boot frio: 30–60 s) com
  TUDO como estava — containers pausados voltam pausados, `flux-db` volta de pé,
  sem pico de re-subida (esta é a razão de escolher Saved em vez de shutdown).
- **Gatilho de dormir**: zero request em TODOS os hosts gerenciados por
  `vm_idle_days` (padrão 30) **E** guardas todas verdes:
  - sync mutagen sem pendências e sem sessão ativa (deploy/build rodando bloqueia);
  - nenhum lease de CLI/painel;
  - nenhuma operação do fluxd em curso (migrate, deploy, backup);
  - backup do SQLite e de checkpoints em dia (o `fluxd` roda o backup ANTES de
    concordar em dormir).
- **Fluxo de dormir**: `fluxd` publica `vm_sleep_ready=true` → `fluxhost`
  confirma guardas → `Save-VM` → estado `saved` no painel. Se Save falhar →
  shutdown ACPI limpo (fallback), evento no painel.
- **Acordar**: request qualquer (porteiro central), `flux up` (CLI via
  fluxhost), `flux vm start`, login no painel (`flux.bitello.xyz` passa pelo
  porteiro central também — painel de VM dormindo mostra página "acordando a
  VM", 200).
- **O que NÃO acorda a VM**: health checks entre si (fluxdb/fluxd conversam
  internos), revisita de crawler sem request real a host gerenciado, monitor do
  próprio tunnel (heartbeats não contam como interação — lição do beat do
  porteiro: só interação HUMANA ou de app real).

### Acordar API sem quebrar o app (R21)

Três cenários, mesma resposta visual ZERO para navegador:

| Estado do prod | Navegador | Cliente de API |
|---|---|---|
| Pausado (`pause`) | `unpause` < 1 s, request original respondido — invisível | igual — `unpause` < 1 s, request segurado pelo `fluxd` e respondido normal |
| Parado (`stop`, pós-pressão) | página de espera 200 | **HTTP 503 JSON** + `Retry-After: 30` (abaixo) |
| VM Saved | página "acordando a VM" 200 (10–20 s) | HTTP 503 JSON + `Retry-After: 30` |

Corpo padrão para API (semântica HTTP pura — é PARA ISSO que `503` +
`Retry-After` existem; apps bem-feitos retryam sozinhos, mal-feitos recebem
JSON claro em vez de HTML que "buga" parser):

```json
{"error": "service_waking", "message": "prod acordando, tente em ~30s", "retry_after": 30}
```

- Detecção de API: header `Accept: application/json`, prefixo `/api/*` do
  projeto, ou `X-Flux-Client: api`. Config por projeto em `[api]` do
  `flux.toml`.
- **Hold configurável**: `[api] hold_secs = 0..45` — em vez do 503 imediato, o
  `fluxd` SEGURA o request até acordar (até o teto). `0` = 503 na hora (default
  p/ dev), `30+` = a maioria dos clients nem vê erro. Nunca responder 200 com
  corpo de erro — 200 falso é o que buga parser.
- **Nunca** responder HTML em rota de API.

### Prioridade: API de prod acorda mais rápido que dev

1. **Fila de wake com prioridade**: prods primeiro, devs depois — uma janela de
   despertar não deixa dev furar lugar de prod.
2. **Prod usa o caminho barato**: `unpause` (0,2–0,5 s) é o estado normal de
   prod; dev só tem o caminho caro (`start` 10–30 s).
3. **Concorrência de acordos limitada** (anti-explosão, abaixo): na disputa,
   prod entra primeiro.
4. **`api_always_warm = true`** por projeto (opt-in, custo RAM contínuo): HTTP
   de prod nunca pausa — para API crítica que não tolera nem os 300 ms. Default
   false; cada ativa aparece no painel com o custo.

### Anti-explosão de RAM/CPU (requisito do Gabriel)

- **Acordar em fila**: máx 2 containers acordando simultaneamente na VM
  (`wake_concurrency`); demais pedidos esperam a vez — pico de CPU é limitado
  pela fila, não pela demanda.
- **Teto por container** já capado (`[resources]`, 11) e dev somado no slice.
- **Saved resume sem pico**: voltar de Saved NÃO acorda nenhum container — só
  o fluxd e o flux-db (estavam de pé no congelamento); prods pausados voltam
  pausados; quem pede, acorda.
- **`flux-db` e `fluxd` nunca participam da fila** — infraestrutura sempre de pé.
- Pós-acordo, o cooldown anti-flapping (04, 5 min) evita loop acordar-dormir
  sob rajada curta de monitor.

## Decisões

| Decisão | Alternativa descartada | Por quê |
|---|---|---|
| Porteiro central no host (cloudflared + fluxhost) | VM Alpine com segundo porteiro | ~50 MB vs ~256 MB, zero distro extra, sem canal guest→host (Hyper-V não deixa guest ligar guest); "mais nativo possível" é o host |
| `Save-VM` | shutdown + boot frio | Resume 10–20 s vs 30–60 s e SEM re-subida de containers (sem pico); RAM vai para `.vmrs` no disco, host libera tudo |
| 503 JSON + `Retry-After` para API parada | segurar request sempre | Padrão HTTP, apps sabem lidar; hold infinito estoura timeout do client do lado dele mesmo e parece "app bugado" |
| Um cloudflared no host sempre | cloudflared na VM + failover | Ingress de tunnel é global; dois connectors não permitem rota diferente por réplica — um caminho só, zero estado |
| Fila de wake com concorrência 2 | acordar tudo em paralelo | Explosão de CPU/RAM era o receio; pico limitado por desenho, não por sorte |

## O que NUNCA fazer

- **Nunca** dormir a VM com guardas vermelhas (sync pendente, deploy, lease,
  backup atrasado).
- **Nunca** contar heartbeat/crawler como interação para o timer de 30 dias.
- **Nunca** responder HTML em rota detectada como API; nunca 200 com corpo de
  erro.
- **Nunca** acordar mais de `wake_concurrency` containers ao mesmo tempo.
- **Nunca** acionar Saved com prods em meio de deploy/rollback/drain.

## Critérios de aceite

1. VM Saved: `docker ps` interno preserva estado; RAM do host cai ~16 GB →
   ~0,5 GB; `.vmrs` no disco do tamanho da RAM usada.
2. Request num prod com VM Saved: página 200, VM responde, rota entregue —
   total < 60 s; request num dev idem.
3. API com VM acordada e prod pausado: request responde em < 1 s, sem 503.
4. API com prod stopped: 503 JSON `Retry-After` + wake na fila — segundo
   request após 30 s passa.
5. `api_always_warm = true`: prod nunca pausa; painel marca o custo.
6. Rajada de 10 hosts dormindo: acordam em fila de 2, pico de CPU da VM dentro
   do esperado (medição `systemd-cgtop`/docker stats).
7. 30 dias sem acesso real (simulado com timer curto) → VM entra em Saved
   sozinha; heartbeats não atrapalham.
