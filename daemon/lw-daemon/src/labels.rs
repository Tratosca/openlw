//! Names published by the device: device names and each channel's name.
//!
//! - Channels: “2 - Studio A L” / “… R” for an input fed by the left / right channel of Livewire
//!   channel 2 (name advertised on the network, falling back to the number); “2 - Studio A
//!   (L+R)” for the sum of both; “4005 - STUDIO MAC L” for a transmitted output. Surround:
//!   suffixes 1–8. Unpatched channel: empty name (CoreAudio displays its default name).
//! - Devices, `duplex` layout: “OpenLW” (fixed name).
//! - Devices, `multi` layout: “OpenLW In n” / “OpenLW Out n”, or with `custom_device_names`
//!   (default) “OpenLW In - Studio A@Omnia One (ch. 2)” (advertised source name @ advertising
//!   terminal), “OpenLW In - Studio A@Omnia One (ch. 2, L+R)” for a mono patch, “OpenLW In
//!   (ch. 2)” when the source is not advertised, “OpenLW Out - MAC 1-2 (ch. 4005)”. An empty
//!   device keeps its number.
//! - Linux PipeWire nodes: same names (duplex: “OpenLW In” and “OpenLW Out”).

use std::collections::BTreeMap;

use serde::Serialize;

use crate::config::Config;

/// Base device names (macos/plugin/src/OpenLWPlugIn.c, daemon/lw-pw).
pub const DEVICE_NAME: &str = "OpenLW";
pub const INPUT_DEVICE_NAME: &str = "OpenLW In";
pub const OUTPUT_DEVICE_NAME: &str = "OpenLW Out";

/// Discovered source: advertised name and advertising terminal.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Announced {
    pub name: String,
    pub terminal: String,
}

/// Names computed for the plugin.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Labels {
    /// Numbered-device layout.
    pub multi: bool,
    /// Duplex device name.
    pub name: String,
    /// Input and output device names, `multi` layout (index 0 = device 1); empty in duplex.
    pub input_device_names: Vec<String>,
    pub output_device_names: Vec<String>,
    /// One name per device input, devices concatenated in order (index 0 = input 1 of the
    /// first device).
    pub input_names: Vec<String>,
    /// One name per device output.
    pub output_names: Vec<String>,
}

impl Labels {
    /// Names without configuration.
    pub fn fallback() -> Self {
        Self {
            multi: false,
            name: DEVICE_NAME.into(),
            input_device_names: Vec::new(),
            output_device_names: Vec::new(),
            input_names: Vec::new(),
            output_names: Vec::new(),
        }
    }
}

fn suffixes(n: usize) -> Vec<String> {
    if n == 2 {
        vec!["L".into(), "R".into()]
    } else {
        (1..=n).map(|i| i.to_string()).collect()
    }
}

/// Side of a tap: “L”, “R”, “L+R” (stereo stream; in parentheses for a channel name when
/// `paren`), or the surround channel number.
fn tap_tag(from: &[u8], stereo: bool, paren: bool) -> String {
    let side = match (stereo, from) {
        (true, [1]) => "L".to_string(),
        (true, [2]) => "R".to_string(),
        (true, [1, 2]) => return if paren { "(L+R)".into() } else { "L+R".into() },
        _ => from.iter().map(u8::to_string).collect::<Vec<_>>().join("+"),
    };
    side
}

fn label(channel: u16, name: Option<&str>) -> String {
    match name.map(str::trim).filter(|s| !s.is_empty()) {
        Some(n) => format!("{channel} - {n}"),
        None => channel.to_string(),
    }
}

fn clean(s: &str) -> Option<&str> {
    Some(s.trim()).filter(|s| !s.is_empty())
}

/// Custom device name: “<base> - name@terminal (ch. N[, tag])” or “<base> (ch. N)”.
fn custom(base: &str, channel: u16, a: Option<&Announced>, tag: Option<&str>) -> String {
    let ch = match tag {
        Some(t) => format!("ch. {channel}, {t}"),
        None => format!("ch. {channel}"),
    };
    let name = a.and_then(|a| clean(&a.name));
    let terminal = a.and_then(|a| clean(&a.terminal));
    match (name, terminal) {
        (Some(n), Some(t)) => format!("{base} - {n}@{t} ({ch})"),
        (Some(n), None) => format!("{base} - {n} ({ch})"),
        _ => format!("{base} ({ch})"),
    }
}

