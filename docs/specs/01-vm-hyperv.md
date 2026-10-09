# 01 — VM Hyper-V (Debian 12 Bookworm)

## Objetivo

Criar e manter, de forma idempotente e sem intervenção manual, a VM Hyper-V que
hospeda o Docker Engine, o Caddy, o cloudflared e o `fluxd`. O Gabriel nunca
precisa abrir o Gerenciador do Hyper-V no dia a dia.

## Mecanismo

### Recursos do host (pré-requisitos verificados pelo `flux setup host`)

- Windows com Hyper-V habilitado (`Get-WindowsOptionalFeature -Online -FeatureName Microsoft-Hyper-V`).
- O WSL2 continua funcionando ao lado (ambos usam a plataforma Hyper-V; não há
  conflito). O arranjo WSL atual segue de pé até a migração terminar.
- Disco: ≥ 40 GB livres; RAM: a VM usa memória dinâmica (mín. 4 GB, máx. 16 GB
  por padrão, configurável).

### Rede

- Switch interno dedicado `FluxSwitch`: host `192.168.140.1`, VM `192.168.140.2`
  (IP estático dentro da VM via cloud-init — Default Switch troca IP a cada boot
  do host, por isso switch próprio).
- Saída para internet (pull de imagens, apt, tunnel): NAT via `New-NetNat`
  (`FluxNat`), DNS da VM aponta para `1.1.1.1` + `8.8.8.8`.
- O host alcança a VM por SSH em `192.168.140.2:22` com chave dedicada
  `%USERPROFILE%\.flux\id_ed25519` (gerada pelo setup, sem passphrase — é rede
  interna e isolada; nota de segurança em [09](09-seguranca-config.md)).

### Provisionamento (`flux setup vm`)

Pipeline idempotente, cada passo verifica antes de agir:

1. Baixa `debian-12-genericcloud-amd64.qcow2` (imagem oficial, versão pinada no
   config) e `qemu-img` (binário estático Windows, versão pinada, checksum
   verificado) para `%USERPROFILE%\.flux\cache\`.
2. Converte qcow2 → VHDX dinâmico e expande para o tamanho alvo (padrão 80 GB).
3. Gera seed cloud-init (datasource NoCloud, label `cidata`): usuário `flux`
   (sem senha, só chave SSH), IP estático, hostname `flux`, SSH hardening
   (sem password auth), pacotes: `docker-ce` (repo oficial Docker),
   `cloudflared` (repo Cloudflare), caddy (repo oficial), `mutagen` não — o
   agente mutagen é injetado pelo daemon do Windows na primeira sync.
   O seed ISO é produzido em Rust puro (crate escritor de ISO9660) para não
   dependência extra de tooling.
4. `New-VM` Gen2, Secure Boot com template Microsoft UEFI CA desabilitado
   (Debian genericcloud não assina com esse CA; se falhar boot, trocar template),
   vCPUs = metade do host (mín. 4), memória dinâmica 4–16 GB, CheckpointStrategy
   `Disabled` em produção do Flux (snapshots manuais via `flux vm checkpoint`).
5. Boot + espera SSH responder (poll 1s, timeout 10 min, progresso no terminal).
6. Instala o binário `fluxd` na VM (`/usr/local/bin/fluxd`) e os units systemd:
   `fluxd.service` (After=docker.service, Restart=always), `caddy.service`.
   O `cloudflared` NÃO vai para a VM: `flux setup tunnel` instala o serviço
   Windows no host (12 — porteiro central precisa dele vivo com a VM dormindo);
   o token do tunnel fica em `%USERPROFILE%\.flux\tunnel.token` (09).

### Ciclo de vida

- **Boot do Windows** → serviço `fluxhost` (SYSTEM) executa `Start-VM Flux -IfOff`
  assíncrono. A VM fica de pé mesmo sem ninguém usar (custo em idle: ~0,5–1 GB
  RAM com memória dinâmica; aceitável).
- **Ociosidade longa (12)**: 30 dias sem acesso real em TODOS os hosts →
  `fluxhost` executa `Save-VM` (guardas do `fluxd` confirmadas antes: sync sem
  pendências, sem lease, sem deploy/backup em curso). Resume em 10–20 s, sem
  re-subida de containers (sem pico). Estado `saved` visível no painel.
- **`flux` detecta VM off/saved** (SSH timeout) → pede ao serviço `fluxhost`
  para subir → espera SSH. Nunca exige UAC no comando do usuário (o serviço é
  quem chama os cmdlets Hyper-V).
- **Desligamento**: manual apenas (`flux vm stop` → ACPI shutdown com timeout,
  depois off). O CLI avisa quantos projetos estão acordados antes.
- **Upgrade**: `flux vm upgrade` cria checkpoint `pre-upgrade-<data>`, atualiza
  apt + fluxd (binário vem do próprio repo compilado), rollback = `Restore-VM`.

### Boot do host, Windows Update e queda de energia (R25)

A VM é a **primeira coisa útil do PC**: sobe no BOOT, antes de qualquer app de
usuário (apps só nascem no login; serviços no boot).

- `vmms` (Hyper-V Virtual Machine Management) é serviço Automatic; `fluxhost`
  é SYSTEM **Automatic** (nunca Delayed — atrasaria a VM à toa) com dependência
  de `vmms`; a VM com `Set-VM -AutomaticStartAction Start -AutomaticStartDelay 0`
  e `Set-VM -AutomaticStopAction Save` (shutdown limpo do host = Save, não
  desligar a seco).
- **O host NÃO é sequestrado** (Gabriel usa o PC com a VM de pé): vCPUs da VM =
  metade dos logical processors (passo 4), memória dinâmica devolve ociosidade,
  builds/acordos enfileirados com teto (11/12). VM sem config nunca toma todos
  os cores.
- **Sequência pós-boot** (Windows Update reiniciou ou queda de luz voltou):
  boot → `vmms` → `fluxhost` → Start-VM → systemd da VM (`docker` + `fluxd`
  enabled) → política do `fluxd`:
  - `flux-db` de pé;
  - prods (HTTP + worker) voltam **DE PÉ** (`restart: unless-stopped`) — nunca
    pausadas pós-boot: invisibilidade primeiro; a pausa por ociosidade
    re-avalia depois do cooldown normal (11);
  - devs ficam DORMINDO (acordam on-demand, 03);
- **Queda de luz**: ext4 journaling + SQLite WAL + restart policies = a VM
  volta ao estado pós-boot acima; nenhuma ação manual.
- **Validação**: `fluxhost` confere SSH da VM em ≤ 2 min pós-boot; falhou →
  evento local + alerta "VM não voltou" (13).

### Layout na VM

```
/srv/flux/
  projects/{name}/        # código sincronizado do Windows (dev)
  projects/{name}-prod/   # snapshot de deploy (prod)
  secrets/                # tunnel.token, db-root.env, panel.argon2 (600, root:flux)
  state/flux.sqlite3      # estado do fluxd
  db/init/                # init SQL do banco (mount ro no flux-db)
  logs/                   # logs de provisionamento por projeto
