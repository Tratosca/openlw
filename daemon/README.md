# Daemon — Rust Livewire / AES67 stack

Cargo workspace for OpenLW's network service ([ADR 0001](../docs/adr/0001-daemon-rust.md)), for macOS, Linux, and Windows on x86_64 and ARM64 ([ADR 0006](../docs/adr/0006-cross-platform.md)).

| Crate | Purpose | Status |
|---|---|---|
| `lw-proto` | Pure codecs, no I/O: channels ↔ groups, stream formats, RTP and L24, TlvMsg, Envelope, advertisements (ADV), SDP, PTPv2, Livewire clock decoding | Implemented |
| `lw-sys` | System layer, one C implementation per system (`csrc/macos`, `csrc/linux`, `csrc/windows`, `csrc/posix`): real-time threads (Mach, `SCHED_FIFO`, MMCSS), precise sleep, host clock, logging, **shared region with the audio client** (`csrc/lw_shm.h`/`.c` v2: SPSC rings, clock seqlock), control channel (`ctl`: XPC, Unix socket, named pipe), qWAVE DSCP, Windows service. Only crate with `unsafe`; every block justified (`SAFETY:`) | Implemented |
| `lw-pw` | Linux: PipeWire nodes of the device (“OpenLW Out” sink → TO_NET ring, “OpenLW In” source ← FROM_NET ring), served by the daemon itself, with reconnection when PipeWire restarts; empty crate on other systems ([ADR 0009](../docs/adr/0009-audio-linux.md)) | Implemented |
| `lw-daemon` | NIC-bound sockets (`IP_BOUND_IF`, `SO_BINDTODEVICE`, `IP_MULTICAST_IF`), real-time RTP transmission (test generator), reception with statistics, ADV advertisements, JSON configuration, control channel (`status`, `ping`, `attach`, patching), virtual device (clock, peaks, internal loopback) | Implemented; PTP slave pending |

Each `lw-proto` module links to its [protocol document](../docs/protocol/README.md). Decoders process unauthenticated traffic: invalid inputs return errors without panicking. `unsafe` is forbidden in the workspace, and `clippy::indexing_slicing` flags indexed access outside tests.

## Commands (from `daemon/`)

```sh
cargo test                                   # Unit, vector, and robustness tests
cargo clippy --all-targets                   # Expect no warnings
cargo build --release                        # Current system
../tools/ci/test-wine.sh                     # Windows ARM64 build tested under Wine (Docker)
```

## Using `lw-daemon`

```sh
cargo build --release
target/release/lw-daemon ifaces                                   # System name, IP, index, friendly name
target/release/lw-daemon send --iface en7 --channel 4001 --format standard --advertise --name "MAC 1"
target/release/lw-daemon recv --iface en7 --channel 1             # Statistics every second
target/release/lw-daemon run --config lw-daemon.json --control    # Format: src/config.rs
target/release/lw-daemon ctl status                               # Service control channel
```

Control channel ([ADR 0007](../docs/adr/0007-control-channel.md)): `--control` without a value publishes the installed service endpoint (XPC on macOS, Unix socket on Linux, named pipe on Windows); use `--control unix:/tmp/lw.sock` or `pipe:NAME` for testing. On Windows, the Service Control Manager starts `lw-daemon service` ([windows/README.md](../windows/README.md)).

Formats: `standard` (240 samples), `aes67` (48), `livestream` (12), `surround` (60, eight channels, 239.196). Use `--tos 136` for AF41. Multicast loopback is disabled on a physical NIC: a transmitter does not receive its own streams.

macOS universal binary (ADR 0004 minimum, measured: x86_64 `LC_VERSION_MIN_MACOSX` 10.13, arm64 `minos` 11.0):

```sh
cargo build --release --target x86_64-apple-darwin && cargo build --release --target aarch64-apple-darwin
lipo -create -output target/lw-daemon-universal target/{x86_64,aarch64}-apple-darwin/release/lw-daemon
```

## Real-time scheduling

Transmit threads enter real-time scheduling and wait for deadlines without busy-waiting: `THREAD_TIME_CONSTRAINT_POLICY` and `mach_wait_until` on macOS, `SCHED_FIFO` and `clock_nanosleep` on Linux, MMCSS “Pro Audio” and a high-resolution timer on Windows. The measurements below are from macOS. Busy-waiting would exceed the declared compute budget and cause kernel demotion: an initial test showed two-second gaps. Maximum measured lateness on `lo0`, five seconds per measurement, ten cores saturated with `yes` under load:

| Stream | Normal, idle | Normal, loaded | Real-time, idle | Real-time, loaded |
|---|---|---|---|---|
| AES67 (1,000 pkt/s) | 9.3 ms | 14 ms, 1,156 late packets | 330 µs | 20 µs, 0 late |
| Livestream (4,000 pkt/s) | 1.1 ms | 12 ms, 13,703 late packets | 42 µs | 38 µs, 0 late |

`--no-rt` reproduces normal scheduling for comparison.

## Virtual device (shared region)

One C contract: [lw-sys/csrc/lw_shm.h](lw-sys/csrc/lw_shm.h) and `lw_shm.c`, compiled **unchanged** by the HAL plugin and Windows driver. Rust duplicates no memory layout: the daemon uses these C functions.

