//! Configuration editing (live patching): pure validated functions without side effects.
//!
//! - Patch an input: set crosspoints (taps) of a stream on device inputs. An input holds one
//!   tap: the stream that occupied it loses that tap (it stays received for statistics, even
//!   without taps). `replace` (former command form) moves the stream: its other taps go.
//!   A tap with no stream channel releases the input.
//! - Unpatch inputs: release them; a stream left without taps stops being received.
//! - Remove an input: stop receiving a stream, patched or not.
//! - Couple or uncouple an input pair (multi layout: an input device). Uncoupling keeps the
//!   audio (a stereo patch becomes left and right, or L + R on a mono device); coupling makes
//!   the stream of the pair's first input a stereo patch and releases the rest.
//! - Patch an output: update or add the source transmitted on this channel.
//! - Change device channel counts: remove patches targeting vanished channels or devices
//!   (stop transmitted streams; a received stream left without taps stops).
//! - Change layout: convert patches, pair k ↔ device k (an input c goes to device ⌈c/2⌉). A
//!   patch that no longer fits (collision, out of range) is dropped: the input stream stays
//!   received, the output stream stops.

use std::collections::BTreeSet;
use std::net::Ipv4Addr;

use crate::config::{
    straight_taps, Config, ConfigError, DestinationConfig, Format, Kind, Layout, SourceConfig, Tap,
};

/// Requested change.
#[derive(Debug, Clone, PartialEq)]
pub enum Edit {
    /// Livewire stream (channel) or AES67 stream (group) to device inputs.
    PatchInput {
        channel: Option<u16>,
        group: Option<Ipv4Addr>,
        port: u16,
        kind: Kind,
        /// Crosspoints to set; a tap with an empty `from` releases its input.
        taps: Vec<Tap>,
        /// Remove the stream's other taps (move rather than add).
        replace: bool,
    },
    /// Release device inputs (`device`: multi layout).
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
    /// Couple input pair (multi layout: input device) `pair` in stereo, or uncouple it.
    SetCoupling {
        pair: u16,
        coupled: bool,
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
    /// Layout: one device or numbered devices (macOS, Linux).
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
            taps,
            replace,
        } => {
            if channel.is_some() == group.is_some() {
                return Err(ConfigError("specify either a channel or a group".into()));
            }
            let target: BTreeSet<(u16, u16)> = taps.iter().map(Tap::slot).collect();
            // Inputs taken: release them (multi layout: a device carries one stream, so the
            // whole device is released when another stream arrives).
            let devices: BTreeSet<u16> = taps.iter().filter_map(|t| t.device).collect();
            let this = |d: &DestinationConfig| d.same_stream(*channel, *group, *port, *kind);
            for d in &mut c.destinations {
                let other = !this(d);
                d.taps.retain(|t| {
                    !(target.contains(&t.slot())
                        || (other && t.device.is_some_and(|n| devices.contains(&n))))
                });
            }
            let idx = match c.destinations.iter().position(this) {
                Some(i) => i,
                None => {
                    c.destinations.push(DestinationConfig {
                        channel: *channel,
                        kind: *kind,
                        group: *group,
                        port: *port,
                        taps: Vec::new(),
                    });
                    c.destinations.len() - 1
                }
            };
            if let Some(d) = c.destinations.get_mut(idx) {
                if *replace {
                    d.taps.clear();
                }
                d.taps
                    .extend(taps.iter().filter(|t| !t.from.is_empty()).cloned());
                d.taps.sort_by_key(Tap::slot);
                if d.taps.is_empty() {
                    c.destinations.remove(idx);
                }
            }
        }
        Edit::UnpatchInput {
            device,
            device_channels,
        } => {
            let target: Vec<(u16, u16)> = device_channels
                .iter()
                .map(|&ch| (device.unwrap_or(0), ch))
                .collect();
            let mut released = false;
            c.destinations.retain_mut(|d| {
                let before = d.taps.len();
                d.taps.retain(|t| !target.contains(&t.slot()));
                released |= d.taps.len() != before;
                // A stream losing its last tap here stops; one already without taps stays.
                !(before > 0 && d.taps.is_empty())
            });
            if !released {
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
            c.destinations
                .retain(|d| !d.same_stream(*channel, *group, *port, *kind));
            if c.destinations.len() == before {
                return Err(ConfigError("this stream is not being received".into()));
            }
        }
        Edit::SetCoupling { pair, coupled } => set_coupling(&mut c, *pair, *coupled)?,
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
            c.sources.retain(|s| {
                if multi {
                    s.device.is_none_or(|n| n <= out_dev)
                } else {
                    s.device_channels
                        .as_ref()
                        .is_none_or(|v| v.iter().all(|&x| u32::from(x) <= out_max))
                }
            });
            c.destinations.retain_mut(|d| {
                let before = d.taps.len();
                d.taps.retain(|t| {
                    if multi {
                        t.device.is_none_or(|n| n <= in_dev)
                    } else {
                        u32::from(t.channel) <= in_max
                    }
                });
                !(before > 0 && d.taps.is_empty())
            });
            let pairs = u16::try_from(in_max.div_ceil(2)).unwrap_or(u16::MAX);
            c.uncoupled_inputs.retain(|&n| n <= pairs);
        }
    }
    c.validate()?;
    Ok(c)
}

