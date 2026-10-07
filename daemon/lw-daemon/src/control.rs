//! Daemon control: shared state and JSON requests received through the control channel (ADR 0005,
//! ADR 0007): XPC (macOS), Unix socket (Linux), named pipe (Windows).
//!
//! Requests: `{"cmd":"ping"}`, `{"cmd":"status"}`, `{"cmd":"attach"}` (the audio client also requests
//! the shared region, attached by the transport to the response). Responses: `{"ok":true,...}` or
//! `{"ok":false,"error":...}`. Configuration apps and `lw-daemon ctl` use this
//! protocol.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use serde::Serialize;
use serde_json::{json, Value};

use crate::config::{Config, Format, Kind, Layout, Mix};
use crate::editor::{self, Edit};
use crate::rx::RxStats;
use crate::tx::TxReport;

pub use lw_sys::ctl::SERVICE_NAME;
use lw_sys::ctl::{Caller, Endpoint};

/// Observable daemon state.
#[derive(Debug, Clone, Serialize)]
pub struct Status {
    pub version: &'static str,
    pub pid: u32,
    pub started_unix: u64,
    pub uptime_s: u64,
    /// Automatically selected interface (`iface: "auto"`).
    pub iface_auto: bool,
    /// No usable interface: searching for the Livewire network (auto) or interface absent.
    pub searching: bool,
    pub iface: String,
    pub iface_friendly: String,
    pub ipv4: String,
    pub tx: BTreeMap<String, TxReport>,
    pub rx: BTreeMap<String, RxStats>,
    pub advertised_sources: usize,
    /// Virtual device (region shared with the plugin), if active.
    pub device: Option<crate::device::DeviceStatus>,
    /// Linux: PipeWire nodes published (false while PipeWire is unreachable); absent elsewhere.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub audio_nodes: Option<bool>,
}

/// Device-state source (refreshed by its own thread).
pub type DeviceStatusRef = Arc<Mutex<crate::device::DeviceStatus>>;

/// State shared between threads and the control-channel handler.
#[derive(Clone)]
pub struct Shared {
    inner: Arc<Mutex<Status>>,
    started: Instant,
    device: Arc<Mutex<Option<DeviceStatusRef>>>,
    directory: crate::discovery::Directory,
    config: Arc<Mutex<Option<ConfigSource>>>,
    heard: crate::detect::Heard,
}

/// Current configuration, its file, and the session reload channel.
struct ConfigSource {
    current: Config,
    path: Option<PathBuf>,
    reload: Sender<Config>,
}

impl Shared {
    /// `iface`: session interface, or `None` until selected.
    pub fn new(iface: Option<&crate::iface::Iface>, advertised_sources: usize) -> Self {
        let started_unix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        Self {
            inner: Arc::new(Mutex::new(Status {
                version: env!("CARGO_PKG_VERSION"),
                pid: std::process::id(),
                started_unix,
                uptime_s: 0,
                iface_auto: false,
                searching: iface.is_none(),
                iface: iface.map(|i| i.name.clone()).unwrap_or_default(),
                iface_friendly: iface.map(|i| i.friendly.clone()).unwrap_or_default(),
                ipv4: iface.map(|i| i.ipv4.to_string()).unwrap_or_default(),
                tx: BTreeMap::new(),
                rx: BTreeMap::new(),
                advertised_sources,
                device: None,
                audio_nodes: None,
            })),
            started: Instant::now(),
            device: Arc::new(Mutex::new(None)),
            directory: crate::discovery::Directory::new(),
            config: Arc::new(Mutex::new(None)),
            heard: crate::detect::Heard::default(),
        }
    }

    /// Interfaces receiving Livewire advertisements (detector).
    pub fn heard(&self) -> &crate::detect::Heard {
        &self.heard
    }

    /// No session: searching for the network (`auto`) or configured interface absent.
    pub fn set_searching(&self, auto: bool) {
        self.with(|s| {
            s.iface_auto = auto;
            s.searching = true;
            s.iface.clear();
            s.iface_friendly.clear();
            s.ipv4.clear();
            s.tx.clear();
            s.rx.clear();
        });
    }

    pub fn set_audio_nodes(&self, published: Option<bool>) {
        self.with(|s| s.audio_nodes = published);
    }

    fn with<R>(&self, f: impl FnOnce(&mut Status) -> R) -> R {
        // A panicking thread must not make state unreadable: recover the data.
        let mut guard = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        f(&mut guard)
    }

