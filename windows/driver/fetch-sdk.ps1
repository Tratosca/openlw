# Download Steinberg ASIO SDK (dual license; used here under GPLv3) to sdk\,
# after hash verification. SDK is never versioned in repository.
#   powershell -ExecutionPolicy Bypass -File windows\driver\fetch-sdk.ps1
$ErrorActionPreference = "Stop"
Set-Location $PSScriptRoot
$Url = "https://download.steinberg.net/sdk_downloads/ASIO-SDK_2.3.4_2025-10-15.zip"
$Sha256 = "d5ebf0c20dd2c5f43771fd0c1418f4b361bf52434ee670097cfa6b3a335e2eca"
if (Test-Path "sdk\ASIOSDK\common\iasiodrv.h") { exit 0 }
New-Item -ItemType Directory -Force sdk | Out-Null
Invoke-WebRequest $Url -OutFile sdk\asiosdk.zip
$h = (Get-FileHash sdk\asiosdk.zip -Algorithm SHA256).Hash.ToLower()
if ($h -ne $Sha256) { throw "unexpected ASIO SDK hash: $h" }
Expand-Archive sdk\asiosdk.zip -DestinationPath sdk -Force
Remove-Item sdk\asiosdk.zip
Write-Host "ASIO SDK: $PSScriptRoot\sdk\ASIOSDK"
