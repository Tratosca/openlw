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

/// Device layout (macOS only; other systems always use `duplex`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Layout {
    /// One multichannel “OpenLW” input/output device, fixed name. `split` (former
    /// “OpenLW In” / “OpenLW Out” layout, same channel space) is read as duplex.
    #[default]
    #[serde(alias = "split")]
    Duplex,
    /// Numbered devices “OpenLW In n” / “OpenLW Out n”, one source each, as wide as their
    /// source (see [`crate::labels`] for names).
    Multi,
}

/// Mono patch of a stereo stream onto one device channel.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Mix {
    /// Left channel only.
    Left,
    /// Right channel only.
    Right,
    /// (L + R) / 2: −6 dB, no clipping on correlated material.
    Sum,
}

impl Mix {
    /// Mono sample from a stereo frame (missing channels read as silence).
    pub fn apply(self, frame: &[f32]) -> f32 {
        let (l, r) = (
            frame.first().copied().unwrap_or(0.0),
            frame.get(1).copied().unwrap_or(0.0),
        );
        match self {
            Mix::Left => l,
            Mix::Right => r,
            Mix::Sum => (l + r) * 0.5,
        }
    }
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
    /// eight for surround). `multi` layout: channels within device `device`. Absent: test generator.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device_channels: Option<Vec<u16>>,
    /// `multi` layout: output device number (“OpenLW Out n”, 1-based).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device: Option<u16>,
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
    /// Device inputs (1-based) fed by this stream, one per stream channel, or one with `mix`.
    /// `multi` layout: channels within device `device`. Absent: receive for statistics only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device_channels: Option<Vec<u16>>,
    /// `multi` layout: input device number (“OpenLW In n”, 1-based).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device: Option<u16>,
    /// Mono patch (stereo or backfeed stream onto one channel).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mix: Option<Mix>,
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

    /// Device channels fed by this stream: one with `mix`, otherwise the stream width.
    pub fn patch_width(&self) -> usize {
        if self.mix.is_some() {
            1
        } else {
            self.stream_channels()
        }
    }
}

/// Device ring of a patch within its direction: device n → n − 1; duplex layout (no device) → 0.
pub fn ring_of(device: Option<u16>) -> usize {
    device.map_or(0, |n| usize::from(n).saturating_sub(1))
}

