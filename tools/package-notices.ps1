#Requires -Version 7.0
param([Parameter(Mandatory)][string]$Destination)
$ErrorActionPreference = 'Stop'
$root = Split-Path $PSScriptRoot -Parent
$output = Join-Path $Destination 'licenses'
New-Item -ItemType Directory -Path $output -Force | Out-Null
$index = [Collections.Generic.List[string]]::new()
$index.Add('Third-party dependencies retain their own licenses. This inventory includes build dependencies as well as linked code.')
$mit = [IO.File]::ReadAllText((Join-Path $root 'LICENSE'))
$mitTerms = $mit.Substring($mit.IndexOf('Permission is hereby'))
function Copy-Notices([string]$Source, [string]$Target) {
    $files = @(Get-ChildItem -LiteralPath $Source -Recurse -File | Where-Object {
        $_.Name -match '^(?i:LICEN[SC]E|COPYING|NOTICE|COPYRIGHT|THIRD[-_]?PARTY[-_]?NOTICES)(?:$|[-_.])'
    })
    foreach ($file in $files) {
        $dest = Join-Path $Target ([IO.Path]::GetRelativePath($Source, $file.FullName))
        New-Item -ItemType Directory -Path (Split-Path $dest -Parent) -Force | Out-Null
        Copy-Item -LiteralPath $file.FullName -Destination $dest
    }
    return $files.Count
}
$json = & cargo metadata --locked --offline --format-version 1 --filter-platform x86_64-pc-windows-msvc
if ($LASTEXITCODE -ne 0) { throw 'Cargo dependency inventory failed.' }
$metadata = $json | ConvertFrom-Json
foreach ($package in $metadata.packages | Sort-Object name) {
    if ($package.id -eq $metadata.resolve.root) { continue }
    $target = Join-Path $output "rust/$($package.name)-$($package.version)"
    New-Item -ItemType Directory -Path $target -Force | Out-Null
    $source = Split-Path $package.manifest_path -Parent
    $count = Copy-Notices $source $target
    $index.Add("Rust: $($package.name) $($package.version) | $($package.license) | $($package.repository)")
    if (-not $count) {
        if ($package.license -ne 'MIT') { throw "No bundled license for $($package.name). Review the dependency." }
        "Package authors: $($package.authors -join ', ')`n`n$mitTerms" | Set-Content (Join-Path $target 'LICENSE-MIT.txt')
    }
    # The vendored disassembler keeps its original MIT notices in C source headers.
    if ($package.name -eq 'libudis86-sys') {
        $headers = Get-ChildItem (Join-Path $source 'libudis86') -File | ForEach-Object {
            $text = [IO.File]::ReadAllText($_.FullName)
            $match = [regex]::Match($text, '(?s)^/\*.*?\*/')
            if ($match.Success -and $match.Value.Contains('Copyright')) { $match.Value }
        }
        $headers | Select-Object -Unique | Set-Content (Join-Path $target 'udis86-notices.txt')
    }
}

$index | Set-Content (Join-Path $Destination 'THIRD-PARTY-NOTICES.txt')