/// Compute names from `cfg`; `announced`: discovered Livewire channels.
pub fn compute(cfg: &Config, announced: &BTreeMap<u32, Announced>) -> Labels {
    let multi = cfg.multi();
    let (in_w, out_w) = (cfg.in_widths(), cfg.out_widths());
    // Start of each device in the concatenated channel space (one device in duplex).
    let starts = |w: &[u32]| -> Vec<usize> {
        w.iter()
            .scan(0usize, |acc, &n| {
                let s = *acc;
                *acc += n as usize;
                Some(s)
            })
            .collect()
    };
    let (in_start, out_start) = (starts(&in_w), starts(&out_w));
    let mut input_names = vec![String::new(); in_w.iter().sum::<u32>() as usize];
    let mut output_names = vec![String::new(); out_w.iter().sum::<u32>() as usize];
    let mut input_device_names: Vec<String> = (1..=in_w.len())
        .map(|n| format!("{INPUT_DEVICE_NAME} {n}"))
        .collect();
    let mut output_device_names: Vec<String> = (1..=out_w.len())
        .map(|n| format!("{OUTPUT_DEVICE_NAME} {n}"))
        .collect();
    // Flat index (0-based) of `dc` (1-based) on device `dev` (None in duplex).
    let flat = |start: &[usize], dev: Option<u16>, dc: u16| -> Option<usize> {
        let d = usize::from(dev.unwrap_or(1)).checked_sub(1)?;
        Some(start.get(d)? + usize::from(dc).checked_sub(1)?)
    };
    for d in &cfg.destinations {
        let Some(ch) = d.channel else {
            continue;
        };
        let a = announced.get(&u32::from(ch));
        let base = label(ch, a.map(|a| a.name.as_str()));
        let stereo = d.stream_channels() == 2;
        for t in &d.taps {
            let name = format!("{base} {}", tap_tag(&t.from, stereo, true));
            if let Some(slot) =
                flat(&in_start, t.device, t.channel).and_then(|i| input_names.get_mut(i))
            {
                *slot = name;
            }
        }
        // Device named after its stream; a mono device also says which side.
        if !(multi && cfg.custom_device_names) {
            continue;
        }
        for n in d
            .taps
            .iter()
            .filter_map(|t| t.device)
            .collect::<std::collections::BTreeSet<_>>()
        {
            let mono = !cfg.coupled(n);
            let tag = d
                .taps
                .iter()
                .find(|t| t.device == Some(n) && mono)
                .map(|t| tap_tag(&t.from, stereo, false));
            if let Some(slot) = usize::from(n)
                .checked_sub(1)
                .and_then(|i| input_device_names.get_mut(i))
            {
                *slot = custom(INPUT_DEVICE_NAME, ch, a, tag.as_deref());
            }
        }
    }
    for s in &cfg.sources {
        let Some(chs) = &s.device_channels else {
            continue;
        };
        let base = label(s.channel, Some(&s.name));
        for (dc, sfx) in chs.iter().zip(suffixes(chs.len())) {
            if let Some(slot) =
                flat(&out_start, s.device, *dc).and_then(|i| output_names.get_mut(i))
            {
                *slot = format!("{base} {sfx}");
            }
        }
        if let (true, true, Some(slot)) = (
            multi,
            cfg.custom_device_names,
            s.device
                .and_then(|n| usize::from(n).checked_sub(1))
                .and_then(|i| output_device_names.get_mut(i)),
        ) {
            let own = Announced {
                name: s.name.clone(),
                terminal: String::new(),
            };
            *slot = custom(OUTPUT_DEVICE_NAME, s.channel, Some(&own), None);
        }
    }
    if !multi {
        input_device_names.clear();
        output_device_names.clear();
    }
    Labels {
        multi,
        name: DEVICE_NAME.into(),
        input_device_names,
        output_device_names,
        input_names,
        output_names,
    }
}

