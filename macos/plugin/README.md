# Plugin — CoreAudio device (AudioServerPlugIn)

HAL plugin loaded by `coreaudiod`: **“OpenLW”** device (manufacturer “François Brille”), float32 at 48 kHz. Channel count is set by the daemon (`device` configuration section, configured through OpenLW, 1–32 per direction). `LWChannelsFromNet` and `LWChannelsToNet` in [Info.plist](Info.plist) (2 and 2) are startup defaults before the daemon's first response.

- **Input latency:** bounded by the plugin, which knows the host block size (512–4096 frames and beyond). The daemon fills the ring while space remains. Before reading N frames, the plugin waits for N + 256 (priming, silence while waiting), then discards excess beyond N + 512. The ring is drained at I/O startup (stale audio). This corrects recording with 2048/4096-frame blocks (Audacity), which previously received silence beyond 1024 frames.
- **Layout** ([ADR 0010](../../docs/adr/0010-macos-device-layouts.md)), according to `layout` in the `geometry` response: one duplex “OpenLW” device (object 2, streams 3 and 4, UID `fr.francois-brille.openlw.device`), or numbered devices “OpenLW In n” (object 100 + 2(n − 1), input stream + 1, UID `….device.in.n`) and “OpenLW Out n” (200 + 2(n − 1), UID `….device.out.n`), n = 1..16, with widths from `in_widths` / `out_widths`. Each device has its own I/O clock and its own ring in the daemon region (region v3, TO_NET rings first). Unpublished objects answer no property.
- **Names:** `geometry` contains device names (`name`; `in_device_names`, `out_device_names` in multi layout) and channel names (`input_names`, `output_names`, devices concatenated in order; `kAudioObjectPropertyElementName`, e.g. “2 - Studio A L”). Changes notify the host through `PropertiesChanged`.
- **Live changes:** a monitoring queue polls `geometry` every 2 s. A device whose width changes gets a host configuration change (`RequestDeviceConfigurationChange`); `PerformDeviceConfigurationChange` applies its new width. A new region generation detaches the region and reattaches at once if I/O runs: devices whose width is unchanged resume without reconfiguration. Each I/O operation checks that its ring has the device's width and outputs silence otherwise. Detached regions are unmapped only on the next detach, as another device's I/O may still read them. The same queue attaches the region when I/O runs without it (daemon started after the application).
- **Audio:** through the daemon's shared region ([../../daemon/lw-sys/csrc/lw_shm.h](../../daemon/lw-sys/csrc/lw_shm.h), compiled unchanged here), obtained on first `StartIO` through XPC (`attach` request, service `fr.francois-brille.openlw.daemon`, declared in `AudioServerPlugIn_MachServices`).
- **Without the daemon:** the device remains visible and silent; the error is logged.
- **Real-time:** `GetZeroTimeStamp` and `DoIOOperation` use no locks, allocations, or system calls.
- **Log:** `log stream --info --predicate 'subsystem == "fr.francois-brille.openlw" AND category == "plugin"'`.

## Build and test without installation

```sh
make            # build/OpenLW.driver: universal, ad hoc signed
make check-min  # x86_64 LC_VERSION_MIN_MACOSX 10.13, arm64 minos 11.0
make test       # Harness: load the bundle like coreaudiod and simulate the daemon
```

The [test harness](test/harness.c) checks:

- CFPlugIn loading and factory;
- HAL-visible properties;
- silence without the daemon;
- XPC attachment, then 100 cycles of 512 identical frames in each direction;
- zero-timestamp period (16,384 frames = 0.341333 s);
- channel-count change, device and channel names, input margin, 4096-frame host blocks;
- numbered devices: list, UIDs, widths 1/8/2, one ring per device without crosstalk, a single targeted reconfiguration when one width changes (the other devices resume on the new region, the changed one stays silent until reconfigured), renaming, return to duplex.

## Install (sudo; restarts coreaudiod)

```sh
make install     # Copy to /Library/Audio/Plug-Ins/HAL, root:wheel, then killall coreaudiod
make uninstall   # Remove and restart coreaudiod
```

Verification: the device appears in Audio MIDI Setup and System Settings > Sound. To carry audio, the daemon must run as a LaunchDaemon with the XPC service ([../launchd/](../launchd/)): a Mach service cannot be published outside launchd.

`killall coreaudiod` briefly interrupts audio in every application. Ad hoc signing suffices for local testing; distribution will use Developer ID and notarization (M8).

## Real-system validation (2026-10-05, macOS 27, Apple Silicon)

Installed using `macos/scripts/install-dev.sh` (daemon as LaunchDaemon, development configuration with internal loopback):

- **Device:** “OpenLW” visible through the HAL (`system_profiler SPAudioDataType`: 8 inputs, 8 outputs, 48 kHz, virtual transport, usable as default output). The plugin runs in `Core-Audio-Driver-Service.helper` and reaches the daemon's XPC service from its sandbox.
- **Audio loopback:** a CoreAudio client plays a 1 kHz sine wave at −12 dBFS on channels 1 and 2 and records the input.

| Measurement | Result |
|---|---|
| Frames played / received | 144,384 / 144,384 (3 s) |
| Input peak | −12.0 dBFS on channels 1 and 2, silence on channels 3–8 |
| Round-trip latency | 10.7 ms (one 512-frame buffer) |
| Loss | 0 overruns; 512 silent frames during priming |

`say -a` does not list every device and crashes on a device with a short name (“FM”): this is a `say` bug unrelated to the plugin. Use an application with device selection or a CoreAudio client for testing.

## Known limitations

- FIFO ring: latency follows occupancy; no sample-time indexing yet (`mInputTime` / `mOutputTime`).
- Nominal clock (48 kHz host clock): synchronization to the daemon's published network clock (`rate_scalar`) remains to be implemented.
- Attachment only at `StartIO`: if the daemon restarts during I/O, the plugin remains on the old region (silence) until the next I/O stop/start.