    pub fn update_tx(&self, report: &TxReport) {
        self.with(|s| s.tx.insert(report.group.clone(), report.clone()));
    }

    pub fn update_rx(&self, stats: &RxStats) {
        self.with(|s| s.rx.insert(stats.group.clone(), stats.clone()));
    }

    /// Advertised source count (zero if advertising is disabled).
    pub fn set_advertised(&self, n: usize) {
        self.with(|s| s.advertised_sources = n);
    }

    /// Remove statistics for a stopped stream.
    pub fn forget_stream(&self, group: &str) {
        self.with(|s| {
            s.tx.remove(group);
            s.rx.remove(group);
        });
    }

    /// Connect current configuration: patch commands modify it, save it to
    /// `path` (if supplied), and send the new version on `reload`.
    pub fn set_config(&self, current: Config, path: Option<PathBuf>, reload: Sender<Config>) {
        *self.config.lock().unwrap_or_else(PoisonError::into_inner) = Some(ConfigSource {
            current,
            path,
            reload,
        });
    }

    /// Clear stream statistics and update the interface (new session).
    pub fn reset_streams(&self, iface: &crate::iface::Iface, auto: bool) {
        self.with(|s| {
            s.iface_auto = auto;
            s.searching = false;
            s.tx.clear();
            s.rx.clear();
            s.iface = iface.name.clone();
            s.iface_friendly = iface.friendly.clone();
            s.ipv4 = iface.ipv4.to_string();
        });
    }

    fn apply_edit(&self, edit: &Edit) -> Value {
        let mut guard = self.config.lock().unwrap_or_else(PoisonError::into_inner);
        let Some(src) = guard.as_mut() else {
            return json!({ "ok": false, "error": "configuration is read-only (daemon started without a configuration file)" });
        };
        let next = match editor::apply(&src.current, edit) {
            Ok(c) => c,
            Err(e) => return json!({ "ok": false, "error": e.0 }),
        };
        if let Some(path) = &src.path {
            if let Err(e) = next.save(path) {
                return json!({ "ok": false, "error": format!("cannot save {}: {e}", path.display()) });
            }
        }
        if src.reload.send(next.clone()).is_err() {
            return json!({ "ok": false, "error": "supervisor stopped" });
        }
        src.current = next.clone();
        crate::info!("patch changed: {edit:?}");
        json!({ "ok": true, "config": next })
    }

    fn current_config(&self) -> Option<Config> {
        self.config
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .as_ref()
            .map(|s| s.current.clone())
    }

    /// Device and channel names from configuration and received advertisements.
    pub fn labels(&self) -> crate::labels::Labels {
        let announced = self
            .directory
            .sources(Instant::now())
            .into_iter()
            .map(|s| {
                (
                    s.channel,
                    crate::labels::Announced {
                        name: s.name,
                        terminal: s.terminal,
                    },
                )
            })
            .collect();
        match self.current_config() {
            Some(cfg) => crate::labels::compute(&cfg, &announced),
            None => crate::labels::Labels::fallback(),
        }
    }

    /// Discovered-source directory (fed by the discovery thread).
    pub fn directory(&self) -> &crate::discovery::Directory {
        &self.directory
    }

    /// Attach virtual-device state.
    pub fn set_device(&self, status: DeviceStatusRef) {
        *self.device.lock().unwrap_or_else(PoisonError::into_inner) = Some(status);
    }

    fn device_status(&self) -> Option<crate::device::DeviceStatus> {
        let dev = self
            .device
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()?;
        let s = dev.lock().unwrap_or_else(PoisonError::into_inner).clone();
        Some(s)
    }

    pub fn snapshot(&self) -> Status {
        let uptime = self.started.elapsed().as_secs();
        let device = self.device_status();
        self.with(|s| {
            s.uptime_s = uptime;
            s.device = device;
            s.clone()
        })
    }

