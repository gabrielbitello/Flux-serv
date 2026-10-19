# 03 — Roteamento: Cloudflare Tunnel, Caddy, página de espera

## Objetivo

Todo `{name}.bitello.xyz` e `{name}-d.bitello.xyz` resolvem de qualquer lugar
(internet, LAN, notebook), com TLS, direcionamento automático por projeto e —
regra do Gabriel — **nunca** 500/404 quando o container está off: aparece a
página de espera estilo porteiro, com auto-refresh, até o app responder.

## Mecanismo

### Cadeia de tráfego

```
Navegador
  → Cloudflare edge (TLS *.bitello.xyz, proxy ON)
  → CNAME wildcard *.bitello.xyz → <tunnel-id>.cfargotunnel.com
  → cloudflared (SERVIÇO WINDOWS, no host — 12: serve mesmo com a VM dormindo)
  → fluxhost :8443 (porteiro central: VM dormindo? Resume-VM primeiro)
  → Caddy (VM): catch-all reverse_proxy → 127.0.0.1:8080 (fluxd)
  → fluxd decide por Host:
        host gerenciado + acordado  → proxy HTTP para IP interno do container
        prod pausado (pause, 11)   → unpause + proxy — invisível (< 1 s)
        host gerenciado + dormindo  → docker start + página de espera (200)
        host gerenciado + acordando → página de espera (200)
        host gerenciado + crash     → página de estado "falhou" (200)
        host desconhecido           → página brandiada "host desconhecido" (404)
```

Ponto-chave do desenho: **o Caddy é estático**. Uma rota catch-all (`*.bitello.xyz`
→ fluxd) que nunca muda. Quem aprende novos projetos é o `fluxd` — nenhum reload
de Caddy por deploy, nenhuma config dinâmica para quebrar.

### Cloudflare (setup único, `flux setup tunnel`)

1. Tunnel criado via dashboard/API com token salvo em `/srv/flux/secrets/tunnel.token`
   (gitignored, fora de qualquer sync).
2. DNS: um registro só — CNAME `*` → `<tunnel-id>.cfargotunnel.com`, proxy ON.
   O wildcard cobre `{name}` e `{name}-d` (um label) e `flux` (painel) de graça.
   Registrar novos projetos = zero toque no Cloudflare.
3. `flux tunnel status` lê a API local do cloudflared (métrica em
   `127.0.0.1:20241/metrics` → conexões ativas) e o systemd.
4. TLS no origin: certificado interno do Caddy (`tls internal`) — o túnel cifra
   de ponta a edge; cloudflared fala HTTPS com Caddy sem validar CA pública.
5. `cloudflared` fica no HOST Windows como serviço (12): é quem garante
   `*.bitello.xyz` respondendo mesmo com a VM em Saved — o porteiro central
   (`fluxhost :8443`) acorda a VM antes de devolver o request.

### Caddy (Caddyfile estático)

```caddy
{
    admin off
    default_sni *.bitello.xyz
}

*.bitello.xyz {
    tls internal
    reverse_proxy 127.0.0.1:8080
}
```

- `default_sni` (incluído em 14/10/2026 após prova real na VM): conexões SEM
  SNI (cloudflared apontando para o IP do origin, acesso LAN por
  `https://192.168.140.2`) usam o cert curinga do `*.bitello.xyz` em vez de
  morrer com `tlsv1 alert internal error`. O bloco separado `:443` da versão
  anterior da spec saiu — sem `default_sni` ele não consegue cert pré-emitido
  e quebra o handshake; com ele, o site curinga cobre tudo sozinho.
- Acesso LAN: opcionalmente o Gabriel aponta `*.bitello.xyz` para
  `192.168.140.2` no DNS local e sai sem passar pela internet; quem decide o
  host continua sendo o `fluxd` pelo header `Host`.
- Caddy roda como systemd com capacidades de bind 443 (pacote oficial já trata).

#### Host LAN-only sem Cloudflare: bloco `http://` dedicado (18/10/2026, caso `zcode-api`)

