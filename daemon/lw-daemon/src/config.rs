//! Persisted daemon configuration (JSON): NIC, transmitted sources, received destinations, advertisements, QoS.
//!
//! Example:
//! ```json
//! {
//!   "iface": "auto",
//!   "terminal_name": "Studio Mac",
//!   "advertise": true,
//!   "tos": 184,
//!   "sources": [ { "channel": 4001, "name": "MAC 1", "format": "standard", "tone_hz": 997, "level_dbfs": -20 } ],
//!   "destinations": [ { "channel": 1, "kind": "stereo" } ]
//! }
//! ```
//!
//! `iface`: `auto` (default) selects the interface receiving Livewire advertisements (see `detect`);
//! otherwise BSD or friendly name. Empty `terminal_name`: computer name.

use std::net::Ipv4Addr;
use std::path::Path;

use lw_proto::adv::{AdvStreamType, Source};
use lw_proto::channel::{Channel, GroupKind, AUDIO_PORT};
use lw_proto::format::StreamFormat;
use serde::{Deserialize, Serialize};

use crate::tx::{Tone, TxStream};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Format {
    Standard,
    Aes67,
    Livestream,
    Surround,
}

impl From<Format> for StreamFormat {
    fn from(f: Format) -> Self {
        match f {
            Format::Standard => StreamFormat::Standard,
            Format::Aes67 => StreamFormat::Aes67,
            Format::Livestream => StreamFormat::Livestream,
            Format::Surround => StreamFormat::Surround,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    #[default]
    Stereo,
    Backfeed,
    Surround,
}

impl From<Kind> for GroupKind {
    fn from(k: Kind) -> Self {
        match k {
            Kind::Stereo => GroupKind::Stereo,
            Kind::Backfeed => GroupKind::Backfeed,
            Kind::Surround => GroupKind::Surround,
        }
    }
}

/// Receive-latency preset: daemon jitter buffer and plugin input margin.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Latency {
    /// ≈ 8 ms added: dedicated network, no other traffic.
    Low,
    /// ≈ 17 ms added.
    #[default]
    Normal,
    /// ≈ 35 ms added: shared network or loaded computer.
    Safe,
}

impl Latency {
    /// Receive jitter-buffer target (frames): 6, 12, or 24 ms.
    pub fn rx_target(self) -> usize {
        match self {
            Latency::Low => 288,
            Latency::Normal => 576,
            Latency::Safe => 1152,
        }
    }

    /// Plugin input margin beyond an I/O block (frames): 2.7, 5.3, or 10.7 ms.
    pub fn input_margin(self) -> u32 {
        match self {
            Latency::Low => 128,
            Latency::Normal => 256,
            Latency::Safe => 512,
        }
    }

    /// Transmit-buffer reserve beyond two packets (frames).
    pub fn tx_cushion(self) -> usize {
        match self {
            Latency::Low => 256,
            Latency::Normal => 512,
            Latency::Safe => 1024,
        }
    }
}

/// Device layout in macOS.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Layout {
    /// One “OpenLW” input/output device, fixed name.
    #[default]
    Duplex,
    /// Two devices, “OpenLW In” and “OpenLW Out”, optionally named after patched channels.
    Split,
}

/// Maximum advertised-name length (`ATRN`, 32 ASCII bytes).
pub const MAX_TERMINAL_NAME: usize = 32;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourceConfig {
    pub channel: u16,
    pub name: String,
    pub format: Format,
    #[serde(default = "default_pt")]
    pub payload_type: u8,
    #[serde(default = "default_tone")]
    pub tone_hz: f64,
    #[serde(default = "default_level")]
    pub level_dbfs: f64,
    /// Device outputs (1-based) transmitted by this stream, one per stream channel (two for stereo,
    /// eight for surround). Absent: test generator.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device_channels: Option<Vec<u16>>,
}

/// Destination: a Livewire channel (and family) or an explicit group (imported SDP, third-party AES67).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DestinationConfig {
    #[serde(default)]
    pub channel: Option<u16>,
    #[serde(default)]
    pub kind: Kind,
    #[serde(default)]
    pub group: Option<Ipv4Addr>,
    #[serde(default = "default_port")]
    pub port: u16,
    /// Device inputs (1-based) fed by this stream, one per stream channel.
    /// Absent: receive for statistics only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device_channels: Option<Vec<u16>>,
}

