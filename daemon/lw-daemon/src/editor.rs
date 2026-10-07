//! Configuration editing (live patching): pure validated functions without side effects.
//!
//! - Patch an input: first release target device inputs (the occupying stream
//!   remains received for statistics, unpatched), then update or add the stream.
//! - Unpatch an input: stop receiving the stream that fed these inputs.
//! - Remove an input: stop receiving a stream, patched or not (a stream displaced by another
//!   patch stays received, unpatched, until removed).
//! - Patch an output: update or add the source transmitted on this channel.
//! - Change device channel counts: remove patches targeting vanished channels or devices
//!   (stop transmitted streams, remove unpatched received streams).
//! - Change layout: convert patches, pair k ↔ device k (a mono patch on channel c goes to
//!   device ⌈c/2⌉). A patch that no longer fits (collision, out of range) is dropped: the input
//!   stream stays received unpatched, the output stream stops.

use std::collections::BTreeSet;
use std::net::Ipv4Addr;

use crate::config::{
    slots, Config, ConfigError, DestinationConfig, Format, Kind, Layout, Mix, SourceConfig,
};

/// Requested change.
#[derive(Debug, Clone, PartialEq)]
pub enum Edit {
    /// Livewire stream (channel) or AES67 stream (group) to device inputs (`device`: `multi`
    /// layout; `mix`: mono patch).
    PatchInput {
        channel: Option<u16>,
        group: Option<Ipv4Addr>,
        port: u16,
        kind: Kind,
        device: Option<u16>,
        mix: Option<Mix>,
        device_channels: Vec<u16>,
    },
    /// Release device inputs.
    UnpatchInput {
        device: Option<u16>,
        device_channels: Vec<u16>,
    },
    /// Stop receiving a stream, patched or not: Livewire channel or AES67 group.
    RemoveInput {
        channel: Option<u16>,
        group: Option<Ipv4Addr>,
        port: u16,
        kind: Kind,
    },
    /// Device outputs to a Livewire channel.
    PatchOutput {
        channel: u16,
        name: String,
        format: Format,
        device: Option<u16>,
        device_channels: Vec<u16>,
    },
    /// Stop transmitting a channel.
    UnpatchOutput {
        channel: u16,
    },
    SetIface(String),
    SetAdvertise(bool),
    /// `multi` layout: devices named after their source.
    SetDeviceNaming(bool),
    /// macOS layout: one device or numbered devices.
    SetDeviceLayout(Layout),
    /// Advanced settings (only supplied fields change).
    SetAdvanced {
        terminal_name: Option<String>,
        latency: Option<crate::config::Latency>,
        dscp: Option<u8>,
    },
    /// Device channels in each direction.
    SetDeviceChannels {
        to_net: u32,
        from_net: u32,
    },
}