    /// Handle a JSON request and return a JSON response (never a caller-side error).
    pub fn handle(&self, request: &str, caller: &Caller) -> String {
        let reply = match serde_json::from_str::<Value>(request) {
            Ok(v) if is_mutating(&v) && !caller.may_edit => {
                json!({ "ok": false, "error": format!("{} ({})", lw_sys::ctl::edit_policy(), caller.label) })
            }
            Ok(v) if is_mutating(&v) => match parse_edit(&v) {
                Ok(edit) => self.apply_edit(&edit),
                Err(e) => json!({ "ok": false, "error": e }),
            },
            Ok(v) => match v.get("cmd").and_then(Value::as_str) {
                Some("config") => match self.current_config() {
                    Some(c) => json!({ "ok": true, "config": c }),
                    None => json!({ "ok": false, "error": "no configuration loaded" }),
                },
                Some("ping") => {
                    json!({ "ok": true, "pong": true, "version": env!("CARGO_PKG_VERSION") })
                }
                Some("status") => match serde_json::to_value(self.snapshot()) {
                    Ok(status) => json!({ "ok": true, "status": status }),
                    Err(e) => json!({ "ok": false, "error": e.to_string() }),
                },
                Some("sources") => {
                    let sources = self.directory.sources(Instant::now());
                    let terminals: Vec<Value> = self
                        .directory
                        .terminals()
                        .into_iter()
                        .map(|(ip, name, known, nums)| json!({ "ip": ip, "name": name, "sources_known": known, "sources_announced": nums }))
                        .collect();
                    json!({ "ok": true, "sources": sources, "terminals": terminals })
                }
                Some("ifaces") => match crate::iface::list() {
                    Ok(list) => {
                        let now = Instant::now();
                        let items: Vec<Value> = list
                            .iter()
                            .map(|i| {
                                let mut v = serde_json::to_value(i).unwrap_or(Value::Null);
                                if let Some(o) = v.as_object_mut() {
                                    o.insert(
                                        "candidate".into(),
                                        crate::detect::is_candidate(i).into(),
                                    );
                                    o.insert(
                                        "livewire".into(),
                                        self.heard.recent(&i.name, now).into(),
                                    );
                                }
                                v
                            })
                            .collect();
                        json!({ "ok": true, "ifaces": items })
                    }
                    Err(e) => json!({ "ok": false, "error": e.to_string() }),
                },
                Some("geometry") => match self.device_status() {
                    Some(d) => {
                        let labels = self.labels();
                        json!({
                            "ok": true,
                            "generation": d.generation,
                            "channels_to_net": d.channels_to_net,
                            "channels_from_net": d.channels_from_net,
                            "layout": if labels.multi { "multi" } else { "duplex" },
                            "in_widths": d.in_widths,
                            "out_widths": d.out_widths,
                            "name": labels.name,
                            "in_device_names": labels.input_device_names,
                            "out_device_names": labels.output_device_names,
                            "input_margin": self.current_config().map(|c| c.latency.input_margin()).unwrap_or(256),
                            "input_names": labels.input_names,
                            "output_names": labels.output_names,
                        })
                    }
                    None => json!({ "ok": false, "error": "no active virtual device" }),
                },
                Some("attach") => match self.device_status() {
                    Some(d) => json!({ "ok": true, "device": d }),
                    None => json!({ "ok": false, "error": "no active virtual device" }),
                },
                Some(other) => {
                    json!({ "ok": false, "error": format!("unknown command: {other}") })
                }
                None => json!({ "ok": false, "error": "missing `cmd` field" }),
            },
            Err(e) => json!({ "ok": false, "error": format!("invalid JSON: {e}") }),
        };
        reply.to_string()
    }

    /// Start the control channel on `endpoint`.
    pub fn serve(&self, endpoint: &Endpoint) -> Result<lw_sys::ctl::Server, lw_sys::ctl::Error> {
        let me = self.clone();
        lw_sys::ctl::Server::start(endpoint, Box::new(move |req, c| me.handle(req, c)))
    }

    /// Anonymous XPC control channel (tests, same process; macOS).
    #[cfg(target_os = "macos")]
    pub fn serve_anonymous_xpc(&self) -> Result<lw_sys::ctl::Server, lw_sys::ctl::Error> {
        let me = self.clone();
        lw_sys::ctl::Server::anonymous_xpc(Box::new(move |req, c| me.handle(req, c)))
    }
}

const MUTATING: [&str; 11] = [
    "patch_input",
    "unpatch_input",
    "remove_input",
    "patch_output",
    "unpatch_output",
    "set_iface",
    "set_advertise",
    "set_device_channels",
    "set_device_naming",
    "set_advanced",
    "set_device_layout",
];

fn is_mutating(v: &Value) -> bool {
    v.get("cmd")
        .and_then(Value::as_str)
        .is_some_and(|c| MUTATING.contains(&c))
}

fn channels_arg(v: &Value, key: &str) -> Result<Vec<u16>, String> {
    let arr = v
        .get(key)
        .and_then(Value::as_array)
        .ok_or(format!("missing `{key}` (channel list)"))?;
    arr.iter()
        .map(|x| {
            x.as_u64()
                .and_then(|n| u16::try_from(n).ok())
                .ok_or(format!("`{key}`: integer expected"))
        })
        .collect()
}

