[CmdletBinding()]
param()

$ErrorActionPreference = 'Stop'
$repositoryRoot = Split-Path -Parent $PSScriptRoot
$excludedDirectories = '[\\/](?:\.agents|\.claude|\.git|bin|obj|target)[\\/]'
$markdownFiles = Get-ChildItem -LiteralPath $repositoryRoot -Recurse -File -Filter '*.md' |
    Where-Object { $_.FullName -notmatch $excludedDirectories }
$linkPattern = '(?<!!)\[[^\]]+\]\((?<target>[^)]+)\)'
$failures = [System.Collections.Generic.List[string]]::new()
$checkedLinks = 0

foreach ($file in $markdownFiles) {
    $content = Get-Content -LiteralPath $file.FullName -Raw
    foreach ($match in [regex]::Matches($content, $linkPattern)) {
        $target = $match.Groups['target'].Value.Trim()
        if ($target.StartsWith('<') -and $target.EndsWith('>')) {
            $target = $target.Substring(1, $target.Length - 2)
        }

        if ($target -match '^(?:[a-z][a-z0-9+.-]*:|#)' -or [string]::IsNullOrWhiteSpace($target)) {
            continue
        }

        # Drop an optional Markdown title and fragment before resolving the file.
        $target = ($target -split '\s+["'']', 2)[0]
        $pathPart = ($target -split '#', 2)[0]
        $pathPart = [System.Uri]::UnescapeDataString($pathPart)
        $resolved = Join-Path $file.DirectoryName $pathPart
        $checkedLinks++

        if (-not (Test-Path -LiteralPath $resolved)) {
            $relativeSource = $file.FullName.Substring($repositoryRoot.Length).TrimStart('\', '/')
            $failures.Add("${relativeSource}: missing relative link target '$target'")
        }
    }
}

if ($failures.Count -gt 0) {
    $failures | ForEach-Object { Write-Error $_ }
    throw "Documentation validation failed with $($failures.Count) missing relative link target(s)."
}

Write-Host "Documentation validation passed: $checkedLinks relative links across $($markdownFiles.Count) Markdown files."
