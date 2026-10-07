//! `lw-daemon`: Livewire / AES67 tool and daemon.
//!
//!     lw-daemon ifaces
//!     lw-daemon send --iface en7 --channel 4001 --format standard [--advertise --name "MAC 1"]
//!     lw-daemon recv --iface en7 --channel 1
//!     lw-daemon run --config lw-daemon.json [--control [ENDPOINT]] [--init-config] [--log-file F]
//!     lw-daemon service              (Windows, started by the Service Control Manager)
//!     lw-daemon ctl status [--endpoint ENDPOINT]

use std::net::Ipv4Addr;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use clap::{Parser, Subcommand};
use lw_daemon::config::{Config, Format, Kind};
use lw_daemon::net::TxOptions;
use lw_daemon::rx::RxStats;
use lw_daemon::{advertise, iface, info, rx, tx, Stop};
use lw_proto::adv::{AdvStreamType, Source};
use lw_proto::channel::{Channel, AUDIO_PORT};
use lw_sys::ctl::Endpoint;

#[derive(Parser)]
#[command(version, about = "OpenLW Livewire / AES67 daemon and tool")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// List IPv4 interfaces (system name, friendly name, address).
    Ifaces {
        #[arg(long)]
        json: bool,
    },
    /// Transmit a test stream (sine wave) to a Livewire channel.
    Send {
        #[arg(long)]
        iface: String,
        #[arg(long)]
        channel: u16,
        #[arg(long, value_enum, default_value = "standard")]
        format: CliFormat,
        /// Duration in seconds (0 = indefinite).
        #[arg(long, default_value_t = 0.0)]
        seconds: f64,
        #[arg(long, default_value_t = 997.0)]
        tone: f64,
        #[arg(long, default_value_t = -20.0, allow_negative_numbers = true)]
        level: f64,
        #[arg(long, default_value_t = 96)]
        pt: u8,
        /// TOS byte (184 = EF, 136 = AF41).
        #[arg(long, default_value_t = 0xB8)]
        tos: u32,
        /// Also advertise the source (ADV) so it appears on Livewire devices.
        #[arg(long)]
        advertise: bool,
        #[arg(long, default_value = "MAC TEST")]
        name: String,
        /// Normal-priority transmit thread (comparison with real-time scheduling).
        #[arg(long)]
        no_rt: bool,
    },
    /// Receive a stream and display statistics every second.
    Recv {
        #[arg(long)]
        iface: String,
        #[arg(long, conflicts_with = "group")]
        channel: Option<u16>,
        #[arg(long, value_enum, default_value = "stereo")]
        kind: CliKind,
        #[arg(long)]
        group: Option<Ipv4Addr>,
        #[arg(long, default_value_t = AUDIO_PORT)]
        port: u16,
        #[arg(long, default_value_t = 0.0)]
        seconds: f64,
        #[arg(long)]
        json: bool,
    },
    /// Run the daemon using a configuration file.
    Run {
        #[arg(long)]
        config: PathBuf,
        #[arg(long, default_value_t = 0.0)]
        seconds: f64,
        /// Publish the control channel: without a value, use the installed service endpoint (XPC on macOS,
        /// Unix socket on Linux, named pipe on Windows); otherwise `unix:PATH`, `pipe:NAME`, `mach:NAME`.
        #[arg(long, num_args = 0..=1, default_missing_value = "service")]
        control: Option<String>,
        /// Legacy form of `--control mach:NAME` (launchd plist).
        #[arg(long, num_args = 0..=1, default_missing_value = lw_sys::ctl::SERVICE_NAME, hide = true)]
        xpc: Option<String>,
        /// Create the default configuration file if absent.
        #[arg(long)]
        init_config: bool,
        /// Also copy logs to this file.
        #[arg(long)]
        log_file: Option<PathBuf>,
    },
    /// Windows service, started by the Service Control Manager: configuration and logs in
    /// %ProgramData%\OpenLW, named-pipe control channel.
    #[cfg(windows)]
    Service {
        #[arg(long)]
        config: Option<PathBuf>,
        #[arg(long)]
        log_file: Option<PathBuf>,
    },
    /// Listen to Livewire advertisements and list network sources.
    Discover {
        #[arg(long)]
        iface: String,
        /// Listening duration; full advertisements may take 2–3 min if the terminal ignores the request.
        #[arg(long, default_value_t = 30.0)]
        seconds: f64,
        #[arg(long)]
        json: bool,
    },
    /// Query or repatch a running daemon through the control channel.
    Ctl {
        #[command(subcommand)]
        action: CtlCmd,
        /// Endpoint: `service` (default), `unix:PATH`, `pipe:NAME`, `mach:NAME`, `mach-user:NAME`.
        #[arg(long, global = true, default_value = "service")]
        endpoint: String,
        /// macOS: Mach service name (overrides `--endpoint`).
        #[arg(long, global = true)]
        service: Option<String>,
        /// macOS: user-domain service (LaunchAgent) rather than system-domain service (LaunchDaemon).
        #[arg(long, global = true)]
        user: bool,
    },
}

