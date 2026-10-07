//! Names published by the device: device name and each channel's name.
//!
//! - Channels: currently “2 - Studio A L” / “… R” for an input patched to Livewire channel 2 (name
//!   advertised on the network, falling back to the number); “4005 - STUDIO MAC L” for a transmitted output.
//!   Surround: suffixes 1–8. Unpatched channel: empty name (CoreAudio displays its default name).
//! - Devices: `duplex` layout uses “OpenLW” (fixed name). With `split` layout,
//!   “OpenLW In” and “OpenLW Out”, or, if `name_device_from_sources` is enabled, “OpenLW In
//!   (2 - Studio A, 21 - STUDIO A)” and “OpenLW Out (31 - Mac 1-2)” (device channel order).

use std::collections::BTreeMap;

use serde::Serialize;

use crate::config::{Config, Layout};

/// Base device names (macos/plugin/src/OpenLWPlugIn.c).
pub const DEVICE_NAME: &str = "OpenLW";
pub const INPUT_DEVICE_NAME: &str = "OpenLW In";
pub const OUTPUT_DEVICE_NAME: &str = "OpenLW Out";

/// Names computed for the plugin.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Labels {
    /// Two-device layout.
    pub split: bool,
    /// Duplex device name.
    pub name: String,
    pub input_device_name: String,
    pub output_device_name: String,
    /// One name per device input (index 0 = input 1).
    pub input_names: Vec<String>,
    /// One name per device output.
    pub output_names: Vec<String>,
}

fn suffixes(n: usize) -> Vec<String> {
    if n == 2 {
        vec!["L".into(), "R".into()]
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

/// Compute names from `cfg`; `announced`: advertised name of each discovered Livewire channel.
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
            ["2 - Studio A L", "2 - Studio A R", "21 L", "21 R"]
        );
        assert_eq!(
            l.output_names,
            ["4005 - STUDIO MAC L", "4005 - STUDIO MAC R"]
        );
        assert_eq!(
            compute(&cfg(false), &announced).name,
            "OpenLW",
            "option disabled"
        );
        let mut empty = cfg(true);
        empty.destinations.clear();
        assert_eq!(
            compute(&empty, &announced).name,
            "OpenLW",
            "no input patched"
        );
    }
}
