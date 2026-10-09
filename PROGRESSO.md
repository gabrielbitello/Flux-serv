# PROGRESSO — Flux

Diário de status do projeto. Entradas acumuladas por sessão, mais recente no topo.

## 08/10/2026 — Specs v0 escritas

**Intenção**: definir a arquitetura completa do Flux antes de qualquer código —
gerenciador de containers Docker em VM Hyper-V Bookworm, CLI `flux` no Windows,
daemon `fluxd` na VM, porteiro como estudo de caso.

**Resultado**: 12 specs em `docs/specs/` (índice no [README](docs/specs/README.md))
+ template de CI em `docs/templates/deploy-workflow.yml`, cobrindo: VM Hyper-V,
sync mutagen, roteamento Cloudflare+Caddy com página de espera (nunca 500/404),
adormecimento por interação, banco único com migrations dev/prod, CLI, fluxd,
painel web, segurança/config, deploy de prod por GitHub Actions com blue-green,
limites de recursos e hibernação de prod.

**Segunda rodada do dia (observações do Gabriel incorporadas, v0.1)**:

- Prod 100% na VM: código entra por GitHub Actions (template pronto), nunca do
  Windows; releases imutáveis + perfil estático (assets pelo fluxd, opcache,
  caches de config/route/view) = mais rápido com menos PHP vivo.
- Zero downtime blue-green com drain: job em andamento termina na versão velha;
  scheduler com lock único; rollback sem build.
- Recursos: cota GLOBAL de dev via slice cgroup (`flux-dev.slice`, kernel
  garante a soma) com sleep-to-admit; prod com limite individual; prod hiberna
  em `docker pause` (< 1 s, invisível) e o worker 24/7 mantém e-mails/
  notificações; pressão de RAM degrada pause→stop.
- Branch routing: `{branch}-{name}-d.bitello.xyz` serve worktree local;
  branch só no GitHub → página brandiada 200.
- Adminer (LAN-only) no lugar do phpmyadmin; painel e Adminer read-only em prod
  por grant SQL SELECT-only.
- `flux tool`: node/php/composer/rust em container efêmero na VM — PC não
  instala toolchain.
- Envs de prod cifrados: age + `systemd-creds --tpm2` (vTPM da VM); VHDX
  roubado não decifra; painel nunca mostra valor.

**Decisões centrais (a validar)**:

- Caddy estático catch-all → `fluxd` decide tudo por `Host` (wake, proxy, páginas).
- Sync = mutagen `one-way-replica` (Windows fonte, VM réplica), `.env` fora do sync.
- Estado do fluxd em SQLite (o daemon sobe antes do banco; zero dependência circular).
- Imagem do banco única: `mysql:8.4` (paridade com o WSL atual — o container
  `mariadb` atual roda MySQL 8.4, não MariaDB).
- Prod (`{name}.bitello.xyz`) nunca reconstrói banco e não dorme; dev
  (`{name}-d.bitello.xyz`) reconstrói em mutação de migration.

**O que NUNCA fazer** (acumulado das specs): publicar `ports:` de container de
projeto ou 3306; servir 500/404 cru para host gerenciado off; sync de
`vendor/`/`node_modules/`/`storage/`/`.env`; reconstruir banco de prod; dormir
`flux-db`; segredo em arquivo versionado/sincronizado.

**Próximo**: revisão do Gabriel nas perguntas em aberto da [00-visao-geral](docs/specs/00-visao-geral.md)
(imagem do banco, Redis no guard, suspend da VM) → F1 (setup host + VM).