/// Device slots (device number, 1-based channel) of a patch; device 0 in `duplex` layout.
pub fn slots(device: Option<u16>, chs: &[u16]) -> Vec<(u16, u16)> {
    chs.iter().map(|&c| (device.unwrap_or(0), c)).collect()
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
    /// `multi` layout: name devices after their source (default), otherwise “OpenLW In n”
    /// (see [`crate::labels`]).
    #[serde(default = "default_true")]
    pub custom_device_names: bool,
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

    /// `multi` layout?
    pub fn multi(&self) -> bool {
        self.device_layout == Layout::Multi
    }

    /// `multi` layout: device count per direction, one per configured pair (1 to 16).
    pub fn device_count(&self, to_net: bool) -> u16 {
        let dev = self.device_config();
        let ch = if to_net {
            dev.channels_to_net
        } else {
            dev.channels_from_net
        };
        u16::try_from(ch.div_ceil(2)).unwrap_or(u16::MAX)
    }

    /// `multi` layout: width of each input device (index 0 = “OpenLW In 1”): its patched
    /// stream's, 2 when empty. Duplex layout: one entry, the device input count.
    pub fn in_widths(&self) -> Vec<u32> {
        if !self.multi() {
            return vec![self.device_config().channels_from_net];
        }
        (1..=self.device_count(false))
            .map(|n| {
                self.destinations
                    .iter()
                    .find(|d| d.device == Some(n) && d.device_channels.is_some())
                    .map_or(2, |d| d.patch_width() as u32)
            })
            .collect()
    }

    /// Output-device widths (see [`Config::in_widths`]).
    pub fn out_widths(&self) -> Vec<u32> {
        if !self.multi() {
            return vec![self.device_config().channels_to_net];
        }
        (1..=self.device_count(true))
            .map(|n| {
                self.sources
                    .iter()
                    .find(|s| s.device == Some(n) && s.device_channels.is_some())
                    .map_or(2, |s| u32::from(StreamFormat::from(s.format).channels()))
            })
            .collect()
    }

    /// Shared-region rings: output devices, then input devices (one of each in duplex layout).
    pub fn rings(&self) -> Vec<lw_sys::shm::RingSpec> {
        let out = self
            .out_widths()
            .into_iter()
            .map(lw_sys::shm::RingSpec::to_net);
        out.chain(
            self.in_widths()
                .into_iter()
                .map(lw_sys::shm::RingSpec::from_net),
        )
        .collect()
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
    /// Duplex layout: one channel space per direction; a stereo patch occupies a pair (odd
    /// first channel), a mono patch one channel. `multi` layout: one stream per device,
    /// channels 1..width within the device.
    fn validate_patch(&self) -> Result<(), ConfigError> {
        let dev = self.device.clone().unwrap_or_default();
        let multi = self.multi();
        // Common checks for a patch: device number per layout, channel range and shape.
        let check = |what: &str,
                     device: Option<u16>,
                     chs: &[u16],
                     width: usize,
                     devices: u16,
                     max: u32|
         -> Result<(), ConfigError> {
            let err = |m: String| Err(ConfigError(format!("{what}: {m}")));
            if chs.len() != width {
                return err(format!("{} device channels, {width} expected", chs.len()));
            }
            match (multi, device) {
                (true, Some(n)) if (1..=devices).contains(&n) => {
                    if chs.iter().zip(1u16..).any(|(&c, i)| c != i) {
                        return err(format!("channels 1..{width} of device {n} expected"));
                    }
                }
                (true, Some(n)) => return err(format!("device {n} outside 1..{devices}")),
                (true, None) => return err("device number missing (multi layout)".into()),
                (false, Some(_)) => {
                    return err("device number given in duplex layout".into());
                }
                (false, None) => {
                    if let Some(c) = chs.iter().find(|&&c| c == 0 || u32::from(c) > max) {
                        return err(format!("channel {c} outside device range 1..{max}"));
                    }
                    if let [a, b] = chs {
                        if a % 2 == 0 || *b != a + 1 {
                            return err(format!("stereo patch on a pair (1-2, 3-4…), not {a}-{b}"));
                        }
                    }
                }
            }
            Ok(())
        };
        let mut used_out = std::collections::BTreeSet::new();
        for s in &self.sources {
            if let Some(chs) = &s.device_channels {
                let want = usize::from(StreamFormat::from(s.format).channels());
                let what = format!("source \"{}\"", s.name);
                check(
                    &what,
                    s.device,
                    chs,
                    want,
                    self.device_count(true),
                    dev.channels_to_net,
                )?;
                // Several streams may share duplex outputs; a multi-layout device has one source.
                if multi && !used_out.insert(s.device) {
                    return Err(ConfigError(format!(
                        "output device {} patched twice",
                        s.device.unwrap_or(0)
                    )));
                }
            } else if s.device.is_some() {
                return Err(ConfigError(format!(
                    "source \"{}\": device number without device channels",
                    s.name
                )));
            }
        }
        let mut used = std::collections::BTreeSet::new();
        for d in &self.destinations {
            if d.mix.is_some() && d.kind == Kind::Surround {
                return Err(ConfigError(format!(
                    "destination {}: mono patch of a surround stream",
                    d.label()
                )));
            }
            if let Some(chs) = &d.device_channels {
                let what = format!("destination {}", d.label());
                check(
                    &what,
                    d.device,
                    chs,
                    d.patch_width(),
                    self.device_count(false),
                    dev.channels_from_net,
                )?;
                for slot in slots(d.device, chs) {
                    if !used.insert(slot) {
                        return Err(ConfigError(if multi {
                            format!("input device {} patched twice", slot.0)
                        } else {
                            format!("device input {} patched twice", slot.1)
                        }));
                    }
                }
            } else if d.device.is_some() {
                return Err(ConfigError(format!(
                    "destination {}: device number without device channels",
                    d.label()
                )));
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

    fn check(json: &str) -> Result<Config, ConfigError> {
        let c: Config = serde_json::from_str(json).unwrap();
        c.validate().map(|()| c)
    }

    #[test]
    fn duplex_patch_shapes() {
        let dev = r#""iface":"x","device":{"channels_to_net":4,"channels_from_net":4}"#;
        let ok = |d: &str| check(&format!(r#"{{{dev},"destinations":[{d}]}}"#));
        assert!(ok(r#"{"channel":1,"device_channels":[3,4]}"#).is_ok());
        assert!(
            ok(r#"{"channel":1,"device_channels":[2,3]}"#).is_err(),
            "stereo off the pair"
        );
        assert!(
            ok(r#"{"channel":1,"mix":"right","device_channels":[2]}"#).is_ok(),
            "mono on any channel"
        );
        assert!(
            ok(r#"{"channel":1,"mix":"left","device_channels":[1,2]}"#).is_err(),
            "mono: one channel"
        );
        assert!(
            ok(r#"{"channel":1,"kind":"surround","mix":"sum","device_channels":[1]}"#).is_err()
        );
        assert!(
            ok(r#"{"channel":1,"device":1,"device_channels":[1,2]}"#).is_err(),
            "device in duplex"
        );
        assert!(
            ok(r#"{"channel":1,"mix":"left","device_channels":[1]},{"channel":2,"device_channels":[1,2]}"#).is_err(),
            "input 1 patched twice"
        );
        assert!(ok(r#"{"channel":1,"mix":"left","device_channels":[1]},{"channel":2,"mix":"sum","device_channels":[2]}"#).is_ok());
        // Former two-device layout: same channel space, read as duplex.
        let c = check(&format!(r#"{{{dev},"device_layout":"split"}}"#)).unwrap();
        assert_eq!(c.device_layout, Layout::Duplex);
        assert!(c.custom_device_names, "custom names by default");
        assert_eq!(c.in_widths(), vec![4]);
    }

    #[test]
    fn multi_patch_shapes_and_widths() {
        let dev = r#""iface":"x","device_layout":"multi","device":{"channels_to_net":2,"channels_from_net":6}"#;
        let ok = |d: &str| check(&format!(r#"{{{dev},"destinations":[{d}]}}"#));
        let c = ok(
            r#"{"channel":1,"device":3,"kind":"surround","device_channels":[1,2,3,4,5,6,7,8]},
                     {"channel":2,"device":1,"mix":"sum","device_channels":[1]},{"channel":3}"#,
        )
        .unwrap();
        assert_eq!(c.device_count(false), 3);
        assert_eq!(c.in_widths(), vec![1, 2, 8], "mono, empty, surround");
        assert_eq!(c.out_widths(), vec![2]);
        assert_eq!(c.rings().len(), 4);
        assert!(
            ok(r#"{"channel":1,"device":4,"device_channels":[1,2]}"#).is_err(),
            "3 devices"
        );
        assert!(
            ok(r#"{"channel":1,"device_channels":[1,2]}"#).is_err(),
            "device missing"
        );
        assert!(
            ok(r#"{"channel":1,"device":1,"device_channels":[2,3]}"#).is_err(),
            "channels 1..w"
        );
        assert!(
            ok(r#"{"channel":1,"device":2,"device_channels":[1,2]},{"channel":2,"device":2,"mix":"left","device_channels":[1]}"#).is_err(),
            "one stream per device"
        );
        let out = |s: &str| check(&format!(r#"{{{dev},"sources":[{s}]}}"#));
        assert!(out(
            r#"{"channel":4001,"name":"A","format":"standard","device":1,"device_channels":[1,2]}"#
        )
        .is_ok());
        assert!(out(
            r#"{"channel":4001,"name":"A","format":"standard","device":2,"device_channels":[1,2]}"#
        )
        .is_err());
    }

    #[test]
    fn mix_values() {
        let f = [0.5, -0.25];
        assert_eq!(
            (
                Mix::Left.apply(&f),
                Mix::Right.apply(&f),
                Mix::Sum.apply(&f)
            ),
            (0.5, -0.25, 0.125)
        );
    }
}
