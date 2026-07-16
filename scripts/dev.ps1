#requires -Version 5.1
<#
    Starts the complete System Atlas development stack in one console:
      1. builds atlas-service, its isolated GPU vendor helper, and the UI (unless -SkipBuild is supplied)
      2. starts TSDB recording (unless -NoRecord is supplied)
      3. starts the named-pipe/shared-memory backend
      4. waits for the pipe, then runs the WinUI app through its native apphost

    Usage:
      .\scripts\dev.ps1
      .\scripts\dev.ps1 -SkipBuild
      .\scripts\dev.ps1 -NoRecord
      .\scripts\dev.ps1 -Configuration Release

    Press Ctrl+C in this console to stop the UI, recorder, and server cleanly.
    If the UI window is closed first, the backend stays alive so rules can be
    restored and the current recording window can be flushed; press Ctrl+C to
    finish the stack.
#>

[CmdletBinding()]
param(
    [ValidateSet('Debug', 'Release')]
    [string]$Configuration = 'Debug',
    [switch]$SkipBuild,
    [switch]$NoRecord
)

$ErrorActionPreference = 'Stop'
$repo = Split-Path -Parent $PSScriptRoot
$profile = if ($Configuration -eq 'Release') { 'release' } else { 'debug' }
$serviceExe = Join-Path $repo "target\$profile\atlas-service.exe"
$gpuVendorHostExe = Join-Path $repo "target\$profile\atlas-gpu-vendor-host.exe"
$uiProject = Join-Path $repo 'src-ui\Atlas.App\Atlas.App.csproj'
$uiRuntime = if ([System.Runtime.InteropServices.RuntimeInformation]::OSArchitecture -eq [System.Runtime.InteropServices.Architecture]::Arm64) {
    'win-arm64'
}
else {
    'win-x64'
}
$uiTargetFramework = 'net10.0-windows10.0.19041.0'
$uiExe = Join-Path $repo "src-ui\Atlas.App\bin\$Configuration\$uiTargetFramework\$uiRuntime\Atlas.App.exe"
$pipeName = "SystemAtlas.dev.$env:USERNAME"
$pipePath = "\\.\pipe\$pipeName"
$started = [System.Collections.Generic.List[System.Diagnostics.Process]]::new()
$uiExitCode = 0

