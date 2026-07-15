<#
    ============================================================================
    System Atlas - M9 MSI build harness
    ============================================================================
    Builds the prerequisite binaries, then invokes WiX v5 to emit
        installer/out/SystemAtlas-<version>-<platform>.msi

    Usage:
        pwsh installer/build.ps1 -Version 0.3.0.0
        pwsh installer/build.ps1 -Version 0.3.0.0 -Platform x64
        pwsh installer/build.ps1 -Version 0.3.0.0 -SkipPrereqs   # .wxs only

    Design notes:
      * On THIS machine an Application Control policy (docs/phases.md decision
        notes, error 4551) blocks executing freshly built/unsigned binaries.
        That affects the -Prereqs (cargo/dotnet produce fresh exes) and can
        affect running the produced MSI. Pass -SkipPrereqs to validate that
        Package.wxs COMPILES against already-present binaries/placeholders
        without rebuilding them. The WiX compile itself does not execute the
        payload, so it is safe.
      * The script FAILS LOUDLY if WiX is missing, with the exact install
        command (see Assert-Wix).
    ============================================================================
#>
[CmdletBinding()]
param(
    # Product version. MSI compares the first 3 fields for upgrade logic; keep
    # the 4th field (build) monotonic. See Package.wxs UpgradeCode/MajorUpgrade.
    [string]$Version = "0.3.0.0",

    # Target architecture. x64 is primary; arm64 is a first-class goal
    # (tech-stack sec 8). See the ARM64 note in Package.wxs / README.md - the
    # arm64 build needs its own UpgradeCode and arm64 prereq binaries.
    [ValidateSet("x64", "arm64")]
    [string]$Platform = "x64",

    # Skip cargo/dotnet prereq builds; compile the .wxs against whatever is
    # already staged (or placeholder files). Use this where App Control blocks
    # fresh build outputs, or for a pure "does the WiX compile?" check.
    [switch]$SkipPrereqs,

    # Emit an unsigned MSI. Signing is a separate, documented post-build step
    # (installer/README.md "Signing"); this harness never signs.
    [switch]$NoSign
)

$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

# --- Paths -------------------------------------------------------------------
$InstallerDir = Split-Path -Parent $MyInvocation.MyCommand.Path
$RepoRoot     = Split-Path -Parent $InstallerDir
$OutDir       = Join-Path $InstallerDir "out"
$StageDir     = Join-Path $InstallerDir "stage"   # where prereq outputs land

# Map WiX/MSI platform -> cargo target triple + dotnet RID.
$RustTarget = if ($Platform -eq "x64") { "x86_64-pc-windows-msvc" } else { "aarch64-pc-windows-msvc" }
$DotnetRid  = if ($Platform -eq "x64") { "win-x64" } else { "win-arm64" }

function Write-Step($msg) { Write-Host "==> $msg" -ForegroundColor Cyan }

# --- WiX presence check (FAIL LOUDLY) ---------------------------------------
function Assert-Wix {
    $wix = Get-Command wix -ErrorAction SilentlyContinue
    if (-not $wix) {
        Write-Host ""
        Write-Host "ERROR: WiX Toolset (v5/v6) is not installed or not on PATH." -ForegroundColor Red
        Write-Host ""
        Write-Host "Install it as a .NET global tool (preferred):" -ForegroundColor Yellow
        Write-Host "    dotnet tool install --global wix --version 5.0.2"
        Write-Host "    wix extension add -g WixToolset.Util.wixext/5.0.2"
        Write-Host ""
        Write-Host "  ...then ensure %USERPROFILE%\.dotnet\tools is on PATH."
        Write-Host "Alternatively:  winget install --id WiXToolset.WiXToolset" -ForegroundColor Yellow
        Write-Host ""
        throw "WiX not found."
    }
    $ver = (& wix --version) 2>&1
    Write-Step "Using WiX $ver"

    # The Util extension is required for util:ServiceConfig, util:PermissionEx,
    # and util:QueryWindowsWellKnownSIDs used by Package.wxs.
    $ext = (& wix extension list -g) 2>&1 | Out-String
    if ($ext -notmatch "WixToolset\.Util\.wixext") {
        Write-Host "ERROR: WiX Util extension missing. Install it with:" -ForegroundColor Red
        Write-Host "    wix extension add -g WixToolset.Util.wixext/5.0.2" -ForegroundColor Yellow
        throw "WixToolset.Util.wixext not found."
    }
}

