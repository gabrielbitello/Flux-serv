# Provisionamento da VM Flux (VirtualBox) — referência do `flux setup vm`

Estado atual: pipeline executado MANUALMENTE na sessão de 13/10/2026 (fase de
fundação, antes do `flux setup vm` existir em Rust). Este documento é a
referência canônica dos passos — cada um já validado nesta máquina.

## Pré-requisitos do host (verificados)

- VirtualBox 7.2.20 instalado (checksum SHA256 conferido contra o oficial).
- Chave SSH dedicada: `%USERPROFILE%\.flux\id_ed25519` (sem passphrase).
- WSL Ubuntu para `qemu-img`/`genisoimage` (tool de PREPARO, nunca runtime):
  `wsl -u root apt-get install -y qemu-utils genisoimage`.
- CRÍTICO: hypervisor do Windows desligado (senão VBox roda em modo NEM
  "Snail": lento e instável — a VM chegou a morrer sozinha). Toggle em
  `%USERPROFILE%\.flux\toggle-hypervisor-off.ps1`, **vale só após reboot**:
  1. Registro: `HKLM\...\DeviceGuard\Scenarios\HypervisorEnforcedCodeIntegrity\Enabled = 0`
  2. `bcdedit /set hypervisorlaunchtype off`
  Reverter (se quiser WSL2 de volta): `Enabled = 1` + `hypervisorlaunchtype auto` + reboot.

## Passos (executados e validados em 13/10/2026)

```powershell
# 1. Imagem Debian (pinada — SHA512 conferido contra cloud.debian.org):
#    a09170d17e13af43da61774666f520e3bbec0f8bd96b4db37c15f3426fb327ccad950b80dbe78c09aa5ca90ce5015e99334b0ba5188fd4018736bc3fb5f0882c
curl -sL -o "$env:USERPROFILE\.flux\cache\debian-12-genericcloud-amd64.qcow2" `
  "https://cloud.debian.org/images/cloud/bookworm/latest/debian-12-genericcloud-amd64.qcow2"

# 2. Converter qcow2 -> VDI (qemu-img NÃO redimensiona VDI; o resize é do VBox):
wsl -e bash -c "qemu-img convert -O vdi '/mnt/c/Users/ieubi/.flux/cache/debian-12-genericcloud-amd64.qcow2' '/mnt/c/Users/ieubi/.flux/cache/debian-12.vdi'"

# 3. Seed cloud-init (NoCloud): %USERPROFILE%\.flux\seed\{meta-data,user-data}
#    user: flux (só chave SSH, sudo NOPASSWD), docker-ce + caddy via runcmd.
wsl -e bash -c "cd /mnt/c/Users/ieubi/.flux/seed && genisoimage -output seed.iso -volid cidata -joliet -rock user-data meta-data"

# 4. VM:
$VB = "C:\Program Files\Oracle\VirtualBox\VBoxManage.exe"
& $VB createvm --name Flux --ostype Debian_64 --basefolder "$env:USERPROFILE\.flux\vms" --register
& $VB modifyvm Flux --firmware efi --cpus 6 --memory 4096 --vram 16 --ioapic on
& $VB storagectl Flux --name VIRTIO --add virtio-scsi --controller VirtIO --portcount 4 --bootable on
& $VB storageattach Flux --storagectl VIRTIO --port 0 --device 0 --type hdd --medium "$env:USERPROFILE\.flux\cache\debian-12.vdi"
& $VB modifymedium disk "$env:USERPROFILE\.flux\cache\debian-12.vdi" --resize 81920
& $VB storageattach Flux --storagectl VIRTIO --port 1 --device 0 --type dvddrive --medium "$env:USERPROFILE\.flux\seed\seed.iso"
& $VB modifyvm Flux --nic1 nat --nic-type1 virtio
& $VB modifyvm Flux --nat-pf1 "ssh,tcp,,2222,,22"
& $VB modifyvm Flux --nat-pf1 "healthz,tcp,,7420,,7420"
& $VB startvm Flux --type headless

# 5. Console serial (debug):
& $VB modifyvm Flux --uart1 0x3F8 4 --uart-mode1 file "C:\Users\ieubi\.flux\vms\serial.log"
```

## Lições registradas (o que NUNCA repetir)

1. **Controlador tem que ser VirtIO SCSI** — `debian-12-genericcloud` não tem
   driver AHCI/IDE; em SATA o kernel não acha a raiz
   (`PARTUUID=... does not exist`, initramfs cai no shell).
2. **`--nat-pf1` no VBox 7.2** (não `--natpf1`; `--nat-dnsproxy1` não existe).
3. `qemu-img resize` não suporta VDI — usar `VBoxManage modifymedium --resize`.
4. Hypervisor do Windows ligado (HVCI/Memória de Integridade) = VBox em modo
   NEM Snail: boot de 10+ min, watchdog de clock no serial, VM morre sozinha.
   Toggle + reboot obrigatórios antes de provisionar de novo.

## Pendente (após o reboot do host)

1. Boot da VM + validar SSH (`ssh -i %USERPROFILE%\.flux\id_ed25519 -p 2222 flux@127.0.0.1`),
   `docker version`, `caddy version`, marker `/var/log/flux-provision-done`.
2. Compilar `fluxd` Linux no WSL (`cargo build --release -p fluxd`), scp para a
   VM, unit `fluxd.service`, validar `curl 127.0.0.1:7420/healthz` (pf `healthz`).
3. Rede host-only: `VBoxManage hostonlyif create` + `ipconfig 192.168.140.1`,
   NIC 2 host-only, netplan estático `192.168.140.2/24` na VM, SSH direto.
4. Ajustar o seed para incluir o netplan desde o primeiro boot (aí o passo 3
   some de provisionamentos futuros).