#[derive(Subcommand)]
enum CtlCmd {
    /// Daemon, stream, and device status.
    Status,
    /// Livewire sources discovered on the network.
    Sources,
    /// Current configuration.
    Config,
    /// Patch a Livewire channel (or AES67 group) to device inputs.
    PatchIn {
        #[arg(long, conflicts_with = "group")]
        channel: Option<u16>,
        #[arg(long)]
        group: Option<Ipv4Addr>,
        #[arg(long, default_value_t = AUDIO_PORT)]
        port: u16,
        #[arg(long, value_enum, default_value = "stereo")]
        kind: CliKind,
        /// Mono patch onto one input: left, right, or sum (L+R, −6 dB).
        #[arg(long, value_enum)]
        mix: Option<CliMix>,
        /// Multi layout: input device number (OpenLW In n); `--to` is then 1,2 / 1 / 1..8.
        #[arg(long)]
        device: Option<u16>,
        /// Device inputs, e.g. 1,2 (or 3 with `--mix`).
        #[arg(long, value_delimiter = ',', required = true)]
        to: Vec<u16>,
    },
    /// Release device inputs.
    UnpatchIn {
        /// Multi layout: input device number.
        #[arg(long)]
        device: Option<u16>,
        #[arg(long, value_delimiter = ',', required = true)]
        to: Vec<u16>,
    },
    /// Transmit device outputs on a Livewire channel.
    PatchOut {
        /// Multi layout: output device number (OpenLW Out n); `--from` is then 1,2.
        #[arg(long)]
        device: Option<u16>,
        /// Device outputs, e.g. 1,2.
        #[arg(long, value_delimiter = ',', required = true)]
        from: Vec<u16>,
        #[arg(long)]
        channel: u16,
        #[arg(long, default_value = lw_daemon::config::DEFAULT_SOURCE_NAME)]
        name: String,
        #[arg(long, value_enum, default_value = "standard")]
        format: CliFormat,
    },
    /// Stop transmitting a channel.
    UnpatchOut {
        #[arg(long)]
        channel: u16,
    },
    /// Change Livewire network interface: BSD name, friendly name, or “auto”.
    SetIface { iface: String },
    /// Device channel count per direction (1–32).
    SetChannels {
        #[arg(long)]
        to_net: u32,
        #[arg(long)]
        from_net: u32,
    },
    /// Layout (macOS, Linux): duplex (one OpenLW device) or multi (OpenLW In n / OpenLW Out n).
    SetLayout { layout: String },
    /// Multi layout: name devices after their source (otherwise OpenLW In n).
    SetNaming {
        #[arg(action = clap::ArgAction::Set)]
        enabled: bool,
    },
    /// Advanced settings: advertised name, receive latency, network priority.
    SetAdvanced {
        /// Advertised name (empty: computer name).
        #[arg(long)]
        name: Option<String>,
        /// low, normal, or safe.
        #[arg(long)]
        latency: Option<String>,
        /// Audio-stream DSCP (46 = EF, 34 = AF41, 0 = none).
        #[arg(long)]
        dscp: Option<u8>,
    },
    /// Network interfaces, including those receiving Livewire.
    Ifaces,
    /// Device geometry (channel count, generation).
    Geometry,
}

#[derive(Clone, Copy, clap::ValueEnum)]
enum CliFormat {
    Standard,
    Aes67,
    Livestream,
    Surround,
}

impl From<CliFormat> for Format {
    fn from(f: CliFormat) -> Self {
        match f {
            CliFormat::Standard => Format::Standard,
            CliFormat::Aes67 => Format::Aes67,
            CliFormat::Livestream => Format::Livestream,
            CliFormat::Surround => Format::Surround,
        }
    }
}

