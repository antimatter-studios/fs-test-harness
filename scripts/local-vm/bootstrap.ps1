$ErrorActionPreference = 'Stop'
$logDir = 'C:\fswth-bootstrap'
New-Item -ItemType Directory -Path $logDir -Force | Out-Null
Start-Transcript -Path "$logDir\provision.log" -Append
try {
    $seed = $PSScriptRoot
    Copy-Item "$seed\winfsp-2.1.25156.msi" "$logDir\winfsp-2.1.25156.msi" -Force
    & pnputil.exe /add-driver "$seed\drivers\netkvm.inf" /install
    if ($LASTEXITCODE -notin 0, 3010) { throw "Network driver install failed: $LASTEXITCODE" }

    $sshRoot = 'C:\Program Files\OpenSSH'
    Expand-Archive -LiteralPath "$seed\OpenSSH-ARM64.zip" -DestinationPath $logDir -Force
    New-Item -ItemType Directory -Path $sshRoot -Force | Out-Null
    Copy-Item "$logDir\OpenSSH-ARM64\*" $sshRoot -Recurse -Force
    & powershell.exe -NoProfile -ExecutionPolicy Bypass -File "$sshRoot\install-sshd.ps1"
    if ($LASTEXITCODE -ne 0) { throw "OpenSSH service install failed: $LASTEXITCODE" }
    $configDir = 'C:\ProgramData\ssh'
    New-Item -ItemType Directory -Path $configDir -Force | Out-Null
    Copy-Item "$seed\authorized_keys" "$configDir\administrators_authorized_keys" -Force
    & icacls.exe "$configDir\administrators_authorized_keys" /inheritance:r /grant:r '*S-1-5-32-544:F' '*S-1-5-18:F'
    if ($LASTEXITCODE -ne 0) { throw 'Could not set SSH key ACL' }
    @'
Port 22
PubkeyAuthentication yes
PasswordAuthentication no
KbdInteractiveAuthentication no
AllowUsers fswth
Subsystem sftp sftp-server.exe
Match Group administrators
    AuthorizedKeysFile __PROGRAMDATA__/ssh/administrators_authorized_keys
'@ | Set-Content -Encoding ascii "$configDir\sshd_config"
    New-ItemProperty -Path HKLM:\SOFTWARE\OpenSSH -Name DefaultShell -Value "$env:SystemRoot\System32\WindowsPowerShell\v1.0\powershell.exe" -PropertyType String -Force | Out-Null
    if (-not (Get-NetFirewallRule -Name FSWTH-SSH -ErrorAction SilentlyContinue)) {
        New-NetFirewallRule -Name FSWTH-SSH -DisplayName 'FSWTH SSH' -Direction Inbound -Protocol TCP -LocalPort 22 -Action Allow | Out-Null
    }
    Set-Service sshd -StartupType Automatic
    Start-Service sshd

    # Windows Installer stalled in specialize on the first experiment.
    # Finish package installation over SSH after Windows setup completes.
    'Network and SSH configured; WinFsp installation pending' | Set-Content "$logDir\network-ready.txt"
} catch {
    $_ | Out-String | Set-Content "$logDir\failed.txt"
    throw
} finally {
    Stop-Transcript
}
