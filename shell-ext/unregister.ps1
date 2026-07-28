$ErrorActionPreference = "Stop"
$packages = Get-AppxPackage -Name "SystemAtlas.Desktop"
foreach ($package in $packages) { Remove-AppxPackage -Package $package.PackageFullName }
Write-Host "Removed System Atlas Explorer integration for the current user."
