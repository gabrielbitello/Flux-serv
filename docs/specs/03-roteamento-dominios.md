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
  → cloudflared (systemd na VM) → https://127.0.0.1 (Caddy)
  → Caddy: catch-all reverse_proxy → 127.0.0.1:8080 (fluxd)
  → fluxd decide por Host:
        host gerenciado + acordado  → proxy HTTP para IP interno do container
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

### Caddy (Caddyfile estático)

```caddy
{
    admin off
}

*.bitello.xyz {
    tls internal
    reverse_proxy 127.0.0.1:8080
}

# Acesso LAN direto (sem passar pela edge), opcional:
:443 {
    tls internal
    reverse_proxy 127.0.0.1:8080
}
```

- Acesso LAN: opcionalmente o Gabriel aponta `*.bitello.xyz` para
  `192.168.140.2` no DNS local (CoreDNS já existente no PC) e sai sem passar
  pela internet; o server block `:443` cobre esse caso.
- Caddy roda como systemd com capacidades de bind 443 (pacote oficial já trata).

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
  (padrão `/up`, Laravel). Só considero "acordado e saudável" após health OK —
  entre start e health OK, o visitante vê página de espera com a fase real.

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
| Asset fingerprinted de release de prod (10) | servido direto do disco pelo fluxd, `Cache-Control: immutable`, sem tocar o container |
| Host não registrado | 404 brandiado "host desconhecido" (único 404; não é caso de container off) |
| VM inteira off / fluxd morto | IMPOSSÍVEL servir página de espera da VM — mitigação: `fluxhost` garante VM de pé no boot e on-demand; queda real da VM aparece como erro do Cloudflare (documentado como limite) |

## Decisões

| Decisão | Alternativa descartada | Por quê |
|---|---|---|
| Caddy estático catch-all → fluxd | fluxd registra rota por projeto no Admin API do Caddy | Zero reconfig por deploy; um caminho só de tráfego; menos um estado distribuído para sincronizar |
| fluxd como proxy L7 (hyper) | Caddy direto ao container + error_page | Caddy não sabe acordar container; wake é o coração do requisito |
| Página de espera com `meta refresh` | SSE/websocket puro | Funciona com JS bloqueado, zero dependência, padrão provado no porteiro |
| Wildcard único no Cloudflare | registro por projeto | "Direcionamento automático": projeto novo = zero DNS manual |
| `tls internal` no Caddy | cert origin da Cloudflare | Um segredo a menos para renovar; a edge já cifra o caminho público |

## O que NUNCA fazer

- **Nunca** devolver 500/502/503/504 cru para host gerenciado off — sempre a
  página de espera/estado com 200.
- **Nunca** publicar `ports:` no container para "testar direto" (fura TLS, fura
  sono, expõe app sem auth na rede interna).
- **Nunca** colocar o painel sem auth atrás do wildcard público.
- **Nunca** deixar o cloudflared apontar direto para um container (pula o fluxd,
  mata wake/sono/página de espera).
- **Nunca** confiar no HTTP 200 da página de espera como sinal de app saudável
  em scripts — usar `flux status`.

## Critérios de aceite

1. Container dev parado: abrir `https://{name}-d.bitello.xyz` da internet →
   página de espera 200 → container acorda → app responde sem F5 (refresh
   automático ≤ 3 s após health OK).
2. Durante o sono, assets/POSTs também não produzem 5xx visível.
3. Host inexistente (`zzz.bitello.xyz`) → página brandiada do Flux.
4. Crash forçado (exit no entrypoint) → página "falhou" com log real após ~3
   restarts, sem nunca um 500 cru.
5. `flux tunnel status` reflete queda do cloudflared em < 30 s no painel.