/// Apply `edit` to a copy of `cfg` and validate the result.
pub fn apply(cfg: &Config, edit: &Edit) -> Result<Config, ConfigError> {
    let mut c = cfg.clone();
    match edit {
        Edit::PatchInput {
            channel,
            group,
            port,
            kind,
            device,
            mix,
            device_channels,
        } => {
            if channel.is_some() == group.is_some() {
                return Err(ConfigError("specify either a channel or a group".into()));
            }
            let target = slots(*device, device_channels);
            for d in &mut c.destinations {
                if overlaps(d.device, d.device_channels.as_deref(), &target) {
                    unpatch(d);
                }
            }
            let same = |d: &DestinationConfig| match (channel, group) {
                (Some(ch), _) => d.channel == Some(*ch) && d.kind == *kind,
                (None, Some(g)) => d.group == Some(*g) && d.port == *port,
                _ => false,
            };
            match c.destinations.iter_mut().find(|d| same(d)) {
                Some(d) => {
                    d.device_channels = Some(device_channels.clone());
                    d.device = *device;
                    d.mix = *mix;
                }
                None => c.destinations.push(DestinationConfig {
                    channel: *channel,
                    kind: *kind,
                    group: *group,
                    port: *port,
                    device_channels: Some(device_channels.clone()),
                    device: *device,
                    mix: *mix,
                }),
            }
        }
        Edit::UnpatchInput {
            device,
            device_channels,
        } => {
            let before = c.destinations.len();
            let target = slots(*device, device_channels);
            c.destinations
                .retain(|d| !overlaps(d.device, d.device_channels.as_deref(), &target));
            if c.destinations.len() == before {
                return Err(ConfigError(format!(
                    "no stream patched to inputs {device_channels:?}"
                )));
            }
        }
        Edit::RemoveInput {
            channel,
            group,
            port,
            kind,
        } => {
            if channel.is_some() == group.is_some() {
                return Err(ConfigError("specify either a channel or a group".into()));
            }
            let before = c.destinations.len();
            c.destinations.retain(|d| match (channel, group) {
                (Some(ch), _) => !(d.channel == Some(*ch) && d.kind == *kind),
                (None, Some(g)) => !(d.group == Some(*g) && d.port == *port),
                _ => true,
            });
            if c.destinations.len() == before {
                return Err(ConfigError("this stream is not being received".into()));
            }
        }
        Edit::PatchOutput {
            channel,
            name,
            format,
            device,
            device_channels,
        } => match c.sources.iter_mut().find(|s| s.channel == *channel) {
            Some(s) => {
                s.name = name.clone();
                s.format = *format;
                s.device = *device;
                s.device_channels = Some(device_channels.clone());
            }
            None => c.sources.push(SourceConfig {
                channel: *channel,
                name: name.clone(),
                format: *format,
                payload_type: 96,
                tone_hz: 997.0,
                level_dbfs: -20.0,
                device_channels: Some(device_channels.clone()),
                device: *device,
            }),
        },
        Edit::UnpatchOutput { channel } => {
            let before = c.sources.len();
            c.sources.retain(|s| s.channel != *channel);
            if c.sources.len() == before {
                return Err(ConfigError(format!(
                    "no source transmitted on channel {channel}"
                )));
            }
        }
        Edit::SetIface(name) => c.iface = name.clone(),
        Edit::SetAdvertise(on) => c.advertise = *on,
        Edit::SetDeviceNaming(on) => c.custom_device_names = *on,
        Edit::SetDeviceLayout(l) => convert_layout(&mut c, *l),
        Edit::SetAdvanced {
            terminal_name,
            latency,
            dscp,
        } => {
            if let Some(n) = terminal_name {
                c.terminal_name = n.trim().to_string();
            }
            if let Some(l) = latency {
                c.latency = *l;
            }
            if let Some(d) = dscp {
                if *d > 63 {
                    return Err(ConfigError(format!("DSCP {d} outside 0..63")));
                }
                c.tos = u32::from(*d) << 2;
            }
        }
        Edit::SetDeviceChannels { to_net, from_net } => {
            let mut dev = c.device_config();
            dev.channels_to_net = *to_net;
            dev.channels_from_net = *from_net;
            c.device = Some(dev);
            let (out_max, in_max) = (*to_net, *from_net);
            let (out_dev, in_dev) = (c.device_count(true), c.device_count(false));
            let multi = c.multi();
            let fits = |dev: Option<u16>, chs: &Option<Vec<u16>>, max: u32, devices: u16| {
                if multi {
                    dev.is_none_or(|n| n <= devices)
                } else {
                    chs.as_ref()
                        .is_none_or(|v| v.iter().all(|&x| u32::from(x) <= max))
                }
            };
            c.sources
                .retain(|s| fits(s.device, &s.device_channels, out_max, out_dev));
            c.destinations
                .retain(|d| fits(d.device, &d.device_channels, in_max, in_dev));
        }
    }
    c.validate()?;
    Ok(c)
}

/// Does a patch (`device`, `chs`) occupy one of the `target` slots?
fn overlaps(device: Option<u16>, chs: Option<&[u16]>, target: &[(u16, u16)]) -> bool {
    chs.is_some_and(|chs| slots(device, chs).iter().any(|s| target.contains(s)))
}

fn unpatch(d: &mut DestinationConfig) {
    d.device_channels = None;
    d.device = None;
    d.mix = None;
}