Cliente que não é browser (IDE apontando `ANTHROPIC_BASE_URL`) não lida bem
com o 308 → HTTPS do curinga nem com o `tls internal` (a CA do Caddy não é
confiada no PC). Padrão: um bloco **HTTP puro, por host**, no Caddyfile — o
resto do curinga segue intocado:

```caddy
# zcode-api (18/10): LAN-only, HTTP puro, sem Cloudflare — roteado pelo fluxd
# (Host header) que acorda o container dormindo antes de proxyar.
http://zcode-api-d.bitello.xyz {
	reverse_proxy 127.0.0.1:8080
}
```

- No PC: `192.168.140.2 zcode-api-d.bitello.xyz` no hosts — tráfego só na
  rede host-only (01); o `cloudflared` nem precisa estar de pé.
- O fluxd decide por `Host` exatamente como nos hosts públicos: sleep/wake,
  matriz de erros e o hold de API (`[sleep] api_hold`, 04) valem igual.
- Prova do não-vazamento: no mesmo `restart`, os demais hosts no :80
  continuaram no `308` do curinga (`vippers-d` conferido) — o bloco
  específico vence o redirect só para o host declarado.
- `admin off` mata o `systemctl reload` (a API :2019 não existe): mudança no
  Caddyfile é `systemctl restart` (~1 s de interrupção).

#### Fechamento do padrão: gate de token + HTTPS negado (19/10/2026)

O túnel público da CF entra no `*.bitello.xyz` **:443** (TLS) — o bloco
coringa — e o fluxd roteava `zcode-api-d` do mesmo jeito: **a URL pública
ficava aberta** (o `zcode-proxy` ignora credenciais de entrada; POST com
`x-api-key` mentirosa retornava 200 e queimava a cota z.ai — provado). Três
camadas fecharam, todas na borda (sem rebuild do container):

```caddy
# 1. HTTPS desse host = só existe para o túnel público — nega tudo.
https://zcode-api-d.bitello.xyz {
	tls internal
	respond 403
}

# 2. HTTP (o caminho do PC via hosts) exige o token do Claude Code
#    (ANTHROPIC_AUTH_TOKEN, que o SDK manda como Authorization: Bearer).
http://zcode-api-d.bitello.xyz {
	@sem_token not header Authorization "Bearer {TOKEN}"
	handle @sem_token {
		respond 401
	}
	reverse_proxy 127.0.0.1:8080
}
```

- **Números medidos (19/10)**: direto via hosts, quente = **36-40 ms**
  (`/v1/models`); a MESMA request forçada pela borda CF = **92-97 ms**
  (o CF custa ~55 ms/request mesmo quente — por isso LAN-only). Wake frio
  com hold = 0,8-2,2 s (POST real de modelo: 2,19 s fim a fim).
- Scanner sem token custa **6-7 ms de 401/403 na borda** e NUNCA acorda
  container (o gate responde antes do fluxd).
- A lição de roteamento: bloco específico vence coringa — o gate no `:80`
  NÃO cobria o túnel, que entra no `:443` do coringa. Sempre provar o
  caminho público FORÇANDO o IP da borda (`curl --resolve host:443:IP-DA-CF
  https://host/...`) — `nslookup` não serve (ignora o hosts) e o caminho
  interno mente sobre o externo.
#### HTTPS local pro Claude Desktop (19/10/2026)

O Desktop valida o campo de endpoint e REJEITA `http://` (validação do app,
sem toggle). Solução: o MESMO host em TLS da CA interna da Caddy (o curinga
`*.bitello.xyz` já é `tls internal`), com a CA root importada no store do
USUÁRIO do Windows (`certutil -user -addstore Root root.crt` — válido até
2036) e o MESMO gate de token dos dois formulários de header:

```caddy
https://zcode-api-d.bitello.xyz {
	tls internal
	@sim_tls `{header.Authorization} == "Bearer {TOKEN}" || {header.x-api-key} == "{TOKEN}"`
	handle @sim_tls {
		reverse_proxy 127.0.0.1:8080
	}
	respond 401
}
```

