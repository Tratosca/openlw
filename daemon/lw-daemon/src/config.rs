//! Configuration persistée du daemon (JSON) : NIC, sources émises, destinations reçues, annonce, QoS.
//!
//! Exemple :
//! ```json
//! {
//!   "iface": "auto",
//!   "terminal_name": "Studio Mac",
//!   "advertise": true,
//!   "tos": 184,
//!   "sources": [ { "channel": 4001, "name": "MAC 1", "format": "standard", "tone_hz": 997, "level_dbfs": -20 } ],
//!   "destinations": [ { "channel": 1, "kind": "stereo" } ]
//! }
//! ```
//!
//! `iface` : `auto` (défaut) choisit l'interface qui entend des annonces Livewire (voir `detect`) ;
//! sinon nom BSD ou nom convivial. `terminal_name` vide : nom de l'ordinateur.

use std::net::Ipv4Addr;
use std::path::Path;

use lw_proto::adv::{AdvStreamType, Source};
use lw_proto::channel::{Channel, GroupKind, AUDIO_PORT};
use lw_proto::format::StreamFormat;
use serde::{Deserialize, Serialize};

use crate::tx::{Tone, TxStream};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Format {
    Standard,
    Aes67,
    Livestream,
    Surround,
}

impl From<Format> for StreamFormat {
    fn from(f: Format) -> Self {
        match f {
            Format::Standard => StreamFormat::Standard,
            Format::Aes67 => StreamFormat::Aes67,
            Format::Livestream => StreamFormat::Livestream,
            Format::Surround => StreamFormat::Surround,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    #[default]
    Stereo,
    Backfeed,
    Surround,
}

impl From<Kind> for GroupKind {
    fn from(k: Kind) -> Self {
        match k {
            Kind::Stereo => GroupKind::Stereo,
            Kind::Backfeed => GroupKind::Backfeed,
            Kind::Surround => GroupKind::Surround,
        }
    }
}

/// Préréglage de latence de réception : tampon de gigue du daemon et marge d'entrée du plugin.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Latency {
    /// ≈ 8 ms ajoutées : réseau dédié, sans autre trafic.
    Low,
    /// ≈ 17 ms ajoutées.
    #[default]
    Normal,
    /// ≈ 35 ms ajoutées : réseau partagé ou ordinateur chargé.
    Safe,
}

impl Latency {
    /// Cible du tampon de gigue en réception (trames) : 6, 12 ou 24 ms.
    pub fn rx_target(self) -> usize {
        match self {
            Latency::Low => 288,
            Latency::Normal => 576,
            Latency::Safe => 1152,
        }
    }

    /// Marge d'entrée du plugin au-delà d'un bloc d'IO (trames) : 2,7, 5,3 ou 10,7 ms.
    pub fn input_margin(self) -> u32 {
        match self {
            Latency::Low => 128,
            Latency::Normal => 256,
            Latency::Safe => 512,
        }
    }

    /// Réserve du tampon d'émission au-delà de deux paquets (trames).
    pub fn tx_cushion(self) -> usize {
        match self {
            Latency::Low => 256,
            Latency::Normal => 512,
            Latency::Safe => 1024,
        }
    }
}

/// Présentation du périphérique dans macOS.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Layout {
    /// Un périphérique « OpenLW », entrée et sortie, nom fixe.
    #[default]
    Duplex,
    /// Deux périphériques « OpenLW In » et « OpenLW Out », nommables d'après les canaux patchés.
    Split,
}

/// Longueur maximale du nom annoncé (`ATRN`, 32 octets ASCII).
pub const MAX_TERMINAL_NAME: usize = 32;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourceConfig {
    pub channel: u16,
    pub name: String,
    pub format: Format,
    #[serde(default = "default_pt")]
    pub payload_type: u8,
    #[serde(default = "default_tone")]
    pub tone_hz: f64,
    #[serde(default = "default_level")]
    pub level_dbfs: f64,
    /// Sorties du périphérique (1-based) émises par ce flux, une par canal du flux (2 en stéréo,
    /// 8 en surround). Absent : générateur de test.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device_channels: Option<Vec<u16>>,
}

