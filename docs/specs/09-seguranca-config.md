# 09 — Segurança, segredos e configuração

## Objetivo

Um mapa único de tudo que é segredo, onde vive, quem lê, e a configuração global
do Flux — para nenhum token vazar por git, sync ou painel.

## Mecanismo

### Inventário de segredos

| Segredo | Onde vive | Permissão | Quem lê |
|---|---|---|---|
| Chave SSH da VM | `%USERPROFILE%\.flux\id_ed25519` (Windows) + `authorized_keys` do user `flux` (VM) | 600 / Windows ACL só do usuário | CLI (Windows); `fluxd` nunca |
| Token do tunnel Cloudflare | `/srv/flux/secrets/tunnel.token` (VM) | 600 `root:flux` | systemd do cloudflared |
| Senha root do banco | `/srv/flux/secrets/db-root.env` (VM) | 600 | compose do `flux-db` |
| Senhas por projeto (user `{name}`) | SQLite do `fluxd` + `.env` do projeto no lado VM | — | `fluxd` (gera), app (lê via env) |
| Hash da senha do painel | `/srv/flux/secrets/panel.argon2` (VM) | 600 | `fluxd` (valida) |
| Token da API de controle | gerado no `flux setup vm`, salvo no SQLite (hash) e no `%USERPROFILE%\.flux\config.toml` (claro — arquivo do usuário, ACL restrita) | — | CLI → header da API |
| Senha root atual do WSL (`root_password`) | FATO: está em texto no `docker inspect` do container atual — a migração para o Flux é a ocasião de trocar isso | — | — |

Rede interna: a `FluxSwitch` é internal-only (sem uplink direto para o host além
do NAT de saída). Portas da VM publicadas no host: 22 (SSH, bind no IP interno
192.168.140.2), 80/443 (Caddy). A API de controle (:7420) NUNCA sai do
localhost da VM.

### Princípios

1. **Segredo nunca em arquivo sincronizado ou versionado.** `flux.toml` é
   versionado e NÃO contém segredo; `.env` não sai da VM (02).
2. **Backup antes de mudança destrutiva.** `flux vm --recreate`, reset de sync,
   troca de certificado: backup automático de SQLite + dump de bancos +
   checkpoint da VM antes.
3. **Falha é visível.** Autenticação recusada, token expirado, tunnel off:
   mensagem com causa no CLI e no painel — nada de retry silencioso infinito.
4. **Menor privilégio**: user `flux` na VM está no grupo `docker` (necessário)
   e não tem sudo; `fluxd` roda como serviço com acesso ao docker.sock e aos
   paths de `/srv/flux` apenas.

### Configuração global (Windows: `%USERPROFILE%\.flux\config.toml`)

```toml
[vm]
switch = "FluxSwitch"
host_ip = "192.168.140.1"
vm_ip = "192.168.140.2"
user = "flux"
key = "~/.flux/id_ed25519"
cpus = 8
memory_min_gb = 4
memory_max_gb = 16
disk_gb = 80

[domain]
zone = "bitello.xyz"        # {name}.bitello.xyz / {name}-d.bitello.xyz
dev_suffix = "-d"
panel_host = "flux.bitello.xyz"

[api]
token = "..."               # token da API do fluxd
base_url = "http://127.0.0.1:7420"  # via forward SSH pelo CLI

[defaults]
idle_minutes = 15
cpu_percent_teto = 5.0
sync_debounce_secs = 5

[image_guard]
block = ["mysql", "mariadb", "postgres", "mongo", "redis", ...]  # default da 05
```

- Diretório `%USERPROFILE%\.flux\` com ACL do usuário apenas; log do CLI em
  `%USERPROFILE%\.flux\logs\` (rotação 10 MB).
- O CLI nunca edita o config sozinho — `flux setup` escreve com `--set k=v`
  explícito e mostra diff.

### Migração do WSL → Flux (uma vez, assistida)

`flux migrate-from-wsl` (fase pós-F9): dump dos bancos do container `mariadb`
atual (`mysqldump` por database → import no `flux-db` com users equivalentes),
re-cadastro dos projetos do `registry.json` da devfactory. O WSL segue de pé
até o Gabriel validar e desligar manualmente — nunca é desligado pelo Flux.

## O que NUNCA fazer

- **Nunca** colocar segredo em `flux.toml`, compose do projeto, SQLite em claro
  ou na sync.
- **Nunca** abrir a API de controle (:7420) para a LAN.
- **Nunca** reutilizar a chave SSH do Flux para outra máquina.
- **Nunca** logar segredo (redact em qualquer log de env/headers).
- **Nunca** desligar o WSL atual automaticamente durante a migração.

## Critérios de aceite

1. `git grep` de segredos no repo limpo; `.env` nunca aparece em
   `/srv/flux/projects/*` vindo do Windows.
2. Portscan da VM de outro host da LAN: só 22/80/443 respondem.
3. Token da API errado → 401 com mensagem clara, logado no `events`.
4. Backup diário do SQLite existe e é restaurável (teste de restauração
   documentado).
5. `flux migrate-from-wsl` reproduz users/DBs do WSL no `flux-db` com checksum
   de dump conferido.
