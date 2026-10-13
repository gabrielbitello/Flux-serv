# 01 — VM Debian 12 Bookworm (VirtualBox)

## Objetivo

Criar e manter, de forma idempotente e sem intervenção manual, a VM que
hospeda o Docker Engine, o Caddy e o `fluxd`. O Gabriel nunca precisa abrir a
GUI do VirtualBox no dia a dia — quem fala com o hypervisor é o serviço
`fluxhost` e o CLI `flux`.

## FATO do host (13/10/2026)

- O PC é **Windows 11 Home** (`EditionID: Core`): o recurso opcional
  `Microsoft-Hyper-V-All` **não existe** no Home — `Enable-WindowsOptionalFeature`
  falha com "Nome do recurso desconhecido". A versão anterior desta spec
  assumia Hyper-V; premissa quebrou e foi refeita.
- Hypervisor escolhido: **VirtualBox 7.2.20** (GPL, custo zero). Instalado com
  checksum verificado contra o oficial (`a81777d2b36380ce042a29e9c554cf032eb46a793f62e3cc82e7411e535c2c26`).
  Alternativas descartadas: VMware Workstation Pro (free pessoal, mas download
  exige conta Broadcom) e upgrade para Pro (custo, decisão do Gabriel, porta
  segue aberta).
- Hardware: 12 logical processors, 32 GB RAM, 157,9 GB livres em C:,
  `HypervisorPresent: True` (pela presença do WSL2).
- Coexistência: com WSL2 ativo, o VBox roda em modo NEM (via plataforma
  Hyper-V) — funciona com overhead; `wsl --shutdown` libera VT-x nativo
  (decisão do Gabriel: nada do WSL precisa ser mantido). WSL é tool de
  PREPARO (qemu-img, ISO), nunca dependência de runtime.
- Se o host virar Pro um dia: `Microsoft-Hyper-V-All` instala com o script
  pronto (`%USERPROFILE%\.flux\install-hyperv.ps1`) e a VM migra por conversão
  VDI → VHDX (~30 min estimados). `fluxd`/CLI NÃO mudam — hypervisor é
  detalhe atrás do `fluxhost`/CLI.

## Mecanismo

### Recursos do host (pré-requisitos verificados pelo `flux setup host`)

- VirtualBox ≥ 7.2 instalado (`VBoxManage` acessível).
- Disco: ≥ 40 GB livres; RAM: a VM usa alocação **FIXA** (o VBox não tem a
  memória dinâmica do Hyper-V) — default 4 GB, ajustável até 8 GB; host tem
  32 GB, ociosidade custa os 4 GB parados (trade-off aceito e registrado).

### Rede

- **NIC 1 (host-only)**: rede dedicada criada por `VBoxManage hostonlyif` —
  host `192.168.140.1/24`, VM `192.168.140.2` estático via cloud-init. Nunca a
  host-only default do VBox (192.168.56.x) nem o NAT default (10.0.2.15 não é
  roteável do host).
- **NIC 2 (NAT)**: saída para internet (pull de imagens, apt). Durante o
  provisionamento, `natpf` no NAT expõe SSH em `127.0.0.1:2222` e o healthz do
  `fluxd` em `127.0.0.1:7420` até o host-only estar validado.
- O host alcança a VM por SSH em `192.168.140.2:22` com chave dedicada
  `%USERPROFILE%\.flux\id_ed25519` (gerada pelo setup, sem passphrase — rede
  interna e isolada; nota de segurança em [09](09-seguranca-config.md)).

### Provisionamento (`flux setup vm`)

Pipeline idempotente, cada passo verifica antes de agir:

1. Baixa `debian-12-genericcloud-amd64.qcow2` (imagem oficial, versão pinada
   com SHA512 registrado no repo) para `%USERPROFILE%\.flux\cache\`.
2. `qemu-img` (via WSL, pacote `qemu-utils` do apt — verificado pelo apt)
   converte qcow2 → **VDI** e expande para 80 GB dinâmico.
3. Gera seed cloud-init (datasource NoCloud, ISO com label `cidata`,
   `genisoimage` no WSL; o objetivo longo-termo é escritor ISO9660 em Rust
   dentro do `flux setup vm`, sem tooling externo): usuário `flux` (sem
   senha, só chave SSH), hostname `flux`, `ssh_pwauth: false`, IP estático na
   host-only, pacotes: `docker-ce` (repo oficial Docker) e `caddy` (repo
   oficial) instalados no runcmd. O `cloudflared` NÃO vai para a VM:
   `flux setup tunnel` instala o serviço Windows no host (12 — porteiro
   central precisa dele vivo com a VM dormindo); o token fica em
   `%USERPROFILE%\.flux\tunnel.token` (09).
4. `VBoxManage createvm --name Flux --ostype Debian_64 --firmware efi` —
   6 vCPUs (metade do host), 4096 MB fixos, VDI 80 GB, seed ISO anexado,
   NIC NAT + `natpf`, depois NIC host-only.
5. Boot headless (`startvm --type headless`) + espera SSH responder (poll 1 s,
   timeout 10 min, progresso no terminal). Validação do primeiro boot:
   `cloud-init status --wait`, `docker version`, `caddy version`.
6. Etapa host-only: cria a rede `192.168.140.1/24`, anexa a NIC 1, aplica o
   netplan estático na VM por SSH e valida `ssh flux@192.168.140.2`.
7. Instala o binário `fluxd` (Linux, compilado do próprio repo) na VM
   (`/usr/local/bin/fluxd`) e os units systemd: `fluxd.service`
   (After=docker.service, Restart=always), `caddy.service`.

### Ciclo de vida

- **Boot do Windows** → serviço `fluxhost` (SYSTEM) executa
  `VBoxManage startvm Flux --type headless` se o estado for `poweroff`.
  DECISÃO: quem sobe a VM no boot é o `fluxhost` — o autostart nativo do
  VirtualBox no Windows é frágil e o serviço já é dono do ciclo (no Hyper-V
  era `AutomaticStartAction`; aqui virou responsabilidade explícita do
  serviço, sem perda de garantia).
- **Ociosidade longa (12)**: 30 dias sem acesso REAL em TODOS os hosts →
  `VBoxManage controlvm Flux savestate` (equivalente ao Save-VM; resume em
  10–20 s, sem re-subida de containers). Guardas do `fluxd` confirmadas
  antes: sync sem pendências, sem lease, sem deploy/backup em curso.
- **`flux` detecta VM off/saved** (SSH timeout) → pede ao serviço `fluxhost`
  para subir (`startvm`; em `saved`, `startvm` também resume) → espera SSH.
  Nunca exige UAC no comando do usuário (o serviço é quem chama o VBoxManage).
- **Desligamento**: manual apenas (`flux vm stop` → `controlvm acpipowerbutton`
  com timeout, depois `poweroff`). O CLI avisa quantos projetos estão
  acordados antes.
- **Upgrade**: `VBoxManage snapshot Flux take pre-upgrade-<data>`, atualiza
  apt + fluxd, rollback = `VBoxManage snapshot Flux restore <nome>`.
  CheckpointStrategy do Hyper-V (`Disabled`) não tem equivalente necessário:
  snapshots do VBox são manuais, sob comando do flux.

### Boot do host, Windows Update e queda de energia (R25)

A VM é a **primeira coisa útil do PC**: sobe no BOOT, antes de qualquer app de
usuário (apps só nascem no login; serviços no boot).

- `fluxhost` é serviço SYSTEM **Automatic** (nunca Delayed); a VM sobe pelo
  serviço no boot. **Sequência pós-boot**: boot → `VBoxSDS`/`vboxdrv` →
  `fluxhost` → `startvm` → systemd da VM (`docker` + `fluxd` enabled) →
  política do `fluxd`:
  - `flux-db` de pé;
  - prods (HTTP + worker) voltam **DE PÉ** (`restart: unless-stopped`) — nunca
    pausadas pós-boot: invisibilidade primeiro; a pausa por ociosidade
    re-avalia depois do cooldown normal (11);
  - devs ficam DORMINDO (acordam on-demand, 03).
- **O host NÃO é sequestrado** (Gabriel usa o PC com a VM de pé): 6 vCPUs
  (metade do host), 4–8 GB fixos, builds/acordos enfileirados com teto (11/12).
- **Queda de luz**: ext4 journaling + SQLite WAL + restart policies = a VM
  volta ao estado pós-boot acima; nenhuma ação manual. `savestate` implícito
  no desligamento limpo do host NÃO é automático no VBox — o `fluxhost`
  intercepta o shutdown do Windows e executa `savestate` (equivalente ao
  `AutomaticStopAction Save` do Hyper-V).
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
| VirtualBox 7.2 | Hyper-V (não existe no Home), VMware free (conta Broadcom), hack de Hyper-V no Home (quebra em Update) | Custo zero, download público sem conta, checksum oficial verificável; mesmas garantias via `VBoxManage` |
| Rede host-only dedicada + IP estático | NAT default do VBox | 10.0.2.15 não é roteável do host; SSH e config exigem IP estável (mesmo motivo do `FluxSwitch`) |
| Quem sobe a VM no boot = serviço `fluxhost` | Autostart nativo do VBox no Windows | Frágil e pouco observável; o serviço já é dono do ciclo e dá API de status |
| Seed NoCloud via `genisoimage` no WSL (fase de fundação) | ferramenta externa permanente no host | Uso único durante provisionamento; pipeline definitivo vira escritor ISO9660 em Rust dentro do `fluxd setup vm` |
| 4–8 GB de RAM fixos | memória dinâmica (não existe no VBox) | Trade-off aceito: host tem 32 GB; ociosidade custa os 4 GB parados |
| Binários pinados com checksum | apt no Windows / winget | Reprodutibilidade: mesma VM criada em qualquer dia é igual |

## O que NUNCA fazer

- **Nunca** usar o NAT default do VBox como única rede da VM (IP não roteável
  do host).
- **Nunca** depender do WSL em runtime — WSL é tool de preparo
  (qemu-img/ISO), o ciclo de vida da VM é só VirtualBox + `fluxhost`.
- **Nunca** usar a GUI do VBox para operar a VM do Flux (state diverge do
  `fluxd`/`fluxhost`).
- **Nunca** recriar a VM (`--recreate`) sem backup do `/srv/flux/state/` e
  dump do banco — o CLI exige confirmação explícita e faz backup automático
  antes.
- **Nunca** desligar a VM com projetos acordados sem o aviso do CLI.
- **Nunca** improvisar ativação de Hyper-V no Home (hack não-oficial quebra
  em Windows Update).

## Critérios de aceite

1. Provisionamento em máquina limpa termina sem erro e `flux vm status`
   mostra: VM Running, SSH ok, Docker ok, Caddy ok, fluxd ok.
2. Reboot do Windows: `fluxhost` sobe a VM; prods de pé sem intervenção;
   `flux up` em um repo funciona logo após o boot sem UAC.
3. `flux vm ssh` abre shell na VM sem senha.
4. Desligar a VM e rodar `flux up`: CLI sobe a VM e continua, mostrando
   progresso.
5. `savestate` → `startvm` resume sem re-subida de containers (sem pico).
