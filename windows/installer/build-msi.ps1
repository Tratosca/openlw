# Builds build\OpenLW-<version>-<arch>.msi: service, audio driver, app.
#   powershell -ExecutionPolicy Bypass -File windows\installer\build-msi.ps1 -Arch arm64   (or x64)
# Prerequisites: Rust (MSVC), Visual Studio Build Tools (C++ and Windows SDK), CMake, .NET 10 SDK,
# WiX 5.0.2 (dotnet tool install --global wix --version 5.0.2): WiX 7 requires accepting the Open
# Source Maintenance Fee EULA, WiX 5 does not. Optional Authenticode signing: $env:SIGN_CERT_THUMBPRINT.
param(
    [ValidateSet("arm64", "x64")] [string]$Arch = "arm64",
    [string]$Version = "0.5.0"
)
$ErrorActionPreference = "Stop"
$root = Resolve-Path "$PSScriptRoot\..\.."
Set-Location $root
$rust = @{ arm64 = "aarch64-pc-windows-msvc"; x64 = "x86_64-pc-windows-msvc" }[$Arch]
$cmakeArch = @{ arm64 = "ARM64"; x64 = "x64" }[$Arch]
$rid = "win-$Arch"
$out = Join-Path $root "build\windows-$Arch"
New-Item -ItemType Directory -Force $out | Out-Null

Write-Host "=== Service ($rust)"
# From daemon\: Cargo reads .cargo\config.toml from the current directory (static C runtime).
Push-Location daemon
cargo build --release -p lw-daemon --target $rust
$code = $LASTEXITCODE
Pop-Location
if ($code) { throw "cargo build" }
$daemon = Join-Path $root "daemon\target\$rust\release\lw-daemon.exe"

Write-Host "=== Audio driver ($cmakeArch)"
& powershell -NoProfile -ExecutionPolicy Bypass -File windows\driver\fetch-sdk.ps1
function Build-Driver([string]$cmArch, [string]$dir, [string]$name) {
    cmake -S windows\driver -B $dir -A $cmArch "-DOPENLW_DRIVER_NAME=$name" | Out-Null
    if ($LASTEXITCODE) { throw "cmake configure ($cmArch)" }
    cmake --build $dir --config Release | Out-Host  # Function output is the DLL path only
    if ($LASTEXITCODE) { throw "cmake build ($cmArch)" }
    Join-Path $dir "Release\$name.dll"
}
$driverDir = Join-Path $out "driver-dlls"
if ($Arch -eq "arm64") {
    # x64 host applications emulated on ARM64 cannot load an ARM64 DLL: ARM64 and x64 builds
    # behind an ARM64X forwarder registered once as OpenLWDriver.dll.
    $arm64Dll = Build-Driver "ARM64" "$out\driver" "OpenLWDriver_arm64"
    $x64Dll = Build-Driver "x64" "$out\driver-x64" "OpenLWDriver_x64"
    & powershell -NoProfile -ExecutionPolicy Bypass -File windows\driver\arm64x\build-forwarder.ps1 `
        -Arm64Dll $arm64Dll -X64Dll $x64Dll -OutDir $driverDir
    if ($LASTEXITCODE) { throw "ARM64X forwarder" }
    $driverFiles = "OpenLWDriver.dll", "OpenLWDriver_arm64.dll", "OpenLWDriver_x64.dll" | ForEach-Object { Join-Path $driverDir $_ }
} else {
    $dll = Build-Driver "x64" "$out\driver" "OpenLWDriver"
    New-Item -ItemType Directory -Force $driverDir | Out-Null
    Copy-Item $dll $driverDir
    $driverFiles = @(Join-Path $driverDir "OpenLWDriver.dll")
}

Write-Host "=== App ($rid)"
dotnet publish windows\app\OpenLW.csproj -c Release -r $rid -p:Platform=$cmakeArch -o "$out\app"
if ($LASTEXITCODE) { throw "dotnet publish" }

if ($env:SIGN_CERT_THUMBPRINT) {
    Write-Host "=== Authenticode signing"
    signtool sign /sha1 $env:SIGN_CERT_THUMBPRINT /fd sha256 /tr http://timestamp.digicert.com /td sha256 `
        $daemon @driverFiles "$out\app\OpenLW.exe"
}

Write-Host "=== MSI"
$wixVersion = "5.0.2"
if (-not ((wix --version) -like "$wixVersion*")) { throw "WiX $wixVersion attendu (dotnet tool install --global wix --version $wixVersion)" }
$msi = Join-Path $root "build\OpenLW-$Version-$Arch.msi"
wix extension add -g "WixToolset.Util.wixext/$wixVersion" "WixToolset.Firewall.wixext/$wixVersion"
if ($LASTEXITCODE) { throw "wix extension add" }
wix build windows\installer\Package.wxs -arch $Arch `
    -ext "WixToolset.Util.wixext/$wixVersion" -ext "WixToolset.Firewall.wixext/$wixVersion" `
    -d DaemonExe=$daemon -d DriverDir=$driverDir -d AppDir="$out\app" -d Version=$Version `
    -o $msi
if ($LASTEXITCODE) { throw "wix build" }
if ($env:SIGN_CERT_THUMBPRINT) {
    signtool sign /sha1 $env:SIGN_CERT_THUMBPRINT /fd sha256 /tr http://timestamp.digicert.com /td sha256 $msi
}
Write-Host "OK : $msi"