- O túnel público da CF cai AQUI também → sem token = 401 (o 403 incondicional
  anterior virou gate; a proteção pública se mantém pelo token).
- O `:80` aceita os DOIS formulários também (expression matcher CEL — matcher
  nomeado repetido NÃO é OR na Caddyfile, é ERRO de adapt).
- **Armadilha schannel**: curl/PowerShell no Windows falham com
  `CERT_TRUST_REVOCATION_STATUS_UNKNOWN` (a CA interna não publica CRL/OCSP);
  clients Chromium/Electron (Desktop) fazem soft-fail e funcionam. Teste de
  curl precisa `--ssl-no-revoke`.
- **Armadilha ssh**: backtick dentro de comando ssh com aspas duplas = command
  substitution LOCAL — o heredoc viaja mastigado e a Caddy valida config com
  matcher VAZIO (que casa TUDO = gate aberto!). Editar Caddyfile remoto via
  script scpiado, nunca inline.
- Provas: HTTPS sem token 401 · Bearer 200 · x-api-key 200 · POST real
  200/1,9 s · HTTP sem token 401 (regressão).

- O hostname no painel da CF (Zero Trust → tunnel) continua configurado;
  com o 403 do HTTPS e o 401 do HTTP, ficou inofensivo. Removê-lo lá é
  limpeza opcional.

### fluxd router (o "porteiro" do Flux)

- Dispatch por `Host` exato: `flux.bitello.xyz` → painel; `{name}-d.bitello.xyz`
  → dev na branch atual; `{branch}-{name}-d.bitello.xyz` → dev no worktree da
  branch (02 — o fluxd tenta os projetos registrados por sufixo mais longo,
  evita ambiguidade com nomes que contêm hífen); `{name}.bitello.xyz` → prod
  (cor ativa do blue-green, 10); resto → desconhecido.
- Proxy para o container: resolve IP na rede `flux-net` via `bollard` (inspect),
  cache com invalidação em erro (mesma lição do porteiro: IP muda em recriação).
- Proxy HTTP/1.1 completo com `hyper` client: websockets upgrade, SSE, streaming
  e uploads grandes passam (o porteiro buffera tudo; o fluxd NÃO — streaming
  desde o primeiro byte, com limite configurável para "injetar" só respostas
  HTML pequenas).
- Health: container Running + resposta HTTP no path de health do projeto
  (default `/up` para laravel; node/rust exigem `[health] path` ou caem para
  check TCP — tabela por stack em 06, R26). Só considero "acordado e saudável"
  após health OK — entre start e health OK, o visitante vê página de espera
  com a fase real.

### Bots: homologação invisível + robots/sitemap automáticos (R22, R23)

O dispatch do `fluxd` intercepta `GET /robots.txt` e `GET /sitemap.xml` ANTES
do proxy:

| Host | `/robots.txt` | `/sitemap.xml` |
|---|---|---|
| dev (`*-d.bitello.xyz`) | SEMPRE gerado — `User-agent: *` + `Disallow: /`. O `robots.txt` do projeto é IGNORADO em dev: homolog não se indexa, política não é negociável | urlset vazio |
| prod (`{name}.bitello.xyz`) | release tem `public/robots.txt`? proxy normal : gerado `User-agent: *` + `Allow: /` + `Sitemap: https://{name}.bitello.xyz/sitemap.xml` | release tem `public/sitemap.xml`? proxy normal : urlset gerado com a raiz e `lastmod` do release ativo |
| painel (`flux.bitello.xyz`) | `Disallow: /` | urlset vazio |

- Toda resposta de host dev — inclusive páginas de espera/estado, 503 de API e
  proxy do app — leva o header `X-Robots-Tag: noindex, nofollow, noarchive`.
  As páginas geradas pelo `fluxd` também incluem
  `<meta name="robots" content="noindex, nofollow">` no HTML.