# --- Prerequisite builds -----------------------------------------------------
# These produce the two binaries the MSI packages. They are commented with the
# EXACT commands and executed only when -SkipPrereqs is NOT set.
function Build-Prereqs {
    New-Item -ItemType Directory -Force -Path $StageDir | Out-Null

    Write-Step "Building atlas-service.exe (release, $RustTarget)"
    # EXACT COMMAND (run from repo root):
    #     cargo build --release -p atlas-service --target $RustTarget
    # NOTE: never run automatically here if App Control blocks fresh binaries.
    Push-Location $RepoRoot
    try {
        & cargo build --release -p atlas-service --target $RustTarget
        if ($LASTEXITCODE -ne 0) { throw "cargo build failed ($LASTEXITCODE)." }
    } finally { Pop-Location }
    $svcSrc = Join-Path $RepoRoot "target\$RustTarget\release\atlas-service.exe"
    Copy-Item $svcSrc (Join-Path $StageDir "atlas-service.exe") -Force

    Write-Step "Publishing Atlas.App (WinUI, self-contained, $DotnetRid)"
    # EXACT COMMAND:
    #     dotnet publish src-ui/Atlas.App -c Release -r $DotnetRid --self-contained true
    # Publishes Atlas.App.exe plus the WindowsAppSDK/.NET runtime payload into
    # the RID publish folder, which we then hand to WiX <Files> harvesting.
    $appProj = Join-Path $RepoRoot "src-ui\Atlas.App\Atlas.App.csproj"
    & dotnet publish $appProj -c Release -r $DotnetRid --self-contained true `
        -p:Platform=$Platform
    if ($LASTEXITCODE -ne 0) { throw "dotnet publish failed ($LASTEXITCODE)." }
}

# Resolve the two inputs Package.wxs needs, whether freshly built or staged.
function Resolve-Inputs {
    # Service exe: prefer staged copy, else the cargo target dir.
    $svc = Join-Path $StageDir "atlas-service.exe"
    if (-not (Test-Path $svc)) {
        $svc = Join-Path $RepoRoot "target\$RustTarget\release\atlas-service.exe"
    }
    if (-not (Test-Path $svc)) {
        $svc = Join-Path $RepoRoot "target\release\atlas-service.exe"  # host-arch fallback
    }

    # App publish dir: the RID publish output.
    $appPub = Join-Path $RepoRoot "src-ui\Atlas.App\bin\$Platform\Release\net10.0-windows10.0.19041.0\$DotnetRid\publish"

    return [pscustomobject]@{ ServiceExe = $svc; AppPublishDir = $appPub }
}

# --- Main --------------------------------------------------------------------
Assert-Wix

if (-not $SkipPrereqs) {
    Build-Prereqs
} else {
    Write-Step "SkipPrereqs set - compiling Package.wxs against existing/staged binaries only."
}

$inputs = Resolve-Inputs

# Validate inputs exist; if not, tell the maintainer precisely what is missing.
if (-not (Test-Path $inputs.ServiceExe)) {
    throw "atlas-service.exe not found at '$($inputs.ServiceExe)'. Build it with: cargo build --release -p atlas-service --target $RustTarget (or drop a copy in installer/stage/)."
}
if (-not (Test-Path $inputs.AppPublishDir)) {
    throw "WinUI publish dir not found at '$($inputs.AppPublishDir)'. Publish it with: dotnet publish src-ui/Atlas.App -c Release -r $DotnetRid --self-contained true"
}

New-Item -ItemType Directory -Force -Path $OutDir | Out-Null
$msiName = "SystemAtlas-$Version-$Platform.msi"
$msiPath = Join-Path $OutDir $msiName

Write-Step "Compiling MSI -> $msiPath"
# WiX v5 single-step build (compile + link + emit MSI). -arch sets the package
# platform; -d passes preprocessor variables into Package.wxs; -ext pulls in the
# Util extension; -bindpath is not needed because File Source paths are absolute.
& wix build `
    (Join-Path $InstallerDir "Package.wxs") `
    -arch $Platform `
    -d "ProductVersion=$Version" `
    -d "ServiceExe=$($inputs.ServiceExe)" `
    -d "AppPublishDir=$($inputs.AppPublishDir)" `
    -d "Platform=$Platform" `
    -ext WixToolset.Util.wixext `
    -o $msiPath
if ($LASTEXITCODE -ne 0) { throw "wix build failed ($LASTEXITCODE)." }

Write-Step "MSI written: $msiPath"

if (-not $NoSign) {
    Write-Host ""
    Write-Host "NOTE: MSI is UNSIGNED. Sign it before distribution - see installer/README.md:" -ForegroundColor Yellow
    Write-Host "    signtool sign /fd SHA256 /tr <RFC3161-URL> /td SHA256 /a `"$msiPath`""
}

Write-Host ""
Write-Host "Done. Do NOT install this MSI on a dev machine casually - it registers a real Windows service." -ForegroundColor Green
