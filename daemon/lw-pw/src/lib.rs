//! OpenLW device PipeWire nodes (Linux, ADR 0009).
//!
//! Daemon publishes two nodes in its own process, connected to the shared region:
//! - “OpenLW Out” sink (`Audio/Sink`): application playback goes to the network
//!   (TO_NET ring producer);
//! - “OpenLW In” source (`Audio/Source`): applications record network audio
//!   (FROM_NET ring consumer), with the service latency margin.
//!
//! Nodes follow the graph driver (sound card or dummy driver); differences from the daemon
//! clock are compensated through buffer slips, like the HAL plugin (ADR 0003). Outside Linux,
//! the crate is empty.

#[cfg(target_os = "linux")]
mod linux;

#[cfg(target_os = "linux")]
pub use linux::{start, Bridge, Report};

/// Node parameters.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BridgeConfig {
    /// Displayed sink name (application outputs to network).
    pub sink_description: String,
    /// Displayed source name (application inputs from network).
    pub source_description: String,
    /// Input latency margin, in frames (64–2048).
    pub input_margin: u32,
}

impl Default for BridgeConfig {
    fn default() -> Self {
        Self {
            sink_description: "OpenLW Out".into(),
            source_description: "OpenLW In".into(),
            input_margin: 256,
        }
    }
}

/// Erreur PipeWire.
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
    /// Pas encore assez d'audio : silence.
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
mod tests {
    use super::*;

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