- Content-Type: `text/plain; charset=utf-8` (robots) e `application/xml`
  (sitemap). `Cache-Control: public, max-age=3600` nos gerados.
- Detecção de arquivo preexistente é no DEPLOY, não por request: o
  deploy orchestrator registra `has_robots`/`has_sitemap` no SQLite ao montar o
  release (10) — o router só consulta o flag. Preexistente vence SEMPRE; o
  gerado existe apenas na ausência.
- Honestidade: `robots.txt`/`noindex` são honorários — bot de busca sério
  (Google, Bing) honra; bot malicioso ignora. Esconder homolog de VERDADE
  exigiria auth — fora de escopo agora; toggle `[dev] basic_auth` é caminho
  futuro se um dia fizer falta.
- Com a VM em Saved (12), o porteiro central responde robots/sitemap
  localmente SEM acordar a VM — crawler jamais acorda a VM (coerente com R19).


### Página de espera / de estado

Uma página, três fases, sempre HTTP 200, auto-refresh a cada 3 s
(`meta http-equiv="refresh"` — zero JS obrigatório) + `location.reload()` via
JS quando detecta mudança de fase (SSE opcional):

| Fase | Quando | Texto |
|---|---|---|
| `acordando` | `docker start` emitido, health ainda não OK | "Acordando {name}… subindo container" |
| `provisionando` | provision hook rodando (composer/npm/artisan) | "Preparando {name}… dependências e build" (com etapa atual) |
| `falhou` | crash loop (≥ 3 restarts em 10 min) ou health falha após timeout (90 s) | "{name} não subiu" + últimas 10 linhas de log do container + link para o painel |

- Visual: mesma família da página do porteiro (gradiente escuro, anel de
  progresso), com branding Flux e o domínio real.
- `Cache-Control: no-store` — igual ao porteiro, **status 200 não prova que o app
  subiu**; diagnóstico real é `flux status` / painel.
- Assets do app pedidos durante o sono também recebem a página (HTML) — mesmo
  comportamento do porteiro, aceito: o browser só carrega assets depois do HTML.
- FORA de dev, TODA página de erro gerada pela plataforma usa a família visual
  Flux (15): 404 de host, 429, 413, estado "falhou", manutenção. Erro gerado
  PELO APP passa intocado (a página própria de prod vence — 15). Em dev, erros
  crus.

### Regra "nunca 500/404" — matriz completa

| Situação | Resposta |
|---|---|
| Container dev off | 200 página de espera (acorda sozinho) |
| Prod hibernando (`pause`, 11) | `docker unpause` e proxy — **sem página de espera**, atraso < 1 s |
| Container subindo | 200 página de espera (fase acordando) |
| Provisionando | 200 página de espera (fase provisionando) |
| Crash loop / health falhando | 200 página de estado "falhou" (honesto, com log) |
| Conexão ao upstream falhou | reavalia estado (porteiro faz isso); se app morto → página "falhou"; nunca 502 |
| Branch de worktree não existente localmente (02) | 200 página brandiada "branch não encontrada localmente" + instrução de `git worktree add` |
| API de prod pausada (11) | `unpause` + request original respondido em < 1 s — nunca erro |
| API de prod/dev PARADA (12) | **HTTP 503 JSON** `{"error":"service_waking","retry_after":30}` + header `Retry-After` — acorda na fila prioritária; alternativa hold até `api.hold_secs`. Nunca HTML em rota de API, nunca 200 com corpo de erro |
| Asset fingerprinted de release de prod (10) | servido direto do disco pelo fluxd, `Cache-Control: immutable`, sem tocar o container |
| Host não registrado | 404 brandiado "host desconhecido" (único 404; não é caso de container off) |
| VM inteira em Saved (12) | porteiro central (fluxhost) acorda a VM e entrega a rota; browser vê página "acordando a VM" 200 (10–20 s), API vê 503 `Retry-After` |
| `GET /robots.txt` em host dev | gerado `Disallow: /` (robots do projeto ignorado em dev, R22) |
| `GET /robots.txt` em prod sem o arquivo no release | gerado `Allow: /` + linha `Sitemap:` (R23) |
| `GET /sitemap.xml` em prod sem o arquivo no release | urlset gerado com a raiz + `lastmod` do release ativo (R23) |
| Qualquer rota de host dev | resposta normal + header `X-Robots-Tag: noindex, nofollow, noarchive` (R22) |
| Rate limit por host excedido (R30, 07) | `429` brandiado (HTML p/ browser, JSON + `Retry-After` p/ API) — nunca 500 cru |
| Upload acima do teto de body (R30, 07) | `413` antes de ler o corpo inteiro (stream aborta cedo) |

