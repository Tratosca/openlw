//! Patch : construit, à partir de la configuration, les bus entre flux réseau et périphérique.
//!
//! - Source avec `device_channels` : sorties du périphérique → bus → flux émis.
//! - Destination avec `device_channels` : flux reçu → bus → entrées du périphérique.
//!
//! Réglages des tampons de gigue (en trames à 48 kHz), selon le préréglage de latence :
//! - émission : cible = 2 paquets + réserve (256, 512 ou 1024), seuil haut = cible + 2048 ;
//! - réception : cible = 288, 576 ou 1152 (6, 12 ou 24 ms), seuil haut = 4 × cible.

use std::net::Ipv4Addr;

use crate::bus::{bus, BusWriter, JitterReader};
use crate::config::Config;
use crate::device::{InRoute, OutRoute, Routes};
use crate::tx::TxStream;

/// Capacité de chaque bus (trames) : 170 ms.
pub const BUS_FRAMES: usize = 8192;
/// Cible du tampon de gigue en réception avec le préréglage par défaut (trames).
pub const RX_TARGET: usize = 576;

/// Flux émis et, le cas échéant, sa source audio.
pub struct TxPlan {
    pub stream: TxStream,
    pub source: Option<JitterReader>,
}

/// Flux reçu et, le cas échéant, son bus vers le périphérique.
pub struct RxPlan {
    pub group: Ipv4Addr,
    pub port: u16,
    pub label: String,
    pub sink: Option<BusWriter>,
}

/// Plan complet : threads réseau à lancer et table de routes du périphérique.
pub struct Plan {
    pub tx: Vec<TxPlan>,
    pub rx: Vec<RxPlan>,
    pub routes: Routes,
}

impl Plan {
    /// Nombre de routes vers ou depuis le périphérique.
    pub fn patched(&self) -> usize {
        self.routes.inputs.len() + self.routes.outputs.len()
    }
}

/// Construit le plan d'une configuration validée.
pub fn build(cfg: &Config) -> Plan {
    let mut routes = Routes::default();
    let mut tx = Vec::new();
    for (stream, src) in cfg.tx_streams().into_iter().zip(&cfg.sources) {
        let source = src.device_channels.as_ref().map(|chs| {
            let ch = usize::from(stream.format.channels());
            let (writer, reader) = bus(ch, BUS_FRAMES);
            let spp = stream.format.samples_per_packet() as usize;
            let target = 2 * spp + cfg.latency.tx_cushion();
            routes.outputs.push(OutRoute {
                label: format!("{} → canal {}", src.name, src.channel),
                device_channels: chs.iter().map(|&c| usize::from(c) - 1).collect(),
                writer,
            });
            JitterReader::new(reader, target, target + 2048)
        });
        tx.push(TxPlan { stream, source });
    }
    let mut rx = Vec::new();
    for (d, (group, port)) in cfg.destinations.iter().zip(cfg.rx_groups()) {
        let sink = d.device_channels.as_ref().map(|chs| {
            let (writer, reader) = bus(d.stream_channels(), BUS_FRAMES);
            routes.inputs.push(InRoute {
                label: format!("{} → entrées {:?}", d.label(), chs),
                device_channels: chs.iter().map(|&c| usize::from(c) - 1).collect(),
                reader: JitterReader::new(
                    reader,
                    cfg.latency.rx_target(),
                    4 * cfg.latency.rx_target(),
                ),
            });
            writer
        });
        rx.push(RxPlan {
            group,
            port,
            label: d.label(),
            sink,
        });
    }
    Plan { tx, rx, routes }
}

#[cfg(test)]
#[allow(clippy::indexing_slicing)]
mod tests {
    use super::*;

    #[test]
    fn builds_routes_from_config() {
        let cfg: Config = serde_json::from_str(
            r#"{"iface":"lo0","device":{"channels_to_net":8,"channels_from_net":8},
                "sources":[{"channel":4001,"name":"MAC 1","format":"standard","device_channels":[1,2]},
                           {"channel":4002,"name":"TEST","format":"aes67"}],
                "destinations":[{"channel":1,"device_channels":[3,4]},{"channel":2}]}"#,
        )
        .unwrap();
        cfg.validate().unwrap();
        let plan = build(&cfg);
        assert_eq!(plan.tx.len(), 2);
        assert!(plan.tx[0].source.is_some() && plan.tx[1].source.is_none());
        assert_eq!(plan.rx.len(), 2);
        assert!(plan.rx[0].sink.is_some() && plan.rx[1].sink.is_none());
        assert_eq!(plan.routes.outputs[0].device_channels, vec![0, 1]);
        assert_eq!(plan.routes.inputs[0].device_channels, vec![2, 3]);
        assert_eq!(plan.patched(), 2);
    }

    #[test]
    fn rejects_bad_patches() {
        for bad in [
            r#"{"iface":"lo0","sources":[{"channel":4001,"name":"X","format":"standard","device_channels":[1]}]}"#,
            r#"{"iface":"lo0","sources":[{"channel":4001,"name":"X","format":"standard","device_channels":[1,9]}]}"#,
            r#"{"iface":"lo0","destinations":[{"channel":1,"device_channels":[1,2]},{"channel":2,"device_channels":[2,3]}]}"#,
            r#"{"iface":"lo0","destinations":[{"channel":5,"kind":"surround","device_channels":[1,2]}]}"#,
        ] {
            let cfg: Config = serde_json::from_str(bad).unwrap();
            assert!(cfg.validate().is_err(), "{bad}");
        }
    }
}
