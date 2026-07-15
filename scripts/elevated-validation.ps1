#requires -Version 5.1
<#
    System Atlas - elevated validation pass
    ---------------------------------------
    Exercises the paths that CANNOT be verified in a standard-user, WDAC-enforced
    session (which is how the agent-driven build ran): live ETW, the Windows
    service host + crash-restart, live privacy (camera/mic) triggers, live plugin
    capability enforcement, and a full record -> diagnose -> report/bundle loop.

    RUN THIS IN:  an ELEVATED terminal (Run as administrator) on a machine where
    the freshly-built (unsigned) atlas-service.exe is ALLOWED TO EXECUTE -- i.e.
    WDAC/App Control is in Audit mode, disabled, or the repo's target\ outputs are
    on the allow policy. If atlas-service.exe is blocked, the preflight will say so.

    It makes NO permanent changes it doesn't undo: it installs the SystemAtlas
    service and UNINSTALLS it at the end; it writes to a scratch DB and deletes it.

    Usage:   powershell -ExecutionPolicy Bypass -File scripts\elevated-validation.ps1

    NOTE: this file is deliberately ASCII-only. Windows PowerShell 5.1 reads a
    BOM-less .ps1 as the system ANSI code page, so any non-ASCII byte here would
    mojibake in the console. We also force UTF-8 console output below so the
    product's own UTF-8 text (e.g. a "(R)" glyph in a signed publisher name)
    renders correctly rather than as garbage.
#>

$ErrorActionPreference = 'Continue'
# Decode child-process (atlas-service) stdout as UTF-8 so registered-publisher
# names and other non-ASCII product output render correctly.
try { [Console]::OutputEncoding = [System.Text.Encoding]::UTF8 } catch {}

$repo = Split-Path -Parent $PSScriptRoot
Set-Location $repo
$results = [ordered]@{}
function Step($name) { Write-Host "`n==== $name ====" -ForegroundColor Cyan }
function Check($name, [bool]$ok, $detail = '') {
    $results[$name] = $ok
    $tag = if ($ok) { 'PASS' } else { 'FAIL' }
    $col = if ($ok) { 'Green' } else { 'Red' }
    Write-Host ("[{0}] {1} {2}" -f $tag, $name, $detail) -ForegroundColor $col
}