fn opt_u16(v: &Value, key: &str) -> Result<Option<u16>, String> {
    match v.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(x) => x
            .as_u64()
            .and_then(|n| u16::try_from(n).ok())
            .map(Some)
            .ok_or(format!("`{key}`: integer expected")),
    }
}

fn parse_edit(v: &Value) -> Result<Edit, String> {
    let cmd = v.get("cmd").and_then(Value::as_str).unwrap_or_default();
    let from_json = |key: &str, default: &str| -> Result<Value, String> {
        Ok(v.get(key)
            .cloned()
            .unwrap_or_else(|| Value::String(default.into())))
    };
    match cmd {
        "patch_input" => Ok(Edit::PatchInput {
            channel: opt_u16(v, "channel")?,
            group: match v.get("group").and_then(Value::as_str) {
                Some(g) => Some(
                    g.parse()
                        .map_err(|_| "`group`: invalid IPv4 address".to_string())?,
                ),
                None => None,
            },
            port: opt_u16(v, "port")?.unwrap_or(5004),
            kind: serde_json::from_value::<Kind>(from_json("kind", "stereo")?)
                .map_err(|e| format!("`kind`: {e}"))?,
            device: opt_u16(v, "device")?,
            mix: match v.get("mix") {
                None | Some(Value::Null) => None,
                Some(m) => Some(
                    serde_json::from_value::<Mix>(m.clone())
                        .map_err(|_| "`mix`: left, right, or sum".to_string())?,
                ),
            },
            device_channels: channels_arg(v, "device_channels")?,
        }),
        "unpatch_input" => Ok(Edit::UnpatchInput {
            device: opt_u16(v, "device")?,
            device_channels: channels_arg(v, "device_channels")?,
        }),
        "remove_input" => Ok(Edit::RemoveInput {
            channel: opt_u16(v, "channel")?,
            group: match v.get("group").and_then(Value::as_str) {
                Some(g) => Some(
                    g.parse()
                        .map_err(|_| "`group`: invalid IPv4 address".to_string())?,
                ),
                None => None,
            },
            port: opt_u16(v, "port")?.unwrap_or(5004),
            kind: serde_json::from_value::<Kind>(from_json("kind", "stereo")?)
                .map_err(|e| format!("`kind`: {e}"))?,
        }),
        "patch_output" => Ok(Edit::PatchOutput {
            channel: opt_u16(v, "channel")?.ok_or("missing `channel`")?,
            name: v
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or(crate::config::DEFAULT_SOURCE_NAME)
                .to_string(),
            format: serde_json::from_value::<Format>(from_json("format", "standard")?)
                .map_err(|e| format!("`format`: {e}"))?,
            device: opt_u16(v, "device")?,
            device_channels: channels_arg(v, "device_channels")?,
        }),
        "unpatch_output" => Ok(Edit::UnpatchOutput {
            channel: opt_u16(v, "channel")?.ok_or("missing `channel`")?,
        }),
        "set_iface" => {
            let name = v
                .get("iface")
                .and_then(Value::as_str)
                .ok_or("missing `iface`")?;
            if name == crate::config::AUTO_IFACE {
                return Ok(Edit::SetIface(name.into()));
            }
            let nic = crate::iface::find(name).map_err(|e| e.to_string())?;
            Ok(Edit::SetIface(nic.name))
        }
        "set_advanced" => Ok(Edit::SetAdvanced {
            terminal_name: v
                .get("terminal_name")
                .and_then(Value::as_str)
                .map(str::to_string),
            latency: match v.get("latency") {
                None | Some(Value::Null) => None,
                Some(l) => Some(
                    serde_json::from_value(l.clone())
                        .map_err(|_| "`latency`: low, normal, or safe".to_string())?,
                ),
            },
            dscp: match v.get("dscp") {
                None | Some(Value::Null) => None,
                Some(d) => Some(
                    d.as_u64()
                        .and_then(|n| u8::try_from(n).ok())
                        .ok_or("`dscp`: integer from 0 to 63")?,
                ),
            },
        }),
        "set_device_layout" => {
            let layout: Layout =
                serde_json::from_value(v.get("layout").cloned().unwrap_or(Value::Null))
                    .map_err(|_| "`layout`: duplex or multi".to_string())?;
            // Numbered devices: macOS HAL plugin and Linux PipeWire nodes. ASIO has a single
            // driver per application.
            if layout == Layout::Multi && cfg!(windows) {
                return Err("multi layout is not available on Windows (ASIO: one device)".into());
            }
            Ok(Edit::SetDeviceLayout(layout))
        }
        "set_device_naming" => Ok(Edit::SetDeviceNaming(
            v.get("enabled")
                .and_then(Value::as_bool)
                .ok_or("missing `enabled` (boolean)")?,
        )),
        "set_device_channels" => {
            let n = |key: &str| -> Result<u32, String> {
                v.get(key)
                    .and_then(Value::as_u64)
                    .and_then(|n| u32::try_from(n).ok())
                    .ok_or(format!("missing `{key}` (integer)"))
            };
            Ok(Edit::SetDeviceChannels {
                to_net: n("to_net")?,
                from_net: n("from_net")?,
            })
        }
        "set_advertise" => Ok(Edit::SetAdvertise(
            v.get("advertise")
                .and_then(Value::as_bool)
                .ok_or("missing `advertise` (boolean)")?,
        )),
        other => Err(format!("unknown command: {other}")),
    }
}

