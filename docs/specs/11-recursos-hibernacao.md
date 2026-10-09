# 11 — Limites de recursos e hibernação de prod

## Objetivo

Dois orçamentos de recursos com naturezas diferentes: **dev** tem cota GLOBAL
(todos os devs somados não podem passar de um teto — é laboratório), **prod**
tem limite INDIVIDUAL por projeto (é compromisso). E prod hiberna de um jeito
que o usuário não percebe, sem parar tarefas agendadas (e-mail, notificação,
API).

## Mecanismo

### Cota global de dev (cgroup slice compartilhado)

- `fluxd` cria um slice systemd `flux-dev.slice` na VM com `CPUQuota` e
  `MemoryMax` (defaults: 400% CPU = 4 vCPUs, 8 GB RAM — config global, 11↔00).
- Todo container de projeto dev nasce com `cgroup_parent=flux-dev.slice`
  (override de compose do `fluxd`, 06). O kernel garante a SOMA — não é soma
  de limites por container, é limite real do conjunto.
- Container `flux-db`, phpmyadmin/Adminer, `fluxd` e containers prod ficam
  FORA do slice (nunca competem com dev).
- **Sleep-to-admit**: dev novo precisa acordar e a soma prevista estoura a
  cota → `fluxd` dorme o dev acordado há mais tempo sem interação (LRU de
  `last_interaction`), com aviso no log e no painel ("acordou vippers,
  hibernou foo — cota dev de 8 GB"). Nunca dorme o dev que acabou de receber
  request (< 60 s).
- Devs continuam com limite individual menor por padrão (`mem_limit` 1,5 GB,
  `cpus` 1,5) para um dev maluco não consumir a cota sozinho.

### Limite individual de prod

- `flux.toml` do projeto:

```toml
[resources]
http_memory_mb = 512     # default
http_cpus = 1.0
worker_memory_mb = 256
worker_cpus = 0.5
```

- Aplicado via `mem_limit`/`cpus` no compose renderizado pelo `fluxd` (plugin
  compose aplica cgroups por container — funciona sem swarm).
- Estouro de memória = OOM kill do container = página de estado "falhou" (03)
  + auto-restart do compose (`unless-stopping`) + evento no painel. OOM nunca
  passa despercebido.

### Hibernação de prod (invisível)

Prod hiberna diferente de dev, porque o objetivo é outro: dev pode demorar
(ninguém esperando); **prod tem gente esperando**.

| | dev | prod |
|---|---|---|
| Ação | `docker stop` (RAM devolvida) | `docker pause` (SIGSTOP no cgroup — RAM retida, CPU zero) |
| Acordar | `docker start` + health (10–30 s) | `docker unpause` (~0,2–0,5 s, estado intacto) |
| Visível? | Sim, página de espera | **Não** — request atrasa < 1 s e o fluxd segurou |
| Gatilho | 15 min ocioso | 30 min sem request (config `[sleep] prod_idle_minutes`) |

Fluxo de um request em prod hibernando: request chega ao `fluxd` (sempre de pé)
→ estado `paused` → `docker unpause` → health curto (o container JÁ estava
saudável, é o mesmo processo) → proxy. O browser espera < 1 s, sem página de
espera, sem reload. RAM fica retida — custo aceito porque o limite individual
capa o tamanho (512 MB default) e é isso que a cota da VM aguenta.

**Quando a RAM aperta** (pressão da VM acima de 85%): o `fluxd` troca
automático de `pause` para `stop` nos prods mais ociosos (LRU), e o próximo
acordar usa o caminho de dev (página de espera aparece; é o preço da pressão).
Estado atual da política visível no painel.

**Checkpoint CRIU** (`docker checkpoint`) fica registrado como fase 2 opt-in:
retomaria prods parados com RAM em disco; não entra na v1 (fragilidade
conhecida do CRIU com sockets/state não trivial).

### Tarefas agendadas e background na hibernação

- **Worker nunca hiberna.** O container de worker/queue/scheduler de prod
  (`{name}-prod-{cor}-worker`) roda 24/7: `queue:work` + `schedule:work`
  (com lock, 10). CPU de worker ocioso ≈ 0 — não é ele o alvo da economia; o
  alvo é o HTTP que serve request.
- HTTP hiberna; worker não. E-mail, notificação, integração de API: rodam no
  worker, sempre de pé, independente do HTTP estar pausado.
- Jobs que precisam do app HTTP para callback externo: o `fluxd` acorda o HTTP
  (unpause) quando o worker configura — mecanismo: worker publica intenção em
  tabela/`cache`, o `fluxd` monitora; ou simplesmente o endpoint chamado
  externamente acorda pelo caminho normal (03). Caso raro, tratado por
  wakeup-on-demand.
- Prod worker tem `restart: unless-stopping`; se morrer, painel acusa.

## Decisões

| Decisão | Alternativa descartada | Por quê |
|---|---|---|
| `pause` para prod | `stop` igual dev | Resume ~0,3 s é invisível; stop de 10–30 s com página de espera NÃO é aceitável em prod |
| Slice cgroup para soma de dev | somar `mem_limit` por container no fluxd | Limite por container não garante soma; kernel garante slice — sem race, sem estimativa |
| Sleep-to-admit (LRU) | recusar acordar o novo | O objetivo da cota é disciplina, não bloqueio; sempre existe um dev menos importante que o que acabou de ser acessado |
| Worker 24/7 fora da hibernação | pausar tudo | Tarefa agendada perdida = requisito violado; worker ocioso custa ~0 CPU |
| OOM visível + auto-restart | esconder restart | Erro silencioso viola regra do projeto; painel acusa com contexto |

## O que NUNCA fazer

- **Nunca** deixar a soma de dev passar da cota — slice no kernel, não confiar
  em contabilidade do `fluxd`.
- **Nunca** pausar o worker de prod (só o HTTP).
- **Nunca** hibernar prod com job HTTP em andamento (pause só com conexões
  ativas = 0).
- **Nunca** acordar prod com página de espera quando dá unpause — página só
  entra no caminho de pressão (stop) ou primeira subida.
- **Nunca** deixar `flux-db`/Adminer/`fluxd` dentro da cota de dev.

## Critérios de aceite

1. 5 devs acordados rodando build simultâneo → consumo do slice não passa de
   `CPUQuota`/`MemoryMax` (medição `systemd-cgtop`), prod ao lado intocado.
2. 6º dev acordando com cota cheia → o mais ocioso dorme, aviso no painel,
   o 6º sobe.
3. Prod com `http_memory_mb = 512` passa de 512 MB → OOM kill → auto-restart +
   evento no painel em < 30 s.
4. Prod hibernando: `curl` timing < 1 s acima do normal, SEM página de espera;
   `docker inspect` mostra `Paused=false` após o request.
5. Worker processa job e manda e-mail com HTTP pausado há horas.
6. Pressão RAM > 85% → prods ociosos vão de `pause` a `stop` e o painel mostra
   a política ativa.
