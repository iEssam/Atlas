#requires -Version 5.1
<#
    System Atlas — MSI install / upgrade / removal lifecycle validation
    -------------------------------------------------------------------
    Drives the FULL packaged-install lifecycle end to end against a real MSI:

        clean install  ->  crash recovery  ->  major upgrade  ->  removal

    and asserts the state the WiX authoring (installer/Package.wxs) promises at
    each step: the SystemAtlas service registered/running as LocalSystem with
    crash-restart failure actions, binaries in %ProgramFiles%\System Atlas, the
    ACL'd %ProgramData%\SystemAtlas data dir, the all-users Start Menu shortcut,
    exactly one Add/Remove-Programs entry, a version bump with NO duplicate
    product after upgrade, data preserved across upgrade, and a clean teardown
    on uninstall.

    WHY IT CAN'T RUN IN THE BUILD SESSION: installing a per-machine MSI needs
    ELEVATION (it registers + starts a real Windows service) and a WDAC posture
    that lets the freshly built, unsigned atlas-service.exe EXECUTE — the service
    is started during install, so a blocked binary makes the install itself fail.
    The build session is standard-user with WDAC user-mode enforcement on, so
    this is handed to you to run in an elevated, WDAC-exempt session.

    It builds TWO MSIs from the same binaries at versions 0.1.0.0 and 0.1.1.0 so
    MajorUpgrade has a real version delta to act on (the payload need not differ
    to exercise the upgrade/removal machinery).

    Usage (elevated):
        powershell -ExecutionPolicy Bypass -File installer\validate-install.ps1
        ...\validate-install.ps1 -BaseVersion 0.1.0.0 -UpgradeVersion 0.1.1.0
        ...\validate-install.ps1 -KeepData      # don't offer to delete ProgramData at the end

    Self-cleaning: uninstalls the product at the end even on failure.
#>
[CmdletBinding()]
param(
    [string]$BaseVersion    = '0.1.0.0',
    [string]$UpgradeVersion = '0.1.1.0',
    [switch]$KeepData
)

$ErrorActionPreference = 'Stop'
$installerDir = Split-Path -Parent $MyInvocation.MyCommand.Path
$repo = Split-Path -Parent $installerDir
$outDir = Join-Path $installerDir 'out'
$results = [ordered]@{}
$installedProductCode = $null