/// Patch position in the other layout: duplex channels `chs` → (device ⌈first/2⌉,
/// channels 1..w), or `multi` device n → channels from 2n − 1. None if out of range.
fn convert(
    to_multi: bool,
    device: Option<u16>,
    chs: &[u16],
    devices: u16,
    max: u32,
) -> Option<(Option<u16>, Vec<u16>)> {
    let w = u16::try_from(chs.len()).ok()?;
    if to_multi {
        let n = chs.iter().min()?.div_ceil(2);
        (n >= 1 && n <= devices).then(|| (Some(n), (1..=w).collect()))
    } else {
        let first = 2 * device?.checked_sub(1)? + 1;
        let last = first + w - 1;
        (u32::from(last) <= max).then(|| (None, (first..=last).collect()))
    }
}

/// Switch layout, converting patches (see module documentation).
fn convert_layout(c: &mut Config, to: Layout) {
    if c.device_layout == to {
        return;
    }
    c.device_layout = to;
    let to_multi = to == Layout::Multi;
    let dev = c.device_config();
    let (in_dev, out_dev) = (c.device_count(false), c.device_count(true));
    // Deterministic order: by device, then first channel.
    let key = |device: Option<u16>, chs: &Option<Vec<u16>>| {
        (
            device.unwrap_or(0),
            chs.as_ref().and_then(|v| v.iter().min().copied()),
        )
    };
    let mut order: Vec<usize> = (0..c.destinations.len()).collect();
    order.sort_by_key(|&i| {
        c.destinations
            .get(i)
            .map(|d| key(d.device, &d.device_channels))
    });
    let mut used = BTreeSet::new();
    for i in order {
        let Some(d) = c.destinations.get_mut(i) else {
            continue;
        };
        let Some(chs) = d.device_channels.take() else {
            continue;
        };
        let target = convert(to_multi, d.device, &chs, in_dev, dev.channels_from_net)
            .filter(|(dv, nc)| slots(*dv, nc).iter().all(|s| !used.contains(s)));
        match target {
            Some((dv, nc)) => {
                used.extend(slots(dv, &nc));
                d.device = dv;
                d.device_channels = Some(nc);
            }
            None => unpatch(d),
        }
    }
    let mut order: Vec<usize> = (0..c.sources.len()).collect();
    order.sort_by_key(|&i| c.sources.get(i).map(|s| key(s.device, &s.device_channels)));
    let mut used = BTreeSet::new();
    let mut dropped = BTreeSet::new();
    for i in order {
        let Some(s) = c.sources.get_mut(i) else {
            continue;
        };
        let Some(chs) = s.device_channels.take() else {
            continue;
        };
        // Duplex outputs may feed several streams; a multi-layout device carries one.
        let target = convert(to_multi, s.device, &chs, out_dev, dev.channels_to_net)
            .filter(|(dv, _)| !to_multi || !used.contains(dv));
        match target {
            Some((dv, nc)) => {
                used.insert(dv);
                s.device = dv;
                s.device_channels = Some(nc);
            }
            None => {
                dropped.insert(s.channel);
            }
        }
    }
    c.sources.retain(|s| !dropped.contains(&s.channel));
}

#[cfg(test)]
#[allow(clippy::indexing_slicing)]
mod tests {
    use super::*;

    fn base() -> Config {
        serde_json::from_str(
            r#"{"iface":"lo0","device":{"channels_to_net":8,"channels_from_net":8}}"#,
        )
        .unwrap()
    }

    fn patch_in(ch: u16, dev: [u16; 2]) -> Edit {
        Edit::PatchInput {
            channel: Some(ch),
            group: None,
            port: 5004,
            kind: Kind::Stereo,
            device: None,
            mix: None,
            device_channels: dev.to_vec(),
        }
    }

