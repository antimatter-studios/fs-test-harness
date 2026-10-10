# Installs virtio-win's ARM64 virtio-fs driver and its WinFsp-based service so
# a host directory shared with `local-vm.py up --share` appears as Z:.
# Idempotent: run again after a virtio-win update.
$ErrorActionPreference = 'Stop'
$dir = $PSScriptRoot
& pnputil.exe /add-driver "$dir\viofs.inf" /install
if ($LASTEXITCODE -notin 0, 3010) { throw "virtio-fs driver install failed: $LASTEXITCODE" }
# virtiofs.exe reads its mount point here; the harness expects Z:.
New-Item -Path 'HKLM:\SOFTWARE\VirtIO-FS' -Force | Out-Null
New-ItemProperty -Path 'HKLM:\SOFTWARE\VirtIO-FS' -Name MountPoint -Value 'Z:' -PropertyType String -Force | Out-Null
if (-not (Get-Service VirtioFsSvc -ErrorAction SilentlyContinue)) {
    # Starts at boot when the share's device is present; without one the
    # driver is absent and the service fails to start, which is harmless.
    & sc.exe create VirtioFsSvc binpath= "$dir\virtiofs.exe" start= auto depend= 'WinFsp.Launcher/VirtioFsDrv' DisplayName= 'Virtio FS Service'
    if ($LASTEXITCODE -ne 0) { throw "VirtioFsSvc could not be created: $LASTEXITCODE" }
}
Restart-Service VirtioFsSvc
$deadline = (Get-Date).AddSeconds(30)
while (-not (Test-Path 'Z:\')) {
    if ((Get-Date) -gt $deadline) { throw 'The share did not appear as Z: within 30 seconds' }
    Start-Sleep -Milliseconds 500
}
'virtio-fs share mounted at Z:'
