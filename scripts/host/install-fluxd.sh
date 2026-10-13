#!/usr/bin/env bash
# Instala o fluxd na VM (roda DENTRO da VM, como root).
# Uso: scp do binário para /tmp/fluxd e depois: sudo bash /tmp/install-fluxd.sh
# Referência: spec 01 (passo 7) e spec 09 (menor privilégio).
set -euo pipefail

[ "$(id -u)" -eq 0 ] || { echo "erro: roda como root"; exit 1; }
[ -f /tmp/fluxd ] || { echo "erro: /tmp/fluxd não existe (scp do binário Linux antes)"; exit 1; }

install -m 0755 /tmp/fluxd /usr/local/bin/fluxd

mkdir -p /srv/flux/state /srv/flux/projects /srv/flux/secrets /srv/flux/logs /etc/fluxd
chown -R flux:flux /srv/flux
chmod 700 /srv/flux/secrets

cat > /etc/systemd/system/fluxd.service <<'UNIT'
[Unit]
Description=Flux daemon (router :8080 + API de controle :7420)
After=docker.service network-online.target
Requires=docker.service

[Service]
ExecStart=/usr/local/bin/fluxd serve --db /srv/flux/state/flux.sqlite3 --config /etc/fluxd/config.toml
Restart=always
RestartSec=2
User=flux
Group=flux
SupplementaryGroups=docker

[Install]
WantedBy=multi-user.target
UNIT

systemctl daemon-reload
systemctl enable --now fluxd
systemctl --no-pager status fluxd | head -5
echo "ok: fluxd instalado e ativo — healthz: curl -s 127.0.0.1:7420/healthz"
