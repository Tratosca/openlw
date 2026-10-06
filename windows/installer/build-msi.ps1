# Builds build\OpenLW-<version>-<arch>.msi: service, audio driver, app.
#   powershell -ExecutionPolicy Bypass -File windows\installer\build-msi.ps1 -Arch arm64   (or x64)
# Prerequisites: Rust (MSVC), Visual Studio Build Tools (C++ and Windows SDK), CMake, .NET 10 SDK,
# WiX (dotnet tool install --global wix). Optional Authenticode signing: $env:SIGN_CERT_THUMBPRINT.
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
cargo build --release --manifest-path daemon\Cargo.toml -p lw-daemon --target $rust
if ($LASTEXITCODE) { throw "cargo build" }
$daemon = Join-Path $root "daemon\target\$rust\release\lw-daemon.exe"

Write-Host "=== Audio driver ($cmakeArch)"
& powershell -NoProfile -ExecutionPolicy Bypass -File windows\driver\fetch-sdk.ps1
cmake -S windows\driver -B "$out\driver" -A $cmakeArch | Out-Null
if ($LASTEXITCODE) { throw "cmake configure" }
cmake --build "$out\driver" --config Release
if ($LASTEXITCODE) { throw "cmake build" }
$driver = Join-Path $out "driver\Release\OpenLWDriver.dll"

Write-Host "=== App ($rid)"
dotnet publish windows\app\OpenLW.csproj -c Release -r $rid -p:Platform=$cmakeArch -o "$out\app"
if ($LASTEXITCODE) { throw "dotnet publish" }

if ($env:SIGN_CERT_THUMBPRINT) {
    Write-Host "=== Authenticode signing"
    signtool sign /sha1 $env:SIGN_CERT_THUMBPRINT /fd sha256 /tr http://timestamp.digicert.com /td sha256 `
        $daemon $driver "$out\app\OpenLW.exe"
}

Write-Host "=== MSI"
$msi = Join-Path $root "build\OpenLW-$Version-$Arch.msi"
wix extension add -g WixToolset.Util.wixext WixToolset.Firewall.wixext | Out-Null
wix build windows\installer\Package.wxs -arch $Arch -culture fr-FR `
    -ext WixToolset.Util.wixext -ext WixToolset.Firewall.wixext `
    -d DaemonExe=$daemon -d DriverDll=$driver -d AppDir="$out\app" -d Version=$Version `
    -o $msi
if ($LASTEXITCODE) { throw "wix build" }
if ($env:SIGN_CERT_THUMBPRINT) {
    signtool sign /sha1 $env:SIGN_CERT_THUMBPRINT /fd sha256 /tr http://timestamp.digicert.com /td sha256 $msi
}
Write-Host "OK : $msi"