    #[test]
    fn patch_input_moves_and_replaces() {
        let c = apply(&base(), &patch_in(1, [1, 2])).unwrap();
        assert_eq!(c.destinations.len(), 1);
        // Another channel on the same inputs: the old stream loses its patch.
        let c = apply(&c, &patch_in(7, [1, 2])).unwrap();
        assert_eq!(c.destinations.len(), 2);
        assert_eq!(c.destinations[0].device_channels, None);
        assert_eq!(c.destinations[1].device_channels, Some(vec![1, 2]));
        // Repatch channel 7 elsewhere: update, no duplicate.
        let c = apply(&c, &patch_in(7, [3, 4])).unwrap();
        assert_eq!(
            c.destinations
                .iter()
                .filter(|d| d.channel == Some(7))
                .count(),
            1
        );
        assert_eq!(c.destinations[1].device_channels, Some(vec![3, 4]));
        // Unpatch.
        let c = apply(
            &c,
            &Edit::UnpatchInput {
                device: None,
                device_channels: vec![3],
            },
        )
        .unwrap();
        assert!(c.destinations.iter().all(|d| d.channel != Some(7)));
        assert!(apply(
            &c,
            &Edit::UnpatchInput {
                device: None,
                device_channels: vec![5]
            }
        )
        .is_err());
    }

    #[test]
    fn remove_input_drops_displaced_stream() {
        let c = apply(&base(), &patch_in(1, [1, 2])).unwrap();
        let c = apply(&c, &patch_in(7, [1, 2])).unwrap();
        let remove = |ch| Edit::RemoveInput {
            channel: Some(ch),
            group: None,
            port: 5004,
            kind: Kind::Stereo,
        };
        // Channel 1 lost its inputs but is still received: only remove_input drops it.
        let c = apply(&c, &remove(1)).unwrap();
        assert_eq!(c.destinations.len(), 1);
        assert_eq!(c.destinations[0].channel, Some(7));
        // Patched stream: removed too, its inputs are released.
        let c = apply(&c, &remove(7)).unwrap();
        assert!(c.destinations.is_empty());
        assert!(apply(&c, &remove(7)).is_err(), "stream absent");
        let backfeed = Edit::RemoveInput {
            channel: Some(7),
            group: None,
            port: 5004,
            kind: Kind::Backfeed,
        };
        let c = apply(&base(), &patch_in(7, [1, 2])).unwrap();
        assert!(
            apply(&c, &backfeed).is_err(),
            "different kind on the same channel"
        );
    }

    #[test]
    fn device_channels_prune_out_of_range_patches() {
        let c = apply(&base(), &patch_in(1, [7, 8])).unwrap();
        let c = apply(
            &c,
            &Edit::PatchOutput {
                channel: 4001,
                name: "MAC".into(),
                format: Format::Standard,
                device: None,
                device_channels: vec![5, 6],
            },
        )
        .unwrap();
        let c = apply(&c, &patch_in(2, [1, 2])).unwrap();
        let c = apply(
            &c,
            &Edit::SetDeviceChannels {
                to_net: 4,
                from_net: 4,
            },
        )
        .unwrap();
        assert!(
            c.sources.is_empty(),
            "outputs 5-6 gone: transmission stopped"
        );
        assert_eq!(c.destinations.len(), 1, "only the 1-2 patch remains");
        assert_eq!(c.device_config().channels_from_net, 4);
        assert!(apply(
            &c,
            &Edit::SetDeviceChannels {
                to_net: 34,
                from_net: 2
            }
        )
        .is_err());
    }

    #[test]
    fn advanced_settings() {
        let c = apply(
            &base(),
            &Edit::SetAdvanced {
                terminal_name: Some("  Studio Mac ".into()),
                latency: Some(crate::config::Latency::Safe),
                dscp: Some(34),
            },
        )
        .unwrap();
        assert_eq!(
            (c.terminal_name.as_str(), c.latency, c.tos),
            ("Studio Mac", crate::config::Latency::Safe, 136)
        );
        let keep = apply(
            &c,
            &Edit::SetAdvanced {
                terminal_name: None,
                latency: None,
                dscp: Some(46),
            },
        )
        .unwrap();
        assert_eq!((keep.terminal_name.as_str(), keep.tos), ("Studio Mac", 184));
        let long = Edit::SetAdvanced {
            terminal_name: Some("x".repeat(33)),
            latency: None,
            dscp: None,
        };
        assert!(apply(&c, &long).is_err(), "name longer than 32 characters");
        let bad = Edit::SetAdvanced {
            terminal_name: None,
            latency: None,
            dscp: Some(64),
        };
        assert!(apply(&c, &bad).is_err());
    }

