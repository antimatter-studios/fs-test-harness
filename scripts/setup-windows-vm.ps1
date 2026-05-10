# setup-windows-vm.ps1 -- provision a Windows VM as a fs-test-harness target.
#
# Two modes:
#   * default   — install (force-overwrite). Works on a fresh VM. May
#                 fail to add new features to an existing partial
#                 install (winget reconfigure ≠ winget reinstall).
#   * -Reinstall — UNINSTALL then install. Works on any starting state.
#                 The "nuclear button" — drives the VM to the declared
#                 end-state regardless of what's already there.
#                 Uninstall errors are tolerated (already-absent
#                 packages are fine).
#
# Args:
#   -Workdir          Directory under which run-tests.sh tars the consumer
#                     source. Default: $env:USERPROFILE\dev.
#   -RustToolchain    rustup default toolchain to set
#                     (e.g. "stable-aarch64-pc-windows-gnullvm").
#                     Default: leave rustup at its current default.
#   -ExtraPackages    Array of winget package entries from harness.toml
#                     [vm.packages]. Each entry is either a bare PkgId
#                     string ("WinFsp.WinFsp") or a hashtable
#                     @{ id = "PkgId"; custom_args = "..." }. The
#                     custom_args string is forwarded to the underlying
#                     MSI/EXE installer via winget's --override flag —
#                     use it for non-default features:
#
#                       @{ id = "WinFsp.WinFsp"
#                          custom_args = "ADDLOCAL=F.Main,F.User,F.Developer" }
#
#                     (WinFsp's `F.Developer` feature ships the headers
#                     + .lib bindgen needs but is off by default.)
#   -Reinstall        Uninstall every package before reinstalling. Use
#                     when the VM has a known-bad / partial install
#                     state and you want to start over.
#
# Usage (on the VM directly, fresh install):
#   powershell -ExecutionPolicy Bypass -File setup-windows-vm.ps1 `
#       -RustToolchain "stable-aarch64-pc-windows-gnullvm" `
#       -ExtraPackages @(
#           "MartinStorsjo.LLVM-MinGW.UCRT",
#           @{ id = "WinFsp.WinFsp"; custom_args = "ADDLOCAL=F.Main,F.User,F.Developer" }
#       )
#
# Usage (driven from run-tests.sh --reinstall on the orchestrator):
#   run-tests.sh scp's this script to the VM and invokes it over SSH
#   with -Reinstall + the [vm.packages] / [vm.rust_toolchain] /
#   [vm.workdir] from harness.toml.

param(
    [string]$Workdir = "$env:USERPROFILE\dev",
    [string]$RustToolchain = "",
    # Bare strings (PkgId) OR hashtables @{ id="PkgId"; custom_args="..." }.
    [object[]]$ExtraPackages = @(),
    [switch]$Reinstall
)

$ErrorActionPreference = "Continue"  # winget writes progress to stderr

function Uninstall-Package {
    # Best-effort uninstall. Ignores errors (already-absent packages).
    param([string]$Id)
    Write-Host "[setup] uninstall $Id"
    winget uninstall $Id --silent --accept-source-agreements 2>&1 |
        Select-Object -Last 3 | ForEach-Object { Write-Host "        $_" }
}

function Install-Package {
    # Force-install. No "already installed" check.
    param(
        [string]$Id,
        [string]$CustomArgs = ""
    )
    Write-Host "[setup] install $Id"
    $wargs = @(
        $Id,
        "--accept-source-agreements",
        "--accept-package-agreements",
        "--silent",
        "--force"
    )
    if ($CustomArgs) {
        $wargs += @("--override", $CustomArgs)
    }
    winget install @wargs 2>&1 |
        Select-Object -Last 3 | ForEach-Object { Write-Host "        $_" }
}

function Resolve-PackageEntry {
    # Normalise a [vm.packages] entry into @{ Id; CustomArgs }.
    param([object]$Entry)
    if (-not $Entry) { return $null }
    if ($Entry -is [string]) {
        return @{ Id = $Entry; CustomArgs = "" }
    }
    if ($Entry -is [hashtable] -or $Entry -is [PSCustomObject]) {
        $id = $Entry.id
        if (-not $id) { $id = $Entry.Id }
        if (-not $id) {
            Write-Warning "[setup] package entry has no 'id' field; skipping"
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

if ($Reinstall) {
    Write-Host "=== REINSTALL mode: uninstall-then-install for every package ==="
} else {
    Write-Host "=== Install mode: force-install only (use -Reinstall to nuke first) ==="
}

# ---------- 1. Rust ------------------------------------------------------
if ($Reinstall) { Uninstall-Package -Id "Rustlang.Rustup" }
Install-Package -Id "Rustlang.Rustup"

$cargoBin = "$env:USERPROFILE\.cargo\bin"
$env:PATH = "$cargoBin;$env:PATH"

if (-not (Get-Command rustup -ErrorAction SilentlyContinue)) {
    throw "rustup not on PATH after install -- shell restart may be needed"
}

if ($RustToolchain) {
    Write-Host "[setup] rustup default $RustToolchain"
    rustup default $RustToolchain 2>&1 |
        Select-Object -Last 3 | ForEach-Object { Write-Host "        $_" }
}

# ---------- 2. Consumer packages -----------------------------------------
foreach ($entry in $ExtraPackages) {
    $resolved = Resolve-PackageEntry -Entry $entry
    if (-not $resolved) { continue }
    if ($Reinstall) { Uninstall-Package -Id $resolved.Id }
    Install-Package -Id $resolved.Id -CustomArgs $resolved.CustomArgs
}

# ---------- 3. Workdir ---------------------------------------------------
$workdirPath = $Workdir.TrimEnd('\','/')
if (-not (Test-Path $workdirPath)) {
    New-Item -ItemType Directory -Path $workdirPath -Force | Out-Null
}
Write-Host "[setup] workdir: $workdirPath"

Write-Host ""
Write-Host "=== Setup complete ==="
