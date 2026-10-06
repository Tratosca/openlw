//! `lw-daemon` : outil et daemon Livewire / AES67.
//!
//!     lw-daemon ifaces
//!     lw-daemon send --iface en7 --channel 4001 --format standard [--advertise --name "MAC 1"]
//!     lw-daemon recv --iface en7 --channel 1
//!     lw-daemon run --config lw-daemon.json [--control [ENDPOINT]] [--init-config] [--log-file F]
//!     lw-daemon service              (Windows, lancé par le gestionnaire de services)
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
#[command(version, about = "Daemon et outil Livewire / AES67 d'OpenLW")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Liste les interfaces IPv4 (nom système, nom convivial, adresse).
    Ifaces {
        #[arg(long)]
        json: bool,
    },
    /// Émet un flux de test (sinusoïde) vers un canal Livewire.
    Send {
        #[arg(long)]
        iface: String,
        #[arg(long)]
        channel: u16,
        #[arg(long, value_enum, default_value = "standard")]
        format: CliFormat,
        /// Durée en secondes (0 = sans fin).
        #[arg(long, default_value_t = 0.0)]
        seconds: f64,
        #[arg(long, default_value_t = 997.0)]
        tone: f64,
        #[arg(long, default_value_t = -20.0, allow_negative_numbers = true)]
        level: f64,
        #[arg(long, default_value_t = 96)]
        pt: u8,
        /// Octet TOS (184 = EF, 136 = AF41).
        #[arg(long, default_value_t = 0xB8)]
        tos: u32,
        /// Annonce aussi la source (ADV) pour qu'elle apparaisse sur les appareils Livewire.
        #[arg(long)]
        advertise: bool,
        #[arg(long, default_value = "MAC TEST")]
        name: String,
        /// Thread d'émission à priorité normale (comparaison avec le temps réel).
        #[arg(long)]
        no_rt: bool,
    },
    /// Reçoit un flux et affiche les statistiques chaque seconde.
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
    /// Lance le daemon selon un fichier de configuration.
    Run {
        #[arg(long)]
        config: PathBuf,
        #[arg(long, default_value_t = 0.0)]
        seconds: f64,
        /// Publie le canal de contrôle : sans valeur, celui du service installé (XPC sous macOS,
        /// socket Unix sous Linux, tube nommé sous Windows) ; sinon `unix:CHEMIN`, `pipe:NOM`, `mach:NOM`.
        #[arg(long, num_args = 0..=1, default_missing_value = "service")]
        control: Option<String>,
        /// Ancienne forme de `--control mach:NOM` (plist launchd).
        #[arg(long, num_args = 0..=1, default_missing_value = lw_sys::ctl::SERVICE_NAME, hide = true)]
        xpc: Option<String>,
        /// Crée le fichier de configuration par défaut s'il n'existe pas.
        #[arg(long)]
        init_config: bool,
        /// Copie aussi le journal dans ce fichier.
        #[arg(long)]
        log_file: Option<PathBuf>,
    },
    /// Service Windows, lancé par le gestionnaire de services : configuration et journal dans
    /// %ProgramData%\OpenLW, canal de contrôle par tube nommé.
    #[cfg(windows)]
    Service {
        #[arg(long)]
        config: Option<PathBuf>,
        #[arg(long)]
        log_file: Option<PathBuf>,
    },
    /// Écoute les annonces Livewire et liste les sources du réseau.
    Discover {
        #[arg(long)]
        iface: String,
        /// Durée d'écoute ; une annonce complète peut mettre 2 à 3 min si le terminal ignore la requête.
        #[arg(long, default_value_t = 30.0)]
        seconds: f64,
        #[arg(long)]
        json: bool,
    },
    /// Interroge ou repatche un daemon en cours, par le canal de contrôle.
    Ctl {
        #[command(subcommand)]
        action: CtlCmd,
        /// Point d'accès : `service` (défaut), `unix:CHEMIN`, `pipe:NOM`, `mach:NOM`, `mach-user:NOM`.
        #[arg(long, global = true, default_value = "service")]
        endpoint: String,
        /// macOS : nom du service Mach (remplace `--endpoint`).
        #[arg(long, global = true)]
        service: Option<String>,
        /// macOS : service du domaine utilisateur (LaunchAgent) plutôt que système (LaunchDaemon).
        #[arg(long, global = true)]
        user: bool,
    },
}