- Region = 4 KiB header + `TO_NET` ring (applications → network; producer: plugin) + `FROM_NET` ring (network → applications; producer: daemon), interleaved float32, lock-free SPSC, 64-bit positions, overrun/underrun counters.
- Seqlock clock: (host time, sample position, rate scalar); the header declares the host clock (`mach_absolute_time`, `QueryPerformanceCounter`, or `CLOCK_MONOTONIC`). The plugin uses it in `GetZeroTimeStamp` (ADR 0003).
- Transfer: client sends `{"cmd":"attach"}` requesting the region; `xpc_shmem` object attached on macOS, section duplicated into the client process on Windows. On Linux, the region stays in the daemon.
- A daemon real-time thread running every 1 ms publishes the clock, measures per-channel peaks, and, with `"device": {"loopback": true}`, returns application output to application input.

Configuration (`"device"`, enabled by default with `--xpc`): `channels_to_net` and `channels_from_net` (default 8, maximum 64), `ring_frames` (default 8192, or 170 ms), `loopback`.

## Discovery and patching

Goal: record a Livewire channel and transmit on a selected channel.

```sh
lw-daemon ctl set-iface en7                     # Livewire interface (BSD or friendly name)
lw-daemon ctl sources                           # Advertised sources (channel, name, terminal)
lw-daemon ctl patch-in --channel 21 --to 1,2    # Livewire channel 21 → device inputs 1–2
lw-daemon ctl patch-out --from 1,2 --channel 4001 --name "MAC 1"   # Outputs 1–2 → channel 4001
lw-daemon ctl unpatch-in --to 1,2 ; lw-daemon ctl unpatch-out --channel 4001
lw-daemon ctl status                            # Streams, routes (priming, slips), peaks
lw-daemon discover --iface en7 --seconds 30     # Discovery without the daemon
```

- **Discovery:** the daemon listens to advertisements (239.192.255.3:4001), accumulates full-advertisement pages, and sends `READ` to unknown terminals. If a terminal does not respond, its sources appear only with the next full advertisement (up to 2–3 min).
- **Live patching:** each command validates and saves the new configuration (`lw-daemon.json`), then reloads the network session without touching the device: the plugin retains its region. Patching an occupied input releases it first.
- **Authorization on macOS:** patch-changing commands require root or `admin` membership (XPC caller's effective UID); `status`, `sources`, and `config` are unrestricted. Other platforms use the caller rules in ADR 0007.
- **Clocks:** received streams follow the transmitter clock; the device follows the Mac clock. The jitter buffer absorbs network jitter and compensates for drift by slipping (discarding or repriming; `slips` and `underruns` counters in `status`). Occasional slight audible jumps may occur until clock synchronization is implemented (ADR 0003, adaptive resampling).
- **Input latency:** approximately 12 ms jitter buffer, plus the application's I/O block and a 256-frame margin (5.3 ms) managed by the plugin, which discards late audio.

## Logging and control

- Unified log: `log stream --info --predicate 'subsystem == "fr.francois-brille.openlw"'` (`daemon` category); stderr in interactive use.
- XPC control: `lw-daemon run --config … --xpc` publishes Mach service `fr.francois-brille.openlw.daemon`, which launchd must declare ([../macos/launchd/fr.francois-brille.openlw.daemon.plist](../macos/launchd/fr.francois-brille.openlw.daemon.plist), installed by the package). Query with `lw-daemon ctl status` (system domain) or `lw-daemon ctl status --user` (test LaunchAgent). JSON protocol: [lw-daemon/src/control.rs](lw-daemon/src/control.rs).
- A Mach service cannot be published outside launchd: tests use an anonymous XPC listener in the same process.

## Tests

- **Contract** (`tests/vectors.rs`): reads `docs/protocol/vectors/` (generated by `tools/lw/make_vectors.py`) and re-encodes advertisements and SDP byte for byte. Independent Python and Rust implementations must agree.
- **C layer** (`lw-sys`): real-time promotion, real-time sleep accuracy, `os_log`, XPC round trip (50 requests), contained handler panic, unknown service.
- **Shared region** (`lw-sys/src/shm.rs`): geometry, unique endpoints, wrapping and counters, 200,000-frame SPSC stress through two real XPC-transferred mappings (no tearing), seqlock with 200,000 concurrent reads.
- **Device** (`lw-daemon/tests/device.rs`): mock plugin attaches through XPC, reads the clock (48 kHz within ±1 ms), writes a 0.5 s sine wave, and reads it back sample-for-sample through internal loopback; per-channel peaks within 0.1 dB.
- **Patching** (`lw-daemon/tests/patch.rs`, `src/editor.rs`, `src/patch.rs`): simulated Livewire terminal on lo0 transmits channel 21; `patch_input` through the control command reloads the session, with inputs 3–4 at −12 dBFS and others silent; `patch_output` sends outputs 1–2 on channel 4005, received elsewhere at −6 dBFS without loss; unauthorized caller rejected.
- **Discovery** (`lw-daemon/tests/discovery.rs`): reconstructed simulated terminal with ten sources (two pages).
- **Control** (`lw-daemon/src/control.rs`): `status`, `ping`, errors; also end to end through XPC.
- **Local loopback** (`lw-daemon/tests/loopback.rs`): Standard, AES67, and surround sent and received on `lo0` (no loss, Δts, payload, SSRC, −20 dBFS level); ten-source advertisement decoded as two pages plus one short advertisement.
- **Robustness** (`tests/robustness.rs`): 20,000 random inputs and 20,000 mutations of valid packets passed to all decoders. Substitute for `cargo-fuzz`, which requires the nightly toolchain.
