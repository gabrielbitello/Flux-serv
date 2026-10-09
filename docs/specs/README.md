# Flux — Specs

Flux é um gerenciador de containers Docker escrito em Rust que roda dentro de uma
VM Hyper-V Debian 12 (Bookworm), substituindo o arranjo atual "Docker no WSL +
porteiro + rsync de 4s" da fábrica de dev servers. Código edita no Windows, roda
na VM, expõe em `*.bitello.xyz` via Cloudflare Tunnel + Caddy.

## Como ler

Ordem de leitura recomendada:

1. [`00-visao-geral.md`](00-visao-geral.md) — problema, arquitetura, componentes, roadmap.
2. [`01-vm-hyperv.md`](01-vm-hyperv.md) — provisionamento e ciclo de vida da VM.
3. [`02-sync-arquivos.md`](02-sync-arquivos.md) — sync contínuo Windows → VM.
4. [`03-roteamento-dominios.md`](03-roteamento-dominios.md) — Cloudflare Tunnel, Caddy, router do `fluxd`, página de espera, regra "nunca 500/404".
5. [`04-adormecimento.md`](04-adormecimento.md) — sono automático por interação (estudo de caso: porteiro).
6. [`05-banco-unico-migrations.md`](05-banco-unico-migrations.md) — MariaDB única, guard de imagens, migrations dev/prod.
7. [`06-cli.md`](06-cli.md) — comandos `flux` no Windows, `flux.toml`, fluxo do `flux up`.
8. [`07-fluxd-daemon.md`](07-fluxd-daemon.md) — daemon na VM: router, API de controle, estado.
9. [`08-painel-web.md`](08-painel-web.md) — painel web leve.
10. [`09-seguranca-config.md`](09-seguranca-config.md) — segredos, chaves, config global, envs de prod cifrados.
11. [`10-deploy-prod-github.md`](10-deploy-prod-github.md) — deploy por GitHub Actions, releases imutáveis, blue-green, perfil estático.
12. [`11-recursos-hibernacao.md`](11-recursos-hibernacao.md) — cota global de dev, limite individual de prod, hibernação invisível, tarefas agendadas.
13. [`12-vm-hibernacao-porteiro-central.md`](12-vm-hibernacao-porteiro-central.md) — VM em Saved após ociosidade longa, porteiro central no host, wake de API com prioridade e anti-explosão.

Template de CI: [`docs/templates/deploy-workflow.yml`](../templates/deploy-workflow.yml)
(instalado no projeto com `flux init ci`).

## Gabarito de cada spec

Toda spec segue o mesmo esqueleto: **Objetivo**, **Mecanismo** (como funciona,
com números e caminhos), **Decisões** (alternativa considerada + recomendação +
trade-off), **O que NUNCA fazer**, **Critérios de aceite**. Onde há decisão do
Gabriel pendente, existe a seção **Perguntas em aberto** com a recomendação
marcada.

## Convenções

- `flux` = CLI no Windows. `fluxd` = daemon único binário na VM (modos `serve` e `ctl`).
- Projetos têm dois ambientes: **dev** (`{name}-d.bitello.xyz`, dorme, banco reconstrutível)
  e **prod** (`{name}.bitello.xyz`, não dorme por padrão, banco só recebe `migrate`).
- Paths na VM: `/srv/flux/projects/{name}`, `/srv/flux/secrets/`, `/srv/flux/state/flux.sqlite3`.
- Paths no Windows: `%USERPROFILE%\.flux\` (config, chave SSH, logs).

## Status

| Spec | Estado |
|---|---|
| 00–09 | v0 escritas em 08/10/2026 |
| 10, 11 + template CI | v0.1 (08/10/2026, observações do Gabriel: deploy GitHub, zero downtime, recursos, hibernação prod, branch routing, Adminer, tool, envs cifrados) |
| 12 + ajustes 00/01/03/05/09 | v0.2 (09/10/2026: FATO do banco real do vippers, hibernação da VM, porteiro central, wake de API) |
| ajustes 00/03/07/10/12 | v0.2.1 (09/10/2026: sync = mutagen decidido; R22 homolog escondida de bots; R23 robots/sitemap automáticos p/ prod sem arquivo próprio) |
| Implementação | não iniciada (repo contém só o esqueleto Rust) |
