[CmdletBinding()]
param(
    [ValidateSet("Debug", "Release")][string]$Configuration = "Release",
    [ValidateSet("x64", "ARM64")][string]$Platform = "x64",
    [ValidatePattern('^\d+\.\d+\.\d+\.\d+$')][string]$Version = "0.3.0.0",
    [string]$Publisher = "CN=Atlas Project"
)

$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $MyInvocation.MyCommand.Path
$vswhere = Join-Path ${env:ProgramFiles(x86)} "Microsoft Visual Studio\Installer\vswhere.exe"
if (-not (Test-Path -LiteralPath $vswhere)) { throw "Visual Studio Build Tools were not found." }
$msbuild = & $vswhere -latest -products * -requires Microsoft.Component.MSBuild -find "MSBuild\**\Bin\MSBuild.exe" | Select-Object -First 1
if (-not $msbuild) { throw "MSBuild with Visual C++ support was not found." }

& $msbuild (Join-Path $root "SystemAtlas.ShellExtension.vcxproj") /m /nologo "/p:Configuration=$Configuration" "/p:Platform=$Platform"
if ($LASTEXITCODE -ne 0) { throw "Shell extension build failed ($LASTEXITCODE)." }

$sdkBinRoot = Join-Path ${env:ProgramFiles(x86)} "Windows Kits\10\bin"
$makeAppx = Get-ChildItem -LiteralPath $sdkBinRoot -Filter MakeAppx.exe -Recurse |
    Where-Object { $_.FullName -match '\\x64\\MakeAppx\.exe$' } |
    Sort-Object FullName -Descending | Select-Object -First 1
if (-not $makeAppx) { throw "MakeAppx.exe was not found in the Windows SDK." }

$output = Join-Path $root "out\$Platform\$Configuration"
$packageStage = Join-Path $output "SparsePackage"
New-Item -ItemType Directory -Force -Path $packageStage | Out-Null
$manifest = [xml](Get-Content -LiteralPath (Join-Path $root "Package\AppxManifest.xml") -Raw)
$manifest.Package.Identity.Version = $Version
$manifest.Package.Identity.Publisher = $Publisher
$manifest.Save((Join-Path $packageStage "AppxManifest.xml"))

$packagePath = Join-Path $output "SystemAtlas.Desktop-$Version.msix"
& $makeAppx.FullName pack /o /d $packageStage /nv /p $packagePath
if ($LASTEXITCODE -ne 0) { throw "Sparse package build failed ($LASTEXITCODE)." }

Write-Host "Shell DLL:      $(Join-Path $output 'SystemAtlas.ShellExtension.dll')"
Write-Host "Sparse package: $packagePath"
Write-Host "The sparse package is unsigned. Sign it with the same certificate subject as -Publisher before registration."