#[derive(Clone, Copy, clap::ValueEnum)]
enum CliMix {
    Left,
    Right,
    Sum,
}

#[derive(Clone, Copy, clap::ValueEnum)]
enum CliKind {
    Stereo,
    Backfeed,
    Surround,
}

impl From<CliKind> for Kind {
    fn from(k: CliKind) -> Self {
        match k {
            CliKind::Stereo => Kind::Stereo,
            CliKind::Backfeed => Kind::Backfeed,
            CliKind::Surround => Kind::Surround,
        }
    }
}

fn main() -> ExitCode {
    match run(Cli::parse()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            // Also in system log and log file (service without stderr).
            lw_daemon::error!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

type Res = Result<(), Box<dyn std::error::Error>>;

fn stop_after(seconds: f64) -> Stop {
    let stop = Stop::new();
    if seconds > 0.0 {
        stop.after(Duration::from_secs_f64(seconds));
    }
    stop
}

fn most<K: std::fmt::Debug>(m: &std::collections::BTreeMap<K, u64>) -> String {
    m.iter()
        .max_by_key(|(_, n)| **n)
        .map(|(k, _)| format!("{k:?}"))
        .unwrap_or_else(|| "-".into())
}

fn print_sources(dir: &lw_daemon::discovery::Directory, json: bool) -> Res {
    let sources = dir.sources(std::time::Instant::now());
    if json {
        println!("{}", serde_json::to_string_pretty(&sources)?);
        return Ok(());
    }
    println!(
        "{:>7}  {:<16} {:<16} {:<10} {:<20} terminal",
        "channel", "name", "group", "type", ""
    );
    for s in &sources {
        println!(
            "{:>7}  {:<16} {:<16} {:<10} {:<20} {}",
            s.channel, s.name, s.stream, s.kind, s.terminal, s.terminal_ip
        );
    }
    for (ip, name, known, nums) in dir.terminals() {
        if known < usize::from(nums) || name.is_none() {
            println!(
                "  (terminal {ip}: {known}/{nums} sources known, waiting for full advertisement)"
            );
        }
    }
    println!("{} source(s)", sources.len());
    Ok(())
}

fn print_rx(s: &RxStats, json: bool) {
    if json {
        if let Ok(line) = serde_json::to_string(s) {
            println!("{line}");
        }
        return;
    }
    info!(
        "{} pkts={} lost={} late/dup={} resync={} PT={} payload={} Δts={} SSRC=group:{} jitter={:.1} smp peak={:.1} dBFS",
        s.group,
        s.packets,
        s.lost,
        s.late_or_dup,
        s.resyncs,
        most(&s.payload_types),
        most(&s.payload_sizes),
        most(&s.ts_steps),
        s.ssrc_is_group,
        s.jitter_samples,
        s.peak_dbfs
    );
}

fn run(cli: Cli) -> Res {
    match cli.cmd {
        Cmd::Ifaces { json } => {
            let list = iface::list()?;
            if json {
                println!("{}", serde_json::to_string_pretty(&list)?);
            } else {
                for i in list {
                    println!(
                        "{:<8} {:<15} index {:<3} {}",
                        i.name, i.ipv4, i.index, i.friendly
                    );
                }
            }
            Ok(())
        }
        Cmd::Send {
            iface,
            channel,
            format,
            seconds,
            tone,
            level,
            pt,
            tos,
            advertise,
            name,
            no_rt,
        } => {
            let nic = iface::find(&iface)?;
            let ch = Channel::new(channel).ok_or("channel outside 1..32766")?;
            let fmt: Format = format.into();
            let mut stream = tx::TxStream::new(ch, fmt.into());
            stream.payload_type = pt;
            stream.tone = tx::Tone::new(tone, level);
            let stop = stop_after(seconds);
            info!(
                "transmitting channel {channel} → {}:{} on {} ({}), {:?}",
                stream.group(),
                stream.port,
                nic.name,
                nic.ipv4,
                fmt
            );
            let adv = if advertise {
                let ty = if fmt == Format::Surround {
                    AdvStreamType::Surround
                } else {
                    AdvStreamType::StereoL24
                };
                let mut a = advertise::Advertiser::new(
                    &nic,
                    "mac-livewire",
                    vec![Source::new(1, ch, &name, ty)],
                )?;
                let s = stop.clone();
                Some(std::thread::spawn(move || {
                    a.run(&s).map(|()| (a.sent_full, a.sent_short))
                }))
            } else {
                None
            };
            let opts = TxOptions {
                ttl: 128,
                tos,
                realtime: !no_rt,
            };
            let report = tx::run(&nic, &stream, opts, &stop)?;
            stop.request();
            if let Some(h) = adv {
                let (full, short) = h.join().map_err(|_| "advertisement thread panicked")??;
                info!("advertisements: {full} full, {short} short");
            }
            info!("{}", serde_json::to_string(&report)?);
            Ok(())
        }
        Cmd::Recv {
            iface,
            channel,
            kind,
            group,
            port,
            seconds,
            json,
        } => {
            let nic = iface::find(&iface)?;
            let group = match (channel, group) {
                (Some(c), _) => Channel::new(c)
                    .ok_or("channel outside 1..32766")?
                    .group(Kind::from(kind).into()),
                (None, Some(g)) => g,
                (None, None) => return Err("specify --channel or --group".into()),
            };
            info!("receiving {group}:{port} on {} ({})", nic.name, nic.ipv4);
            let stop = stop_after(seconds);
            let final_stats = rx::run(&nic, group, port, &stop, Duration::from_secs(1), |s| {
                print_rx(s, json)
            })?;
            print_rx(&final_stats, json);
            Ok(())
        }
        Cmd::Run {
            config,
            seconds,
            control,
            xpc,
            init_config,
            log_file,
        } => {
            if let Some(f) = &log_file {
                lw_daemon::log::to_file(f)?;
            }
            if init_config {
                Config::init_if_missing(&config)?;
            }
            let endpoint = match (control, xpc) {
                (Some(c), _) => Some(Endpoint::parse(&c)?),
                (None, Some(name)) => Some(Endpoint::parse(&format!("mach:{name}"))?),
                (None, None) => None,
            };
            run_config(
                &Config::load(&config)?,
                Some(&config),
                seconds,
                endpoint.as_ref(),
            )
        }
        #[cfg(windows)]
        Cmd::Service { config, log_file } => run_windows_service(config, log_file),
        Cmd::Discover {
            iface,
            seconds,
            json,
        } => {
            let nic = iface::find(&iface)?;
            info!(
                "listening for advertisements on {} ({}) for {seconds} s",
                nic.name, nic.ipv4
            );
            let dir = lw_daemon::discovery::Directory::new();
            let stop = stop_after(seconds);
            lw_daemon::discovery::run(&nic, &dir, &stop)?;
            print_sources(&dir, json)
        }
        Cmd::Ctl {
            action,
            endpoint,
            service,
            user,
        } => {
            let fmt_name = |f: CliFormat| match f {
                CliFormat::Standard => "standard",
                CliFormat::Aes67 => "aes67",
                CliFormat::Livestream => "livestream",
                CliFormat::Surround => "surround",
            };
            let kind_name = |k: CliKind| match k {
                CliKind::Stereo => "stereo",
                CliKind::Backfeed => "backfeed",
                CliKind::Surround => "surround",
            };
            let request = match &action {
                CtlCmd::Status => serde_json::json!({ "cmd": "status" }),
                CtlCmd::Sources => serde_json::json!({ "cmd": "sources" }),
                CtlCmd::Config => serde_json::json!({ "cmd": "config" }),
                CtlCmd::PatchIn {
                    channel,
                    group,
                    port,
                    kind,
                    mix,
                    device,
                    to,
                } => serde_json::json!({
                    "cmd": "patch_input", "channel": channel, "group": group.map(|g| g.to_string()),
                    "port": port, "kind": kind_name(*kind), "device": device,
                    "mix": mix.map(|m| match m { CliMix::Left => "left", CliMix::Right => "right", CliMix::Sum => "sum" }),
                    "device_channels": to }),
                CtlCmd::UnpatchIn { device, to } => {
                    serde_json::json!({ "cmd": "unpatch_input", "device": device, "device_channels": to })
                }
                CtlCmd::PatchOut {
                    device,
                    from,
                    channel,
                    name,
                    format,
                } => serde_json::json!({
                    "cmd": "patch_output", "channel": channel, "name": name, "format": fmt_name(*format),
                    "device": device, "device_channels": from }),
                CtlCmd::UnpatchOut { channel } => {
                    serde_json::json!({ "cmd": "unpatch_output", "channel": channel })
                }
                CtlCmd::SetIface { iface } => {
                    serde_json::json!({ "cmd": "set_iface", "iface": iface })
                }
                CtlCmd::SetChannels { to_net, from_net } => {
                    serde_json::json!({ "cmd": "set_device_channels", "to_net": to_net, "from_net": from_net })
                }
                CtlCmd::SetLayout { layout } => {
                    serde_json::json!({ "cmd": "set_device_layout", "layout": layout })
                }
                CtlCmd::SetNaming { enabled } => {
                    serde_json::json!({ "cmd": "set_device_naming", "enabled": enabled })
                }
                CtlCmd::SetAdvanced {
                    name,
                    latency,
                    dscp,
                } => serde_json::json!({
                    "cmd": "set_advanced", "terminal_name": name, "latency": latency, "dscp": dscp
                }),
                CtlCmd::Ifaces => serde_json::json!({ "cmd": "ifaces" }),
                CtlCmd::Geometry => serde_json::json!({ "cmd": "geometry" }),
            };
            let endpoint = match (service, user) {
                (Some(name), true) => Endpoint::parse(&format!("mach-user:{name}"))?,
                (Some(name), false) => Endpoint::parse(&format!("mach:{name}"))?,
                (None, true) => {
                    Endpoint::parse(&format!("mach-user:{}", lw_sys::ctl::SERVICE_NAME))?
                }
                (None, false) => Endpoint::parse(&endpoint)?,
            };
            let client = lw_sys::ctl::Client::connect(&endpoint)?;
            let reply: serde_json::Value =
                serde_json::from_str(&client.call(&request.to_string())?)?;
            if let (CtlCmd::Sources, Some(list)) =
                (&action, reply.get("sources").and_then(|s| s.as_array()))
            {
                println!(
                    "{:>7}  {:<16} {:<16} {:<10} terminal",
                    "channel", "name", "group", "type"
                );
                for s in list {
                    println!(
                        "{:>7}  {:<16} {:<16} {:<10} {} ({})",
                        s["channel"],
                        s["name"].as_str().unwrap_or(""),
                        s["stream"].as_str().unwrap_or(""),
                        s["kind"].as_str().unwrap_or(""),
                        s["terminal"].as_str().unwrap_or(""),
                        s["terminal_ip"].as_str().unwrap_or("")
                    );
                }
                println!("{} source(s)", list.len());
            } else {
                println!("{}", serde_json::to_string_pretty(&reply)?);
            }
            if reply.get("ok") == Some(&serde_json::Value::Bool(true)) {
                Ok(())
            } else {
                Err("the daemon returned an error".into())
            }
        }
    }
}

/// Windows service name (Service Control Manager, Event Log).
#[cfg(windows)]
const WINDOWS_SERVICE: &str = "OpenLW";

#[cfg(windows)]
fn run_windows_service(config: Option<PathBuf>, log_file: Option<PathBuf>) -> Res {
    let base = std::env::var_os("PROGRAMDATA")
        .map_or_else(|| PathBuf::from(r"C:\ProgramData"), PathBuf::from)
        .join("OpenLW");
    let config = config.unwrap_or_else(|| base.join("lw-daemon.json"));
    lw_daemon::log::to_file(&log_file.unwrap_or_else(|| base.join("Logs").join("lw-daemon.log")))?;
    Config::init_if_missing(&config)?;
    lw_sys::service::run(
        WINDOWS_SERVICE,
        Box::new(move |flag| {
            let stop = Stop::new();
            let watcher = stop.clone();
            std::thread::spawn(move || {
                while !flag.load(std::sync::atomic::Ordering::Relaxed) && !watcher.requested() {
                    std::thread::sleep(Duration::from_millis(100));
                }
                watcher.request();
            });
            let result = Config::load(&config)
                .map_err(|e| e.to_string())
                .and_then(|cfg| {
                    info!("service {WINDOWS_SERVICE} started ({})", config.display());
                    lw_daemon::supervisor::run(cfg, Some(config), &stop, Some(&Endpoint::service()))
                        .map_err(|e| e.to_string())
                });
            stop.request();
            if let Err(e) = &result {
                lw_daemon::error!("service {WINDOWS_SERVICE}: {e}");
            }
            result
        }),
    )?;
    Ok(())
}

fn run_config(
    cfg: &Config,
    path: Option<&std::path::Path>,
    seconds: f64,
    control: Option<&Endpoint>,
) -> Res {
    let stop = stop_after(seconds);
    lw_daemon::supervisor::run(cfg.clone(), path.map(|p| p.to_path_buf()), &stop, control)
}
