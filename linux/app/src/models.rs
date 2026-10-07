//! Models decoded from daemon JSON responses (same as macos/app/Sources/Models.swift and
//! windows/app/Daemon/Models.cs). “-inf” peaks arrive as null.

use std::net::Ipv4Addr;

use lw_proto::channel::{Channel, GroupKind};
use serde_json::Value;

fn s(v: &Value, key: &str) -> String {
    v.get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

fn b(v: &Value, key: &str, fallback: bool) -> bool {
    v.get(key).and_then(Value::as_bool).unwrap_or(fallback)
}

fn u(v: &Value, key: &str) -> Option<u32> {
    v.get(key)
        .and_then(Value::as_u64)
        .and_then(|n| u32::try_from(n).ok())
}

fn channel(v: &Value) -> Option<u16> {
    u(v, "channel").and_then(|n| u16::try_from(n).ok())
}

fn ints(v: &Value, key: &str) -> Vec<u32> {
    v.get(key)
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(Value::as_u64)
                .filter_map(|n| u32::try_from(n).ok())
                .collect()
        })
        .unwrap_or_default()
}

fn objects<'a>(v: &'a Value, key: &str) -> impl Iterator<Item = &'a Value> {
    v.get(key)
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|o| o.is_object())
}

fn peaks(v: &Value, key: &str) -> Vec<Option<f64>> {
    v.get(key)
        .and_then(Value::as_array)
        .map(|a| a.iter().map(Value::as_f64).collect())
        .unwrap_or_default()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Iface {
    pub name: String,
    pub friendly: String,
    pub ipv4: String,
    pub candidate: bool,
    pub livewire: bool,
}

impl Iface {
    pub fn from(v: &Value) -> Option<Self> {
        let (name, ipv4) = (s(v, "name"), s(v, "ipv4"));
        if name.is_empty() || ipv4.is_empty() {
            return None;
        }
        let loopback = b(v, "loopback", false);
        let friendly = Some(s(v, "friendly"))
            .filter(|f| !f.is_empty())
            .unwrap_or_else(|| name.clone());
        Some(Self {
            candidate: b(v, "candidate", !loopback),
            livewire: b(v, "livewire", false),
            name,
            friendly,
            ipv4,
        })
    }

    /// Menu label: friendly name, address.
    pub fn title(&self) -> String {
        let base = if self.friendly == self.name {
            format!("{} · {}", self.name, self.ipv4)
        } else {
            format!("{} ({}) · {}", self.friendly, self.name, self.ipv4)
        };
        if self.livewire {
            base + " · " + crate::i18n::tr("Livewire network")
        } else {
            base
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveredSource {
    pub channel: u16,
    pub name: String,
    /// Advertised multicast group (empty: derived from the channel).
    pub stream: String,
    pub kind: String,
    pub terminal: String,
}

impl DiscoveredSource {
    pub fn from(v: &Value) -> Option<Self> {
        Some(Self {
            channel: channel(v)?,
            name: s(v, "name"),
            stream: s(v, "stream"),
            kind: Some(s(v, "kind"))
                .filter(|k| !k.is_empty())
                .unwrap_or_else(|| "stereo".into()),
            terminal: s(v, "terminal"),
        })
    }

    pub fn manual(channel: u16, kind: &str) -> Self {
        Self {
            channel,
            name: String::new(),
            stream: String::new(),
            kind: kind.into(),
            terminal: String::new(),
        }
    }

    /// Daemon `kind` value for the input patch (advertised stereo variants yield “stereo”).
    pub fn patch_kind(&self) -> &str {
        patch_kind(&self.kind)
    }

    /// Group to join for the preview.
    pub fn group(&self) -> Option<Ipv4Addr> {
        if self.stream.is_empty() {
            group(self.channel, self.patch_kind())
        } else {
            self.stream.parse().ok()
        }
    }
}

pub fn patch_kind(kind: &str) -> &str {
    match kind {
        "stereo" | "backfeed" | "surround" => kind,
        _ => "stereo",
    }
}

/// Multicast group of a Livewire channel (docs/protocol/01-channels.md).
pub fn group(channel: u16, kind: &str) -> Option<Ipv4Addr> {
    let k = match kind {
        "backfeed" => GroupKind::Backfeed,
        "surround" => GroupKind::Surround,
        _ => GroupKind::Stereo,
    };
    Channel::new(channel).map(|c| c.group(k))
}

/// Configured received stream (destination).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InputPatch {
    pub channel: Option<u16>,
    pub kind: String,
    /// Duplex layout: channels of the input device; multi layout: channels within `device`.
    pub device_channels: Vec<u32>,
    /// Multi layout: input device number (“OpenLW In n”, 1-based).
    pub device: Option<u32>,
    /// Mono patch: "left", "right" or "sum".
    pub mix: Option<String>,
}

impl InputPatch {
    /// Device width this patch needs (multi layout).
    pub fn width(&self) -> u32 {
        if self.mix.is_some() {
            1
        } else if self.kind == "surround" {
            8
        } else {
            2
        }
    }
}

/// Configured transmitted stream (source).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutputPatch {
    pub channel: u16,
    pub name: String,
    pub format: String,
    pub device_channels: Option<Vec<u32>>,
    /// Multi layout: output device number (“OpenLW Out n”, 1-based).
    pub device: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DaemonConfig {
    pub iface: String,
    pub advertise: bool,
    pub inputs: Vec<InputPatch>,
    pub outputs: Vec<OutputPatch>,
    pub channels_to_net: u32,
    pub channels_from_net: u32,
    pub terminal_name: String,
    pub latency: String,
    pub tos: u32,
    /// "duplex" (“OpenLW In” and “OpenLW Out”) or "multi" (“OpenLW In n” / “OpenLW Out n”).
    pub layout: String,
    /// Multi layout: devices named after their source (default), otherwise “OpenLW In n”.
    pub custom_names: bool,
}

impl Default for DaemonConfig {
    fn default() -> Self {
        Self {
            iface: String::new(),
            advertise: true,
            inputs: Vec::new(),
            outputs: Vec::new(),
            channels_to_net: 2,
            channels_from_net: 2,
            terminal_name: String::new(),
            latency: "normal".into(),
            tos: 184,
            layout: "duplex".into(),
            custom_names: true,
        }
    }
}

impl DaemonConfig {
    /// Automatically selected interface.
    pub fn auto_iface(&self) -> bool {
        self.iface.is_empty() || self.iface == "auto"
    }

    pub fn multi(&self) -> bool {
        self.layout == "multi"
    }

    /// Multi layout: device count per direction (one per configured pair).
    pub fn in_devices(&self) -> u32 {
        self.channels_from_net.div_ceil(2)
    }

    pub fn out_devices(&self) -> u32 {
        self.channels_to_net.div_ceil(2)
    }

    /// Multi layout: width of each input device after `inputs` (2 when empty), as computed by
    /// the daemon (daemon/lw-daemon/src/config.rs, `in_widths`).
    pub fn in_widths(inputs: &[InputPatch], devices: u32) -> Vec<u32> {
        (1..=devices.max(1))
            .map(|n| {
                inputs
                    .iter()
                    .find(|i| i.device == Some(n) && !i.device_channels.is_empty())
                    .map_or(2, InputPatch::width)
            })
            .collect()
    }

    pub fn from(c: &Value) -> Self {
        let d = Self::default();
        let dev = c.get("device").cloned().unwrap_or(Value::Null);
        Self {
            iface: s(c, "iface"),
            advertise: b(c, "advertise", true),
            terminal_name: s(c, "terminal_name"),
            latency: Some(s(c, "latency"))
                .filter(|l| !l.is_empty())
                .unwrap_or(d.latency),
            tos: u(c, "tos").unwrap_or(d.tos),
            layout: match s(c, "device_layout").as_str() {
                "multi" => "multi".into(),
                _ => d.layout,
            },
            custom_names: b(c, "custom_device_names", true),
            channels_to_net: u(&dev, "channels_to_net").unwrap_or(2),
            channels_from_net: u(&dev, "channels_from_net").unwrap_or(2),
            inputs: objects(c, "destinations")
                .map(|v| InputPatch {
                    channel: channel(v),
                    kind: Some(s(v, "kind"))
                        .filter(|k| !k.is_empty())
                        .unwrap_or_else(|| "stereo".into()),
                    device_channels: ints(v, "device_channels"),
                    device: u(v, "device"),
                    mix: Some(s(v, "mix")).filter(|m| !m.is_empty()),
                })
                .collect(),
            outputs: objects(c, "sources")
                .filter_map(|v| {
                    Some(OutputPatch {
                        channel: channel(v)?,
                        name: s(v, "name"),
                        format: Some(s(v, "format"))
                            .filter(|f| !f.is_empty())
                            .unwrap_or_else(|| "standard".into()),
                        device_channels: v
                            .get("device_channels")
                            .and_then(Value::as_array)
                            .map(|_| ints(v, "device_channels")),
                        device: u(v, "device"),
                    })
                })
                .collect(),
        }
    }
}

/// Device meters and input route state, from `status.device`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct DeviceMeters {
    /// Peaks per channel, devices concatenated in order (one device in duplex layout).
    pub to_net: Vec<Option<f64>>,
    pub from_net: Vec<Option<f64>>,
    /// Channels of each output / input device of the running region.
    pub out_widths: Vec<u32>,
    pub in_widths: Vec<u32>,
    /// Received routes (1-based channels, devices concatenated) → jitter buffer primed.
    pub inputs: Vec<(Vec<u32>, bool)>,
}

impl DeviceMeters {
    pub fn from(status: &Value) -> Self {
        let Some(dev) = status.get("device").filter(|d| d.is_object()) else {
            return Self::default();
        };
        Self {
            to_net: peaks(dev, "to_net_peak_dbfs"),
            from_net: peaks(dev, "from_net_peak_dbfs"),
            out_widths: ints(dev, "out_widths"),
            in_widths: ints(dev, "in_widths"),
            inputs: objects(dev, "inputs")
                .map(|r| (ints(r, "device_channels"), b(r, "primed", false)))
                .collect(),
        }
    }
}

/// Concatenated 1-based channels of device `n` (1-based) given device widths.
pub fn device_channels(n: u32, widths: &[u32]) -> Vec<u32> {
    let Some(i) = (n as usize).checked_sub(1) else {
        return Vec::new();
    };
    let Some(&w) = widths.get(i) else {
        return Vec::new();
    };
    let start: u32 = widths.iter().take(i).sum();
    (start + 1..=start + w).collect()
}

/// Peak (dBFS) of a 1-based channel, or `None` for silence.
pub fn peak(values: &[Option<f64>], channel: u32) -> Option<f64> {
    let i = usize::try_from(channel).ok()?.checked_sub(1)?;
    values.get(i).copied().flatten()
}

/// Network connection: session interface, or searching.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkStatus {
    pub auto: bool,
    pub searching: bool,
    pub iface: String,
    pub friendly: String,
    pub ipv4: String,
}

impl Default for LinkStatus {
    fn default() -> Self {
        Self {
            auto: true,
            searching: true,
            iface: String::new(),
            friendly: String::new(),
            ipv4: String::new(),
        }
    }
}

impl LinkStatus {
    pub fn from(v: &Value) -> Self {
        let iface = s(v, "iface");
        let friendly = Some(s(v, "iface_friendly"))
            .filter(|f| !f.is_empty())
            .unwrap_or_else(|| iface.clone());
        Self {
            auto: b(v, "iface_auto", false),
            searching: b(v, "searching", iface.is_empty()),
            iface,
            friendly,
            ipv4: s(v, "ipv4"),
        }
    }
}

#[cfg(test)]
#[allow(clippy::indexing_slicing)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn config_and_status() {
        let c = DaemonConfig::from(&json!({
            "iface": "auto", "advertise": false, "tos": 136, "latency": "low",
            "device": {"channels_to_net": 4, "channels_from_net": 6},
            "destinations": [{"channel": 101, "kind": "surround", "device_channels": [1,2,3,4,5,6,7,8]},
                             {"group": "239.192.0.9", "device_channels": [3,4]}],
            "sources": [{"channel": 2001, "name": "PC", "device_channels": [1,2]},
                        {"channel": 2002}, {"name": "no channel"}],
        }));
        assert!(c.auto_iface() && !c.advertise);
        assert_eq!((c.channels_to_net, c.channels_from_net, c.tos), (4, 6, 136));
        assert_eq!(c.inputs.len(), 2);
        assert_eq!(c.inputs[1].channel, None);
        assert_eq!(c.outputs.len(), 2);
        assert_eq!(c.outputs[0].device_channels, Some(vec![1, 2]));
        assert_eq!(c.outputs[1].device_channels, None);
        assert_eq!(c.outputs[1].format, "standard");
        assert!(!c.multi() && c.custom_names);

        let m = DaemonConfig::from(&json!({
            "device_layout": "multi", "custom_device_names": false,
            "device": {"channels_to_net": 4, "channels_from_net": 6},
            "destinations": [
                {"channel": 1, "device": 1, "device_channels": [1], "mix": "sum"},
                {"channel": 2, "kind": "surround", "device": 3, "device_channels": [1,2,3,4,5,6,7,8]}],
            "sources": [{"channel": 2001, "device": 2, "device_channels": [1,2]}],
        }));
        assert!(m.multi() && !m.custom_names);
        assert_eq!((m.in_devices(), m.out_devices()), (3, 2));
        assert_eq!(m.inputs[0].mix.as_deref(), Some("sum"));
        assert_eq!(m.outputs[0].device, Some(2));
        assert_eq!(DaemonConfig::in_widths(&m.inputs, 3), vec![1, 2, 8]);
        assert_eq!(device_channels(3, &[1, 2, 8]), (4..=11).collect::<Vec<_>>());
        assert_eq!(device_channels(1, &[1, 2, 8]), vec![1]);
        assert!(device_channels(0, &[2]).is_empty() && device_channels(2, &[2]).is_empty());

        let st = json!({"iface": "eth0", "ipv4": "10.0.0.2", "iface_auto": true, "searching": false,
            "device": {"from_net_peak_dbfs": [-12.0, null], "to_net_peak_dbfs": [], "in_widths": [2],
                       "inputs": [{"device_channels": [1,2], "primed": true}]}});
        let m = DeviceMeters::from(&st);
        assert_eq!(peak(&m.from_net, 1), Some(-12.0));
        assert_eq!(peak(&m.from_net, 2), None);
        assert_eq!(peak(&m.from_net, 0), None);
        assert_eq!(m.inputs, vec![(vec![1, 2], true)]);
        assert_eq!((m.in_widths, m.out_widths), (vec![2], vec![]));
        let l = LinkStatus::from(&st);
        assert!(l.auto && !l.searching && l.friendly == "eth0");
    }

    #[test]
    fn groups() {
        assert_eq!(group(1, "stereo"), Some(Ipv4Addr::new(239, 192, 0, 1)));
        assert_eq!(group(300, "backfeed"), Some(Ipv4Addr::new(239, 193, 1, 44)));
        assert_eq!(group(5, "surround"), Some(Ipv4Addr::new(239, 196, 0, 5)));
        assert_eq!(group(0, "stereo"), None);
        let s = DiscoveredSource::from(
            &json!({"channel": 7, "kind": "stereo-l16", "stream": "239.192.0.70"}),
        )
        .unwrap();
        assert_eq!(s.patch_kind(), "stereo");
        assert_eq!(s.group(), Some(Ipv4Addr::new(239, 192, 0, 70)));
    }
}
