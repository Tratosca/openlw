//! Édition de la configuration (patch à chaud) : fonctions pures, validées, sans effet de bord.
//!
//! - Patcher une entrée : les entrées du périphérique visées sont d'abord libérées (le flux qui les
//!   occupait reste reçu pour les statistiques, sans patch), puis le flux est mis à jour ou ajouté.
//! - Dépatcher une entrée : le flux qui alimentait ces entrées n'est plus reçu.
//! - Patcher une sortie : la source émise sur ce canal est mise à jour ou ajoutée.
//! - Changer le nombre de canaux du périphérique : les patchs qui visent des canaux disparus sont
//!   retirés (flux émis arrêtés, flux reçus non patchés retirés).

use std::net::Ipv4Addr;

use crate::config::{Config, ConfigError, DestinationConfig, Format, Kind, SourceConfig};

/// Modification demandée.
#[derive(Debug, Clone, PartialEq)]
pub enum Edit {
    /// Flux Livewire (canal) ou AES67 (groupe) vers des entrées du périphérique.
    PatchInput {
        channel: Option<u16>,
        group: Option<Ipv4Addr>,
        port: u16,
        kind: Kind,
        device_channels: Vec<u16>,
    },
    /// Libère des entrées du périphérique.
    UnpatchInput {
        device_channels: Vec<u16>,
    },
    /// Sorties du périphérique vers un canal Livewire.
    PatchOutput {
        channel: u16,
        name: String,
        format: Format,
        device_channels: Vec<u16>,
    },
    /// Arrête l'émission d'un canal.
    UnpatchOutput {
        channel: u16,
    },
    SetIface(String),
    SetAdvertise(bool),
    /// Nom du périphérique d'après les sources reçues.
    SetDeviceNaming(bool),
    /// Présentation dans macOS : un périphérique ou deux.
    SetDeviceLayout(crate::config::Layout),
    /// Réglages avancés (seuls les champs présents changent).
    SetAdvanced {
        terminal_name: Option<String>,
        latency: Option<crate::config::Latency>,
        dscp: Option<u8>,
    },
    /// Canaux du périphérique dans chaque sens.
    SetDeviceChannels {
        to_net: u32,
        from_net: u32,
    },
}

/// Applique `edit` à une copie de `cfg` et valide le résultat.
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
                return Err(ConfigError("indiquer soit un canal, soit un groupe".into()));
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
                    "aucun flux patché sur les entrées {device_channels:?}"
                )));
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
                    "aucune source émise sur le canal {channel}"
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
                    return Err(ConfigError(format!("DSCP {d} hors 0..63")));
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
        // Un autre canal sur les mêmes entrées : l'ancien flux perd son patch.
        let c = apply(&c, &patch_in(7, [1, 2])).unwrap();
        assert_eq!(c.destinations.len(), 2);
        assert_eq!(c.destinations[0].device_channels, None);
        assert_eq!(c.destinations[1].device_channels, Some(vec![1, 2]));
        // Repatcher le canal 7 ailleurs : mise à jour, pas de doublon.
        let c = apply(&c, &patch_in(7, [3, 4])).unwrap();
        assert_eq!(
            c.destinations
                .iter()
                .filter(|d| d.channel == Some(7))
                .count(),
            1
        );
        assert_eq!(c.destinations[1].device_channels, Some(vec![3, 4]));
        // Dépatcher.
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
            "sortie 5-6 disparue : émission arrêtée"
        );
        assert_eq!(c.destinations.len(), 1, "seul le patch 1-2 reste");
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
        assert!(apply(&c, &long).is_err(), "nom de plus de 32 caractères");
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
            "stéréo : 2 sorties exigées"
        );
        assert!(
            apply(&c, &out(4002, vec![8, 9])).is_err(),
            "sortie 9 inexistante"
        );
        assert!(apply(&c, &out(0, vec![1, 2])).is_err(), "canal 0 invalide");
        let c = apply(&c, &Edit::UnpatchOutput { channel: 4001 }).unwrap();
        assert!(c.sources.is_empty());
    }
}