/// Destination : un canal Livewire (et sa famille) ou un groupe explicite (SDP importé, AES67 tiers).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DestinationConfig {
    #[serde(default)]
    pub channel: Option<u16>,
    #[serde(default)]
    pub kind: Kind,
    #[serde(default)]
    pub group: Option<Ipv4Addr>,
    #[serde(default = "default_port")]
    pub port: u16,
    /// Entrées du périphérique (1-based) alimentées par ce flux, une par canal du flux.
    /// Absent : réception pour statistiques seulement.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device_channels: Option<Vec<u16>>,
}

impl DestinationConfig {
    /// Libellé lisible : « canal 12 » ou « 239.192.0.9:5004 ».
    pub fn label(&self) -> String {
        match (self.channel, self.group) {
            (Some(c), _) => format!("canal {c}"),
            (None, Some(g)) => format!("{g}:{}", self.port),
            _ => "?".into(),
        }
    }

    /// Nombre de canaux du flux reçu (8 en surround, 2 sinon).
    pub fn stream_channels(&self) -> usize {
        if self.kind == Kind::Surround {
            8
        } else {
            2
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    /// Nom BSD (enX) ou nom convivial de l'interface Livewire, ou [`AUTO_IFACE`].
    #[serde(default = "default_iface")]
    pub iface: String,
    #[serde(default = "default_terminal")]
    pub terminal_name: String,
    #[serde(default = "default_true")]
    pub advertise: bool,
    /// Octet TOS des flux audio (DSCP × 4) ; 184 = EF.
    #[serde(default = "default_tos")]
    pub tos: u32,
    /// Préréglage de latence de réception.
    #[serde(default)]
    pub latency: Latency,
    #[serde(default)]
    pub sources: Vec<SourceConfig>,
    #[serde(default)]
    pub destinations: Vec<DestinationConfig>,
    /// Périphérique virtuel partagé avec le client audio (activé par défaut avec `--control`).
    #[serde(default)]
    pub device: Option<crate::device::DeviceConfig>,
    /// Présentation du périphérique dans macOS.
    #[serde(default)]
    pub device_layout: Layout,
    /// En présentation `split`, nomme les périphériques d'après les canaux patchés (voir [`crate::labels`]).
    #[serde(default)]
    pub name_device_from_sources: bool,
}

fn default_pt() -> u8 {
    96
}
fn default_tone() -> f64 {
    997.0
}
fn default_level() -> f64 {
    -20.0
}
fn default_port() -> u16 {
    AUDIO_PORT
}
fn default_terminal() -> String {
    String::new()
}
fn default_iface() -> String {
    AUTO_IFACE.into()
}

/// Nom par défaut d'une source émise (patch de sortie sans nom).
pub const DEFAULT_SOURCE_NAME: &str = if cfg!(target_os = "macos") { "MAC" } else { "PC" };

/// Valeur de `iface` pour le choix automatique de l'interface.
pub const AUTO_IFACE: &str = "auto";

/// Nombre maximal de canaux du périphérique dans chaque sens (16 canaux Livewire stéréo).
pub const MAX_DEVICE_CHANNELS: u32 = 32;

/// Nom de l'ordinateur, annoncé par défaut sur le réseau.
/// macOS : Réglages Système > Général > Partage, à défaut le nom d'hôte ; Linux : nom convivial de
/// `/etc/machine-info` (`hostnamectl --pretty`), à défaut le nom d'hôte ; Windows : nom NetBIOS.
pub fn computer_name() -> String {
    let clean = |s: String| Some(s.trim().to_string()).filter(|s| !s.is_empty());
    #[cfg(target_os = "macos")]
    let name = {
        let run = |cmd: &str, args: &[&str]| {
            std::process::Command::new(cmd)
                .args(args)
                .output()
                .ok()
                .and_then(|o| String::from_utf8(o.stdout).ok())
                .and_then(clean)
        };
        run("/usr/sbin/scutil", &["--get", "ComputerName"]).or_else(|| run("/bin/hostname", &["-s"]))
    };
    #[cfg(target_os = "linux")]
    let name = std::fs::read_to_string("/etc/machine-info")
        .ok()
        .and_then(|t| {
            t.lines()
                .find_map(|l| l.strip_prefix("PRETTY_HOSTNAME="))
                .map(|v| v.trim_matches('"').to_string())
        })
        .and_then(clean)
        .or_else(|| {
            std::fs::read_to_string("/proc/sys/kernel/hostname")
                .ok()
                .and_then(clean)
        });
    #[cfg(windows)]
    let name = std::env::var("COMPUTERNAME").ok().and_then(clean);
    name.unwrap_or_else(|| "OpenLW".into())
}
fn default_true() -> bool {
    true
}
fn default_tos() -> u32 {
    0xB8
}

/// Erreur de configuration, avec un message destiné à l'utilisateur.
#[derive(Debug)]
pub struct ConfigError(pub String);

impl std::fmt::Display for ConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ConfigError {}

impl Config {
    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| ConfigError(format!("{} : {e}", path.display())))?;
        let cfg: Config = serde_json::from_str(&text)
            .map_err(|e| ConfigError(format!("{} : {e}", path.display())))?;
        cfg.validate()?;
        Ok(cfg)
    }