impl DestinationConfig {
    /// Readable label: “channel 12” or “239.192.0.9:5004”.
    pub fn label(&self) -> String {
        match (self.channel, self.group) {
            (Some(c), _) => format!("channel {c}"),
            (None, Some(g)) => format!("{g}:{}", self.port),
            _ => "?".into(),
        }
    }

    /// Received stream channel count (eight for surround, otherwise two).
    pub fn stream_channels(&self) -> usize {
        if self.kind == Kind::Surround {
            8
        } else {
            2
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    /// Livewire interface BSD name (enX), friendly name, or [`AUTO_IFACE`].
    #[serde(default = "default_iface")]
    pub iface: String,
    #[serde(default = "default_terminal")]
    pub terminal_name: String,
    #[serde(default = "default_true")]
    pub advertise: bool,
    /// Audio-stream TOS byte (DSCP × 4); 184 = EF.
    #[serde(default = "default_tos")]
    pub tos: u32,
    /// Receive-latency preset.
    #[serde(default)]
    pub latency: Latency,
    #[serde(default)]
    pub sources: Vec<SourceConfig>,
    #[serde(default)]
    pub destinations: Vec<DestinationConfig>,
    /// Virtual device shared with the audio client (enabled by default with `--control`).
    #[serde(default)]
    pub device: Option<crate::device::DeviceConfig>,
    /// Device layout in macOS.
    #[serde(default)]
    pub device_layout: Layout,
    /// With `split` layout, name devices after patched channels (see [`crate::labels`]).
    #[serde(default)]
    pub name_device_from_sources: bool,
}

fn default_pt() -> u8 {
    96
}
fn default_tone() -> f64 {
    997.0
}
fn default_level() -> f64 {
    -20.0
}
fn default_port() -> u16 {
    AUDIO_PORT
}
fn default_terminal() -> String {
    String::new()
}
fn default_iface() -> String {
    AUTO_IFACE.into()
}

/// Default transmitted source name (unnamed output patch).
pub const DEFAULT_SOURCE_NAME: &str = if cfg!(target_os = "macos") {
    "MAC"
} else {
    "PC"
};

/// `iface` value for automatic interface selection.
pub const AUTO_IFACE: &str = "auto";

/// Maximum device channel count per direction (16 stereo Livewire channels).
pub const MAX_DEVICE_CHANNELS: u32 = 32;

/// Computer name, advertised on the network by default.
/// macOS: System Settings > General > Sharing, falling back to hostname; Linux: friendly name from
/// `/etc/machine-info` (`hostnamectl --pretty`), falling back to hostname; Windows: NetBIOS name.
pub fn computer_name() -> String {
    let clean = |s: String| Some(s.trim().to_string()).filter(|s| !s.is_empty());
    #[cfg(target_os = "macos")]
    let name = {
        let run = |cmd: &str, args: &[&str]| {
            std::process::Command::new(cmd)
                .args(args)
                .output()
                .ok()
                .and_then(|o| String::from_utf8(o.stdout).ok())
                .and_then(clean)
        };
        run("/usr/sbin/scutil", &["--get", "ComputerName"])
            .or_else(|| run("/bin/hostname", &["-s"]))
    };
    #[cfg(target_os = "linux")]
    let name = std::fs::read_to_string("/etc/machine-info")
        .ok()
        .and_then(|t| {
            t.lines()
                .find_map(|l| l.strip_prefix("PRETTY_HOSTNAME="))
                .map(|v| v.trim_matches('"').to_string())
        })
        .and_then(clean)
        .or_else(|| {
            std::fs::read_to_string("/proc/sys/kernel/hostname")
                .ok()
                .and_then(clean)
        });
    #[cfg(windows)]
    let name = std::env::var("COMPUTERNAME").ok().and_then(clean);
    name.unwrap_or_else(|| "OpenLW".into())
}
fn default_true() -> bool {
    true
}
fn default_tos() -> u32 {
    0xB8
}

/// Configuration error with a user-facing message.
#[derive(Debug)]
pub struct ConfigError(pub String);

impl std::fmt::Display for ConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ConfigError {}

impl Config {
    /// Default installation configuration (`daemon/lw-daemon.default.json`).
    pub const DEFAULT_JSON: &'static str = include_str!("../../lw-daemon.default.json");

    /// Write default configuration to `path` if absent (including directories).
    pub fn init_if_missing(path: &Path) -> Result<(), ConfigError> {
        if path.exists() {
            return Ok(());
        }
        let err = |e: std::io::Error| ConfigError(format!("{}: {e}", path.display()));
        if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
            std::fs::create_dir_all(dir).map_err(err)?;
        }
        std::fs::write(path, Self::DEFAULT_JSON).map_err(err)
    }

    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| ConfigError(format!("{}: {e}", path.display())))?;
        let cfg: Config = serde_json::from_str(&text)
            .map_err(|e| ConfigError(format!("{}: {e}", path.display())))?;
        cfg.validate()?;
        Ok(cfg)
    }

    /// Save atomically: write an adjacent file, then rename (a crash
    /// leaves either the old or new configuration, never a truncated file).
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        let text = serde_json::to_string_pretty(self).map_err(std::io::Error::other)?;
        let mut tmp = path.as_os_str().to_owned();
        tmp.push(".tmp");
        let tmp = std::path::PathBuf::from(tmp);
        std::fs::write(&tmp, text + "\n")?;
        std::fs::rename(&tmp, path)
    }

    /// Automatic interface selection?
    pub fn auto_iface(&self) -> bool {
        self.iface.is_empty() || self.iface == AUTO_IFACE
    }

    /// Advertised terminal name.
    pub fn terminal(&self) -> String {
        if self.terminal_name.trim().is_empty() {
            computer_name()
        } else {
            self.terminal_name.clone()
        }
    }

    /// Virtual device parameters (default: two channels per direction).
    pub fn device_config(&self) -> crate::device::DeviceConfig {
        self.device.clone().unwrap_or_default()
    }

    pub fn validate(&self) -> Result<(), ConfigError> {
        if self.terminal_name.chars().count() > MAX_TERMINAL_NAME {
            return Err(ConfigError(format!(
                "advertised name: {MAX_TERMINAL_NAME} characters at most"
            )));
        }
        if self.tos > 0xFC || self.tos % 4 != 0 {
            return Err(ConfigError(format!(
                "network priority: invalid TOS byte {} (DSCP 0 to 63, × 4)",
                self.tos
            )));
        }
        let dev = self.device_config();
        for (n, what) in [
            (dev.channels_to_net, "outputs"),
            (dev.channels_from_net, "inputs"),
        ] {
            if !(1..=MAX_DEVICE_CHANNELS).contains(&n) {
                return Err(ConfigError(format!(
                    "device: {n} {what}, expected 1 to {MAX_DEVICE_CHANNELS}"
                )));
            }
        }
        let mut seen = std::collections::BTreeSet::new();
        for s in &self.sources {
            Channel::new(s.channel).ok_or_else(|| {
                ConfigError(format!(
                    "source \"{}\": channel {} outside 1..32766",
                    s.name, s.channel
                ))
            })?;
            if !seen.insert(s.channel) {
                return Err(ConfigError(format!(
                    "channel {} transmitted twice",
                    s.channel
                )));
            }
        }
        if self.sources.len() > lw_proto::adv::MAX_SOURCES {
            return Err(ConfigError("more than 240 sources".into()));
        }
        for d in &self.destinations {
            match (d.channel, d.group) {
                (Some(c), None) => {
                    Channel::new(c).ok_or_else(|| {
                        ConfigError(format!("destination: channel {c} outside 1..32766"))
                    })?;
                }
                (None, Some(g)) if g.is_multicast() => {}
                (None, Some(g)) => {
                    return Err(ConfigError(format!("destination: {g} is not multicast")))
                }
                _ => {
                    return Err(ConfigError(
                        "destination: specify either `channel` or `group`".into(),
                    ))
                }
            }
        }
        self.validate_patch()
    }

    /// Patch: existing device channels, count matching the stream, unshared inputs.
    fn validate_patch(&self) -> Result<(), ConfigError> {
        let dev = self.device.clone().unwrap_or_default();
        for s in &self.sources {
            if let Some(chs) = &s.device_channels {
                let want = usize::from(StreamFormat::from(s.format).channels());
                if chs.len() != want {
                    return Err(ConfigError(format!(
                        "source \"{}\": {} device channels for a {want}-channel stream",
                        s.name,
                        chs.len()
                    )));
                }
                if let Some(c) = chs
                    .iter()
                    .find(|&&c| c == 0 || u32::from(c) > dev.channels_to_net)
                {
                    return Err(ConfigError(format!(
                        "source \"{}\": output {c} outside device range 1..{}",
                        s.name, dev.channels_to_net
                    )));
                }
            }
        }
        let mut used = std::collections::BTreeSet::new();
        for d in &self.destinations {
            if let Some(chs) = &d.device_channels {
                if chs.len() != d.stream_channels() {
                    return Err(ConfigError(format!(
                        "destination {}: {} device inputs for a {}-channel stream",
                        d.label(),
                        chs.len(),
                        d.stream_channels()
                    )));
                }
                for &c in chs {
                    if c == 0 || u32::from(c) > dev.channels_from_net {
                        return Err(ConfigError(format!(
                            "destination {}: input {c} outside device range 1..{}",
                            d.label(),
                            dev.channels_from_net
                        )));
                    }
                    if !used.insert(c) {
                        return Err(ConfigError(format!("device input {c} patched twice")));
                    }
                }
            }
        }
        Ok(())
    }

    pub fn tx_streams(&self) -> Vec<TxStream> {
        self.sources
            .iter()
            .filter_map(|s| {
                let mut t = TxStream::new(Channel::new(s.channel)?, s.format.into());
                t.payload_type = s.payload_type;
                t.tone = Tone::new(s.tone_hz, s.level_dbfs);
                Some(t)
            })
            .collect()
    }

    /// Sources to advertise (surround advertises `FAST=4`, others L24 stereo).
    pub fn adv_sources(&self) -> Vec<Source> {
        self.sources
            .iter()
            .enumerate()
            .filter_map(|(i, s)| {
                let ty = if s.format == Format::Surround {
                    AdvStreamType::Surround
                } else {
                    AdvStreamType::StereoL24
                };
                Some(Source::new(
                    u16::try_from(i + 1).ok()?,
                    Channel::new(s.channel)?,
                    &s.name,
                    ty,
                ))
            })
            .collect()
    }

    /// Groups to receive.
    pub fn rx_groups(&self) -> Vec<(Ipv4Addr, u16)> {
        self.destinations
            .iter()
            .filter_map(|d| match (d.channel, d.group) {
                (Some(c), _) => Some((Channel::new(c)?.group(d.kind.into()), d.port)),
                (None, Some(g)) => Some((g, d.port)),
                _ => None,
            })
            .collect()
    }
}