    #[test]
    fn layout_conversion_both_ways() {
        let mut c = apply(&base(), &patch_in(1, [3, 4])).unwrap();
        c = apply(
            &c,
            &Edit::PatchInput {
                channel: Some(2),
                group: None,
                port: 5004,
                kind: Kind::Stereo,
                device: None,
                mix: Some(Mix::Sum),
                device_channels: vec![6],
            },
        )
        .unwrap();
        // Mono on 5 would collide with the mono on 6 (both → device 3).
        c = apply(
            &c,
            &Edit::PatchInput {
                channel: Some(5),
                group: None,
                port: 5004,
                kind: Kind::Stereo,
                device: None,
                mix: Some(Mix::Left),
                device_channels: vec![5],
            },
        )
        .unwrap();
        c = apply(
            &c,
            &Edit::PatchOutput {
                channel: 4001,
                name: "MAC".into(),
                format: Format::Standard,
                device: None,
                device_channels: vec![7, 8],
            },
        )
        .unwrap();
        let m = apply(&c, &Edit::SetDeviceLayout(Layout::Multi)).unwrap();
        let find = |cfg: &Config, ch: u16| {
            cfg.destinations
                .iter()
                .find(|d| d.channel == Some(ch))
                .map(|d| (d.device, d.device_channels.clone(), d.mix))
                .unwrap()
        };
        assert_eq!(
            find(&m, 1),
            (Some(2), Some(vec![1, 2]), None),
            "pair 3-4 → In 2"
        );
        assert_eq!(
            find(&m, 5),
            (Some(3), Some(vec![1]), Some(Mix::Left)),
            "channel 5 → In 3"
        );
        assert_eq!(
            find(&m, 2),
            (None, None, None),
            "channel 6 → In 3, taken: unpatched"
        );
        assert_eq!(m.sources[0].device, Some(4), "outputs 7-8 → Out 4");
        assert_eq!(m.sources[0].device_channels, Some(vec![1, 2]));
        // Back to duplex: device n → channels from 2n − 1.
        let d = apply(&m, &Edit::SetDeviceLayout(Layout::Duplex)).unwrap();
        assert_eq!(find(&d, 1), (None, Some(vec![3, 4]), None));
        assert_eq!(find(&d, 5), (None, Some(vec![5]), Some(Mix::Left)));
        assert_eq!(d.sources[0].device_channels, Some(vec![7, 8]));
        // Fewer devices: patches on removed devices go.
        let small = apply(
            &m,
            &Edit::SetDeviceChannels {
                to_net: 2,
                from_net: 4,
            },
        )
        .unwrap();
        assert!(small.sources.is_empty(), "Out 4 gone: transmission stopped");
        assert!(
            small.destinations.iter().all(|d| d.channel != Some(5)),
            "In 3 gone: stream removed"
        );
        assert_eq!(
            small.destinations.len(),
            2,
            "In 2 (channel 1) and unpatched channel 2 kept"
        );
        // Multi-layout unpatch by device.
        let u = apply(
            &m,
            &Edit::UnpatchInput {
                device: Some(3),
                device_channels: vec![1],
            },
        )
        .unwrap();
        assert!(u.destinations.iter().all(|d| d.channel != Some(5)));
    }

    #[test]
    fn patch_output_and_validation() {
        let out = |ch, dev: Vec<u16>| Edit::PatchOutput {
            channel: ch,
            name: "MAC 1".into(),
            format: Format::Standard,
            device: None,
            device_channels: dev,
        };
        let c = apply(&base(), &out(4001, vec![1, 2])).unwrap();
        let c = apply(&c, &out(4001, vec![3, 4])).unwrap();
        assert_eq!(c.sources.len(), 1);
        assert_eq!(c.sources[0].device_channels, Some(vec![3, 4]));
        assert!(
            apply(&c, &out(4002, vec![1])).is_err(),
            "stereo: 2 outputs required"
        );
        assert!(
            apply(&c, &out(4002, vec![8, 9])).is_err(),
            "output 9 does not exist"
        );
        assert!(apply(&c, &out(0, vec![1, 2])).is_err(), "channel 0 invalid");
        let c = apply(&c, &Edit::UnpatchOutput { channel: 4001 }).unwrap();
        assert!(c.sources.is_empty());
    }
}