/// Couple or uncouple input pair (duplex) or input device (multi) `n` (see module docs).
fn set_coupling(c: &mut Config, n: u16, coupled: bool) -> Result<(), ConfigError> {
    let pairs = c.device_count(false);
    if n == 0 || n > pairs {
        return Err(ConfigError(format!("input pair {n} outside 1..{pairs}")));
    }
    if c.coupled(n) == coupled {
        return Ok(());
    }
    if coupled {
        c.uncoupled_inputs.retain(|&x| x != n);
    } else {
        c.uncoupled_inputs.push(n);
        c.uncoupled_inputs.sort_unstable();
    }
    let multi = c.multi();
    // Inputs of the pair: duplex channels 2n − 1, 2n; multi: the device.
    let in_pair = |t: &Tap| {
        if multi {
            t.device == Some(n)
        } else {
            t.channel == 2 * n - 1 || t.channel == 2 * n
        }
    };
    // Stream that keeps the pair: the first input's, else the second's.
    let mut owner: Option<(usize, u16)> = None;
    for (i, d) in c.destinations.iter().enumerate() {
        for t in d.taps.iter().filter(|t| in_pair(t)) {
            if owner.is_none_or(|(_, ch)| t.channel < ch) {
                owner = Some((i, t.channel));
            }
        }
    }
    let Some((owner, _)) = owner else {
        return Ok(());
    };
    let surround = c
        .destinations
        .get(owner)
        .is_some_and(|d| d.kind == Kind::Surround);
    match (multi, coupled) {
        // Duplex, uncoupling: left and right stay on their inputs.
        (false, false) => {}
        // Duplex, coupling: stereo patch of the owner on the pair (a surround block is kept).
        (false, true) => {
            if !surround {
                for d in &mut c.destinations {
                    d.taps.retain(|t| !in_pair(t));
                }
                if let Some(d) = c.destinations.get_mut(owner) {
                    d.taps.extend(straight_taps(None, 2 * n - 1, 2));
                    d.taps.sort_by_key(Tap::slot);
                }
            }
        }
        // Multi: mono device (L + R of a stereo stream; a surround stream does not fit) or
        // stereo device.
        (true, _) => {
            if let Some(d) = c.destinations.get_mut(owner) {
                d.taps.retain(|t| !in_pair(t));
                if !surround {
                    if coupled {
                        d.taps.extend(straight_taps(Some(n), 1, 2));
                    } else {
                        d.taps.push(Tap {
                            device: Some(n),
                            channel: 1,
                            from: vec![1, 2],
                        });
                    }
                    d.taps.sort_by_key(Tap::slot);
                }
            }
        }
    }
    Ok(())
}

