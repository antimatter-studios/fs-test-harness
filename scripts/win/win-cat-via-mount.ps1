# scripts/win/win-cat-via-mount.ps1 -- generic v2 op: read a file via the
# mount, assert against expectations.
# Self-contained mount-do-unmount per call; see _lib.ps1 Invoke-WithMount.

param(
    [Parameter(Mandatory=$true)] [string]$BinaryCmd,
    [Parameter(Mandatory=$true)] [string]$ReadyLine,
    [Parameter(Mandatory=$true)] [string]$Drive,
    [Parameter(Mandatory=$true)] [string]$Path,
    [AllowEmptyString()][string]$ExpectContent = '',
    [AllowEmptyString()][string]$ExpectSize    = '',
    [AllowEmptyString()][string]$ExpectSha256 = ''
)

. "$PSScriptRoot\_lib.ps1"

$op = {
    param($DriveLetter)
    $target = Resolve-MountedPath -DriveLetter $DriveLetter -Path $Path
    if (-not (Test-Path -LiteralPath $target)) {
        throw "$target does not exist on the mount"
    }
    $bytes = [System.IO.File]::ReadAllBytes($target)
    $fails = @()

    if ($ExpectSize -and $ExpectSize -ne '__none__') {
        if ([int64]$bytes.Length -ne [int64]$ExpectSize) {
            $fails += "size mismatch: got=$($bytes.Length) want=$ExpectSize"
        }
    }
    if ($ExpectSha256 -and $ExpectSha256 -ne '__none__') {
        $sha = [BitConverter]::ToString(
            [System.Security.Cryptography.SHA256]::Create().ComputeHash($bytes)
        ).Replace('-','').ToLower()
        if ($sha -ne $ExpectSha256.ToLower()) {
            $fails += "sha256 mismatch: got=$sha want=$ExpectSha256"
        }
    }
    if ($ExpectContent -and $ExpectContent -ne '__none__') {
        $expectBytes = [System.Text.Encoding]::UTF8.GetBytes($ExpectContent)
        $same = ($bytes.Length -eq $expectBytes.Length)
        if ($same) {
            for ($i = 0; $i -lt $bytes.Length; $i++) {
                if ($bytes[$i] -ne $expectBytes[$i]) { $same = $false; break }
            }
        }
        if (-not $same) {
            $got = [System.Text.Encoding]::UTF8.GetString($bytes, 0, [Math]::Min($bytes.Length, 200))
            $fails += "content mismatch: got first 200 bytes='$got' want='$ExpectContent'"
        }
    }

    if ($fails.Count -gt 0) {
        throw "win-cat-via-mount drift at ${target}: $($fails -join ' / ')"
    }
    Write-Output "ok ${target} ($($bytes.Length) bytes)"
}.GetNewClosure()
Invoke-WithMount -BinaryCmd $BinaryCmd -ReadyLine $ReadyLine -Drive $Drive -ScriptBlock $op
