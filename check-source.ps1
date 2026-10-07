#Requires -Version 7.0
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$root = $PSScriptRoot
$files = @(Get-ChildItem -LiteralPath $root -Recurse -File | Where-Object {
    $_.FullName -notmatch '[\\/](target|dist|exports|logs|\.git)[\\/]' -and
    $_.Extension -in @('.rs','.ps1','.toml','.md')
})
foreach ($file in $files) {
    $text = [IO.File]::ReadAllText($file.FullName)
    if ($text -match '(?i)[a-z]:[\\/](?:users|work)[\\/]') { throw "Workstation path: $($file.Name)" }
    if ($text -match '(?m)[ \t]+$') { throw "Trailing whitespace: $($file.Name)" }
    if ($file.Extension -eq '.md') {
        $prose = [regex]::Replace($text, '(?s)```.*?```', '')
        foreach ($match in [regex]::Matches($prose, '\]\(([^)]+)\)')) {
            $target = $match.Groups[1].Value -split '#', 2 | Select-Object -First 1
            if (-not $target -or $target -match '^https?://') { continue }
            $resolved = [IO.Path]::GetFullPath((Join-Path $file.DirectoryName $target))
            if (-not $resolved.StartsWith($root + [IO.Path]::DirectorySeparatorChar, [StringComparison]::OrdinalIgnoreCase)) {
                throw "Link escapes project: $($file.Name)"
            }
            if (-not (Test-Path -LiteralPath $resolved)) { throw "Missing link: $($file.Name): $target" }
        }
    }
}
Write-Host "Source hygiene passed ($($files.Count) files)."
