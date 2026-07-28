[CmdletBinding()]
param(
    [Parameter(Mandatory)][string]$PackagePath,
    [Parameter(Mandatory)][string]$ExternalLocation
)

$ErrorActionPreference = "Stop"
$package = (Resolve-Path -LiteralPath $PackagePath).Path
$external = (Resolve-Path -LiteralPath $ExternalLocation).Path
foreach ($required in @("Atlas.App.exe", "SystemAtlas.ShellExtension.dll")) {
    if (-not (Test-Path -LiteralPath (Join-Path $external $required))) {
        throw "ExternalLocation must contain $required. Missing: $(Join-Path $external $required)"
    }
}

Add-AppxPackage -Path $package -ExternalLocation $external
Write-Host "Registered System Atlas Explorer integration for the current user."
Write-Host "Restart File Explorer or sign out and back in before testing the command."
