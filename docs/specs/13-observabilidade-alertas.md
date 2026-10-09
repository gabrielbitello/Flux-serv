# 13 — Observabilidade, alertas e logs

## Objetivo

"Fechou" não é operar às cegas: o `fluxd` MEDe tudo (CPU/RAM por container,
latência por host, disco) e guarda histórico; quando algo quebra ele AVISA
(Telegram/Discord/e-mail) sem depender de alguém abrir o painel; e se o próprio
`fluxd` travar, o porteiro central percebe (dead-man's switch). Logs têm
rotação e retenção — nada cresce infinito.

Requisitos desta spec: R24 (observável: métricas, histórico, alertas, uptime
externo, logs com retenção).

## Mecanismo

### Coleta (sampler embutido — zero dependência nova)

- O `fluxd` amostra a cada 30 s, via `bollard` stats (one-shot por container de
  pé): CPU %, RAM MB, rede rx/tx. O router mede latência por host em janela de
  1 min (p95) e conta requests/5xx. Disco: `statvfs` em `/srv/flux` +
  `docker system df` resumido 1x/hora.
- Rollup de 1 minuto grava no SQLite (WAL, mesma instância do estado):

```sql
metrics(ts_minute, project, env, cpu_pct, mem_mb, net_rx_kb, net_tx_kb,
        req_count, req_5xx, latency_p95_ms, PK(ts_minute, project, env))
host_metrics(ts_minute, disk_used_pct, load1, containers_up, PK(ts_minute))
```

- Retenção 30 dias; prune diário junto do backup (07). Custo estimado: ~20
  containers × 1440 min = ~29k linhas/dia — SQLite engole de graça.
- `GET :7420/metrics` expõe o mesmo conteúdo em formato Prometheus (localhost,
  mesma auth da API) — para um dia ligar Grafana. Nenhum servidor Prometheus
  roda por padrão.

### Painel: histórico, não só estado

- Gráficos SVG gerados server-side (askama) — **zero lib JS externa**; o teto
  de < 100 kB do painel (08) se mantém. Janelas 1h / 24h / 7d.
- Cards: CPU/RAM por container, requests + p95 por host, 5xx, disco usado.
  "Prod lenta ontem à noite" vira número, não adivinhação.

### Alertas outbound (R24)

Config global (09): `[alerts] channel = "telegram"|"discord"|"email"` +
credenciais em `/srv/flux/secrets/alerts.env` (VM) e
`%USERPROFILE%\.flux\alerts.token` (HOST — o fluxhost precisa mandar alerta
com a VM morta). Gatilhos, com cooldown de 1 h por tipo (críticos repetem a
cada 15 min):

| Evento | Fonte |
|---|---|
| Crash loop (≥ 3 restarts em 10 min) | docker events (04) |
| Health falha pós-wake / provisionamento falhou | fluxd |
| OOM kill | docker events (11) |
| Disco > 80% (aviso) / > 90% (crítico) | host_metrics |
| Tunnel down > 2 min | cloudflared metrics (03) |
| VM não respondeu pós-resume em 90 s | fluxhost (12) |
| Deploy falho ou rollback executado | deploy orchestrator (10) |
| `flux-db` parado / worker de prod morto | healthchecks (05/11) |

Corpo do alerta: projeto, evento, hora, link do painel. **Nunca** segredo, nunca
valor de env, nunca trecho de log com dado de usuário.

### Dead-man's switch (o monitor do monitor)

- O `fluxd` manda `POST :8443/heartbeat` no `fluxhost` (host `192.168.140.1`,
  rede interna do switch, 01) a cada 60 s.
- 5 min sem heartbeat com a VM marcada **Running** → o `fluxhost` manda o
  alerta DIRETO do host (PowerShell `Invoke-RestMethod` no canal configurado) —
  independe da VM estar viva. Cobre: `fluxd` travado, Caddy morto, Docker
  morto, kernel da VM em pânico.
- Quando a VM está propositalmente dormindo (Saved/Stopped), o fluxhost
  desarma o switch — ausência de heartbeat não é alerta.

### Uptime visto de fora (R24)

- `flux setup uptime` cria o endpoint público `https://flux.bitello.xyz/healthz`
  no `fluxd`: resposta de 1 linha, sem auth, sem detalhe — `200 ok` com tudo de
  pé (flux-db saudável, Docker respondendo) ou `503 degraded`.
- O Gabriel registra um monitor gratuito externo (UptimeRobot ou similar)
  apontando para esse endpoint a cada 5 min. Infra de terceiros é opcional; o
  endpoint é do Flux. Serve também de prova "a internet me enxerga" — health
  interna nunca prova isso (03/05).

### Logs com rotação e retenção (R24)

- `flux setup vm` escreve `/etc/docker/daemon.json`:
  `"log-driver": "json-file", "log-opts": {"max-size": "10m", "max-file": "3"}`
  — log de container tem teto (30 MB/container) desde o dia 1.
- journald: drop-in `SystemMaxUse=500M`.
- `/srv/flux/logs/` (provisionamento): retenção `[logs] retention_days` =
  14 dias default, prune diário.
- Tabela `events` do SQLite: 30 dias (mesma rotina do backup).
- `flux logs {svc} --since 2h --until 1h` passa `--since/--until` ao
  `docker logs` — janela histórica sem grep manual.

## Decisões

| Decisão | Alternativa descartada | Por quê |
|---|---|---|
| Sampler + SQLite embutido | Prometheus + Grafana rodando na VM | Dois serviços de pé para ~20 containers é canhão pra matar pássaro; rollup em SQLite cobre o painel e o `/metrics` deixa a porta aberta |
| Gráfico SVG server-side | Chart.js/apex no painel | Teto de peso do painel (08); SVG estático resolve 3 janelas de linha |
| Dead-man no fluxhost (host) | segundo processo na VM | Processo dentro da VM não se vigora quando a VM é o problema |
| `healthz` público sem detalhe | status page com estado completo | Expõe superfície à toa; detalhe mora no painel autenticado |

## O que NUNCA fazer

- **Nunca** mandar segredo, valor de env ou trecho de log com dado de usuário
  num alerta.
- **Nunca** carregar lib JS externa no painel para gráfico (teto de peso, 08).
- **Nunca** gravar no rollup corpo de request, query string ou IP — métrica sem
  PII.
- **Nunca** deixar log de container/journald sem `max-size` — cresce infinito é
  como disco enche sem ninguém notar.
- **Nunca** silenciar um tipo de alerta sem registrar o motivo no `events`.

## Critérios de aceite

1. Painel com 24 h rodando: gráfico 24h mostra curvas reais de CPU/RAM/latência
   (dados do sampler, não mock).
2. Crash loop forçado (entrypoint com exit) → alerta no canal em < 2 min.
3. Arquivo grande de teste leva disco a > 80% → alerta; removido → sem alerta
   novo (cooldown respeitado).
4. `kill -STOP` no fluxd (simula travamento) → alerta do dead-man's switch em
   ≤ 7 min (5 de detecção + envio), emitido PELO HOST.
5. VM em Saved → dead-man desarmado, sem falso positivo.
6. `curl flux.bitello.xyz/healthz`: 200 com tudo de pé; `docker stop flux-db`
   → 503 em ≤ 30 s.
7. Após 30 dias rodando: `/var/lib/docker/containers` tem teto real (inspecionar
   tamanho); `flux logs --since 2h --until 1h` devolve exatamente a janela.
