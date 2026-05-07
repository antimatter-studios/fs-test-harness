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
#       Each is passed verbatim to `winget install --id`.
#
# Idempotent: every step checks before installing, so re-running is safe.
#
# Usage (on the VM directly):
#   powershell -ExecutionPolicy Bypass -File setup-windows-vm.ps1 `
#       -RustToolchain "stable-aarch64-pc-windows-gnullvm" `
#       -ExtraPackages @("MartinStorsjo.LLVM-MinGW.UCRT","cloudbase.qemu-img")
#
# Or invoked over SSH from the Mac side:
#   ssh $VM_HOST 'powershell -ExecutionPolicy Bypass -File <path>\setup-windows-vm.ps1 ...'

param(
    [string]$Workdir = "$env:USERPROFILE\dev",
    [string]$RustToolchain = "",
    [string[]]$ExtraPackages = @()
)

$ErrorActionPreference = "Continue"  # winget writes progress to stderr

function Test-WingetPackage {
    param([string]$Id)
    $listing = winget list --id $Id --exact 2>&1 | Out-String
    return $listing -match [regex]::Escape($Id)
}

function Install-IfMissing {
    param([string]$Id, [string]$Description)
    Write-Host "[setup] $Description ($Id)"
    if (Test-WingetPackage -Id $Id) {
        Write-Host "        already installed -- skipping"
        return
    }
    winget install $Id --accept-source-agreements --accept-package-agreements --silent 2>&1 |
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
foreach ($pkg in $ExtraPackages) {
    if (-not $pkg) { continue }
    Install-IfMissing -Id $pkg -Description "consumer-declared package"
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
foreach ($pkg in $ExtraPackages) {
    if (Test-WingetPackage -Id $pkg) {
        Write-Host "$pkg: installed"
    } else {
        Write-Host "WARN: $pkg: missing"
    }
}

Write-Host ""
Write-Host "=== Setup complete ==="
Write-Host "Run <harness>/scripts/test-windows-matrix.sh from the Mac side."
