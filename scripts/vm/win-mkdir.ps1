# scripts/win/win-mkdir.ps1 -- generic v2 op: create a dir on a mounted volume.
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
    New-Item -ItemType Directory -Path $target -Force | Out-Null
    Write-Output "mkdir $target"
}.GetNewClosure()
Invoke-WithMount -BinaryCmd $BinaryCmd -ReadyLine $ReadyLine -Drive $Drive -ScriptBlock $op
