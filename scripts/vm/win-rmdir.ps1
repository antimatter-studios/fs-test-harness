# scripts/win/win-rmdir.ps1 -- generic v2 op: remove a directory from a mount.
# Self-contained mount-do-unmount per call; see _lib.ps1 Invoke-WithMount.

param(
    [Parameter(Mandatory=$true)] [string]$BinaryCmd,
    [Parameter(Mandatory=$true)] [string]$ReadyLine,
    [Parameter(Mandatory=$true)] [string]$Drive,
    [Parameter(Mandatory=$true)] [string]$Path
)

. "$PSScriptRoot\_lib.ps1"

$op = {
    param($DriveLetter)
    $target = Resolve-MountedPath -DriveLetter $DriveLetter -Path $Path
    Remove-Item -LiteralPath $target -Recurse -Force -ErrorAction Stop
    Write-Output "removed dir $target"
}.GetNewClosure()
Invoke-WithMount -BinaryCmd $BinaryCmd -ReadyLine $ReadyLine -Drive $Drive -ScriptBlock $op