#[cfg(test)]
#[allow(clippy::indexing_slicing)]
mod tests {
    use super::*;
    use crate::config::Layout;

    fn announced() -> BTreeMap<u32, Announced> {
        BTreeMap::from([(
            2,
            Announced {
                name: "Studio A".into(),
                terminal: "Omnia One".into(),
            },
        )])
    }

    #[test]
    fn duplex_channel_names() {
        let c: Config = serde_json::from_str(
            r#"{"iface":"lo0","device":{"channels_to_net":2,"channels_from_net":6},
                "destinations":[{"channel":21,"device_channels":[3,4]},{"channel":2,"device_channels":[1,2]},
                                {"channel":7,"mix":"sum","device_channels":[6]},{"channel":9}],
                "sources":[{"channel":4005,"name":"STUDIO MAC","format":"standard","device_channels":[1,2]}]}"#,
        )
        .unwrap();
        c.validate().unwrap();
        let l = compute(&c, &announced());
        assert!(!l.multi);
        assert_eq!(l.name, "OpenLW");
        assert!(l.input_device_names.is_empty() && l.output_device_names.is_empty());
        assert_eq!(
            l.input_names,
            [
                "2 - Studio A L",
                "2 - Studio A R",
                "21 L",
                "21 R",
                "",
                "7 (L+R)"
            ]
        );
        assert_eq!(
            l.output_names,
            ["4005 - STUDIO MAC L", "4005 - STUDIO MAC R"]
        );
    }

    fn multi(custom: Option<bool>) -> Config {
        let naming = match custom {
            Some(b) => format!(r#","custom_device_names":{b}"#),
            None => String::new(),
        };
        let c: Config = serde_json::from_str(&format!(
            r#"{{"iface":"lo0","device_layout":"multi","device":{{"channels_to_net":4,"channels_from_net":8}}{naming},"uncoupled_inputs":[2],
                "destinations":[{{"channel":2,"device":1,"device_channels":[1,2]}},
                                {{"channel":21,"device":2,"mix":"left","device_channels":[1]}},
                                {{"channel":5,"kind":"surround","device":4,"device_channels":[1,2,3,4,5,6,7,8]}}],
                "sources":[{{"channel":4005,"name":"MAC 1-2","format":"standard","device":2,"device_channels":[1,2]}}]}}"#
        ))
        .unwrap();
        c.validate().unwrap();
        c
    }

    #[test]
    fn multi_device_names() {
        let l = compute(&multi(None), &announced());
        assert!(l.multi);
        assert_eq!(
            l.input_device_names,
            [
                "OpenLW In - Studio A@Omnia One (ch. 2)",
                "OpenLW In (ch. 21, L)",
                "OpenLW In 3",
                "OpenLW In (ch. 5)"
            ],
            "custom names by default; empty device keeps its number"
        );
        assert_eq!(
            l.output_device_names,
            ["OpenLW Out 1", "OpenLW Out - MAC 1-2 (ch. 4005)"]
        );
        // Channels concatenated in device order: 2 + 1 + 2 (empty) + 8.
        assert_eq!(l.input_names.len(), 13);
        assert_eq!(
            &l.input_names[..3],
            ["2 - Studio A L", "2 - Studio A R", "21 L"]
        );
        assert_eq!(l.input_names[5], "5 1");
        assert_eq!(l.output_names[2], "4005 - MAC 1-2 L");
        let g = compute(&multi(Some(false)), &announced());
        assert_eq!(g.input_device_names[0], "OpenLW In 1", "generic names");
        assert_eq!(g.output_device_names[1], "OpenLW Out 2");
    }

    #[test]
    fn source_without_terminal() {
        let a = BTreeMap::from([(
            2,
            Announced {
                name: "Studio A".into(),
                terminal: " ".into(),
            },
        )]);
        let mut c = multi(None);
        c.device_layout = Layout::Multi;
        let l = compute(&c, &a);
        assert_eq!(l.input_device_names[0], "OpenLW In - Studio A (ch. 2)");
    }
}
