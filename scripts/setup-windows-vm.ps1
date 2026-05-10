# setup-windows-vm.ps1 -- one-time provisioning of a Windows VM as a
# fs-test-harness target.
#
# Generic: installs only the cross-consumer essentials. Consumers
# declare any extra winget package IDs in `harness.toml [vm.packages]`
# and pass them in via the -ExtraPackages parameter.
#
# Always installed:
#   - Rustlang.Rustup           Rust installer (consumers' run-matrix
#                               binary builds on the VM).
#
# Consumer-supplied (per harness.toml):
#   -RustToolchain "<channel-target-triple>"
#       e.g. "stable-aarch64-pc-windows-gnullvm" (gnullvm-target NTFS
#       project) or "stable-x86_64-pc-windows-msvc". Default = leave
#       rustup at its current default.
#   -ExtraPackages @("WinFsp.WinFsp","LLVM.LLVM",...)
#       Each entry is either a bare string (PkgId — installed with
#       default features) or a hashtable @{ id="PkgId"; custom_args="..." }
#       where custom_args is forwarded to the installer via winget's
#       --override flag. Use the object form for packages whose MSI/EXE
#       has non-default feature flags you need — e.g.
#       @{ id="WinFsp.WinFsp"; custom_args="ADDLOCAL=F.Core,F.Developer" }
#       to pull in headers + .lib alongside the runtime.
#
# Idempotent: every step checks before installing, so re-running is safe.
# If a package is already installed but its desired feature set differs
# from what's on disk (e.g. WinFsp runtime-only when you wanted Developer
# too), passing custom_args triggers `winget install --force` so the
# installer re-runs and adds the missing features.
#
# Usage (on the VM directly):
#   powershell -ExecutionPolicy Bypass -File setup-windows-vm.ps1 `
#       -RustToolchain "stable-aarch64-pc-windows-gnullvm" `
#       -ExtraPackages @(
#           "MartinStorsjo.LLVM-MinGW.UCRT",
#           "cloudbase.qemu-img",
#           @{ id = "WinFsp.WinFsp"; custom_args = "ADDLOCAL=F.Core,F.Developer" }
#       )
#
# Or invoked over SSH from the Mac side:
#   ssh $VM_HOST 'powershell -ExecutionPolicy Bypass -File <path>\setup-windows-vm.ps1 ...'

param(
    [string]$Workdir = "$env:USERPROFILE\dev",
    [string]$RustToolchain = "",
    # Bare strings (PkgId) OR hashtables @{ id="PkgId"; custom_args="..." }.
    # Type is widened from [string[]] so PowerShell accepts the mixed form.
    [object[]]$ExtraPackages = @()
)

$ErrorActionPreference = "Continue"  # winget writes progress to stderr

function Test-WingetPackage {
    param([string]$Id)
    $listing = winget list --id $Id --exact 2>&1 | Out-String
    return $listing -match [regex]::Escape($Id)
}

function Install-IfMissing {
    param(
        [string]$Id,
        [string]$Description,
        [string]$CustomArgs = ""
    )
    Write-Host "[setup] $Description ($Id)"
    $alreadyInstalled = Test-WingetPackage -Id $Id
    if ($alreadyInstalled -and -not $CustomArgs) {
        Write-Host "        already installed -- skipping"
        return
    }

    # Build the winget argv. --override forwards CustomArgs to the
    # underlying MSI/EXE installer (e.g. ADDLOCAL=F.Core,F.Developer
    # for WinFsp). --force is needed when the package is already
    # installed but with a different feature set, because winget
    # otherwise short-circuits to "already installed".
    $wargs = @($Id, "--accept-source-agreements", "--accept-package-agreements", "--silent")
    if ($CustomArgs) {
        $wargs += @("--override", $CustomArgs)
        if ($alreadyInstalled) {
            Write-Host "        already installed -- re-running with --force to apply custom_args"
            $wargs += "--force"
        }
    }
    winget install @wargs 2>&1 |
        Select-Object -Last 3 | ForEach-Object { Write-Host "        $_" }
}

# ---------- 1. Rust ------------------------------------------------------
Install-IfMissing -Id "Rustlang.Rustup" -Description "Rustup (Rust installer)"

$cargoBin = "$env:USERPROFILE\.cargo\bin"
$env:PATH = "$cargoBin;$env:PATH"

if (-not (Get-Command rustup -ErrorAction SilentlyContinue)) {
    throw "rustup not on PATH after install -- shell restart may be needed"
}

if ($RustToolchain) {
    Write-Host "[setup] rustup default toolchain = $RustToolchain"
    $current = (rustup show active-toolchain 2>&1) -replace '\s.*',''
    if ($current -ne $RustToolchain) {
        rustup default $RustToolchain 2>&1 |
            Select-Object -Last 3 | ForEach-Object { Write-Host "        $_" }
    } else {
        Write-Host "        already set"
    }
}

# ---------- 2. Consumer extras -------------------------------------------
function Resolve-PackageEntry {
    # Normalise a [vm.packages] entry into @{ Id; CustomArgs }.
    # Accepts bare string OR hashtable / PSCustomObject with `id` +
    # optional `custom_args` fields. Returns $null on empty entries
    # so the caller can `continue`.
    param([object]$Entry)
    if (-not $Entry) { return $null }
    if ($Entry -is [string]) {
        return @{ Id = $Entry; CustomArgs = "" }
    }
    if ($Entry -is [hashtable] -or $Entry -is [PSCustomObject]) {
        $id = $Entry.id
        if (-not $id) { $id = $Entry.Id }
        if (-not $id) {
            Write-Warning "[setup] package entry has no 'id' field; skipping: $($Entry | ConvertTo-Json -Compress)"
            return $null
        }
        $args_ = ""
        if ($Entry.custom_args) { $args_ = [string]$Entry.custom_args }
        elseif ($Entry.CustomArgs) { $args_ = [string]$Entry.CustomArgs }
        return @{ Id = $id; CustomArgs = $args_ }
    }
    Write-Warning "[setup] unsupported package entry type $($Entry.GetType().Name); skipping"
    return $null
}

foreach ($entry in $ExtraPackages) {
    $resolved = Resolve-PackageEntry -Entry $entry
    if (-not $resolved) { continue }
    Install-IfMissing -Id $resolved.Id -Description "consumer-declared package" -CustomArgs $resolved.CustomArgs
}

# ---------- 3. Workdir ---------------------------------------------------
$workdirPath = $Workdir.TrimEnd('\','/')
if (-not (Test-Path $workdirPath)) {
    New-Item -ItemType Directory -Path $workdirPath -Force | Out-Null
}
Write-Host "[setup] workdir: $workdirPath"

# ---------- 4. Verify ---------------------------------------------------
Write-Host ""
Write-Host "=== Verification ==="
& rustc --version 2>&1 | Select-Object -First 1
& cargo --version 2>&1 | Select-Object -First 1
foreach ($entry in $ExtraPackages) {
    $resolved = Resolve-PackageEntry -Entry $entry
    if (-not $resolved) { continue }
    if (Test-WingetPackage -Id $resolved.Id) {
        Write-Host "$($resolved.Id): installed"
    } else {
        Write-Host "WARN: $($resolved.Id): missing"
    }
}

Write-Host ""
Write-Host "=== Setup complete ==="
Write-Host "Run <harness>/scripts/run-tests.sh from the Mac side."
