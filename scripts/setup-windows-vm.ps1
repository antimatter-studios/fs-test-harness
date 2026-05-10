# setup-windows-vm.ps1 -- provision a Windows VM as a fs-test-harness target.
#
# Two modes:
#   * default   -- install (force-overwrite). Works on a fresh VM. May
#                 fail to add new features to an existing partial
#                 install (winget reconfigure != winget reinstall).
#   * -Reinstall -- UNINSTALL then install. Works on any starting state.
#                 The "nuclear button" -- drives the VM to the declared
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
#                     MSI/EXE installer via winget's --override flag --
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
    param([string]$Id)
    Write-Host "[setup] uninstall $Id"
    & winget uninstall --id $Id --exact --silent --verbose 2>&1 |
        ForEach-Object { Write-Host "        $_" }
    Write-Host "        exit=$LASTEXITCODE"
}

function Install-Package {
    # --verbose so a long download (LLVM ~500MB, WinFsp ~3MB) shows
    # live progress instead of looking indistinguishable from a hang.
    param(
        [string]$Id,
        [string]$CustomArgs = ""
    )
    Write-Host "[setup] install $Id"
    $wargs = @(
        "install",
        "--id", $Id,
        "--exact",
        "--accept-source-agreements",
        "--accept-package-agreements",
        "--silent",
        "--force",
        "--verbose"
    )
    if ($CustomArgs) {
        $wargs += @("--override", $CustomArgs)
    }
    & winget @wargs 2>&1 |
        ForEach-Object { Write-Host "        $_" }
    Write-Host "        exit=$LASTEXITCODE"
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

# ---------- 0. Clean up any hung winget state ----------------------------
# An ssh-launched winget can deadlock during init (likely on a COM
# mutex) and never write a log line or return. The next winget call
# queues behind the zombie indefinitely, so retries pile up rather
# than recovering. Also: cached installer files in WinGet's Temp dir
# can stay file-locked by zombie processes ("file in use" errors on
# the next install).
#
# Clear the slate by killing all winget + companion processes and
# wiping the WinGet Temp cache before we start.
$kill = Get-Process -Name "winget","AppInstaller*","WinGetServer*","DesktopAppInstaller*" -ErrorAction SilentlyContinue
if ($kill) {
    Write-Host "[setup] killing $($kill.Count) hung winget/AppInstaller process(es) from previous runs"
    $kill | Stop-Process -Force -ErrorAction SilentlyContinue
    Start-Sleep -Seconds 2
}
$wingetTemp = Join-Path $env:LOCALAPPDATA "Temp\WinGet"
if (Test-Path $wingetTemp) {
    Write-Host "[setup] wiping WinGet Temp cache at $wingetTemp"
    Remove-Item $wingetTemp -Recurse -Force -ErrorAction SilentlyContinue
}

# ---------- 1. Rust ------------------------------------------------------
# Bypass winget for rustup -- rustup-init.exe is a custom installer
# that hangs over SSH under winget's invocation chain even with
# --override "-y" + --silent + --accept-*-agreements. Direct-download
# from rustup's CDN and run rustup-init.exe with -y instead.
# If rustup is already on PATH, skip -- its self-update path
# (`rustup self update`) is for the user to invoke when they want.
# Same answer regardless of -Reinstall: rustup is self-managing.
$cargoBin = "$env:USERPROFILE\.cargo\bin"
$env:PATH = "$cargoBin;$env:PATH"

if (-not (Get-Command rustup -ErrorAction SilentlyContinue)) {
    Write-Host "[setup] install rustup (direct download -- winget hangs on rustup-init)"
    $arch = if ($env:PROCESSOR_ARCHITECTURE -eq "ARM64") { "aarch64" } else { "x86_64" }
    $url  = "https://win.rustup.rs/$arch"
    $tmp  = Join-Path $env:TEMP "rustup-init.exe"
    Invoke-WebRequest -Uri $url -OutFile $tmp -UseBasicParsing
    & $tmp -y --default-toolchain none --no-modify-path 2>&1 |
        Select-Object -Last 5 | ForEach-Object { Write-Host "        $_" }
    Remove-Item $tmp -ErrorAction SilentlyContinue
} else {
    Write-Host "[setup] rustup already on PATH -- skipping install"
}

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
