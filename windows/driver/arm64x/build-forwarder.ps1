# Builds the ARM64X pure forwarder OpenLWDriver.dll next to OpenLWDriver_arm64.dll and
# OpenLWDriver_x64.dll: one COM registration serves native ARM64 host applications and x64 ones
# emulated on ARM64 (the loader picks the DLL matching the process).
# https://learn.microsoft.com/windows/arm/arm64x-build#building-an-arm64x-pure-forwarder-dll
#   powershell -ExecutionPolicy Bypass -File windows\driver\arm64x\build-forwarder.ps1 `
#       -Arm64Dll <OpenLWDriver_arm64.dll> -X64Dll <OpenLWDriver_x64.dll> -OutDir <folder>
# Requires the MSVC ARM64 tools (cl /arm64EC, link /machine:arm64x), on an ARM64 or x64 machine.
param(
    [Parameter(Mandatory)] [string]$Arm64Dll,
    [Parameter(Mandatory)] [string]$X64Dll,
    [Parameter(Mandatory)] [string]$OutDir
)
$ErrorActionPreference = "Stop"
$vswhere = "${env:ProgramFiles(x86)}\Microsoft Visual Studio\Installer\vswhere.exe"
$vs = & $vswhere -latest -products * -requires Microsoft.VisualStudio.Component.VC.Tools.ARM64 -property installationPath
if (-not $vs) { throw "MSVC ARM64 tools not found (Visual Studio component VC.Tools.ARM64)" }
$vcvars = Join-Path $vs "VC\Auxiliary\Build\vcvarsall.bat"
$tools = if ($env:PROCESSOR_ARCHITECTURE -eq "ARM64") { "arm64" } else { "amd64_arm64" }

New-Item -ItemType Directory -Force $OutDir | Out-Null
Copy-Item $Arm64Dll (Join-Path $OutDir "OpenLWDriver_arm64.dll")
Copy-Item $X64Dll (Join-Path $OutDir "OpenLWDriver_x64.dll")
$work = Join-Path $OutDir "obj-arm64x"
New-Item -ItemType Directory -Force $work | Out-Null

# The forwarder has no code: two empty objects (ARM64, ARM64EC), and one export table per
# architecture pointing to the matching DLL.
Set-Content -Encoding ascii (Join-Path $work "empty.cpp") ""
$exports = "DllGetClassObject", "DllCanUnloadNow", "DllRegisterServer", "DllUnregisterServer"
foreach ($a in "arm64", "x64") {
    @("EXPORTS") + ($exports | ForEach-Object { "    $_ = OpenLWDriver_$a.$_" }) |
        Set-Content -Encoding ascii (Join-Path $work "forward_$a.def")
}
@(
    "@echo off",
    "call `"$vcvars`" $tools >nul || exit /b 1",
    "cl /nologo /c /Foempty_arm64.obj empty.cpp || exit /b 1",
    "cl /nologo /c /arm64EC /Foempty_x64.obj empty.cpp || exit /b 1",
    # Import libraries carry the forwarded symbols. LNK4104 (COM entry points not PRIVATE) is
    # expected: the forwarder's own import library is never used.
    "link /lib /nologo /ignore:4104 /machine:arm64 /def:forward_arm64.def /out:forward_arm64.lib || exit /b 1",
    "link /lib /nologo /ignore:4104 /machine:x64 /def:forward_x64.def /out:forward_x64.lib || exit /b 1",
    ("link /nologo /ignore:4104 /dll /noentry /machine:arm64x /defArm64Native:forward_arm64.def /def:forward_x64.def " +
        "empty_arm64.obj empty_x64.obj forward_arm64.lib forward_x64.lib /out:..\OpenLWDriver.dll || exit /b 1")
) | Set-Content -Encoding ascii (Join-Path $work "build.cmd")
Push-Location $work
try {
    cmd /c "build.cmd < NUL"  # No tool may wait for input
    if ($LASTEXITCODE) { throw "ARM64X forwarder build failed" }
} finally {
    Pop-Location
}
Write-Host "OK : $(Join-Path $OutDir 'OpenLWDriver.dll') (ARM64X)"
