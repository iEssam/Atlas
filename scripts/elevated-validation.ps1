#requires -Version 5.1
<#
    System Atlas — elevated validation pass
    ---------------------------------------
    Exercises the paths that CANNOT be verified in a standard-user, WDAC-enforced
    session (which is how the agent-driven build ran): live ETW, the Windows
    service host + crash-restart, live privacy (camera/mic) triggers, live plugin
    capability enforcement, and a full record -> diagnose -> report/bundle loop.

    RUN THIS IN:  an ELEVATED terminal (Run as administrator) on a machine where
    the freshly-built (unsigned) atlas-service.exe is ALLOWED TO EXECUTE — i.e.
    WDAC/App Control is in Audit mode, disabled, or the repo's target\ outputs are
    on the allow policy. If atlas-service.exe is blocked, the preflight will say so.

    It makes NO permanent changes it doesn't undo: it installs the SystemAtlas
    service and UNINSTALLS it at the end; it writes to a scratch DB and deletes it.

    Usage:   powershell -ExecutionPolicy Bypass -File scripts\elevated-validation.ps1
#>

$ErrorActionPreference = 'Continue'
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
# Then the events command: stream briefly while spawning a throwaway process.
$evtJob = Start-Job -ScriptBlock { param($exe) & $exe events 2>&1 } -ArgumentList $exe
Start-Sleep -Seconds 2
$np = Start-Process notepad -PassThru; Start-Sleep -Milliseconds 800; Stop-Process -Id $np.Id -Force
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
$burners = 1..([Environment]::ProcessorCount) | ForEach-Object { Start-Process powershell -ArgumentList '-NoProfile','-Command','$x=0.0; while($true){$x+=[math]::Sqrt($x+1)}' -PassThru -WindowStyle Hidden }
& $exe record --db $db --duration 34 --flush-secs 8 2>&1 | Select-String 'incidents=|stopped' | ForEach-Object { "   $($_.ToString().Trim())" }
$burners | ForEach-Object { Stop-Process -Id $_.Id -Force -ErrorAction SilentlyContinue }
$inc = & $exe incidents --db $db --minutes 10 2>&1
Check 'A CPU-saturation incident was detected with live ETW' ("$inc" -match 'CPU satur')
& $exe diagnose --db $db --incident 1 2>&1 | Select-Object -First 10 | ForEach-Object { "   $_" }
$bundle = Join-Path $env:TEMP 'atlas-validation-bundle.html'
& $exe support-bundle --db $db --format html --redact-paths --redact-users --out $bundle 2>&1 | Select-Object -Last 1
Check 'Redacted support bundle generated' (Test-Path $bundle)
Remove-Item $bundle -Force -ErrorAction SilentlyContinue

# --- 4. Signed plugin framework: live capability enforcement ---------------
Step '4. Plugin framework: signed accepted, unsigned refused, out-of-scope DENIED'
# NOTE: --db / --pipe are parent-level args and MUST precede the subcommand.
# On the clean scratch DB the signed plugin gets id 1; the unsigned one that is
# actually registered (with --allow-unsigned) gets id 2 (the REFUSED attempt
# inserts nothing, so it consumes no id). `plugin launch <id>` spawns the bundled
# example itself against the running serve.
$signed = "$env:SystemRoot\System32\notepad.exe"
$example = Join-Path $repo 'target\debug\atlas-plugin-example.exe'
& $exe plugin --db $db register $signed --caps snapshot 2>&1 | Select-String 'SIGNED|publisher|registered|verified' | ForEach-Object { "   $_" }
$refuse = & $exe plugin --db $db register $example --caps snapshot 2>&1
Check 'Unsigned plugin is REFUSED (no --allow-unsigned)' ("$refuse" -match 'refus|unsigned|REFUS')
& $exe plugin --db $db register $example --caps snapshot --allow-unsigned 2>&1 | Select-Object -Last 1 | ForEach-Object { "   $_" }
# serve, then launch example id 2 — it tries a granted read, an ungranted read,
# and a mutation, and prints its own PASS line iff scope was enforced exactly.
$srv = Start-Process -FilePath $exe -ArgumentList 'serve','--db',$db,'--pipe','validate' -PassThru -WindowStyle Hidden
Start-Sleep -Seconds 3
$plog = & $exe plugin --db $db --pipe validate launch 2 2>&1
Stop-Process -Id $srv.Id -Force -ErrorAction SilentlyContinue
Check 'Plugin: granted read ALLOWED' ("$plog" -match 'ALLOWED')
Check 'Plugin: ungranted read + mutation DENIED' ("$plog" -match 'DENIED')
Check 'Plugin: example self-reports enforcement PASS' ("$plog" -match '\[plugin\] PASS')
$plog | Select-String 'GetSnapshot|Search|Bookmark|PASS|FAIL|ALLOWED|DENIED' | ForEach-Object { "   " + $_.ToString().Trim() }

# --- 5. Live privacy trigger (MANUAL) --------------------------------------
Step '5. Live camera/microphone privacy trigger (MANUAL — needs a real capture app)'
Write-Host '   Add a rule, run serve, then OPEN a mic/cam app (e.g. Voice Recorder / Camera):'
& $exe privacy-alert --db $db add --capability microphone --condition any-use --name 'Validation mic' 2>&1 | Select-Object -Last 1 | ForEach-Object { "   $_" }
Write-Host '   >>> Start serve in another elevated window:  atlas-service serve --db "'$db'" --pipe validate'
Write-Host '   >>> Open the Voice Recorder and record ~3s, then stop.'
Write-Host '   >>> Then run:  atlas-service fired-alerts --db "'$db'"   (expect a microphone alert)'
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
