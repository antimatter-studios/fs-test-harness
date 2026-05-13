# setup-windows-vm.ps1 -- one-time provisioning of a Windows VM as a
# fs-test-harness target.
#
# Generic: installs only the cross-consumer essentials. Consumers
# declare any extra winget packages in `harness.toml [vm.packages]`
# and pass them in either via:
#
#   -ExtraPackages @("LLVM.LLVM",...)               bare-ID legacy
#   -PackagesJson  '[...harness.toml [vm.packages] verbatim JSON...]'
#                                                   structured form
#
# The JSON form supports the v3.5.0+ object-shape entries, which
# pass `--override "<custom_args>"` through to winget. Required for
# packages whose default feature set is wrong for the consumer's
# build (e.g. WinFsp.WinFsp's default is runtime-only — no headers
# or .lib — so consumers building bindgen against winfsp.h must opt
# into ADDLOCAL=F.Core,F.Developer via custom_args).
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
#   -ExtraPackages @("LLVM.LLVM",...)
#       Each is passed verbatim to `winget install --id`. Bare-string
#       only — use -PackagesJson for entries needing custom_args.
#   -PackagesJson '[<spec>, ...]'
#       JSON-array serialised from harness.toml [vm.packages]. Each
#       entry is either a bare string OR an object
#       {"id": "...", "custom_args": "..."} (the latter triggers
#       `winget install --override "<custom_args>"`). Empty default.
#       When supplied, ExtraPackages and PackagesJson are merged
#       (PackagesJson entries win on duplicate IDs).
#
# Idempotent: every step checks before installing, so re-running is safe.
#
# Usage (on the VM directly):
#   powershell -ExecutionPolicy Bypass -File setup-windows-vm.ps1 `
#       -RustToolchain "stable-aarch64-pc-windows-gnullvm" `
#       -PackagesJson '[{"id":"WinFsp.WinFsp","custom_args":"ADDLOCAL=F.Core,F.Developer"},"LLVM.LLVM"]'
#
# Or invoked over SSH from the Mac side:
#   ssh $VM_HOST 'powershell -ExecutionPolicy Bypass -File <path>\setup-windows-vm.ps1 ...'

param(
    [string]$Workdir = "$env:USERPROFILE\dev",
    [string]$RustToolchain = "",
    [string[]]$ExtraPackages = @(),
    [string]$PackagesJson = ""
)

$ErrorActionPreference = "Continue"  # winget writes progress to stderr

function Test-WingetPackage {
    param([string]$Id)
    $listing = winget list --id $Id --exact 2>&1 | Out-String
    return $listing -match [regex]::Escape($Id)
}

function Install-IfMissing {
    # -CustomArgs (optional): forwarded to `winget install --override
    # "<custom_args>"`. Use for packages whose default feature set is
    # wrong (e.g. WinFsp.WinFsp + ADDLOCAL=F.Core,F.Developer).
    #
    # Note re-installs: if the package is already installed but
    # WITHOUT the requested feature set, winget on its own won't
    # change the feature selection. We log a hint in that case so
    # the operator can manually `msiexec /fa <product-code> ADDLOCAL=...`
    # or uninstall + reinstall. Self-repair is left to the operator
    # because winget's behaviour around ADDLOCAL re-runs is
    # version-dependent and we don't want a surprise wipe.
    param(
        [string]$Id,
        [string]$Description,
        [string]$CustomArgs = ""
    )
    Write-Host "[setup] $Description ($Id)"
    if (Test-WingetPackage -Id $Id) {
        if ($CustomArgs) {
            Write-Host "        already installed -- skipping (NOTE: $Id needs custom_args='$CustomArgs')"
            Write-Host "        if a feature is missing, msiexec /fa <product-code> $CustomArgs to repair-install."
        } else {
            Write-Host "        already installed -- skipping"
        }
        return
    }
    $args = @($Id, "--accept-source-agreements", "--accept-package-agreements", "--silent")
    if ($CustomArgs) {
        $args += @("--override", $CustomArgs)
    }
    winget install @args 2>&1 |
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
# Resolve final spec list: ExtraPackages (bare-string legacy) + any
# entries from PackagesJson. JSON entries win on duplicate IDs so a
# consumer can override a bare entry with one carrying custom_args.
$resolvedSpecs = @()
foreach ($pkg in $ExtraPackages) {
    if (-not $pkg) { continue }
    $resolvedSpecs += [pscustomobject]@{ Id = $pkg; CustomArgs = "" }
}
if ($PackagesJson -and $PackagesJson.Trim() -ne "") {
    try {
        $jsonEntries = $PackagesJson | ConvertFrom-Json
    } catch {
        throw "PackagesJson failed to parse as JSON: $_"
    }
    foreach ($entry in $jsonEntries) {
        if ($entry -is [string]) {
            $id = $entry
            $args = ""
        } elseif ($entry -is [psobject] -or $entry -is [hashtable]) {
            $id = "$($entry.id)"
            $args = if ($entry.custom_args) { "$($entry.custom_args)" } else { "" }
        } else {
            Write-Host "WARN: PackagesJson entry of unsupported shape (got $($entry.GetType().Name)) -- skipping"
            continue
        }
        if (-not $id) {
            Write-Host "WARN: PackagesJson entry missing 'id' -- skipping"
            continue
        }
        # JSON entry overrides any bare-string ExtraPackages with same Id.
        $resolvedSpecs = @($resolvedSpecs | Where-Object { $_.Id -ne $id })
        $resolvedSpecs += [pscustomobject]@{ Id = $id; CustomArgs = $args }
    }
}

foreach ($spec in $resolvedSpecs) {
    Install-IfMissing -Id $spec.Id -Description "consumer-declared package" -CustomArgs $spec.CustomArgs
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
foreach ($spec in $resolvedSpecs) {
    if (Test-WingetPackage -Id $spec.Id) {
        $note = if ($spec.CustomArgs) { " (custom_args=$($spec.CustomArgs))" } else { "" }
        Write-Host "$($spec.Id): installed$note"
    } else {
        Write-Host "WARN: $($spec.Id): missing"
    }
}

Write-Host ""
Write-Host "=== Setup complete ==="
Write-Host "Run <harness>/scripts/run-tests.sh from the Mac side."
