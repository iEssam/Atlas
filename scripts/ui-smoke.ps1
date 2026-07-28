[CmdletBinding()]
param(
    [ValidateSet('overview', 'activity', 'graphics', 'experiments', 'gaming')]
    [string]$StartPage = 'overview',

    [string]$ExpectedElementName = 'Overview',

    [string]$FindUsingPath,

    [ValidateRange(5, 60)]
    [int]$TimeoutSeconds = 20
)

$ErrorActionPreference = 'Stop'
$repositoryRoot = Split-Path -Parent $PSScriptRoot
$outputRoot = Join-Path $repositoryRoot 'src-ui\Atlas.App\bin\x64\Debug'
$executable = Get-ChildItem -LiteralPath $outputRoot -Recurse -Filter 'Atlas.App.exe' |
    Where-Object { $_.FullName -match '[\\/]win-x64[\\/]' } |
    Sort-Object LastWriteTimeUtc -Descending |
    Select-Object -First 1

if (-not $executable) {
    throw "Atlas.App.exe was not found below $outputRoot. Build Atlas.App for Debug x64 first."
}

Add-Type -AssemblyName UIAutomationClient
$previousStartPage = $env:ATLAS_START_PAGE
$process = $null

try {
    if ($FindUsingPath -or $StartPage -eq 'overview') {
        Remove-Item Env:ATLAS_START_PAGE -ErrorAction SilentlyContinue
    }
    else {
        $env:ATLAS_START_PAGE = $StartPage
    }

    if ($FindUsingPath) {
        $launchArguments = @('--find-using', ('"{0}"' -f $FindUsingPath))
        $process = Start-Process -FilePath $executable.FullName -ArgumentList $launchArguments -PassThru
    } else {
        $process = Start-Process -FilePath $executable.FullName -PassThru
    }
    $deadline = (Get-Date).AddSeconds($TimeoutSeconds)
    $window = $null

    do {
        Start-Sleep -Milliseconds 200
        $process.Refresh()
        if ($process.HasExited) {
            throw "Atlas.App exited before showing a window (exit code $($process.ExitCode))."
        }

        if ($process.MainWindowHandle -ne 0) {
            $window = [System.Windows.Automation.AutomationElement]::FromHandle(
                [IntPtr]$process.MainWindowHandle)
        }
    } while ($null -eq $window -and (Get-Date) -lt $deadline)

    if ($null -eq $window) {
        throw "Atlas.App did not expose a top-level window within $TimeoutSeconds seconds."
    }

    if ($process.MainWindowTitle -ne 'System Atlas') {
        throw "Expected the window title 'System Atlas', got '$($process.MainWindowTitle)'."
    }

    $searchCondition = [System.Windows.Automation.PropertyCondition]::new(
        [System.Windows.Automation.AutomationElement]::NameProperty,
        'Search System Atlas')
    $expectedCondition = [System.Windows.Automation.PropertyCondition]::new(
        [System.Windows.Automation.AutomationElement]::NameProperty,
        $ExpectedElementName)
    $search = $null
    $expected = $null

    do {
        $search = $window.FindFirst(
            [System.Windows.Automation.TreeScope]::Descendants,
            $searchCondition)
        $expected = $window.FindFirst(
            [System.Windows.Automation.TreeScope]::Descendants,
            $expectedCondition)
        if ($null -eq $search -or $null -eq $expected) {
            Start-Sleep -Milliseconds 200
        }
    } while (($null -eq $search -or $null -eq $expected) -and (Get-Date) -lt $deadline)

    if ($null -eq $search) {
        throw "The shell search control was not present in the UI Automation tree."
    }

    if ($null -eq $expected) {
        throw "The expected '$ExpectedElementName' element was not present in the UI Automation tree."
    }

    if ($FindUsingPath) {
        $pathCondition = [System.Windows.Automation.PropertyCondition]::new(
            [System.Windows.Automation.AutomationElement]::NameProperty,
            'File path to search')
        $pathInput = $window.FindFirst(
            [System.Windows.Automation.TreeScope]::Descendants,
            $pathCondition)
        if ($null -eq $pathInput) {
            throw "The File Locks path input was not present in the UI Automation tree."
        }
        $valuePattern = $pathInput.GetCurrentPattern(
            [System.Windows.Automation.ValuePattern]::Pattern)
        if ($valuePattern.Current.Value -ne $FindUsingPath) {
            throw "Expected selected path '$FindUsingPath', got '$($valuePattern.Current.Value)'."
        }
    }

    Write-Host "UI smoke passed: title='$($process.MainWindowTitle)', page='$ExpectedElementName', pid=$($process.Id)."
}
finally {
    if ($process -and -not $process.HasExited) {
        Stop-Process -Id $process.Id -Force
    }

    if ($null -eq $previousStartPage) {
        Remove-Item Env:ATLAS_START_PAGE -ErrorAction SilentlyContinue
    }
    else {
        $env:ATLAS_START_PAGE = $previousStartPage
    }
}
