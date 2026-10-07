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

/// Device layout (macOS and Linux; Windows always uses `duplex`).
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

/// Former mono-patch field (`mix`), read from older configurations and commands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Mix {
    /// Left channel only.
    Left,
    /// Right channel only.
    Right,
    /// (L + R) / 2.
    Sum,
}

impl Mix {
    /// Stream channels (1-based) of this mix.
    pub fn from_channels(self) -> Vec<u8> {
        match self {
            Mix::Left => vec![1],
            Mix::Right => vec![2],
            Mix::Sum => vec![1, 2],
        }
    }
}

/// Crosspoint: one device input fed by one or two stream channels.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Tap {
    /// `multi` layout: input device number (“OpenLW In n”, 1-based). Absent in duplex layout.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device: Option<u16>,
    /// Device input, 1-based (within `device` in `multi` layout).
    pub channel: u16,
    /// Stream channels (1-based) feeding it: [1] left, [2] right, [1, 2] (L + R) / 2 (−6 dB,
    /// no clipping on correlated material), [k] channel k of a surround stream.
    pub from: Vec<u8>,
}

impl Tap {
    /// Device slot (device number, or 0 in duplex layout; channel).
    pub fn slot(&self) -> (u16, u16) {
        (self.device.unwrap_or(0), self.channel)
    }
}