## Decisões

| Decisão | Alternativa descartada | Por quê |
|---|---|---|
| Caddy estático catch-all → fluxd | fluxd registra rota por projeto no Admin API do Caddy | Zero reconfig por deploy; um caminho só de tráfego; menos um estado distribuído para sincronizar |
| fluxd como proxy L7 (hyper) | Caddy direto ao container + error_page | Caddy não sabe acordar container; wake é o coração do requisito |
| Página de espera com `meta refresh` | SSE/websocket puro | Funciona com JS bloqueado, zero dependência, padrão provado no porteiro |
| Wildcard único no Cloudflare | registro por projeto | "Direcionamento automático": projeto novo = zero DNS manual |
| `tls internal` no Caddy | cert origin da Cloudflare | Um segredo a menos para renovar; a edge já cifra o caminho público |

## O que NUNCA fazer

- **Nunca** devolver 500/502/503/504 **cru** (HTML de erro de proxy) para host
  gerenciado off — sempre página de espera/estado com 200. Única exceção
  deliberada: API parada recebe `503` **JSON com `Retry-After`** (12) —
  semântica HTTP padrão, corpo JSON, nunca cru.
- **Nunca** responder HTML em rota detectada como API; nunca 200 com corpo de
  erro (200 falso é o que buga parser).
- **Nunca** publicar `ports:` no container para "testar direto" (fura TLS, fura
  sono, expõe app sem auth na rede interna).
- **Nunca** colocar o painel sem auth atrás do wildcard público.
- **Nunca** deixar o cloudflared apontar direto para um container (pula o fluxd,
  mata wake/sono/página de espera).
- **Nunca** confiar no HTTP 200 da página de espera como sinal de app saudável
  em scripts — usar `flux status`.
- **Nunca** servir página de erro padrão (cinza do browser, HTML de proxy) fora
  de dev — família Flux ou página do app (R31, 15).
- **Nunca** servir o `robots.txt` do projeto em host de dev — em dev a política
  é SEMPRE `Disallow: /` (R22).
- **Nunca** sobrescrever `robots.txt`/`sitemap.xml` preexistentes do projeto em
  prod — o gerado só existe quando o release NÃO traz o arquivo (R23).

## Critérios de aceite

1. Container dev parado: abrir `https://{name}-d.bitello.xyz` da internet →
   página de espera 200 → container acorda → app responde sem F5 (refresh
   automático ≤ 3 s após health OK).
2. Durante o sono, assets/POSTs também não produzem 5xx visível.
3. Host inexistente (`zzz.bitello.xyz`) → página brandiada do Flux.
4. Crash forçado (exit no entrypoint) → página "falhou" com log real após ~3
   restarts, sem nunca um 500 cru.
5. `flux tunnel status` reflete queda do cloudflared em < 30 s no painel.
6. `curl https://{name}-d.bitello.xyz/robots.txt` → `Disallow: /`; qualquer
   resposta de `-d` carrega `X-Robots-Tag: noindex, nofollow, noarchive`.
7. Prod SEM `public/robots.txt` no release → `/robots.txt` e `/sitemap.xml`
   gerados; projeto COM os arquivos → eles passam intactos (diff vazio contra
   o release).
