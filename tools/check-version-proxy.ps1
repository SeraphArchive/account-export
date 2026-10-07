#Requires -Version 7.0
param([Parameter(Mandatory)][string]$DllPath, [Parameter(Mandatory)][string]$VersionedFile)
$ErrorActionPreference = 'Stop'
Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
public static class AccountExportVersionSmoke {
    [UnmanagedFunctionPointer(CallingConvention.Winapi, CharSet=CharSet.Unicode)]
    public delegate uint Size(string file, out uint handle);
    [UnmanagedFunctionPointer(CallingConvention.Winapi, CharSet=CharSet.Unicode)]
    [return: MarshalAs(UnmanagedType.Bool)]
    public delegate bool Info(string file, uint handle, uint length, IntPtr data);
    [UnmanagedFunctionPointer(CallingConvention.Winapi, CharSet=CharSet.Unicode)]
    [return: MarshalAs(UnmanagedType.Bool)]
    public delegate bool Query(IntPtr block, string subblock, out IntPtr value, out uint length);
}
'@
$module = [Runtime.InteropServices.NativeLibrary]::Load([IO.Path]::GetFullPath($DllPath))
try {
    foreach ($name in @('GetFileVersionInfoA','GetFileVersionInfoW','GetFileVersionInfoExA','GetFileVersionInfoExW',
        'GetFileVersionInfoSizeA','GetFileVersionInfoSizeW','GetFileVersionInfoSizeExA','GetFileVersionInfoSizeExW',
        'VerQueryValueA','VerQueryValueW','VerFindFileA','VerFindFileW','VerInstallFileA','VerInstallFileW','VerLanguageNameA','VerLanguageNameW')) {
        [Runtime.InteropServices.NativeLibrary]::GetExport($module, $name) | Out-Null
    }
    $sizeApi = [Runtime.InteropServices.Marshal]::GetDelegateForFunctionPointer(
        [Runtime.InteropServices.NativeLibrary]::GetExport($module, 'GetFileVersionInfoSizeW'), [AccountExportVersionSmoke+Size])
    $infoApi = [Runtime.InteropServices.Marshal]::GetDelegateForFunctionPointer(
        [Runtime.InteropServices.NativeLibrary]::GetExport($module, 'GetFileVersionInfoW'), [AccountExportVersionSmoke+Info])
    $queryApi = [Runtime.InteropServices.Marshal]::GetDelegateForFunctionPointer(
        [Runtime.InteropServices.NativeLibrary]::GetExport($module, 'VerQueryValueW'), [AccountExportVersionSmoke+Query])
    [uint32]$handle = 0
    $size = $sizeApi.Invoke([IO.Path]::GetFullPath($VersionedFile), [ref]$handle)
    if ($size -eq 0) { throw 'Proxy returned no version resource.' }
    $buffer = [Runtime.InteropServices.Marshal]::AllocHGlobal([int]$size)
    try {
        if (-not $infoApi.Invoke([IO.Path]::GetFullPath($VersionedFile), 0, $size, $buffer)) { throw 'Version resource read failed.' }
        $value = [IntPtr]::Zero; [uint32]$length = 0
        if (-not $queryApi.Invoke($buffer, '\', [ref]$value, [ref]$length)) { throw 'Version resource query failed.' }
        if ($length -lt 52 -or [Runtime.InteropServices.Marshal]::ReadInt32($value) -ne -17890115) { throw 'Invalid fixed version info.' }
    } finally { [Runtime.InteropServices.Marshal]::FreeHGlobal($buffer) }
    Write-Host 'Version proxy passed: 16 exports and real version-resource read/query.'
} finally { [Runtime.InteropServices.NativeLibrary]::Free($module) }