```

## Decisões

| Decisão | Alternativa descartada | Por quê |
|---|---|---|
| Switch interno + IP estático | Default Switch (NAT dinâmico) | IP do Default Switch muda a cada boot do host; SSH e config quebrariam |
| Debian genericcloud + cloud-init | ISO netinst + preseed | genericcloud é a imagem feita para VM cloud; preseed de netinst é frágil e demora |
| Serviço Windows `fluxhost` para cmdlets Hyper-V | UAC a cada comando / tarefa agendada | Comando do usuário nunca pede elevação; tarefa agendada não dá API de status |
| Binários pinados com checksum | apt no Windows / winget | Reprodutibilidade: mesmo VM criada em qualquer dia é igual |

## O que NUNCA fazer

- **Nunca** usar o Default Switch para a VM do Flux.
- **Nunca** deixar o provisionamento rodar `rsync`/cópia manual de assets dentro
  da VM (regra herdada: provisionamento é sempre via flux/porteiro, nunca na mão).
- **Nunca** recriar a VM (`--recreate`) sem backup do `/srv/flux/state/` e dump
  do banco — o CLI exige confirmação explícita e faz backup automático antes.
- **Nunca** desligar a VM pelo Gerenciador do Hyper-V com projetos acordados.

## Critérios de aceite

1. `flux setup vm` em máquina limpa termina sem erro e `flux vm status` mostra:
   VM Running, SSH ok, Docker ok, Caddy ok, cloudflared ok, fluxd ok.
2. Reboot do Windows: VM volta sozinha; prods de pé sem intervenção; `flux up`
   em um repo funciona logo após o boot sem UAC.
3. Reboot simulado com 3 prods ativos: HTTP de todos responde ≤ 3 min após o
   login, sem comando manual.
4. `flux vm ssh` abre shell na VM sem senha.
4. Desligar a VM e rodar `flux up`: CLI sobe a VM e continua, mostrando progresso.
