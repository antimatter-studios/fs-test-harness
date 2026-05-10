# scripts/win/win-rename.ps1 -- generic v2 op: rename / move within a mount.
# Self-contained mount-do-unmount per call; see _lib.ps1 Invoke-WithMount.

param(
    [Parameter(Mandatory=$true)] [string]$BinaryCmd,
    [Parameter(Mandatory=$true)] [string]$ReadyLine,
    [Parameter(Mandatory=$true)] [string]$Drive,
    [Parameter(Mandatory=$true)] [string]$From,
    [Parameter(Mandatory=$true)] [string]$To
)

. "$PSScriptRoot\_lib.ps1"

$op = {
    param($DriveLetter)
    $src = Resolve-MountedPath -DriveLetter $DriveLetter -Path $From
    $dst = Resolve-MountedPath -DriveLetter $DriveLetter -Path $To
    Move-Item -LiteralPath $src -Destination $dst -Force -ErrorAction Stop
    Write-Output "renamed $src -> $dst"
}.GetNewClosure()
Invoke-WithMount -BinaryCmd $BinaryCmd -ReadyLine $ReadyLine -Drive $Drive -ScriptBlock $op
