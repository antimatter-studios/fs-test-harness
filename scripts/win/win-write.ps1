# scripts/win/win-write.ps1 -- generic v2 op: write a file to a mounted volume.
#
# Self-contained: mounts via -BinaryCmd, writes the file, unmounts.
# See _lib.ps1 Invoke-WithMount for the lifecycle rationale.
#
# Args:
#   -BinaryCmd   Full mount command line.
#   -ReadyLine   Ready-line regex.
#   -Drive       Drive letter (single char, no colon).
#   -Path        Volume-relative file path (e.g. "/new.txt").
#   -Content     UTF-8 string to write (no BOM, no trailing newline).

param(
    [Parameter(Mandatory=$true)] [string]$BinaryCmd,
    [Parameter(Mandatory=$true)] [string]$ReadyLine,
    [Parameter(Mandatory=$true)] [string]$Drive,
    [Parameter(Mandatory=$true)] [string]$Path,
    [AllowEmptyString()][string]$Content = ''
)

. "$PSScriptRoot\_lib.ps1"

if ($Content -eq '__none__') { $Content = '' }

$op = {
    param($DriveLetter)
    $target = Resolve-MountedPath -DriveLetter $DriveLetter -Path $Path
    $bytes = [System.Text.Encoding]::UTF8.GetBytes($Content)
    [System.IO.File]::WriteAllBytes($target, $bytes)
    Write-Output "wrote $($bytes.Length) bytes to $target"
}.GetNewClosure()
Invoke-WithMount -BinaryCmd $BinaryCmd -ReadyLine $ReadyLine -Drive $Drive -ScriptBlock $op