function Step($m) { Write-Host "`n==== $m ====" -ForegroundColor Cyan }
function Check($name, [bool]$ok, $detail = '') {
    $results[$name] = $ok
    $tag = if ($ok) { 'PASS' } else { 'FAIL' }
    $col = if ($ok) { 'Green' } else { 'Red' }
    Write-Host ("[{0}] {1} {2}" -f $tag, $name, $detail) -ForegroundColor $col
}
# Registry ARP (Add/Remove Programs) rows for "System Atlas", both 64/32 views.
function Get-AtlasArp {
    $keys = @(
        'HKLM:\SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall\*',
        'HKLM:\SOFTWARE\WOW6432Node\Microsoft\Windows\CurrentVersion\Uninstall\*'
    )
    Get-ItemProperty $keys -ErrorAction SilentlyContinue |
        Where-Object { $_.DisplayName -eq 'System Atlas' }
}
function Invoke-Msi($argline, $logName) {
    $log = Join-Path $env:TEMP $logName
    $p = Start-Process msiexec.exe -ArgumentList ($argline + " /qn /norestart /l*v `"$log`"") -Wait -PassThru
    return [pscustomobject]@{ Code = $p.ExitCode; Log = $log }
}

# --- Preflight -------------------------------------------------------------
Step 'Preflight: elevation, WiX, MSIs'
$elev = ([Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()).IsInRole([Security.Principal.WindowsBuiltinRole]::Administrator)
Check 'Running elevated' $elev
if (-not $elev) { Write-Host 'STOP: re-run from an elevated terminal (MSI install needs it).' -ForegroundColor Red; return }

# Refuse to run if a copy is already installed (don't clobber a real install).
if (Get-AtlasArp) { Write-Host 'STOP: System Atlas is already installed. Uninstall it first.' -ForegroundColor Red; return }

$baseMsi    = Join-Path $outDir "SystemAtlas-$BaseVersion-x64.msi"
$upgradeMsi = Join-Path $outDir "SystemAtlas-$UpgradeVersion-x64.msi"
foreach ($v in @($BaseVersion, $UpgradeVersion)) {
    $msi = Join-Path $outDir "SystemAtlas-$v-x64.msi"
    if (-not (Test-Path $msi)) {
        Write-Host "Building MSI $v ..." -ForegroundColor Yellow
        & (Join-Path $installerDir 'build.ps1') -Version $v -Platform x64 -NoSign
        if ($LASTEXITCODE -ne 0 -and -not (Test-Path $msi)) { throw "build.ps1 did not produce $msi" }
    }
}
Check 'Base MSI present'    (Test-Path $baseMsi)    $baseMsi
Check 'Upgrade MSI present' (Test-Path $upgradeMsi) $upgradeMsi
if (-not ((Test-Path $baseMsi) -and (Test-Path $upgradeMsi))) { return }

$pf   = Join-Path $env:ProgramFiles 'System Atlas'
$data = Join-Path $env:ProgramData  'SystemAtlas'
$lnk  = Join-Path $env:ProgramData  'Microsoft\Windows\Start Menu\Programs\System Atlas\System Atlas.lnk'

try {
    # --- 1. Clean install --------------------------------------------------
    Step '1. Clean install (msiexec /i base.msi)'
    $r = Invoke-Msi "/i `"$baseMsi`"" 'atlas-install.log'
    Check 'Installer exit 0' ($r.Code -eq 0) "(exit $($r.Code); log $($r.Log))"
    Start-Sleep -Seconds 3
    $svc = Get-Service SystemAtlas -ErrorAction SilentlyContinue
    Check 'Service SystemAtlas registered' ($null -ne $svc)
    $wmi = Get-CimInstance Win32_Service -Filter "Name='SystemAtlas'" -ErrorAction SilentlyContinue
    Check 'Service is LocalSystem + auto-start' ($wmi.StartName -eq 'LocalSystem' -and $wmi.StartMode -eq 'Auto') "($($wmi.StartName), $($wmi.StartMode))"
    Check 'Service is Running' ($svc.Status -eq 'Running') "($($svc.Status))"
    # Crash-restart failure actions baked in by util:ServiceConfig.
    $fa = & sc.exe qfailure SystemAtlas 2>&1 | Out-String
    Check 'Crash-restart failure actions configured' ($fa -match 'RESTART')
    Check 'atlas-service.exe installed' (Test-Path (Join-Path $pf 'atlas-service.exe'))
    Check 'Atlas.App.exe installed'     (Test-Path (Join-Path $pf 'Atlas.App.exe'))
    Check 'Data dir created'            (Test-Path $data)
    Check 'Start Menu shortcut created' (Test-Path $lnk)
    $arp = @(Get-AtlasArp)
    Check 'Exactly one ARP entry'       ($arp.Count -eq 1) "(count $($arp.Count))"
    Check 'ARP version = base'          ($arp[0].DisplayVersion -eq $BaseVersion) "($($arp[0].DisplayVersion))"
    $installedProductCode = $arp[0].PSChildName
    # Data-dir ACL: Users should have read/traverse but NOT write at the root.
    $acl = (Get-Acl $data).Access | Where-Object { $_.IdentityReference -match 'Users' }
    Check 'Data-dir ACL grants Users read (not write) at root' (($acl | Where-Object { $_.FileSystemRights -match 'Write' }).Count -eq 0)

    # Leave a marker in the data dir to prove upgrade PRESERVES data.
    $marker = Join-Path $data 'validation-marker.txt'
    'preserve-me' | Out-File $marker -Encoding utf8 -Force

    # --- 2. Crash recovery (MSI-configured SCM failure action) -------------
    Step '2. Crash recovery — kill the service, SCM must restart it'
    $spid = (Get-CimInstance Win32_Service -Filter "Name='SystemAtlas'").ProcessId
    if ($spid) {
        Stop-Process -Id $spid -Force
        Write-Host "   killed pid $spid; waiting for SCM restart (5s delay)..."
        Start-Sleep -Seconds 10
        (Get-Service SystemAtlas).Refresh()
        $back = (Get-Service SystemAtlas).Status -eq 'Running'
        $newpid = (Get-CimInstance Win32_Service -Filter "Name='SystemAtlas'").ProcessId
        Check 'Service auto-restarted after crash' ($back -and $newpid -ne $spid) "(pid $spid -> $newpid)"
    }

    # --- 3. Major upgrade --------------------------------------------------
    Step '3. Major upgrade (msiexec /i upgrade.msi) — one product, version bumps, data kept'
    $r = Invoke-Msi "/i `"$upgradeMsi`"" 'atlas-upgrade.log'
    Check 'Upgrade exit 0' ($r.Code -eq 0) "(exit $($r.Code))"
    Start-Sleep -Seconds 3
    $arp = @(Get-AtlasArp)
    Check 'Still exactly one ARP entry (old product removed)' ($arp.Count -eq 1) "(count $($arp.Count))"
    Check 'ARP version = upgrade'  ($arp[0].DisplayVersion -eq $UpgradeVersion) "($($arp[0].DisplayVersion))"
    Check 'Service still Running after upgrade' ((Get-Service SystemAtlas -ErrorAction SilentlyContinue).Status -eq 'Running')
    Check 'Only one SystemAtlas service exists' (@(Get-Service SystemAtlas -ErrorAction SilentlyContinue).Count -eq 1)
    Check 'Data preserved across upgrade' (Test-Path $marker)

    # --- 4. Removal --------------------------------------------------------
    Step '4. Removal (msiexec /x) — service gone, files gone, ARP gone'
    $r = Invoke-Msi "/x `"$upgradeMsi`"" 'atlas-uninstall.log'
    Check 'Uninstall exit 0' ($r.Code -eq 0) "(exit $($r.Code))"
    Start-Sleep -Seconds 3
    Check 'Service removed'          ($null -eq (Get-Service SystemAtlas -ErrorAction SilentlyContinue))
    Check 'ProgramFiles dir removed' (-not (Test-Path $pf))
    Check 'Start Menu shortcut removed' (-not (Test-Path $lnk))
    Check 'ARP entry removed'        (@(Get-AtlasArp).Count -eq 0)
    # By design the data dir is NOT removed by the MSI (survives reinstall). Verify.
    Check 'Data dir intentionally preserved on uninstall (by design)' (Test-Path $data)
    $installedProductCode = $null
}
finally {
    # --- Safety net: make sure nothing is left installed -------------------
    if ($installedProductCode) {
        Write-Host "`nCleanup: force-uninstalling leftover product $installedProductCode ..." -ForegroundColor Yellow
        Invoke-Msi "/x $installedProductCode" 'atlas-cleanup.log' | Out-Null
    }
    if (-not $KeepData -and (Test-Path $data)) {
        Write-Host "Removing leftover data dir $data (pass -KeepData to keep it)." -ForegroundColor Yellow
        Remove-Item $data -Recurse -Force -ErrorAction SilentlyContinue
    }
}

# --- Summary ---------------------------------------------------------------
Step 'Summary'
$pass = ($results.Values | Where-Object { $_ }).Count
Write-Host ("{0}/{1} checks passed." -f $pass, $results.Count) -ForegroundColor $(if ($pass -eq $results.Count) { 'Green' } else { 'Yellow' })
$results.GetEnumerator() | ForEach-Object { Write-Host ("  {0}  {1}" -f ($(if ($_.Value) { 'PASS' } else { 'FAIL' })), $_.Key) }