#[cfg(test)]
#[allow(clippy::indexing_slicing)]
mod tests {
    use super::*;

    #[test]
    fn default_configuration_is_valid_and_initialised_once() {
        let dir = std::env::temp_dir().join(format!("openlw-config-{}", std::process::id()));
        let path = dir.join("sub").join("lw-daemon.json");
        Config::init_if_missing(&path).unwrap();
        let c = Config::load(&path).unwrap();
        assert!(c.auto_iface());
        std::fs::write(&path, r#"{"iface":"x"}"#).unwrap();
        Config::init_if_missing(&path).unwrap();
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            r#"{"iface":"x"}"#,
            "existing file kept"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn parse_and_validate() {
        let cfg: Config = serde_json::from_str(
            r#"{"iface":"en7","sources":[{"channel":4001,"name":"MAC 1","format":"standard"},
               {"channel":5,"name":"SURR","format":"surround"}],
               "destinations":[{"channel":1},{"group":"239.192.0.9","port":5004}]}"#,
        )
        .unwrap();
        cfg.validate().unwrap();
        assert_eq!(cfg.tos, 0xB8);
        assert_eq!(cfg.tx_streams()[1].group(), Ipv4Addr::new(239, 196, 0, 5));
        assert_eq!(cfg.adv_sources()[1].stream_type, AdvStreamType::Surround);
        assert_eq!(
            cfg.rx_groups(),
            vec![
                (Ipv4Addr::new(239, 192, 0, 1), 5004),
                (Ipv4Addr::new(239, 192, 0, 9), 5004)
            ]
        );
        let bad: Config = serde_json::from_str(
            r#"{"iface":"x","sources":[{"channel":0,"name":"z","format":"aes67"}]}"#,
        )
        .unwrap();
        assert!(bad.validate().is_err());
    }
}
