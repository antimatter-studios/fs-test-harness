# scripts/win/win-ls-via-mount.ps1 -- generic v2 op: list a directory via the
# mount, assert against expected names + count.
# Self-contained mount-do-unmount per call; see _lib.ps1 Invoke-WithMount.
#
# -ExpectNames is comma-separated. Get-ChildItem doesn't return "." / ".."
# on Windows so they're synthesised here to match the ext4/POSIX-readdir
# convention v1 expectations were captured against.

param(
    [Parameter(Mandatory=$true)] [string]$BinaryCmd,
    [Parameter(Mandatory=$true)] [string]$ReadyLine,
    [Parameter(Mandatory=$true)] [string]$Drive,
    [Parameter(Mandatory=$true)] [string]$Path,
    [AllowEmptyString()][string]$ExpectNames = '',
    [AllowEmptyString()][string]$ExpectCount = ''
)

. "$PSScriptRoot\_lib.ps1"

$op = {
    param($DriveLetter)
    $target = Resolve-MountedPath -DriveLetter $DriveLetter -Path $Path
    if (-not (Test-Path -LiteralPath $target -PathType Container)) {
        throw "$target is not a directory on the mount"
    }
    $gotEntries = @('.', '..') + (Get-ChildItem -Force -LiteralPath $target | Select-Object -ExpandProperty Name)

    $fails = @()
    if ($ExpectNames -and $ExpectNames -ne '__none__') {
        $want       = $ExpectNames -split ',' | ForEach-Object { $_.Trim() }
        $got        = $gotEntries | Sort-Object -Unique
        $wantSorted = $want | Sort-Object -Unique
        $missing = $wantSorted | Where-Object { $got -notcontains $_ }
        $extra   = $got        | Where-Object { $wantSorted -notcontains $_ }
        if ($missing -or $extra) {
            $msg = "name-set drift"
            if ($missing) { $msg += " missing=[$($missing -join ',')]" }
            if ($extra)   { $msg += " unexpected=[$($extra -join ',')]" }
            $fails += $msg
        }
    }
    if ($ExpectCount -and $ExpectCount -ne '__none__') {
        if ([int]$gotEntries.Count -ne [int]$ExpectCount) {
            $fails += "count mismatch: got=$($gotEntries.Count) want=$ExpectCount"
        }
    }

    if ($fails.Count -gt 0) {
        throw "win-ls-via-mount drift at ${target}: $($fails -join ' / ')"
    }
    Write-Output "ok ${target} ($($gotEntries.Count) entries)"
}.GetNewClosure()
Invoke-WithMount -BinaryCmd $BinaryCmd -ReadyLine $ReadyLine -Drive $Drive -ScriptBlock $op
