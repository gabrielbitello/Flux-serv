# 16 — Segurança própria da plataforma (headers + WAF local)

## Objetivo

Todo o servidor protegido por segurança QUE É NOSSA — rodando no `fluxd`, sem
serviço de terceiros: security headers em TODOS os sites por padrão, WAF
próprio (padrões OWASP, bad bots, brute-force, reputação local de IP) na
borda, antes de qualquer proxy. Nada disso depende de WAF SaaS.

Requisitos desta spec: R32 (segurança própria: headers + WAF local, sem
terceiros). Rate limit/teto de body (R30, 07) são camada irmã desta.

## Mecanismo

### Camada 1 — Security headers (TODOS os sites, por padrão)

Injetados pelo router em toda resposta de prod (o app pode adicionar, nunca
remover sem `--ack`):

| Header | Default |
|---|---|
| `Strict-Transport-Security` | `max-age=15552000` (180 d) — edge já é TLS (03) |
| `X-Content-Type-Options` | `nosniff` |
| `Referrer-Policy` | `strict-origin-when-cross-origin` |
| `X-Frame-Options` | `SAMEORIGIN` (app que precisa de frame legítimo sobrescreve em config) |
| `Permissions-Policy` | `camera=(), microphone=(), geolocation=()` |
| `server` / `X-Powered-By` | REMOVIDOS (não anunciar versão) |

CSP completa por site é trabalho do app (quebra fácil); o Flux não inventa CSP
genérica que derruba site de cliente.

### Camada 2 — WAF próprio (rule engine na borda, antes do proxy)

Motor de regras no router (regex compiladas, custo O(n) por request):

- **Padrões de ataque** (subconjunto OWASP core, curado no repo):
  SQLi (`union select`, `--`, `sleep(`…), XSS (`<script`, `javascript:`…),
  path traversal (`../`, `/etc/passwd`, `%2e%2e`), command injection
  (`; cat `, `$(…)` em query). Inspeciona query string, headers relevantes
  (User-Agent, Referer) e corpo — corpo só até o teto de inspeção (64 KB;
  acima disso, só query/headers: upload grande não vira CPU de regex).
- **Paths sensíveis**: `/wp-admin`, `/wp-login`, `/.env`, `/.git`,
  `/phpmyadmin`, `/vendor/phpunit` → `403` na hora + log + ponto de infração
  (site Laravel não tem isso; quem pede é scanner).
- **Bad bots**: lista de User-Agent curada no repo (arquivo de config, atualiza
  sem release) — `403` direto.
- Resposta de bloqueio: `403` com página da família (15), **JSON em rota de
  API**. Log de bloqueio no `events` com regra casada e path — **nunca** o
  corpo do request (privacidade).

### Camada 3 — Anti-brute-force

- `[shield] auth_paths = ["/login", "/admin"]` por projeto (default: detecta
  `login`/`wp-json/jwt-auth`).
- N falhas (401/302 de login, 403 de WAF) do mesmo IP nos paths de auth em
  M minutos (default 10 em 10) → IP bloqueado **15 min** (sob renovação).
- Locklist em SQLite com expiração; desbloqueio 1-click no painel (e
  `flux shield unban {ip}`).

### Camada 4 — Reputação local (fail2ban de fábrica)

- Infrações (WAF + brute-force) somam pontos por IP com decaimento (1 ponto
  halts em 1 h). Pontuação alta → bloqueio progressivo: 10 pts = 1 h,
  30 pts = 24 h, 100 pts = 7 dias.
- Lista vive no SQLite (sobrevive restart, 07); nunca bloqueia o IP do host
  (192.168.140.1) nem IP do Gabriel (config `[shield] allowlist`).
- Opcional por site: **tarpit** para scanner reincidente (resposta atrasada
  em 10 s — custo de rastejar fica no atacante).

### Shadow mode — a regra que impede derrubar cliente

- Site novo entra em `log` (shadow): regras casam, nada bloqueia, evento
  registra o que SERIA bloqueado. 7 dias de shadow sem falso positivo →
  `enforce` automático (com alerta de virada). Falso positivo no shadow →
  regra ajustada antes de machucar.
- Painel: seção Shield — bloqueios por regra/IP, taxas, últimos desbloqueios.

### Custo

~1 req/s de tráfego real: regex na borda é ruído (µs por request). Medição de
p95 antes/depois no aceite. Picos de scanner (100 req/s de padrões) são
barrados PELO rate limit (07) antes de gastar CPU de regex.

## Decisões

| Decisão | Alternativa descartada | Por quê |
|---|---|---|
| WAF próprio no fluxd | Cloudflare WAF / serviço SaaS | Decisão do Gabriel: segurança é nossa, sem terceiros; tráfego ~1 req/s não justifica motor externo; regras versionam no repo |
| Shadow mode antes de enforce | regra nova direto em bloqueio | Falso positivo em WAF = site de cliente fora do ar — inaceitável; 7 dias de sombra medem antes |
| Corpo inspecionado só até 64 KB | regex no corpo inteiro | Upload de 10 GB vira DoS de CPU; query+headers pegam a maioria dos padrões |
| Pontos com decaimento | ban permanente por 1 infração | IP residencial compartilhado (CGNAT) pune inocente; decaimento equilibra |

## O que NUNCA fazer

- **Nunca** logar corpo de request no WAF (privacidade/LGPD — só regra casada,
  path, IP, timestamp).
- **Nunca** ativar regra nova direto em `enforce` (shadow primeiro).
- **Nunca** bloquear IP do host, do Gabriel ou do allowlist (mesmo com 1000
  pontos).
- **Nunca** remover security header por padrão — só override consciente por
  projeto, auditado.
- **Nunca** inventar CSP genérica para site de app alheio (quebra tela).

## Critérios de aceite

1. `curl -I` em qualquer prod: headers da tabela presentes, `server` e
   `X-Powered-By` ausentes.
2. `?id=1 union select password` → 403 família + evento com a regra casada.
3. `/.env` → 403 + ponto de infração; 10 pedidos → IP fora por 15 min
   (resposta antes do proxy, provado por ausência no log de acesso do app).
4. 7 dias de shadow num site real: relatório de falso positivo ZERO antes da
   virada para enforce.
5. p95 de latência em prod com WAF enforce vs shadow: diferença < 2 ms
   (medição do sampler 13, antes/depois).
6. Painel mostra bloqueios por regra e desbloqueia IP em 1 clique.
