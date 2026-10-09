# 08 — Painel web

## Objetivo

Painel único para ver e operar tudo sem SSH: projetos, sono, sync, banco,
tunnel, logs. Profissional e **leve**: sem SPA, sem build de frontend, sem
framework JS pesado — HTML server-side + htmx + CSS próprio.

## Mecanismo

### Stack

| Camada | Escolha | Por quê |
|---|---|---|
| Server | axum + askama (templates) | Mesmo runtime do router; zero processo novo |
| Interatividade | htmx (1 arquivo, ~14 kB) + SSE | Páginas vivem (logs, status) sem React/Vite/Node |
| Estilo | CSS próprio embutido via `rust-embed` (~1 arquivo, design tokens) | Identidade própria, sem CDN, funciona offline |
| Ícones | SVG inline | Sem pacote de ícones |

Total de JS de terceiros: só o htmx. O painel inteiro carrega em < 100 kB
comprimido.

### Acesso

- URL: `https://flux.bitello.xyz` (via tunnel + Caddy + fluxd, 03) — funciona
  fora de casa; na LAN, mesmo host resolve via DNS local.
- Auth: usuário único (Gabriel), senha com hash Argon2id em
  `/srv/flux/secrets/panel.argon2`; sessão por cookie httpOnly + SameSite=Lax,
  expira em 12 h. Rate limit de login (5/min). Opcional: Cloudflare Access na
  frente (o painel respeita o header `Cf-Access-Authenticated-User` se
  configurado).
- `flux panel` no CLI abre e autentica (token de sessão efêmero via URL, 1 uso).

### Páginas

1. **Dashboard** — cartão por projeto: nome, ambientes (dev/prod) com dot de
   status (acordado/dormindo/falhou/subindo), URL clicável, sono em contagem
   regressiva, sync ok/pendente, migrations pendentes, última atividade. Ações
   rápidas: acordar/dormir/restart/ver logs. Barra superior: VM (uptime, RAM,
   CPU), flux-db saudável, tunnel conectado, caddy ativo.
2. **Projeto** — abas:
   - *Visão geral*: compose dos services, health, últimos deploys (do
     `deploy_log` com motivo de cada migration/rebuild);
   - *Logs*: seletor de service + stream SSE com follow, filtro de texto,
     botão copiar; dormir enquanto assiste é bloqueado (lease do painel);
   - *Banco*: DBs do projeto, tamanho, migrations (aplicadas/pendentes/mutadas
     com hash), botões migrate/fresh (fresh **invisível em prod**); consultas
     em prod rodam com `flux_panel_ro` (SELECT-only, 05) — a UI de escrita
     some em prod, e o grant barra mesmo se forçado; link para o Adminer
     (LAN-only, 05);
   - *Sync*: estado da sessão mutagen, latência, pendências, botão reset;
   - *Domínios*: URLs dev/prod, branches de worktree ativas (02), status
     DNS/tunnel;
   - *Deploys* (prod): releases, cor ativa do blue-green, histórico com SHA e
     autor do push (10).
3. **Infra** — VM (recursos, checkpoints), containers (todos, com sono), flux-db
   (conexões, tamanho por DB), caddy/cloudflared status, eventos recentes.
4. **Config** — leitura do config global + secrets com valor oculto (nunca
   renderiza segredo; botão "copiar" para token de API do fluxd).

### Comportamento

- Poll/refresh: dashboard usa SSE (muda de estado = atualiza linha sem reload);
  tudo continua legível com JS desligado (refresh manual).
- Dark mode automático (`prefers-color-scheme`) — cada cor do light tem
  equivalente no dark, nenhuma cor some (regra de UI do Gabriel, herdada do
  padrão Filament).
- Mobile: usable (cartões empilham; logs quebram linha).
- Erros do painel nunca silenciosos: toast com a mensagem real da API + log no
  `events`.

## Decisões

| Decisão | Alternativa descartada | Por quê |
|---|---|---|
| Server-side + htmx | SPA (React/Next) ou Tauri | Leveza e zero toolchain de frontend; "profissional e leve" do Gabriel |
| SSE para logs/status | websocket | Unidirecional, reconexão automática do browser, passa pelo proxy do fluxd sem upgrade |
| CSS próprio | Tailwind CDN / Pico | Sem CDN (funciona offline/LAN), bundle mínimo, tokens versionados no repo |
| Login único + Argon2id | multi-user/SSO complexo | O painel é de uma pessoa; Cloudflare Access cobre SSO se quiser |

## O que NUNCA fazer

- **Nunca** renderizar valor de segredo na tela (mostrar `••••` + copiar).
- **Nunca** criar dependência de build de frontend (Node/Vite) para o painel —
  `cargo build` gera tudo.
- **Nunca** expor ação destrutiva (fresh em prod, VM recreate) — não existe
  botão; vem nem CLI.
- **Nunca** deixar o painel sem auth no wildcard público.
- **Nunca** deixar cor do tema light sem par no dark.

## Critérios de aceite

1. Login → dashboard com estado REAL (comparado com `docker ps` da VM).
2. Log ao vivo: `flux exec` gerando output aparece no painel em < 1 s (SSE).
3. Painel inteiro (gzip) < 100 kB; Lighthouse performance ≥ 90 em desktop.
4. Com JS desligado: páginas renderizam e ações funcionam (POST normal).
5. Dark e light: contraste AA verificado por script (regra do Gabriel) — nenhuma
   cor some na troca.