/// Tap position in the other layout (see module documentation), or None if out of range.
/// `base`: first duplex input of the stream's block (surround: 8 inputs from a pair start).
fn convert_tap(c: &Config, to_multi: bool, t: &Tap, surround_base: Option<u16>) -> Option<Tap> {
    if to_multi {
        let n = surround_base.unwrap_or(t.channel).div_ceil(2);
        let channel = match surround_base {
            Some(base) => t.channel.checked_sub(base)? + 1,
            None if c.coupled(n) => (t.channel - 1) % 2 + 1,
            None => 1,
        };
        (n >= 1 && n <= c.device_count(false)).then(|| Tap {
            device: Some(n),
            channel,
            from: t.from.clone(),
        })
    } else {
        let channel = 2 * t.device?.checked_sub(1)? + t.channel;
        (u32::from(channel) <= c.device_config().channels_from_net).then(|| Tap {
            device: None,
            channel,
            from: t.from.clone(),
        })
    }
}

/// Switch layout, converting patches (see module documentation).
fn convert_layout(c: &mut Config, to: Layout) {
    if c.device_layout == to {
        return;
    }
    let to_multi = to == Layout::Multi;
    let dev = c.device_config();
    let out_dev = u16::try_from(dev.channels_to_net.div_ceil(2)).unwrap_or(u16::MAX);
    // Inputs, in device-slot order: an input or a device taken once stays taken.
    let mut order: Vec<usize> = (0..c.destinations.len()).collect();
    order.sort_by_key(|&i| {
        c.destinations
            .get(i)
            .and_then(|d| d.taps.iter().map(Tap::slot).min())
    });
    let mut used = BTreeSet::new();
    let mut device_owner = std::collections::BTreeMap::new();
    let mut converted: Vec<(usize, Vec<Tap>)> = Vec::new();
    for i in order {
        let Some(d) = c.destinations.get(i) else {
            continue;
        };
        let base = (d.kind == Kind::Surround)
            .then(|| d.taps.iter().map(|t| t.channel).min())
            .flatten()
            .map(|b| b - (b + 1) % 2); // Pair start
        let mut taps = Vec::new();
        for t in &d.taps {
            let Some(nt) = convert_tap(c, to_multi, t, base) else {
                continue;
            };
            let owner_ok = nt
                .device
                .is_none_or(|n| *device_owner.entry(n).or_insert(i) == i);
            if owner_ok && used.insert(nt.slot()) {
                taps.push(nt);
            }
        }
        converted.push((i, taps));
    }
    for (i, taps) in converted {
        if let Some(d) = c.destinations.get_mut(i) {
            d.taps = taps;
        }
    }
    c.device_layout = to;
    // Outputs.
    let mut order: Vec<usize> = (0..c.sources.len()).collect();
    order.sort_by_key(|&i| {
        c.sources.get(i).map(|s| {
            (
                s.device.unwrap_or(0),
                s.device_channels
                    .as_ref()
                    .and_then(|v| v.iter().min().copied()),
            )
        })
    });
    let mut used = BTreeSet::new();
    let mut dropped = BTreeSet::new();
    for i in order {
        let Some(s) = c.sources.get_mut(i) else {
            continue;
        };
        let Some(chs) = s.device_channels.take() else {
            continue;
        };
        let w = u16::try_from(chs.len()).unwrap_or(0);
        // Duplex outputs may feed several streams; a multi-layout device carries one.
        let target = if to_multi {
            chs.iter()
                .min()
                .map(|m| m.div_ceil(2))
                .filter(|&n| n >= 1 && n <= out_dev && used.insert(n))
                .map(|n| (Some(n), (1..=w).collect::<Vec<u16>>()))
        } else {
            s.device
                .and_then(|n| n.checked_sub(1))
                .map(|k| 2 * k + 1)
                .filter(|&first| u32::from(first + w - 1) <= dev.channels_to_net)
                .map(|first| (None, (first..first + w).collect()))
        };
        match target {
            Some((dv, nc)) => {
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

    fn tap(channel: u16, from: &[u8]) -> Tap {
        Tap {
            device: None,
            channel,
            from: from.to_vec(),
        }
    }

    /// Former command form: stereo on `dev`, moving the stream.
    fn patch_in(ch: u16, dev: [u16; 2]) -> Edit {
        Edit::PatchInput {
            channel: Some(ch),
            group: None,
            port: 5004,
            kind: Kind::Stereo,
            taps: straight_taps(None, dev[0], 2),
            replace: true,
        }
    }

    /// Crosspoints added to stream `ch` (stereo).
    fn taps_in(ch: u16, taps: Vec<Tap>) -> Edit {
        Edit::PatchInput {
            channel: Some(ch),
            group: None,
            port: 5004,
            kind: Kind::Stereo,
            taps,
            replace: false,
        }
    }

    fn taps_of(c: &Config, ch: u16) -> Vec<(u16, Vec<u8>)> {
        c.destinations
            .iter()
            .find(|d| d.channel == Some(ch))
            .map(|d| d.taps.iter().map(|t| (t.channel, t.from.clone())).collect())
            .unwrap_or_default()
    }

    #[test]
    fn patch_input_moves_and_replaces() {
        let c = apply(&base(), &patch_in(1, [1, 2])).unwrap();
        assert_eq!(c.destinations.len(), 1);
        // Another channel on the same inputs: the old stream loses its taps, stays received.
        let c = apply(&c, &patch_in(7, [1, 2])).unwrap();
        assert_eq!(c.destinations.len(), 2);
        assert!(taps_of(&c, 1).is_empty());
        assert_eq!(taps_of(&c, 7), [(1, vec![1]), (2, vec![2])]);
        // Former form moves the stream.
        let c = apply(&c, &patch_in(7, [3, 4])).unwrap();
        assert_eq!(taps_of(&c, 7), [(3, vec![1]), (4, vec![2])]);
        // Unpatch: the stream loses its last tap and stops.
        let c = apply(
            &c,
            &Edit::UnpatchInput {
                device: None,
                device_channels: vec![3, 4],
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
    fn free_crosspoints() {
        // Left of channel 1 on inputs 1 and 2, stereo of channel 2 on 3-4… and on 5-6 too.
        let c = apply(&base(), &taps_in(1, vec![tap(1, &[1]), tap(2, &[1])])).unwrap();
        let c = apply(&c, &taps_in(2, vec![tap(3, &[1]), tap(4, &[2])])).unwrap();
        let c = apply(&c, &taps_in(2, vec![tap(5, &[1]), tap(6, &[2])])).unwrap();
        assert_eq!(taps_of(&c, 1), [(1, vec![1]), (2, vec![1])]);
        assert_eq!(taps_of(&c, 2).len(), 4, "added, not moved");
        // Stereo across a pair boundary (2-3) and L + R on one input.
        let c = apply(&c, &taps_in(3, vec![tap(2, &[1]), tap(3, &[2])])).unwrap();
        assert_eq!(taps_of(&c, 1), [(1, vec![1])], "input 2 taken over");
        assert_eq!(taps_of(&c, 3), [(2, vec![1]), (3, vec![2])]);
        let c = apply(&c, &taps_in(1, vec![tap(1, &[1, 2])])).unwrap();
        assert_eq!(taps_of(&c, 1), [(1, vec![1, 2])]);
        // Empty `from` releases; the stream's last tap gone, it stops.
        let c = apply(&c, &taps_in(1, vec![tap(1, &[])])).unwrap();
        assert!(c.destinations.iter().all(|d| d.channel != Some(1)));
        // Invalid: right of a surround is channel 2, L + R only for a stereo stream.
        let surround = Edit::PatchInput {
            channel: Some(9),
            group: None,
            port: 5004,
            kind: Kind::Surround,
            taps: vec![tap(8, &[1, 2])],
            replace: false,
        };
        assert!(apply(&c, &surround).is_err());
        assert!(
            apply(&c, &taps_in(1, vec![tap(1, &[3])])).is_err(),
            "stereo has 2 channels"
        );
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
        let c = apply(&c, &remove(1)).unwrap();
        assert_eq!(c.destinations.len(), 1);
        let c = apply(&c, &remove(7)).unwrap();
        assert!(c.destinations.is_empty());
        assert!(apply(&c, &remove(7)).is_err(), "stream absent");
    }

    #[test]
    fn coupling_in_duplex_layout() {
        let c = apply(&base(), &patch_in(1, [3, 4])).unwrap();
        let couple = |pair, coupled| Edit::SetCoupling { pair, coupled };
        // Uncoupling keeps left and right.
        let c = apply(&c, &couple(2, false)).unwrap();
        assert_eq!(c.uncoupled_inputs, [2]);
        assert_eq!(taps_of(&c, 1), [(3, vec![1]), (4, vec![2])]);
        // Left of channel 2 on input 3, right of channel 1 stays on 4.
        let c = apply(&c, &taps_in(2, vec![tap(3, &[1])])).unwrap();
        assert_eq!(taps_of(&c, 1), [(4, vec![2])]);
        // Coupling: the first input's stream (channel 2) becomes stereo on 3-4.
        let c = apply(&c, &couple(2, true)).unwrap();
        assert!(c.uncoupled_inputs.is_empty());
        assert_eq!(taps_of(&c, 2), [(3, vec![1]), (4, vec![2])]);
        assert!(taps_of(&c, 1).is_empty());
        assert!(apply(&c, &couple(5, false)).is_err(), "4 pairs");
    }

    #[test]
    fn coupling_in_multi_layout() {
        let mut c = apply(&base(), &patch_in(1, [3, 4])).unwrap();
        c = apply(&c, &Edit::SetDeviceLayout(Layout::Multi)).unwrap();
        let dev = |c: &Config| c.destinations[0].taps.clone();
        assert_eq!(dev(&c), straight_taps(Some(2), 1, 2), "pair 2 → In 2");
        // Uncoupled device: mono, L + R of the stereo stream.
        let c = apply(
            &c,
            &Edit::SetCoupling {
                pair: 2,
                coupled: false,
            },
        )
        .unwrap();
        assert_eq!(c.in_widths(), [2, 1, 2, 2]);
        assert_eq!(dev(&c)[0].from, [1, 2]);
        let c = apply(
            &c,
            &Edit::SetCoupling {
                pair: 2,
                coupled: true,
            },
        )
        .unwrap();
        assert_eq!(dev(&c), straight_taps(Some(2), 1, 2));
        assert_eq!(c.in_widths(), [2, 2, 2, 2]);
        // Another stream on a device takes the whole device.
        let other = Edit::PatchInput {
            channel: Some(5),
            group: None,
            port: 5004,
            kind: Kind::Stereo,
            taps: vec![Tap {
                device: Some(2),
                channel: 1,
                from: vec![1],
            }],
            replace: false,
        };
        let c = apply(&c, &other).unwrap();
        assert!(taps_of(&c, 1).is_empty());
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
            &Edit::SetCoupling {
                pair: 4,
                coupled: false,
            },
        )
        .unwrap();
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
        assert!(c.uncoupled_inputs.is_empty(), "pair 4 gone");
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
    fn layout_conversion_both_ways() {
        let mut c = apply(&base(), &patch_in(1, [3, 4])).unwrap();
        c = apply(
            &c,
            &Edit::SetCoupling {
                pair: 3,
                coupled: false,
            },
        )
        .unwrap();
        // Pair 3 uncoupled: left of 2 on input 5, L + R of 5 on input 6 (both → In 3, mono).
        c = apply(&c, &taps_in(2, vec![tap(5, &[1])])).unwrap();
        c = apply(&c, &taps_in(5, vec![tap(6, &[1, 2])])).unwrap();
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
                .map(|d| d.taps.clone())
                .unwrap()
        };
        assert_eq!(find(&m, 1), straight_taps(Some(2), 1, 2), "pair 2 → In 2");
        assert_eq!(
            find(&m, 2),
            [Tap {
                device: Some(3),
                channel: 1,
                from: vec![1]
            }],
            "input 5 → In 3 (mono)"
        );
        assert!(find(&m, 5).is_empty(), "In 3 taken: unpatched");
        assert_eq!(m.sources[0].device, Some(4), "outputs 7-8 → Out 4");
        let d = apply(&m, &Edit::SetDeviceLayout(Layout::Duplex)).unwrap();
        assert_eq!(find(&d, 1), straight_taps(None, 3, 2));
        assert_eq!(find(&d, 2), [tap(5, &[1])]);
        assert_eq!(d.sources[0].device_channels, Some(vec![7, 8]));
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
