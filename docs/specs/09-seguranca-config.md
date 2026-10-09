# 09 — Segurança, segredos e configuração

## Objetivo

Um mapa único de tudo que é segredo, onde vive, quem lê, e a configuração global
do Flux — para nenhum token vazar por git, sync ou painel.

## Mecanismo

### Inventário de segredos

| Segredo | Onde vive | Permissão | Quem lê |
|---|---|---|---|
| Chave SSH da VM | `%USERPROFILE%\.flux\id_ed25519` (Windows) + `authorized_keys` do user `flux` (VM) | 600 / Windows ACL só do usuário | CLI (Windows); `fluxd` nunca |
| Token do tunnel Cloudflare | `%USERPROFILE%\.flux\tunnel.token` (HOST Windows — o cloudflared é serviço do host, 12) | ACL do usuário | serviço Windows do cloudflared |
| Senha root do banco | `/srv/flux/secrets/db-root.env` (VM) | 600 | compose do `flux-db` |
| `.env` de produção | **cifrado** em `/var/lib/flux-creds/{name}.cred` (VM) — mecanismo abaixo | 600 `root:root` | `fluxd` (decifra em runtime, injeta no container) |
| Chave age de prod | sealed pelo vTPM da VM (`systemd-creds`); backup cifrado no Windows | 600 | `fluxd` |
| Senhas por projeto (user `{name}`) | SQLite do `fluxd` + `.env` do projeto no lado VM | — | `fluxd` (gera), app (lê via env) |
| Hash da senha do painel | `/srv/flux/secrets/panel.argon2` (VM) | 600 | `fluxd` (valida) |
| Token da API de controle | gerado no `flux setup vm`, salvo no SQLite (hash) e no `%USERPROFILE%\.flux\config.toml` (claro — arquivo do usuário, ACL restrita) | — | CLI → header da API |
| Token de deploy por projeto | `flux project token {name}` → GitHub Secrets do projeto; hash no SQLite | — | GitHub Actions → endpoint de deploy (10) |
| Senha root atual do WSL (`root_password`) | FATO: está em texto no `docker inspect` do container atual — a migração para o Flux é a ocasião de trocar isso | — | — |

Rede interna: a `FluxSwitch` é internal-only (sem uplink direto para o host além
do NAT de saída). Portas da VM publicadas no host: 22 (SSH, bind no IP interno
192.168.140.2), 80/443 (Caddy). A API de controle (:7420) NUNCA sai do
localhost da VM.

### Envs de produção cifrados em repouso (R18)

Cadeado com duas camadas, sem entraves na operação:

1. **age (X25519, moderno, toolchain Rust)** cifra o `.env` de prod. Cifra é
   rápida (milissegundos para arquivos de KB) e o formato é auditável.
2. A chave privada do age NUNCA fica em texto claro: é *sealed* pelo
   **vTPM da VM** via `systemd-creds encrypt --tpm2` (VM Gen2, vTPM ativada no
   provisionamento, 01). O `fluxd` decifra o `.cred` em runtime para injetar
   no container — mas quem copiar o VHDX inteiro **não** decifra: o TPM da
   VM original é o desbloqueio, disco roubado = dados cifrados.

Workflow: `flux env prod set {name}` (edita no editor do Windows, cifra e
envia já cifrado — a chave nunca trafega), `flux env prod show {name}`
(decifra on-the-fly no terminal, nunca grava claro), rotação = novo age key +
re-seal com aviso no painel. Fallback documentado: sem vTPM disponível,
arquivo 600 `root:root` com alerta visível no painel até o TPM ser ligado.

Painel: **nunca** exibe valor de env de prod — só chave, hash e data de
atualização (R18).

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
- **Nunca** gravar `.env` de prod em texto claro em disco na VM (só cifrado;
  decifra é em memória/injeção).
- **Nunca** deixar o painel escrever em dados de prod (grant SELECT-only, 05).
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
6. Copiar o VHDX da VM para outro host → `.env` de prod não decifra (TPM
   diferente); na VM original, container sobe com env correto.
7. Painel mostra env de prod: só nomes de chave e datas — valor nunca sai.
