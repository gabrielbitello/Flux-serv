# Instala o cloudflared como SERVIÇO WINDOWS do host (spec 03/12) usando o
# token do tunnel. O porteiro central (fluxhost :8443, F13) entra NA FRENTE
# dele depois; até lá o ingress vai direto ao Caddy da VM via dashboard.
#
# Pré-requisitos (uma vez, fora daqui — envolvem conta do Gabriel):
#   1. Zero Trust Dashboard → Networks → Tunnels → Create tunnel (cloudflared).
#   2. Copiar o TOKEN do tunnel para %USERPROFILE%\.flux\tunnel.token
#      (só o valor eyJ...; o arquivo é gitignored e fica fora do repo).
#   3. No mesmo tunnel, Public Hostname: *.bitello.xyz → Service
#      HTTPS://192.168.140.2:443 com "No TLS Verify" ON (o Caddy usa
#      cert interno — a edge cifra o caminho público).
#   4. Rodar ESTE script num PowerShell ADMIN (o serviço precisa de SYSTEM).
#
# Custos: ~30 MB RAM permanentes. Custo zero em dinheiro (plano free).

$ErrorActionPreference = "Stop"

$tokenFile = Join-Path $env:USERPROFILE ".flux\tunnel.token"
$bin = Join-Path $env:USERPROFILE ".flux\bin\cloudflared.exe"

if (-not (Test-Path $bin)) {
    throw "cloudflared ausente: $bin (baixe cloudflared-windows-amd64.exe da release oficial)"
}
if (-not (Test-Path $tokenFile)) {
    throw "token ausente: $tokenFile — cole o token do tunnel nesse arquivo (só o valor eyJ...)"
}
$token = (Get-Content $tokenFile -Raw).Trim()
if (-not $token -or $token -notmatch "^eyJ") {
    throw "token inválido em $tokenFile — esperado o valor eyJ... do dashboard do tunnel"
}

# Serviço idempotente: se já existe, remove antes (troca de token).
$existe = Get-Service -Name cloudflared -ErrorAction SilentlyContinue
if ($existe) {
    Write-Host "serviço cloudflared já existe — removendo para reinstalar com o token atual"
    & $bin service uninstall
    Start-Sleep -Seconds 2
}

& $bin service install $token
Start-Sleep -Seconds 2
Start-Service cloudflared
Start-Sleep -Seconds 3

$serv = Get-Service -Name cloudflared
Write-Host ""
Write-Host "cloudflared: $($serv.Status)"
Write-Host "logs: Get-WinEvent -LogName Application -ProviderName cloudflared -MaxEvents 20"
Write-Host "conferência: https://zzz.bitello.xyz deve mostrar a página 'host desconhecido' do Flux"