    /// Enregistre de façon atomique : écriture d'un fichier voisin, puis renommage (un arrêt brutal
    /// laisse l'ancienne ou la nouvelle configuration, jamais un fichier tronqué).
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        let text = serde_json::to_string_pretty(self).map_err(std::io::Error::other)?;
        let mut tmp = path.as_os_str().to_owned();
        tmp.push(".tmp");
        let tmp = std::path::PathBuf::from(tmp);
        std::fs::write(&tmp, text + "\n")?;
        std::fs::rename(&tmp, path)
    }

    /// Choix automatique de l'interface ?
    pub fn auto_iface(&self) -> bool {
        self.iface.is_empty() || self.iface == AUTO_IFACE
    }

    /// Nom annoncé du terminal.
    pub fn terminal(&self) -> String {
        if self.terminal_name.trim().is_empty() {
            computer_name()
        } else {
            self.terminal_name.clone()
        }
    }

    /// Paramètres du périphérique virtuel (défaut : 2 canaux dans chaque sens).
    pub fn device_config(&self) -> crate::device::DeviceConfig {
        self.device.clone().unwrap_or_default()
    }

    pub fn validate(&self) -> Result<(), ConfigError> {
        if self.terminal_name.chars().count() > MAX_TERMINAL_NAME {
            return Err(ConfigError(format!(
                "nom annoncé : {MAX_TERMINAL_NAME} caractères au plus"
            )));
        }
        if self.tos > 0xFC || self.tos % 4 != 0 {
            return Err(ConfigError(format!(
                "priorité réseau : octet TOS {} invalide (DSCP 0 à 63, × 4)",
                self.tos
            )));
        }
        let dev = self.device_config();
        for (n, what) in [
            (dev.channels_to_net, "sorties"),
            (dev.channels_from_net, "entrées"),
        ] {
            if !(1..=MAX_DEVICE_CHANNELS).contains(&n) {
                return Err(ConfigError(format!(
                    "périphérique : {n} {what}, attendu 1 à {MAX_DEVICE_CHANNELS}"
                )));
            }
        }
        let mut seen = std::collections::BTreeSet::new();
        for s in &self.sources {
            Channel::new(s.channel).ok_or_else(|| {
                ConfigError(format!(
                    "source « {} » : canal {} hors 1..32766",
                    s.name, s.channel
                ))
            })?;
            if !seen.insert(s.channel) {
                return Err(ConfigError(format!("canal {} émis deux fois", s.channel)));
            }
        }
        if self.sources.len() > lw_proto::adv::MAX_SOURCES {
            return Err(ConfigError("plus de 240 sources".into()));
        }
        for d in &self.destinations {
            match (d.channel, d.group) {
                (Some(c), None) => {
                    Channel::new(c).ok_or_else(|| {
                        ConfigError(format!("destination : canal {c} hors 1..32766"))
                    })?;
                }
                (None, Some(g)) if g.is_multicast() => {}
                (None, Some(g)) => {
                    return Err(ConfigError(format!(
                        "destination : {g} n'est pas multicast"
                    )))
                }
                _ => {
                    return Err(ConfigError(
                        "destination : indiquer soit `channel`, soit `group`".into(),
                    ))
                }
            }
        }
        self.validate_patch()
    }

    /// Patch : canaux du périphérique existants, nombre égal aux canaux du flux, entrées non partagées.
    fn validate_patch(&self) -> Result<(), ConfigError> {
        let dev = self.device.clone().unwrap_or_default();
        for s in &self.sources {
            if let Some(chs) = &s.device_channels {
                let want = usize::from(StreamFormat::from(s.format).channels());
                if chs.len() != want {
                    return Err(ConfigError(format!(
                        "source « {} » : {} canaux du périphérique pour un flux de {want} canaux",
                        s.name,
                        chs.len()
                    )));
                }
                if let Some(c) = chs
                    .iter()
                    .find(|&&c| c == 0 || u32::from(c) > dev.channels_to_net)
                {
                    return Err(ConfigError(format!(
                        "source « {} » : sortie {c} hors 1..{} du périphérique",
                        s.name, dev.channels_to_net
                    )));
                }
            }
        }
        let mut used = std::collections::BTreeSet::new();
        for d in &self.destinations {
            if let Some(chs) = &d.device_channels {
                if chs.len() != d.stream_channels() {
                    return Err(ConfigError(format!(
                        "destination {} : {} entrées du périphérique pour un flux de {} canaux",
                        d.label(),
                        chs.len(),
                        d.stream_channels()
                    )));
                }
                for &c in chs {
                    if c == 0 || u32::from(c) > dev.channels_from_net {
                        return Err(ConfigError(format!(
                            "destination {} : entrée {c} hors 1..{} du périphérique",
                            d.label(),
                            dev.channels_from_net
                        )));
                    }
                    if !used.insert(c) {
                        return Err(ConfigError(format!(
                            "entrée {c} du périphérique patchée deux fois"
                        )));
                    }
                }
            }
        }
        Ok(())
    }

    pub fn tx_streams(&self) -> Vec<TxStream> {
        self.sources
            .iter()
            .filter_map(|s| {
                let mut t = TxStream::new(Channel::new(s.channel)?, s.format.into());
                t.payload_type = s.payload_type;
                t.tone = Tone::new(s.tone_hz, s.level_dbfs);
                Some(t)
            })
            .collect()
    }

    /// Sources à annoncer (le surround est annoncé `FAST=4`, le reste en stéréo L24).
    pub fn adv_sources(&self) -> Vec<Source> {
        self.sources
            .iter()
            .enumerate()
            .filter_map(|(i, s)| {
                let ty = if s.format == Format::Surround {
                    AdvStreamType::Surround
                } else {
                    AdvStreamType::StereoL24
                };
                Some(Source::new(
                    u16::try_from(i + 1).ok()?,
                    Channel::new(s.channel)?,
                    &s.name,
                    ty,
                ))
            })
            .collect()
    }

    /// Groupes à recevoir.
    pub fn rx_groups(&self) -> Vec<(Ipv4Addr, u16)> {
        self.destinations
            .iter()
            .filter_map(|d| match (d.channel, d.group) {
                (Some(c), _) => Some((Channel::new(c)?.group(d.kind.into()), d.port)),
                (None, Some(g)) => Some((g, d.port)),
                _ => None,
            })
            .collect()
    }
}

#[cfg(test)]
#[allow(clippy::indexing_slicing)]
mod tests {
    use super::*;

    #[test]
    fn parse_and_validate() {
        let cfg: Config = serde_json::from_str(
            r#"{"iface":"en7","sources":[{"channel":4001,"name":"MAC 1","format":"standard"},
               {"channel":5,"name":"SURR","format":"surround"}],
               "destinations":[{"channel":1},{"group":"239.192.0.9","port":5004}]}"#,
        )
        .unwrap();
        cfg.validate().unwrap();
        assert_eq!(cfg.tos, 0xB8);
        assert_eq!(cfg.tx_streams()[1].group(), Ipv4Addr::new(239, 196, 0, 5));
        assert_eq!(cfg.adv_sources()[1].stream_type, AdvStreamType::Surround);
        assert_eq!(
            cfg.rx_groups(),
            vec![
                (Ipv4Addr::new(239, 192, 0, 1), 5004),
                (Ipv4Addr::new(239, 192, 0, 9), 5004)
            ]
        );
        let bad: Config = serde_json::from_str(
            r#"{"iface":"x","sources":[{"channel":0,"name":"z","format":"aes67"}]}"#,
        )
        .unwrap();
        assert!(bad.validate().is_err());
    }
}