# --- Preflight -------------------------------------------------------------
Step 'Preflight: elevation + WDAC + toolchain'
$elev = ([Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()).IsInRole([Security.Principal.WindowsBuiltinRole]::Administrator)
Check 'Running elevated' $elev
if (-not $elev) { Write-Host 'STOP: re-run this from an elevated terminal.' -ForegroundColor Red; return }

$cargo = (Get-Command cargo -ErrorAction SilentlyContinue).Source
if (-not $cargo) { $cargo = "$env:USERPROFILE\.cargo\bin\cargo.exe" }
Get-Process atlas-service -ErrorAction SilentlyContinue | Stop-Process -Force -Confirm:$false
Write-Host 'Building atlas-service + atlas-plugin-example (debug)...'
& $cargo build -q -p atlas-service -p atlas-plugin-example 2>$null
$exe = Join-Path $repo 'target\debug\atlas-service.exe'
$example = Join-Path $repo 'target\debug\atlas-plugin-example.exe'
Check 'Service binary built' (Test-Path $exe)

# WDAC-exempt smoke: can the fresh binary actually execute?
$ver = & $exe --version 2>&1
$canRun = ($LASTEXITCODE -eq 0) -and ("$ver" -notmatch 'Application Control')
Check 'Fresh binary is allowed to execute (WDAC-exempt)' $canRun "$ver"
if (-not $canRun) {
    Write-Host 'STOP: WDAC/App Control is blocking the unsigned binary. Put target\ on the allow policy or use Audit mode, then re-run.' -ForegroundColor Red
    return
}
$db = Join-Path $env:LOCALAPPDATA 'SystemAtlas\dev\atlas-validation.db'
Remove-Item "$db*" -Force -ErrorAction SilentlyContinue

# --- 1. Live ETW process events -------------------------------------------
Step '1. Live ETW process start/stop (needs elevation to start the kernel session)'
# Run any ignored live collector tests first (self-contained; informational).
& $cargo test -q -p atlas-collectors -- --ignored 2>&1 | Select-String 'test result|process|etw' | ForEach-Object { "   " + $_.ToString().Trim() }
# Stream briefly while spawning throwaway processes. We use short-lived cmd.exe
# processes that start AND exit on their own (reliable Win32 START/STOP ETW
# events), so there is nothing to kill -- avoids the "process already exited"
# race that Stop-Process hit against the Store-app notepad launcher.
$evtJob = Start-Job -ScriptBlock { param($exe) & $exe events 2>&1 } -ArgumentList $exe
Start-Sleep -Seconds 2
1..3 | ForEach-Object { Start-Process cmd -ArgumentList '/c','ver >NUL' -WindowStyle Hidden | Out-Null; Start-Sleep -Milliseconds 300 }
Start-Sleep -Seconds 1
$evt = Receive-Job $evtJob; Stop-Job $evtJob; Remove-Job $evtJob -Force
$sawStart = "$evt" -match 'START'
Check 'ETW live: saw process START events (not the elevation message)' $sawStart
$evt | Select-Object -First 6 | ForEach-Object { "   $_" }

# --- 2. Windows service host + crash-restart ------------------------------
Step '2. Windows service install / start / status / crash-restart / uninstall'
& $exe service install 2>&1 | ForEach-Object { "   $_" }
Start-Sleep -Seconds 2
$svc = Get-Service -Name SystemAtlas -ErrorAction SilentlyContinue
Check 'Service registered (SystemAtlas)' ($null -ne $svc)
if ($svc) {
    if ($svc.Status -ne 'Running') { Start-Service SystemAtlas -ErrorAction SilentlyContinue; Start-Sleep -Seconds 2 }
    $svc.Refresh()
    Check 'Service reaches Running' ($svc.Status -eq 'Running')
    & $exe service status 2>&1 | ForEach-Object { "   $_" }
    # Crash-restart: kill the service process, confirm SCM restarts it (failure actions).
    $spid = (Get-CimInstance Win32_Service -Filter "Name='SystemAtlas'").ProcessId
    if ($spid -and $spid -ne 0) {
        Write-Host "   killing service pid $spid to test crash-restart..."
        Stop-Process -Id $spid -Force -ErrorAction SilentlyContinue
        Start-Sleep -Seconds 8   # failure action = restart after 5s
        $svc.Refresh()
        Check 'Service auto-restarts after a crash (SCM failure action)' ($svc.Status -eq 'Running')
    }
    & $exe service uninstall 2>&1 | ForEach-Object { "   $_" }
    Start-Sleep -Seconds 2
    Check 'Service uninstalled cleanly' ($null -eq (Get-Service SystemAtlas -ErrorAction SilentlyContinue))
}

# --- 3. Full record -> diagnose -> report/bundle under load ----------------
Step '3. Record (with live ETW) -> diagnose an incident -> support bundle'
# The CPU-saturation detector needs system CPU >= 85% sustained >= 10s
# (atlas-collectors detectors.rs). Spawn (cores + 2) tight busy loops so total
# system CPU clears 85% even with scheduler overhead, let them RAMP for 3s, then
# record for 45s so the detector sees a comfortably-longer-than-10s run.
$nproc = [Environment]::ProcessorCount
Write-Host "   spawning $($nproc + 2) CPU burners on $nproc logical processors; ramping 3s..."
$burners = 1..($nproc + 2) | ForEach-Object {
    Start-Process powershell -ArgumentList '-NoProfile', '-Command', '$x=0.0; while($true){ $x = [math]::Sqrt($x*$x + 1.0000001) }' -PassThru -WindowStyle Hidden
}
Start-Sleep -Seconds 3
& $exe record --db $db --duration 45 --flush-secs 10 2>&1 | Select-String 'incidents|stopped' | ForEach-Object { "   $($_.ToString().Trim())" }
$burners | ForEach-Object { Stop-Process -Id $_.Id -Force -ErrorAction SilentlyContinue }
$inc = & $exe incidents --db $db --minutes 10 2>&1
$gotIncident = "$inc" -match 'CPU satur'
Check 'A CPU-saturation incident was detected with live ETW' $gotIncident
if (-not $gotIncident) {
    # Diagnostic: show the peak system CPU actually recorded, so a failure tells
    # us whether the workload was too weak vs. a detector problem.
    Write-Host '   (no incident) recorded system-CPU history for triage:' -ForegroundColor Yellow
    & $exe history --db $db --metric sys-cpu --minutes 5 2>&1 | Select-Object -Last 8 | ForEach-Object { "   $_" }
    "$inc" -split "`n" | Select-Object -First 3 | ForEach-Object { "   $_" }
} else {
    & $exe diagnose --db $db --incident 1 2>&1 | Select-Object -First 10 | ForEach-Object { "   $_" }
}
$bundle = Join-Path $env:TEMP 'atlas-validation-bundle.html'
& $exe support-bundle --db $db --format html --redact-paths --redact-users --out $bundle 2>&1 | Select-Object -Last 1
Check 'Redacted support bundle generated' (Test-Path $bundle)
Remove-Item $bundle -Force -ErrorAction SilentlyContinue

# --- 4. Signed plugin framework: live capability enforcement ---------------
Step '4. Plugin framework: signed accepted, unsigned refused, out-of-scope DENIED'
# NOTE: --db / --pipe are PARENT-level args and MUST precede the subcommand.
# Plugins register DISABLED by default (a signature check is not implicit trust),
# so the example MUST be enabled before launch. We capture the example's plugin
# id from `plugin list` instead of assuming it, enable it, then mint a launch
# nonce with --print-nonce and run the example DIRECTLY so its stdout/stderr is
# captured cleanly (rather than relying on the nested spawn inside `launch`).
$signed = "$env:SystemRoot\System32\notepad.exe"
& $exe plugin --db $db register $signed --caps snapshot 2>&1 | Select-String 'SIGNED|publisher|registered|verified' | ForEach-Object { "   $_" }
$refuse = & $exe plugin --db $db register $example --caps snapshot 2>&1
Check 'Unsigned plugin is REFUSED (no --allow-unsigned)' ("$refuse" -match 'refus|unsigned|REFUS')
& $exe plugin --db $db register $example --caps snapshot --allow-unsigned 2>&1 | Select-Object -Last 1 | ForEach-Object { "   $_" }

# Find the example's plugin id from the listing (name line, not the exe line).
$listOut = & $exe plugin --db $db list 2>&1
$exId = $null
$idLine = $listOut | Select-String 'atlas-plugin-example v' | Select-Object -First 1
if ($idLine -and ("$idLine" -match '#(\d+)')) { $exId = $matches[1] }
if ($exId) { Write-Host "   example plugin registered as id #$exId (enabling before launch)" }

if ($exId) {
    # Enable it (registered disabled by default). Do this BEFORE serve starts so
    # the server sees it enabled when the session is opened.
    & $exe plugin --db $db enable $exId 2>&1 | ForEach-Object { "   $_" }

    $srv = Start-Process -FilePath $exe -ArgumentList 'serve', '--db', $db, '--pipe', 'validate' -PassThru -WindowStyle Hidden
    Start-Sleep -Seconds 3

    # Mint a one-time nonce; run the example directly with it so we capture output.
    $nonceOut = & $exe plugin --db $db --pipe validate launch $exId --print-nonce 2>&1
    $nonce = $null; $pipeName = 'validate'
    if (($nonceOut | Out-String) -match 'ATLAS_PLUGIN_NONCE=(\S+)') { $nonce = $matches[1] }
    if (($nonceOut | Out-String) -match 'ATLAS_PLUGIN_PIPE=(\S+)')  { $pipeName = $matches[1] }

    $plog = ''
    if ($nonce) {
        $env:ATLAS_PLUGIN_ID = $exId
        $env:ATLAS_PLUGIN_NONCE = $nonce
        $env:ATLAS_PLUGIN_PIPE = $pipeName
        $plog = & $example 2>&1 | Out-String
        Remove-Item Env:ATLAS_PLUGIN_ID, Env:ATLAS_PLUGIN_NONCE, Env:ATLAS_PLUGIN_PIPE -ErrorAction SilentlyContinue
    } else {
        Write-Host '   FAILED to mint launch nonce:' -ForegroundColor Red
        $nonceOut | ForEach-Object { "   $_" }
    }
    Stop-Process -Id $srv.Id -Force -ErrorAction SilentlyContinue

    Check 'Plugin: granted read ALLOWED' ($plog -match 'ALLOWED')
    Check 'Plugin: ungranted read + mutation DENIED' ($plog -match 'DENIED')
    Check 'Plugin: example self-reports enforcement PASS' ($plog -match '\[plugin\] PASS')
    # Always show the example's probe/verdict lines so a failure is diagnosable.
    ($plog -split "`n") | Where-Object { $_ -match 'plugin\]|GetSnapshot|Search|Bookmark|ALLOWED|DENIED|PASS|FAIL' } | ForEach-Object { "   " + $_.Trim() }
} else {
    Check 'Plugin: granted read ALLOWED' $false
    Check 'Plugin: ungranted read + mutation DENIED' $false
    Check 'Plugin: example self-reports enforcement PASS' $false
    Write-Host '   plugin listing (for triage):' -ForegroundColor Yellow
    $listOut | ForEach-Object { "   $_" }
}

# --- 5. Live privacy trigger (MANUAL) --------------------------------------
Step '5. Live camera/microphone privacy trigger (MANUAL - needs a real capture app)'
Write-Host '   Add a rule, run serve, then OPEN a mic/cam app (e.g. Voice Recorder / Camera):'
& $exe privacy-alert --db $db add --capability microphone --condition any-use --name 'Validation mic' 2>&1 | Select-Object -Last 1 | ForEach-Object { "   $_" }
# Single double-quoted strings (escaped inner quotes) so the printed commands are
# copy-paste runnable -- no arg-splitting spaces inside the quoted --db path.
Write-Host "   >>> Start serve in another elevated window:"
Write-Host "         atlas-service serve --db `"$db`" --pipe validate"
Write-Host "   >>> Open the Voice Recorder and record ~3s, then stop."
Write-Host "   >>> Then run:"
Write-Host "         atlas-service fired-alerts --db `"$db`""
Write-Host "       (expect a microphone alert)"
Write-Host '   (left manual because it needs a real interactive capture session on the desktop.)' -ForegroundColor Yellow

# --- Summary ---------------------------------------------------------------
Step 'Summary'
$pass = ($results.Values | Where-Object { $_ }).Count
$total = $results.Count
Write-Host ("{0}/{1} automated checks passed. (Step 5 is manual.)" -f $pass, $total) -ForegroundColor $(if ($pass -eq $total) { 'Green' } else { 'Yellow' })
$results.GetEnumerator() | ForEach-Object { Write-Host ("  {0}  {1}" -f ($(if ($_.Value) { 'PASS' } else { 'FAIL' })), $_.Key) }
Remove-Item "$db*" -Force -ErrorAction SilentlyContinue
Get-Process atlas-service -ErrorAction SilentlyContinue | Stop-Process -Force -Confirm:$false
Write-Host "`nDone. Scratch DB removed; service uninstalled." -ForegroundColor Cyan
