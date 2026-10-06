//! Noms publiés par le périphérique : nom du périphérique et nom de chaque canal.
//!
//! - Canaux : « 2 - Studio A G » / « … D » pour une entrée patchée sur le canal Livewire 2 (nom
//!   annoncé par le réseau, à défaut le numéro seul) ; « 4005 - STUDIO MAC G » pour une sortie diffusée.
//!   Surround : suffixes 1 à 8. Canal non patché : nom vide (CoreAudio affiche son nom par défaut).
//! - Périphériques : en présentation `duplex`, « OpenLW » (nom fixe). En présentation `split`,
//!   « OpenLW In » et « OpenLW Out », ou, si `name_device_from_sources` est activé, « OpenLW In
//!   (2 - Studio A, 21 - STUDIO A) » et « OpenLW Out (31 - Mac 1-2) » (ordre des canaux du périphérique).

use std::collections::BTreeMap;

use serde::Serialize;

use crate::config::{Config, Layout};

/// Noms de base des périphériques (plugin/src/OpenLWPlugIn.c).
pub const DEVICE_NAME: &str = "OpenLW";
pub const INPUT_DEVICE_NAME: &str = "OpenLW In";
pub const OUTPUT_DEVICE_NAME: &str = "OpenLW Out";

/// Noms calculés pour le plugin.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Labels {
    /// Présentation en deux périphériques.
    pub split: bool,
    /// Nom du périphérique duplex.
    pub name: String,
    pub input_device_name: String,
    pub output_device_name: String,
    /// Un nom par entrée du périphérique (index 0 = entrée 1).
    pub input_names: Vec<String>,
    /// Un nom par sortie du périphérique.
    pub output_names: Vec<String>,
}

fn suffixes(n: usize) -> Vec<String> {
    if n == 2 {
        vec!["G".into(), "D".into()]
    } else {
        (1..=n).map(|i| i.to_string()).collect()
    }
}

fn label(channel: u16, name: Option<&str>) -> String {
    match name.map(str::trim).filter(|s| !s.is_empty()) {
        Some(n) => format!("{channel} - {n}"),
        None => channel.to_string(),
    }
}

/// Calcule les noms de `cfg` ; `announced` : nom annoncé de chaque canal Livewire découvert.
pub fn compute(cfg: &Config, announced: &BTreeMap<u32, String>) -> Labels {
    let dev = cfg.device_config();
    let mut input_names = vec![String::new(); dev.channels_from_net as usize];
    let mut output_names = vec![String::new(); dev.channels_to_net as usize];
    let mut patched: Vec<(u16, String)> = Vec::new();
    let mut emitted: Vec<(u16, String)> = Vec::new();
    for d in &cfg.destinations {
        let (Some(ch), Some(chs)) = (d.channel, &d.device_channels) else {
            continue;
        };
        let base = label(ch, announced.get(&u32::from(ch)).map(String::as_str));
        for (dc, sfx) in chs.iter().zip(suffixes(chs.len())) {
            if let Some(slot) = input_names.get_mut(usize::from(*dc).wrapping_sub(1)) {
                *slot = format!("{base} {sfx}");
            }
        }
        if let Some(first) = chs.iter().min() {
            patched.push((*first, base));
        }
    }
    for s in &cfg.sources {
        let Some(chs) = &s.device_channels else {
            continue;
        };
        let base = label(s.channel, Some(&s.name));
        for (dc, sfx) in chs.iter().zip(suffixes(chs.len())) {
            if let Some(slot) = output_names.get_mut(usize::from(*dc).wrapping_sub(1)) {
                *slot = format!("{base} {sfx}");
            }
        }
        if let Some(first) = chs.iter().min() {
            emitted.push((*first, base));
        }
    }
    let split = cfg.device_layout == Layout::Split;
    let named = |base: &str, mut list: Vec<(u16, String)>| -> String {
        list.sort();
        if split && cfg.name_device_from_sources && !list.is_empty() {
            let l: Vec<String> = list.into_iter().map(|(_, l)| l).collect();
            format!("{base} ({})", l.join(", "))
        } else {
            base.into()
        }
    };
    Labels {
        split,
        name: DEVICE_NAME.into(),
        input_device_name: named(INPUT_DEVICE_NAME, patched),
        output_device_name: named(OUTPUT_DEVICE_NAME, emitted),
        input_names,
        output_names,
    }
}

#[cfg(test)]
#[allow(clippy::indexing_slicing)]
mod tests {
    use super::*;

    fn cfg(naming: bool) -> Config {
        cfg_layout(naming, Layout::Split)
    }

    fn cfg_layout(naming: bool, layout: Layout) -> Config {
        let mut c: Config = serde_json::from_str(
            r#"{"iface":"lo0","device":{"channels_to_net":2,"channels_from_net":4},
                "destinations":[{"channel":21,"device_channels":[3,4]},{"channel":2,"device_channels":[1,2]},{"channel":9}],
                "sources":[{"channel":4005,"name":"STUDIO MAC","format":"standard","device_channels":[1,2]}]}"#,
        )
        .unwrap();
        c.name_device_from_sources = naming;
        c.device_layout = layout;
        c
    }

    #[test]
    fn names_from_announcements_and_patch() {
        let announced = BTreeMap::from([(2, "Studio A".to_string())]);
        let l = compute(&cfg(true), &announced);
        assert!(l.split);
        assert_eq!(l.input_device_name, "OpenLW In (2 - Studio A, 21)");
        assert_eq!(l.output_device_name, "OpenLW Out (4005 - STUDIO MAC)");
        assert_eq!(
            l.input_names,
            ["2 - Studio A G", "2 - Studio A D", "21 G", "21 D"]
        );
        assert_eq!(
            l.output_names,
            ["4005 - STUDIO MAC G", "4005 - STUDIO MAC D"]
        );
        assert_eq!(
            compute(&cfg(false), &announced).name,
            "OpenLW",
            "option désactivée"
        );
        let mut empty = cfg(true);
        empty.destinations.clear();
        assert_eq!(
            compute(&empty, &announced).name,
            "OpenLW",
            "aucune entrée patchée"
        );
    }
}
