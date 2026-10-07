#Requires -Version 7.0
param([Parameter(Mandatory)][string]$DllPath, [Parameter(Mandatory)][string]$HostPath)
$ErrorActionPreference = 'Stop'
foreach ($mode in @('thread-attach','version')) {
    & $HostPath $DllPath $mode
    if ($LASTEXITCODE -ne 0) { throw "Native small-stack $mode regression failed (exit $LASTEXITCODE)." }
}