#[cfg(test)]
#[allow(clippy::indexing_slicing)]
mod tests {
    use super::*;
    use std::net::Ipv4Addr;

    fn shared() -> Shared {
        let lo = crate::iface::Iface {
            name: "lo0".into(),
            friendly: "lo0".into(),
            index: 1,
            ipv4: Ipv4Addr::LOCALHOST,
            loopback: true,
        };
        Shared::new(Some(&lo), 2)
    }

    #[test]
    fn requests() {
        let s = shared();
        s.update_tx(&TxReport {
            group: "239.192.15.161".into(),
            packets: 42,
            ..TxReport::default()
        });
        let me = Caller::trusted("test");
        let v: Value = serde_json::from_str(&s.handle(r#"{"cmd":"status"}"#, &me)).unwrap();
        assert_eq!(v["ok"], true);
        assert_eq!(v["status"]["tx"]["239.192.15.161"]["packets"], 42);
        assert_eq!(v["status"]["advertised_sources"], 2);
        let v: Value = serde_json::from_str(&s.handle(r#"{"cmd":"nope"}"#, &me)).unwrap();
        assert_eq!(v["ok"], false);
        let v: Value = serde_json::from_str(&s.handle("not json", &me)).unwrap();
        assert_eq!(v["ok"], false);
    }

    #[test]
    fn edits_require_permission() {
        let s = shared();
        let guest = Caller {
            may_edit: false,
            ..Caller::trusted("guest")
        };
        let v: Value =
            serde_json::from_str(&s.handle(r#"{"cmd":"set_advertise","advertise":true}"#, &guest))
                .unwrap();
        assert_eq!(v["ok"], false);
        assert!(v["error"].as_str().unwrap().contains("guest"));
        let v: Value = serde_json::from_str(&s.handle(r#"{"cmd":"ping"}"#, &guest)).unwrap();
        assert_eq!(v["ok"], true, "reads unrestricted");
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn over_xpc() {
        let s = shared();
        let server = s.serve_anonymous_xpc().unwrap();
        let client = lw_sys::xpc::Client::from_endpoint(&server.xpc().unwrap().endpoint()).unwrap();
        let v: Value = serde_json::from_str(&client.call(r#"{"cmd":"ping"}"#).unwrap()).unwrap();
        assert_eq!(v["pong"], true);
        let v: Value = serde_json::from_str(&client.call(r#"{"cmd":"status"}"#).unwrap()).unwrap();
        assert_eq!(v["status"]["iface"], "lo0");
    }

    #[test]
    fn over_stream_transport() {
        let s = shared();
        #[cfg(unix)]
        let ep = Endpoint::Socket(
            std::env::temp_dir().join(format!("openlw-control-{}.sock", std::process::id())),
        );
        #[cfg(windows)]
        let ep = Endpoint::Pipe(format!(
            "fr.francois-brille.openlw.control-test.{}",
            std::process::id()
        ));
        let _server = s.serve(&ep).unwrap();
        let client = lw_sys::ctl::Client::connect(&ep).unwrap();
        let v: Value = serde_json::from_str(&client.call(r#"{"cmd":"status"}"#).unwrap()).unwrap();
        assert_eq!(v["status"]["iface"], "lo0");
        assert_eq!(v["status"]["pid"], std::process::id());
    }
}
