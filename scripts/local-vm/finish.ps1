param([string]$Msi = 'C:\fswth-bootstrap\winfsp-2.1.25156.msi')
$ErrorActionPreference = 'Stop'

function Set-LocalTestExecutionPolicy {
    # SSH bootstraps this script with Process=Bypass. Align that scope first:
    # otherwise Set-ExecutionPolicy writes CurrentUser but throws
    # ExecutionPolicyOverride, aborting provisioning with ErrorAction=Stop.
    Set-ExecutionPolicy -Scope Process -ExecutionPolicy RemoteSigned -Force
    Set-ExecutionPolicy -Scope CurrentUser -ExecutionPolicy RemoteSigned -Force
}

$logDir = 'C:\fswth-bootstrap'
Start-Transcript -Path "$logDir\finish-provision.log" -Append
try {
    if ((Get-ItemProperty 'HKLM:\SYSTEM\Setup').SystemSetupInProgress -ne 0) {
        throw 'Windows setup must finish before installing WinFsp'
    }
    # Run over SSH as the dedicated test account: the Rust runner invokes
    # its copied lease script directly in the account's default shell.
    Set-LocalTestExecutionPolicy
    $licenseFilter = "ApplicationID='55c92734-d682-4d71-983e-d6ec3f16059f' AND PartialProductKey IS NOT NULL"
    $license = Get-CimInstance SoftwareLicensingProduct -Filter $licenseFilter
    if ($license.LicenseStatus -ne 1) {
        # Ordinary activation of the newly installed evaluation, not rearm
        # or a mechanism for renewing an expired evaluation.
        & cscript.exe //nologo "$env:SystemRoot\System32\slmgr.vbs" /ato
        $license = Get-CimInstance SoftwareLicensingProduct -Filter $licenseFilter
        if ($license.LicenseStatus -ne 1) { throw 'Windows evaluation activation did not succeed' }
    }
    $signature = Get-AuthenticodeSignature -LiteralPath $Msi
    if ($signature.Status -ne 'Valid') { throw "WinFsp signature: $($signature.Status)" }
    $install = Start-Process msiexec.exe -ArgumentList "/i `"$Msi`" /qn /norestart ADDLOCAL=ALL /l*v $logDir\winfsp-after-setup.log" -PassThru
    if (-not $install.WaitForExit(180000)) { throw "WinFsp installer did not finish within 180 seconds; PID $($install.Id)" }
    if ($install.ExitCode -notin 0,3010) { throw "WinFsp install failed: $($install.ExitCode)" }
    $memfs = 'C:\Program Files (x86)\WinFsp\bin\memfs-a64.exe'
    Get-Item -LiteralPath $memfs | Select-Object FullName,Length
    & powercfg.exe /change standby-timeout-ac 0
    if ($LASTEXITCODE -ne 0) { throw 'Could not disable AC idle sleep for the test VM' }
    'complete' | Set-Content "$logDir\complete.txt"
    & cscript.exe //nologo "$env:SystemRoot\System32\slmgr.vbs" /xpr
} catch {
    $_ | Out-String | Set-Content "$logDir\finish-failed.txt"
    throw
} finally {
    Stop-Transcript
}