#[derive(Subcommand)]
enum CtlCmd {
    /// État du daemon, des flux et du périphérique.
    Status,
    /// Sources Livewire découvertes sur le réseau.
    Sources,
    /// Configuration courante.
    Config,
    /// Patche un canal Livewire (ou un groupe AES67) sur des entrées du périphérique.
    PatchIn {
        #[arg(long, conflicts_with = "group")]
        channel: Option<u16>,
        #[arg(long)]
        group: Option<Ipv4Addr>,
        #[arg(long, default_value_t = AUDIO_PORT)]
        port: u16,
        #[arg(long, value_enum, default_value = "stereo")]
        kind: CliKind,
        /// Entrées du périphérique, ex. 1,2.
        #[arg(long, value_delimiter = ',', required = true)]
        to: Vec<u16>,
    },
    /// Libère des entrées du périphérique.
    UnpatchIn {
        #[arg(long, value_delimiter = ',', required = true)]
        to: Vec<u16>,
    },
    /// Émet des sorties du périphérique sur un canal Livewire.
    PatchOut {
        /// Sorties du périphérique, ex. 1,2.
        #[arg(long, value_delimiter = ',', required = true)]
        from: Vec<u16>,
        #[arg(long)]
        channel: u16,
        #[arg(long, default_value = lw_daemon::config::DEFAULT_SOURCE_NAME)]
        name: String,
        #[arg(long, value_enum, default_value = "standard")]
        format: CliFormat,
    },
    /// Arrête l'émission d'un canal.
    UnpatchOut {
        #[arg(long)]
        channel: u16,
    },
    /// Change l'interface réseau Livewire : nom BSD, nom convivial ou « auto ».
    SetIface { iface: String },
    /// Nombre de canaux du périphérique dans chaque sens (1 à 32).
    SetChannels {
        #[arg(long)]
        to_net: u32,
        #[arg(long)]
        from_net: u32,
    },
    /// Présentation dans macOS : duplex (un périphérique) ou split (OpenLW In / OpenLW Out).
    SetLayout { layout: String },
    /// En présentation split, nomme les périphériques d'après les canaux patchés.
    SetNaming {
        #[arg(action = clap::ArgAction::Set)]
        enabled: bool,
    },
    /// Réglages avancés : nom annoncé, latence de réception, priorité réseau.
    SetAdvanced {
        /// Nom annoncé (vide : nom de l'ordinateur).
        #[arg(long)]
        name: Option<String>,
        /// low, normal ou safe.
        #[arg(long)]
        latency: Option<String>,
        /// DSCP des flux audio (46 = EF, 34 = AF41, 0 = aucune).
        #[arg(long)]
        dscp: Option<u8>,
    },
    /// Interfaces réseau, et celles où Livewire est entendu.
    Ifaces,
    /// Géométrie du périphérique (nombre de canaux, génération).
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
            // Aussi dans le journal du système et le fichier journal (service sans stderr).
            lw_daemon::error!("erreur : {e}");
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
        "{:>6}  {:<16} {:<16} {:<10} {:<20} terminal",
        "canal", "nom", "groupe", "type", ""
    );
    for s in &sources {
        println!(
            "{:>6}  {:<16} {:<16} {:<10} {:<20} {}",
            s.channel, s.name, s.stream, s.kind, s.terminal, s.terminal_ip
        );
    }
    for (ip, name, known, nums) in dir.terminals() {
        if known < usize::from(nums) || name.is_none() {
            println!(
                "  (terminal {ip} : {known}/{nums} sources connues, annonce complète en attente)"
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
        "{} pkts={} perdus={} retard/doublon={} resync={} PT={} charge={} Δts={} SSRC=groupe:{} gigue={:.1} éch crête={:.1} dBFS",
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
            let ch = Channel::new(channel).ok_or("canal hors 1..32766")?;
            let fmt: Format = format.into();
            let mut stream = tx::TxStream::new(ch, fmt.into());
            stream.payload_type = pt;
            stream.tone = tx::Tone::new(tone, level);
            let stop = stop_after(seconds);
            info!(
                "émission canal {channel} → {}:{} sur {} ({}), {:?}",
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
                let (full, short) = h.join().map_err(|_| "thread d'annonce")??;
                info!("annonces : {full} complètes, {short} courtes");
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
                    .ok_or("canal hors 1..32766")?
                    .group(Kind::from(kind).into()),
                (None, Some(g)) => g,
                (None, None) => return Err("indiquer --channel ou --group".into()),
            };
            info!("réception {group}:{port} sur {} ({})", nic.name, nic.ipv4);
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
                "écoute des annonces sur {} ({}) pendant {seconds} s",
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
                    to,
                } => serde_json::json!({
                    "cmd": "patch_input", "channel": channel, "group": group.map(|g| g.to_string()),
                    "port": port, "kind": kind_name(*kind), "device_channels": to }),
                CtlCmd::UnpatchIn { to } => {
                    serde_json::json!({ "cmd": "unpatch_input", "device_channels": to })
                }
                CtlCmd::PatchOut {
                    from,
                    channel,
                    name,
                    format,
                } => serde_json::json!({
                    "cmd": "patch_output", "channel": channel, "name": name, "format": fmt_name(*format), "device_channels": from }),
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
                    "{:>6}  {:<16} {:<16} {:<10} terminal",
                    "canal", "nom", "groupe", "type"
                );
                for s in list {
                    println!(
                        "{:>6}  {:<16} {:<16} {:<10} {} ({})",
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
                Err("le daemon a renvoyé une erreur".into())
            }
        }
    }
}

/// Nom du service Windows (gestionnaire de services, journal des événements).
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
                    info!("service {WINDOWS_SERVICE} démarré ({})", config.display());
                    lw_daemon::supervisor::run(cfg, Some(config), &stop, Some(&Endpoint::service()))
                        .map_err(|e| e.to_string())
                });
            stop.request();
            if let Err(e) = &result {
                lw_daemon::error!("service {WINDOWS_SERVICE} : {e}");
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
