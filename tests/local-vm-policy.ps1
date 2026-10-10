# Exercise the real Windows scope precedence without running MSI/activation.
param([string]$ProvisionScript = "$PSScriptRoot\..\scripts\local-vm\finish.ps1")
$ErrorActionPreference = 'Stop'
$tokens = $null
$parseErrors = $null
$ast = [System.Management.Automation.Language.Parser]::ParseFile(
    $ProvisionScript, [ref]$tokens, [ref]$parseErrors)
if ($parseErrors.Count -ne 0) { throw "Invalid provisioning script: $parseErrors" }
$function = $ast.Find({ param($node)
    $node -is [System.Management.Automation.Language.FunctionDefinitionAst] -and
    $node.Name -eq 'Set-LocalTestExecutionPolicy'
}, $true)
if (-not $function) { throw 'Missing Set-LocalTestExecutionPolicy' }
Invoke-Expression $function.Extent.Text

$originalUser = Get-ExecutionPolicy -Scope CurrentUser
$originalProcess = Get-ExecutionPolicy -Scope Process
try {
    Set-ExecutionPolicy -Scope Process -ExecutionPolicy Bypass -Force
    Set-LocalTestExecutionPolicy
    foreach ($scope in 'Process', 'CurrentUser') {
        if ((Get-ExecutionPolicy -Scope $scope) -ne 'RemoteSigned') {
            throw "$scope policy was not set to RemoteSigned"
        }
    }
    Write-Output 'local VM execution-policy tests: ok'
} finally {
    Remove-Item Env:PSExecutionPolicyPreference -ErrorAction SilentlyContinue
    Set-ExecutionPolicy -Scope CurrentUser -ExecutionPolicy $originalUser -Force
    Set-ExecutionPolicy -Scope Process -ExecutionPolicy $originalProcess -Force
}
