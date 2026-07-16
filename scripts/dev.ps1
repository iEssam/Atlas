#requires -Version 5.1
<#
    Starts the complete System Atlas development stack in one console:
      1. builds atlas-service (unless -SkipBuild is supplied)
      2. starts TSDB recording (unless -NoRecord is supplied)
      3. starts the named-pipe/shared-memory backend
      4. waits for the pipe, then runs the WinUI app

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
$uiProject = Join-Path $repo 'src-ui\Atlas.App\Atlas.App.csproj'
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
    if (-not $SkipBuild) {
        Write-Host "Building atlas-service ($Configuration)..." -ForegroundColor Cyan
        $cargoArgs = @('build', '-p', 'atlas-service')
        if ($Configuration -eq 'Release') { $cargoArgs += '--release' }
        & cargo @cargoArgs
        if ($LASTEXITCODE -ne 0) {
            throw "cargo build failed with exit code $LASTEXITCODE"
        }
    }

    if (-not (Test-Path -LiteralPath $serviceExe)) {
        throw "Backend executable not found: $serviceExe. Run without -SkipBuild first."
    }
    if (-not (Test-Path -LiteralPath $uiProject)) {
        throw "UI project not found: $uiProject"
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
    & dotnet run --project $uiProject --configuration $Configuration
    $uiExitCode = $LASTEXITCODE

    if ($started.Count -gt 0 -and ($started | Where-Object { -not $_.HasExited })) {
        Write-Host 'The UI exited. Press Ctrl+C to flush recording and stop the backend cleanly.' -ForegroundColor Yellow
        while ($started | Where-Object { -not $_.HasExited }) {
            Start-Sleep -Milliseconds 500
        }
    }
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
