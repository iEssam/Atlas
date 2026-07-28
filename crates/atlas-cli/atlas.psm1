# atlas.psm1 — thin PowerShell automation wrappers over the read-only `atlas` CLI.
#
# Demonstrates the Atlas automation story (PRD §7.5): each cmdlet shells
# out to `atlas <command> --json` and parses the machine-readable output into
# PowerShell objects you can pipe, filter, and format. Everything here is
# READ-ONLY — the underlying CLI never mutates anything.
#
# Usage:
#   Import-Module ./atlas.psm1
#   Get-AtlasProcess -Limit 5 | Format-Table pid, cpu_percent, image_name
#   Get-AtlasProcess | Where-Object { $_.cpu_percent -gt 10 }
#   Get-AtlasIncident -Minutes 120
#   Get-AtlasCapability
#
# The cmdlets locate `atlas` on PATH by default; override with -AtlasPath, and
# target a specific service pipe with -Pipe (matching `serve --pipe`).

Set-StrictMode -Version Latest

function Invoke-Atlas {
    <#
    .SYNOPSIS
    Runs `atlas <args> --json` and returns the parsed JSON as objects.
    .DESCRIPTION
    The shared plumbing for the Get-Atlas* cmdlets. Non-zero exit codes (e.g.
    the service not running) are surfaced as terminating errors.
    #>
    [CmdletBinding()]
    param(
        [Parameter(Mandatory)][string[]] $Arguments,
        [string] $Pipe,
        [string] $AtlasPath = 'atlas'
    )

    $argv = @($Arguments)
    if ($Pipe) { $argv += @('--pipe', $Pipe) }
    $argv += '--json'

    $raw = & $AtlasPath @argv
    if ($LASTEXITCODE -ne 0) {
        throw "atlas exited with code $LASTEXITCODE (is 'atlas-service serve' running?)"
    }
    # `atlas --json` prints a single pretty-printed JSON document.
    return ($raw | Out-String | ConvertFrom-Json)
}

function Get-AtlasProcess {
    <#
    .SYNOPSIS
    Top processes by CPU (maps to `atlas top --json`).
    .EXAMPLE
    Get-AtlasProcess -Limit 5 | Format-Table pid, cpu_percent, image_name
    #>
    [CmdletBinding()]
    param(
        [int] $Limit = 15,
        [string] $Pipe,
        [string] $AtlasPath = 'atlas'
    )
    $result = Invoke-Atlas -Arguments @('top', '--limit', "$Limit") -Pipe $Pipe -AtlasPath $AtlasPath
    return $result.processes
}

function Get-AtlasSystem {
    <#
    .SYNOPSIS
    The system gauge summary from the current snapshot (`atlas top --json`).
    #>
    [CmdletBinding()]
    param([string] $Pipe, [string] $AtlasPath = 'atlas')
    $result = Invoke-Atlas -Arguments @('top', '--limit', '1') -Pipe $Pipe -AtlasPath $AtlasPath
    return $result.system
}

function Get-AtlasIncident {
    <#
    .SYNOPSIS
    Detected incidents in the last N minutes (`atlas incidents --json`).
    .EXAMPLE
    Get-AtlasIncident -Minutes 120 | Where-Object { $_.severity -eq 'SEVERITY_HIGH' }
    #>
    [CmdletBinding()]
    param(
        [int] $Minutes = 1440,
        [string] $Pipe,
        [string] $AtlasPath = 'atlas'
    )
    $result = Invoke-Atlas -Arguments @('incidents', '--minutes', "$Minutes") -Pipe $Pipe -AtlasPath $AtlasPath
    return $result.incidents
}

function Get-AtlasListeningPort {
    <#
    .SYNOPSIS
    Listening ports with owning process (`atlas ports --json`).
    #>
    [CmdletBinding()]
    param([string] $Pipe, [string] $AtlasPath = 'atlas')
    $result = Invoke-Atlas -Arguments @('ports') -Pipe $Pipe -AtlasPath $AtlasPath
    return $result.ports
}

function Get-AtlasService {
    <#
    .SYNOPSIS
    Windows services inventory (`atlas services --json`).
    #>
    [CmdletBinding()]
    param(
        [string] $Filter = '',
        [string] $Pipe,
        [string] $AtlasPath = 'atlas'
    )
    $args = @('services')
    if ($Filter) { $args += @('--filter', $Filter) }
    $result = Invoke-Atlas -Arguments $args -Pipe $Pipe -AtlasPath $AtlasPath
    return $result.services
}

function Get-AtlasCapability {
    <#
    .SYNOPSIS
    Service version + advertised capability flags (`atlas capabilities --json`).
    #>
    [CmdletBinding()]
    param([string] $Pipe, [string] $AtlasPath = 'atlas')
    return Invoke-Atlas -Arguments @('capabilities') -Pipe $Pipe -AtlasPath $AtlasPath
}

Export-ModuleMember -Function `
    Invoke-Atlas, Get-AtlasProcess, Get-AtlasSystem, Get-AtlasIncident, `
    Get-AtlasListeningPort, Get-AtlasService, Get-AtlasCapability
