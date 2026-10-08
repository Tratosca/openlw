//! OpenLW device PipeWire nodes (Linux).
//!
//! Daemon publishes one node per shared-region ring, in its own process:
//! - sink (`Audio/Sink`) per TO_NET ring: application playback goes to the network (ring
//!   producer); duplex layout: “OpenLW Out” (`openlw_out`);
//! - source (`Audio/Source`) per FROM_NET ring: applications record network audio (ring
//!   consumer), with the service latency margin; duplex layout: “OpenLW In” (`openlw_in`).
//!
//! Multi layout: one sink per output device (`openlw_out_n`) and one source per
//! input device (`openlw_in_n`), named like the macOS devices.
//!
//! Nodes follow the graph driver (sound card or dummy driver); differences from the daemon
//! clock are compensated through buffer slips, like the HAL plugin. Outside Linux,
//! the crate is empty.

#[cfg(target_os = "linux")]
mod linux;

#[cfg(target_os = "linux")]
pub use linux::{start, Bridge, Report};

/// One node: region ring, direction, PipeWire names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NodeConfig {
    /// Shared-region ring index.
    pub ring: usize,
    /// Sink (TO_NET ring, applications play) or source (FROM_NET ring, applications record).
    pub sink: bool,
    /// `node.name` (stable identifier, e.g. `openlw_in_2`).
    pub name: String,
    /// `node.description` (displayed name).
    pub description: String,
}

/// Node parameters.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BridgeConfig {
    /// Nodes to publish, in region ring order.
    pub nodes: Vec<NodeConfig>,
    /// Input latency margin, in frames (64–2048).
    pub input_margin: u32,
}

impl BridgeConfig {
    /// Duplex layout: “OpenLW Out” sink on ring 0, “OpenLW In” source on ring 1.
    pub fn duplex(input_margin: u32) -> Self {
        Self::numbered(
            &["OpenLW Out".into()],
            &["OpenLW In".into()],
            false,
            input_margin,
        )
    }

    /// Sinks for `outputs` then sources for `inputs` (display names), in ring order.
    /// `numbered`: node names `openlw_out_n` / `openlw_in_n`, otherwise `openlw_out` /
    /// `openlw_in` (one of each).
    pub fn numbered(
        outputs: &[String],
        inputs: &[String],
        numbered: bool,
        input_margin: u32,
    ) -> Self {
        let node = |ring: usize, sink: bool, n: usize, description: &String| NodeConfig {
            ring,
            sink,
            name: match (sink, numbered) {
                (true, true) => format!("openlw_out_{n}"),
                (false, true) => format!("openlw_in_{n}"),
                (true, false) => "openlw_out".into(),
                (false, false) => "openlw_in".into(),
            },
            description: description.clone(),
        };
        let mut nodes: Vec<NodeConfig> = outputs
            .iter()
            .enumerate()
            .map(|(i, d)| node(i, true, i + 1, d))
            .collect();
        nodes.extend(
            inputs
                .iter()
                .enumerate()
                .map(|(i, d)| node(outputs.len() + i, false, i + 1, d)),
        );
        Self {
            nodes,
            input_margin,
        }
    }
}

impl BridgeConfig {
    /// Same nodes (rings, directions, `node.name`, margin), possibly other descriptions:
    /// a rename is enough, no need to recreate the nodes.
    pub fn same_nodes(&self, other: &Self) -> bool {
        self.input_margin == other.input_margin
            && self.nodes.len() == other.nodes.len()
            && self
                .nodes
                .iter()
                .zip(&other.nodes)
                .all(|(a, b)| a.ring == b.ring && a.sink == b.sink && a.name == b.name)
    }

    /// Node descriptions, in node order.
    pub fn descriptions(&self) -> Vec<String> {
        self.nodes.iter().map(|n| n.description.clone()).collect()
    }
}

impl Default for BridgeConfig {
    fn default() -> Self {
        Self::duplex(256)
    }
}

/// PipeWire error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error(pub String);

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Error {}

/// Advertised channel positions: stereo FL/FR for two channels, AUX0… beyond (independent Livewire
/// channels, without spatial meaning).
pub fn channel_positions(channels: u32) -> String {
    if channels == 2 {
        "FL,FR".into()
    } else if channels == 1 {
        "MONO".into()
    } else {
        (0..channels)
            .map(|i| format!("AUX{i}"))
            .collect::<Vec<_>>()
            .join(",")
    }
}

/// Input read decision: frames to discard before reading `frames`, or silence until
/// ring is primed. Same rule as HAL plugin and Windows driver: prime at
/// `frames + margin` available frames; catch up above `frames + 2 × margin`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputPlan {
    /// Not enough audio yet: silence.
    Silence,
    /// Discard `skip` frames, then read.
    Read { skip: u32 },
}

/// Apply priming rule; `primed` becomes true upon priming.
pub fn plan_input(readable: u32, frames: u32, margin: u32, primed: &mut bool) -> InputPlan {
    if !*primed && readable >= frames.saturating_add(margin) {
        *primed = true;
    }
    if !*primed {
        return InputPlan::Silence;
    }
    let high = frames.saturating_add(margin.saturating_mul(2));
    let skip = if readable > high {
        readable - frames - margin
    } else {
        0
    };
    InputPlan::Read { skip }
}

#[cfg(test)]
#[allow(clippy::indexing_slicing)]
mod tests {
    use super::*;

    #[test]
    fn node_names() {
        let d = BridgeConfig::default();
        assert_eq!(
            d.nodes
                .iter()
                .map(|n| (n.ring, n.sink, n.name.as_str(), n.description.as_str()))
                .collect::<Vec<_>>(),
            [
                (0, true, "openlw_out", "OpenLW Out"),
                (1, false, "openlw_in", "OpenLW In")
            ]
        );
        let m = BridgeConfig::numbered(
            &["OpenLW Out 1".into()],
            &["OpenLW In - A (ch. 2)".into(), "OpenLW In 2".into()],
            true,
            256,
        );
        assert_eq!(m.nodes[2].ring, 2);
        assert_eq!(m.nodes[2].name, "openlw_in_2");
        assert_eq!(m.nodes[0].name, "openlw_out_1");
    }

    #[test]
    fn positions() {
        assert_eq!(channel_positions(1), "MONO");
        assert_eq!(channel_positions(2), "FL,FR");
        assert_eq!(channel_positions(4), "AUX0,AUX1,AUX2,AUX3");
    }

    #[test]
    fn input_priming_and_catch_up() {
        let mut primed = false;
        assert_eq!(plan_input(100, 256, 256, &mut primed), InputPlan::Silence);
        assert!(!primed);
        assert_eq!(
            plan_input(512, 256, 256, &mut primed),
            InputPlan::Read { skip: 0 }
        );
        assert!(primed);
        // Once primed, read even when short (pad with silence, counted as underrun).
        assert_eq!(
            plan_input(10, 256, 256, &mut primed),
            InputPlan::Read { skip: 0 }
        );
        // Backlog beyond two margins: return to one margin.
        assert_eq!(
            plan_input(2000, 256, 256, &mut primed),
            InputPlan::Read { skip: 1488 }
        );
    }
}