/// Taps of a stream patched “as is”: stream channel i on device channel `first + i`.
pub fn straight_taps(device: Option<u16>, first: u16, channels: usize) -> Vec<Tap> {
    (0..channels)
        .filter_map(|i| {
            Some(Tap {
                device,
                channel: first.checked_add(u16::try_from(i).ok()?)?,
                from: vec![u8::try_from(i + 1).ok()?],
            })
        })
        .collect()
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
#[serde(from = "DestinationRepr")]
pub struct DestinationConfig {
    pub channel: Option<u16>,
    pub kind: Kind,
    pub group: Option<Ipv4Addr>,
    pub port: u16,
    /// Device inputs fed by this stream. Empty: received for statistics only.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub taps: Vec<Tap>,
}

/// Destination as stored, including the former patch fields (`device_channels`, `device`,
/// `mix`), converted to taps.
#[derive(Deserialize)]
struct DestinationRepr {
    #[serde(default)]
    channel: Option<u16>,
    #[serde(default)]
    kind: Kind,
    #[serde(default)]
    group: Option<Ipv4Addr>,
    #[serde(default = "default_port")]
    port: u16,
    #[serde(default)]
    taps: Vec<Tap>,
    #[serde(default)]
    device_channels: Option<Vec<u16>>,
    #[serde(default)]
    device: Option<u16>,
    #[serde(default)]
    mix: Option<Mix>,
}

impl From<DestinationRepr> for DestinationConfig {
    fn from(r: DestinationRepr) -> Self {
        let mut taps = r.taps;
        if let (true, Some(chs)) = (taps.is_empty(), r.device_channels) {
            taps = match (r.mix, chs.first()) {
                (Some(m), Some(&c)) => vec![Tap {
                    device: r.device,
                    channel: c,
                    from: m.from_channels(),
                }],
                _ => chs
                    .iter()
                    .zip(1u8..)
                    .map(|(&c, i)| Tap {
                        device: r.device,
                        channel: c,
                        from: vec![i],
                    })
                    .collect(),
            };
        }
        Self {
            channel: r.channel,
            kind: r.kind,
            group: r.group,
            port: r.port,
            taps,
        }
    }
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

    /// Patched to at least one device input?
    pub fn patched(&self) -> bool {
        !self.taps.is_empty()
    }

    /// Same stream (Livewire channel and kind, or group and port)?
    pub fn same_stream(
        &self,
        channel: Option<u16>,
        group: Option<Ipv4Addr>,
        port: u16,
        kind: Kind,
    ) -> bool {
        match (channel, group) {
            (Some(ch), _) => self.channel == Some(ch) && self.kind == kind,
            (None, Some(g)) => self.group == Some(g) && self.port == port,
            _ => false,
        }
    }
}

/// Device ring of a patch within its direction: device n → n − 1; duplex layout (no device) → 0.
pub fn ring_of(device: Option<u16>) -> usize {
    device.map_or(0, |n| usize::from(n).saturating_sub(1))
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
    /// Device layout (macOS and Linux).
    #[serde(default)]
    pub device_layout: Layout,
    /// `multi` layout: name devices after their source (default), otherwise “OpenLW In n”
    /// (see [`crate::labels`]).
    #[serde(default = "default_true")]
    pub custom_device_names: bool,
    /// Input pairs (duplex layout: pair n = inputs 2n − 1 and 2n) or input devices (multi
    /// layout: “OpenLW In n”) that are not coupled in stereo. A coupled pair takes a stereo
    /// patch in one click; an uncoupled one takes left, right, or L + R per input. An
    /// uncoupled input device is mono.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub uncoupled_inputs: Vec<u16>,
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

    /// Is input pair (or input device) `n` coupled in stereo?
    pub fn coupled(&self, n: u16) -> bool {
        !self.uncoupled_inputs.contains(&n)
    }

    /// `multi` layout: width of each input device (index 0 = “OpenLW In 1”): 1 when
    /// uncoupled, 8 with a surround stream, otherwise 2. Duplex layout: one entry, the device
    /// input count.
    pub fn in_widths(&self) -> Vec<u32> {
        if !self.multi() {
            return vec![self.device_config().channels_from_net];
        }
        (1..=self.device_count(false))
            .map(|n| {
                let surround = self.destinations.iter().any(|d| {
                    d.kind == Kind::Surround && d.taps.iter().any(|t| t.device == Some(n))
                });
                if !self.coupled(n) {
                    1
                } else if surround {
                    8
                } else {
                    2
                }
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

    /// Patch: existing device channels, unshared inputs. Outputs: one stream per pair (duplex
    /// layout, odd first output) or per device (multi layout). Inputs: crosspoints (taps), at
    /// most one per device input; multi layout: one stream per device, within its width.
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
        // Inputs: each device input takes at most one tap; a multi-layout device carries one
        // stream, within its width.
        let widths = self.in_widths();
        let mut used = std::collections::BTreeSet::new();
        let mut device_stream: std::collections::BTreeMap<u16, usize> = Default::default();
        for (i, d) in self.destinations.iter().enumerate() {
            let what = format!("destination {}", d.label());
            let err = |m: String| Err(ConfigError(format!("{what}: {m}")));
            for t in &d.taps {
                let n = d.stream_channels();
                if t.from.is_empty()
                    || t.from.len() > 2
                    || t.from.iter().any(|&f| f == 0 || usize::from(f) > n)
                    || (t.from.len() == 2 && (n != 2 || t.from != [1, 2]))
                {
                    return err(format!(
                        "input {} fed by stream channels {:?}",
                        t.channel, t.from
                    ));
                }
                match (multi, t.device) {
                    (true, Some(dev_n)) => {
                        let w = widths.get(usize::from(dev_n).wrapping_sub(1)).copied();
                        let Some(w) = w else {
                            return err(format!("device {dev_n} outside 1..{}", widths.len()));
                        };
                        if t.channel == 0 || u32::from(t.channel) > w {
                            return err(format!(
                                "channel {} outside device {dev_n} (1..{w})",
                                t.channel
                            ));
                        }
                        if *device_stream.entry(dev_n).or_insert(i) != i {
                            return Err(ConfigError(format!(
                                "input device {dev_n}: one stream per device"
                            )));
                        }
                    }
                    (true, None) => return err("device number missing (multi layout)".into()),
                    (false, Some(_)) => return err("device number given in duplex layout".into()),
                    (false, None) => {
                        if t.channel == 0 || u32::from(t.channel) > dev.channels_from_net {
                            return err(format!(
                                "input {} outside device range 1..{}",
                                t.channel, dev.channels_from_net
                            ));
                        }
                    }
                }
                if !used.insert(t.slot()) {
                    return Err(ConfigError(if multi {
                        format!(
                            "input device {} channel {} patched twice",
                            t.slot().0,
                            t.channel
                        )
                    } else {
                        format!("device input {} patched twice", t.channel)
                    }));
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

    fn check(json: &str) -> Result<Config, ConfigError> {
        let c: Config = serde_json::from_str(json).unwrap();
        c.validate().map(|()| c)
    }

    #[test]
    fn duplex_taps() {
        let dev = r#""iface":"x","device":{"channels_to_net":4,"channels_from_net":4}"#;
        let ok = |d: &str| check(&format!(r#"{{{dev},"destinations":[{d}]}}"#));
        // Former fields converted to taps.
        let c = ok(r#"{"channel":1,"device_channels":[3,4]},{"channel":2,"mix":"sum","device_channels":[1]}"#).unwrap();
        assert_eq!(c.destinations[0].taps, straight_taps(None, 3, 2));
        assert_eq!(c.destinations[1].taps[0].from, [1, 2]);
        let json = serde_json::to_string(&c.destinations[0]).unwrap();
        assert!(
            json.contains("taps") && !json.contains("device_channels"),
            "{json}"
        );
        // Free crosspoints: stereo on 2-3, the same left on two inputs.
        assert!(
            ok(r#"{"channel":1,"taps":[{"channel":2,"from":[1]},{"channel":3,"from":[2]}]}"#)
                .is_ok()
        );
        assert!(
            ok(r#"{"channel":1,"taps":[{"channel":1,"from":[1]},{"channel":2,"from":[1]}]}"#)
                .is_ok()
        );
        assert!(ok(r#"{"channel":1,"taps":[{"channel":1,"from":[1]}]},{"channel":2,"taps":[{"channel":1,"from":[2]}]}"#).is_err(), "input 1 twice");
        assert!(
            ok(r#"{"channel":1,"taps":[{"channel":5,"from":[1]}]}"#).is_err(),
            "4 inputs"
        );
        assert!(ok(r#"{"channel":1,"taps":[{"channel":1,"from":[]}]}"#).is_err());
        assert!(
            ok(r#"{"channel":1,"kind":"surround","taps":[{"channel":1,"from":[1,2]}]}"#).is_err()
        );
        assert!(
            ok(r#"{"channel":1,"taps":[{"device":1,"channel":1,"from":[1]}]}"#).is_err(),
            "device in duplex"
        );
        // Former two-device layout: same channel space, read as duplex.
        let c = check(&format!(r#"{{{dev},"device_layout":"split"}}"#)).unwrap();
        assert_eq!(c.device_layout, Layout::Duplex);
        assert!(c.custom_device_names, "custom names by default");
        assert_eq!(c.in_widths(), vec![4]);
    }

    #[test]
    fn multi_taps_and_widths() {
        let dev = r#""iface":"x","device_layout":"multi","device":{"channels_to_net":2,"channels_from_net":6},"uncoupled_inputs":[1]"#;
        let ok = |d: &str| check(&format!(r#"{{{dev},"destinations":[{d}]}}"#));
        let c = ok(
            r#"{"channel":1,"device":3,"kind":"surround","device_channels":[1,2,3,4,5,6,7,8]},
                     {"channel":2,"taps":[{"device":1,"channel":1,"from":[1,2]}]},{"channel":3}"#,
        )
        .unwrap();
        assert_eq!(c.device_count(false), 3);
        assert_eq!(c.in_widths(), vec![1, 2, 8], "uncoupled, empty, surround");
        assert_eq!(c.out_widths(), vec![2]);
        assert_eq!(c.rings().len(), 4);
        assert!(
            ok(r#"{"channel":1,"taps":[{"device":4,"channel":1,"from":[1]}]}"#).is_err(),
            "3 devices"
        );
        assert!(
            ok(r#"{"channel":1,"taps":[{"channel":1,"from":[1]}]}"#).is_err(),
            "device missing"
        );
        assert!(
            ok(r#"{"channel":1,"taps":[{"device":1,"channel":2,"from":[1]}]}"#).is_err(),
            "mono device"
        );
        assert!(
            ok(r#"{"channel":1,"taps":[{"device":2,"channel":1,"from":[1]}]},{"channel":2,"taps":[{"device":2,"channel":2,"from":[2]}]}"#).is_err(),
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
}
