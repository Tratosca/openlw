# OpenLW Windows audio driver

<img src="../../docs/assets/asio-compatible-logo.png" alt="ASIO Compatible" height="64" align="right">

User-mode ASIO® driver: ASIO-compatible audio applications (DAWs, broadcast software) see it as **“OpenLW”**. Decision: [ADR 0008](../../docs/adr/0008-audio-windows.md).

- In-process COM DLL `OpenLWDriver.dll`, Windows 10 22H2 and 11, x64 and ARM64; static C runtime (no Visual C++ Redistributable).
- On ARM64, `OpenLWDriver.dll` is an ARM64X forwarder to `OpenLWDriver_arm64.dll` and `OpenLWDriver_x64.dll`: native ARM64 hosts and x64 hosts under emulation use the same registration.
- 48 kHz, 32-bit float samples, 64–2048-frame buffers (default 256).
- Channels and names follow the OpenLW device, configured in the app (input/output patches).
- One host application at a time; a second receives an explicit “OpenLW is already in use by another application (…)” error.

## License

This directory is licensed under **GPL version 3** ([LICENSE](LICENSE)): the driver is built with Steinberg's ASIO SDK, used under GPLv3. The rest of OpenLW is Apache 2.0; the driver uses its GPLv3-compatible system and shared-region layers (`daemon/lw-sys/csrc`) and communicates with the service only through IPC.

The SDK is not versioned: `fetch-sdk.ps1` (Windows) or `fetch-sdk.sh` downloads it from steinberg.net and checks its SHA-256 hash.

## Build

Prerequisites: Visual Studio Build Tools (C++), CMake 3.20 or later.

```powershell
powershell -ExecutionPolicy Bypass -File windows\driver\fetch-sdk.ps1
cmake -S windows\driver -B build\driver -A ARM64      # Or -A x64
cmake --build build\driver --config Release
```

Development installation (elevated PowerShell, OpenLW service running; see [windows/README.md](../README.md)):

```powershell
Copy-Item build\driver\Release\OpenLWDriver.dll "$env:ProgramFiles\OpenLW\"
regsvr32 "$env:ProgramFiles\OpenLW\OpenLWDriver.dll"       # Remove: regsvr32 /u …
```

For x64 hosts on ARM64, build both architectures (`-DOPENLW_DRIVER_NAME=OpenLWDriver_arm64`, `-A x64 -DOPENLW_DRIVER_NAME=OpenLWDriver_x64`) and generate the forwarder with `windows\driver\arm64x\build-forwarder.ps1` (requires the MSVC ARM64 tools); `windows\installer\build-msi.ps1 -Arch arm64` does all of this.

Registration creates `HKCR\CLSID\{4F9DD084-E18A-4E0E-8D38-C2854029E846}` and `HKLM\SOFTWARE\ASIO\OpenLW`. Host applications' Control Panel button opens the OpenLW app (`OpenLW.exe`, installed beside the driver).

## Test

`openlw-driver-test.exe` loads the driver as a host application, plays a sine wave on two outputs, and checks that it returns on two inputs through the service's internal loopback (configuration `"device": {"loopback": true}`):

```powershell
lw-daemon.exe run --config loopback.json --control
build\driver\Release\openlw-driver-test.exe "$PWD\build\driver\Release\OpenLWDriver.dll"
```

The installed driver is tested the same way, with the registered path, by both the ARM64 and x64 test hosts on Windows 11 ARM64.

Without a Windows machine, `tools/ci/test-wine.sh` (ARM64, native Wine on an arm64 host) builds the driver with llvm-mingw and runs the same test, including rejection of a second process. CI runs it on Windows x64 and ARM64 with MSVC.

## Operation

- `init`: connect to the service's named pipe, read device geometry (channels, names, latency margin), and `attach` requesting the region; the service duplicates the shared section into the host process. The region must declare the `QueryPerformanceCounter` clock.
- MMCSS “Pro Audio” audio thread: buffer exchanges follow the service's published clock; inputs are read from the network → applications ring with the service latency margin (catch-up beyond two margins); outputs are written to the applications → network ring. Uses `ASIOTime` when supported by the host, with timestamps derived from `timeGetTime()` as required by the Windows ASIO specification.
- Monitoring: a second connection rereads geometry every 2 s; device recreation (channel count, generation) or service loss triggers `kAsioResetRequest` to the host.
- Release: `detach` on driver destruction; if the host process disappears without notice, the service detects it at the next `attach`.

## Limitations

- Applications without ASIO support (browsers, conferencing) cannot see OpenLW: see the WaveRT kernel driver in [docs/roadmap.md](../../docs/roadmap.md).
- 32-bit host applications: not supported (no x86 build, no `WOW6432Node` registration).
- Pending real-world tests: commercial hosts (Reaper, broadcast software), coexistence with the Axia driver.

---
ASIO is a registered trademark of Steinberg Media Technologies GmbH.