function Test-AtlasPipe {
    try {
        return [System.IO.Directory]::GetFiles('\\.\pipe\') -contains $pipePath
    }
    catch {
        return $false
    }
}

function Test-SmartAppControlEnforced {
    try {
        $policy = Get-ItemProperty `
            -LiteralPath 'HKLM:\SYSTEM\CurrentControlSet\Control\CI\Policy' `
            -Name 'VerifiedAndReputablePolicyState' `
            -ErrorAction Stop
        return $policy.VerifiedAndReputablePolicyState -eq 1
    }
    catch {
        return $false
    }
}

function Assert-NativeDevBinariesCanRun {
    if (-not (Test-SmartAppControlEnforced)) {
        return
    }

    $requiresUnsignedBuild = -not $SkipBuild
    $unsignedExistingBinary = $false
    if ($SkipBuild) {
        foreach ($path in @($serviceExe, $gpuVendorHostExe)) {
            if ((Test-Path -LiteralPath $path) -and
                (Get-AuthenticodeSignature -LiteralPath $path).Status -ne 'Valid') {
                $unsignedExistingBinary = $true
                break
            }
        }
    }

    if (-not $requiresUnsignedBuild -and -not $unsignedExistingBinary) {
        return
    }

    throw @'
Smart App Control is in enforcement mode. Cargo Debug/Release output is unsigned,
so Windows will block atlas-service.exe and atlas-gpu-vendor-host.exe regardless
of which PowerShell launch command is used.

Choose one supported development setup:
  1. Turn Smart App Control off on this development PC. This cannot be reversed
     without resetting or reinstalling Windows.
  2. Use a Windows development VM with Smart App Control off. Physical GPU and
     NVML validation may not be available through the VM's virtual GPU.
  3. Sign the native binaries with an RSA code-signing certificate from a CA in
     Microsoft's Trusted Root Program, then run this script with -SkipBuild.

A locally generated self-signed certificate is not accepted by Smart App Control.
Open: Windows Security > App & browser control > Smart App Control settings.
'@
}

function Start-AtlasProcess([string[]]$Arguments, [string]$Label) {
    Write-Host "Starting $Label..." -ForegroundColor Cyan
    $process = Start-Process `
        -FilePath $serviceExe `
        -ArgumentList $Arguments `
        -WorkingDirectory $repo `
        -NoNewWindow `
        -PassThru
    $started.Add($process)
    return $process
}

Push-Location $repo
try {
    Assert-NativeDevBinariesCanRun

    if (-not $SkipBuild) {
        Write-Host "Building atlas-service ($Configuration)..." -ForegroundColor Cyan
        $cargoArgs = @('build', '-p', 'atlas-service')
        if ($Configuration -eq 'Release') { $cargoArgs += '--release' }
        & cargo @cargoArgs
        if ($LASTEXITCODE -ne 0) {
            throw "cargo build failed with exit code $LASTEXITCODE"
        }
        $helperArgs = @('build', '-p', 'atlas-collectors', '--bin', 'atlas-gpu-vendor-host')
        if ($Configuration -eq 'Release') { $helperArgs += '--release' }
        & cargo @helperArgs
        if ($LASTEXITCODE -ne 0) {
            throw "GPU vendor helper build failed with exit code $LASTEXITCODE"
        }

        Write-Host "Building Atlas UI ($Configuration, $uiRuntime)..." -ForegroundColor Cyan
        & dotnet build $uiProject --configuration $Configuration --runtime $uiRuntime
        if ($LASTEXITCODE -ne 0) {
            throw "Atlas UI build failed with exit code $LASTEXITCODE"
        }
    }

    if (-not (Test-Path -LiteralPath $serviceExe)) {
        throw "Backend executable not found: $serviceExe. Run without -SkipBuild first."
    }
    if (-not (Test-Path -LiteralPath $gpuVendorHostExe)) {
        throw "GPU vendor helper not found: $gpuVendorHostExe. Run without -SkipBuild first."
    }
    if (-not (Test-Path -LiteralPath $uiProject)) {
        throw "UI project not found: $uiProject"
    }
    if (-not (Test-Path -LiteralPath $uiExe)) {
        throw "UI executable not found: $uiExe. Run without -SkipBuild first."
    }

    if (Test-AtlasPipe) {
        Write-Host "Using the backend already listening on $pipePath" -ForegroundColor Yellow
        if (-not $NoRecord) {
            Write-Host 'Recording was not started because this script does not own the existing backend. Start `atlas-service record` separately if that backend is not already recording.' -ForegroundColor Yellow
        }
    }
    else {
        if (-not $NoRecord) {
            $null = Start-AtlasProcess @('record') 'history recorder'
        }
        $server = Start-AtlasProcess @('serve') 'IPC backend'

        Write-Host "Waiting for $pipePath..." -ForegroundColor DarkGray
        $ready = $false
        for ($attempt = 0; $attempt -lt 100; $attempt++) {
            if ($server.HasExited) {
                throw "Atlas backend exited before opening its pipe (exit code $($server.ExitCode))."
            }
            if (Test-AtlasPipe) {
                $ready = $true
                break
            }
            Start-Sleep -Milliseconds 100
        }
        if (-not $ready) {
            throw "Timed out waiting for $pipePath"
        }
    }

    Write-Host "Starting Atlas UI ($Configuration)..." -ForegroundColor Green
    Write-Host 'Press Ctrl+C here to stop the complete stack cleanly.' -ForegroundColor DarkGray
    # WinUI's generated native apphost performs Windows App SDK bootstrap and
    # activation that `dotnet exec Atlas.App.dll` does not provide.
    & $uiExe
    $uiExitCode = $LASTEXITCODE

    if ($uiExitCode -ne 0) {
        throw "Atlas UI exited with code $uiExitCode"
    }

    if ($started.Count -gt 0 -and ($started | Where-Object { -not $_.HasExited })) {
        Write-Host 'The UI exited. Press Ctrl+C to flush recording and stop the backend cleanly.' -ForegroundColor Yellow
        while ($started | Where-Object { -not $_.HasExited }) {
            Start-Sleep -Milliseconds 500
        }
    }
}
catch {
    $uiExitCode = 1
    Write-Host $_.Exception.Message -ForegroundColor Red
}
finally {
    # Ctrl+C is delivered to all processes sharing this console. Give the Rust
    # handlers time to restore rule interventions and flush the current TSDB
    # window. Deliberately avoid Stop-Process: forced termination can skip both.
    foreach ($process in $started) {
        if (-not $process.HasExited) {
            $null = $process.WaitForExit(5000)
        }
        if (-not $process.HasExited) {
            Write-Warning "Backend process $($process.Id) is still running. Stop it with Ctrl+C in this console rather than terminating it forcibly."
        }
        $process.Dispose()
    }
    Pop-Location
}

exit $uiExitCode
