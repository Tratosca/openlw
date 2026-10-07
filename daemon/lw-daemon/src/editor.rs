//! Configuration editing (live patching): pure validated functions without side effects.
//!
//! - Patch an input: first release target device inputs (the occupying stream
//!   remains received for statistics, unpatched), then update or add the stream.
//! - Unpatch an input: stop receiving the stream that fed these inputs.
//! - Remove an input: stop receiving a stream, patched or not (a stream displaced by another
//!   patch stays received, unpatched, until removed).
//! - Patch an output: update or add the source transmitted on this channel.
//! - Change device channel counts: remove patches targeting vanished channels
//!   (stop transmitted streams, remove unpatched received streams).

use std::net::Ipv4Addr;

use crate::config::{Config, ConfigError, DestinationConfig, Format, Kind, SourceConfig};

/// Requested change.
#[derive(Debug, Clone, PartialEq)]
pub enum Edit {
    /// Livewire stream (channel) or AES67 stream (group) to device inputs.
    PatchInput {
        channel: Option<u16>,
        group: Option<Ipv4Addr>,
        port: u16,
        kind: Kind,
        device_channels: Vec<u16>,
    },
    /// Release device inputs.
    UnpatchInput {
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
        device_channels: Vec<u16>,
    },
    /// Stop transmitting a channel.
    UnpatchOutput {
        channel: u16,
    },
    SetIface(String),
    SetAdvertise(bool),
    /// Device name based on received sources.
    SetDeviceNaming(bool),
    /// macOS layout: one device or two.
    SetDeviceLayout(crate::config::Layout),
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
            device_channels,
        } => {
            if channel.is_some() == group.is_some() {
                return Err(ConfigError("specify either a channel or a group".into()));
            }
            for d in &mut c.destinations {
                if d.device_channels
                    .as_ref()
                    .is_some_and(|chs| chs.iter().any(|x| device_channels.contains(x)))
                {
                    d.device_channels = None;
                }
            }
            let same = |d: &DestinationConfig| match (channel, group) {
                (Some(ch), _) => d.channel == Some(*ch) && d.kind == *kind,
                (None, Some(g)) => d.group == Some(*g) && d.port == *port,
                _ => false,
            };
            match c.destinations.iter_mut().find(|d| same(d)) {
                Some(d) => d.device_channels = Some(device_channels.clone()),
                None => c.destinations.push(DestinationConfig {
                    channel: *channel,
                    kind: *kind,
                    group: *group,
                    port: *port,
                    device_channels: Some(device_channels.clone()),
                }),
            }
        }
        Edit::UnpatchInput { device_channels } => {
            let before = c.destinations.len();
            c.destinations.retain(|d| {
                !d.device_channels
                    .as_ref()
                    .is_some_and(|chs| chs.iter().any(|x| device_channels.contains(x)))
            });
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
            device_channels,
        } => match c.sources.iter_mut().find(|s| s.channel == *channel) {
            Some(s) => {
                s.name = name.clone();
                s.format = *format;
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
        Edit::SetDeviceNaming(on) => c.name_device_from_sources = *on,
        Edit::SetDeviceLayout(l) => c.device_layout = *l,
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
            let fits = |chs: &Option<Vec<u16>>, max: u32| {
                chs.as_ref()
                    .is_none_or(|v| v.iter().all(|&x| u32::from(x) <= max))
            };
            c.sources.retain(|s| fits(&s.device_channels, out_max));
            c.destinations.retain(|d| fits(&d.device_channels, in_max));
        }
    }
    c.validate()?;
    Ok(c)
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
                device_channels: vec![3],
            },
        )
        .unwrap();
        assert!(c.destinations.iter().all(|d| d.channel != Some(7)));
        assert!(apply(
            &c,
            &Edit::UnpatchInput {
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
    fn patch_output_and_validation() {
        let out = |ch, dev: Vec<u16>| Edit::PatchOutput {
            channel: ch,
            name: "MAC 1".into(),
            format: Format::Standard,
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
