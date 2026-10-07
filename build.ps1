#Requires -Version 7.0
param([switch]$SkipTests)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
if (-not $IsWindows) { throw 'Build on Windows x64.' }
foreach ($tool in @('cargo','rustc','lld-link')) { Get-Command $tool -ErrorAction Stop | Out-Null }
function Checked([string]$Program, [string[]]$Arguments) {
    & $Program @Arguments
    if ($LASTEXITCODE -ne 0) { throw "$Program failed (exit $LASTEXITCODE)." }
}
$root = $PSScriptRoot
$oldFlags = $env:RUSTFLAGS
$oldEncoded = $env:CARGO_ENCODED_RUSTFLAGS
$oldTarget = $env:CARGO_TARGET_DIR
Push-Location $root
try {
    & ./check-source.ps1
    $env:RUSTFLAGS = $null
    $env:CARGO_TARGET_DIR = Join-Path $root 'target'
    $sysroot = (& rustc --print sysroot).Trim()
    $profile = [Environment]::GetFolderPath('UserProfile')
    $prefixes = @($root, $profile, $sysroot)
    $flags = foreach ($prefix in $prefixes) {
        "--remap-path-prefix=$prefix=build"
        "--remap-path-prefix=$($prefix.Replace('\','/'))=build"
    }
    $env:CARGO_ENCODED_RUSTFLAGS = $flags -join [char]0x1f
    Checked cargo @('fmt','--','--check')
    if (-not $SkipTests) { Checked cargo @('test','--locked','--target','x86_64-pc-windows-msvc') }
    Checked cargo @('build','--locked','--release','--lib','--target','x86_64-pc-windows-msvc')
    if (-not $SkipTests) {
        Checked cargo @('build','--locked','--target','x86_64-pc-windows-msvc','--bin','startup-smoke')
        & ./tools/check-startup.ps1 -DllPath (Join-Path $root 'target/x86_64-pc-windows-msvc/release/hbr_account_export.dll') -HostPath (Join-Path $root 'target/x86_64-pc-windows-msvc/debug/startup-smoke.exe')
    }
    $stage = Join-Path $root ('dist/staging/' + [guid]::NewGuid().ToString('N'))
    $payload = Join-Path $stage 'account-export'
    New-Item -ItemType Directory -Path $payload -Force | Out-Null
    Copy-Item -LiteralPath 'target/x86_64-pc-windows-msvc/release/hbr_account_export.dll' -Destination (Join-Path $stage 'version.dll')
    foreach ($name in @('README.md','LICENSE')) { Copy-Item -LiteralPath $name -Destination $payload }
    New-Item -ItemType Directory -Path (Join-Path $payload 'docs') | Out-Null
    Copy-Item -LiteralPath 'docs/development.md' -Destination (Join-Path $payload 'docs/development.md')
    & ./tools/package-notices.ps1 -Destination $payload
    & ./tools/check-version-proxy.ps1 -DllPath (Join-Path $stage 'version.dll') -VersionedFile (Join-Path ([Environment]::GetFolderPath('System')) 'kernel32.dll')
    foreach ($file in Get-ChildItem -LiteralPath $stage -Recurse -File) {
        if ($file.LastWriteTime.Year -lt 1980) { $file.LastWriteTime = [datetime]'1980-01-01' }
        if ($file.Extension -in @('.pdb','.exp','.lib')) { throw "Published symbol/build artifact: $($file.Name)" }
        $bytes = [IO.File]::ReadAllBytes($file.FullName)
        foreach ($encoding in @([Text.Encoding]::UTF8,[Text.Encoding]::Unicode)) {
            $text = $encoding.GetString($bytes)
            foreach ($prefix in $prefixes) {
                if ($text.Contains($prefix,[StringComparison]::OrdinalIgnoreCase) -or
                    $text.Contains($prefix.Replace('\','/'),[StringComparison]::OrdinalIgnoreCase)) {
                    throw "Build-machine path in package: $($file.Name)"
                }
            }
        }
    }
    $version = [regex]::Match([IO.File]::ReadAllText((Join-Path $root 'Cargo.toml')), '(?m)^version = "([^"]+)"').Groups[1].Value
    $zip = Join-Path $root "dist/hbr-account-export-v$version-win-x64.zip"
    Compress-Archive -Path (Join-Path $stage '*') -DestinationPath $zip -Force
    Get-FileHash -LiteralPath $zip -Algorithm SHA256 | Format-List
    Write-Host "Built $zip"
} finally {
    $env:RUSTFLAGS = $oldFlags
    $env:CARGO_ENCODED_RUSTFLAGS = $oldEncoded
    $env:CARGO_TARGET_DIR = $oldTarget
    Pop-Location
}
