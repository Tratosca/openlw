//! Network interfaces: system name, friendly name, index, IPv4 address.
//!
//! System name: `en7` (macOS), `enp3s0` (Linux), connection name (“Ethernet 2”, Windows).

use std::collections::BTreeMap;
use std::net::Ipv4Addr;
use std::num::NonZeroU32;
#[cfg(target_os = "macos")]
use std::process::Command;

use serde::Serialize;

/// Interface usable for Livewire (configured IPv4).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Iface {
    /// System name, e.g. `en7`, `enp3s0`, “Ethernet 2”.
    pub name: String,
    /// Friendly name, e.g. “Thunderbolt Ethernet” (macOS: `networksetup`); system name elsewhere.
    pub friendly: String,
    pub index: u32,
    pub ipv4: Ipv4Addr,
    pub loopback: bool,
}

impl Iface {
    pub fn index_nz(&self) -> Option<NonZeroU32> {
        NonZeroU32::new(self.index)
    }
}

/// Interfaces with an IPv4 address, sorted by name.
pub fn list() -> std::io::Result<Vec<Iface>> {
    let friendly = friendly_names();
    let mut out: Vec<Iface> = if_addrs::get_if_addrs()?
        .into_iter()
        .filter_map(|i| match i.addr {
            if_addrs::IfAddr::V4(ref v4) => Some(Iface {
                friendly: friendly
                    .get(&i.name)
                    .cloned()
                    .unwrap_or_else(|| i.name.clone()),
                index: i.index.unwrap_or(0),
                ipv4: v4.ip,
                loopback: v4.ip.is_loopback(),
                name: i.name,
            }),
            _ => None,
        })
        .collect();
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out.dedup_by(|a, b| a.name == b.name);
    Ok(out)
}

/// Resolve an interface by system or friendly name.
pub fn find(name: &str) -> std::io::Result<Iface> {
    list()?
        .into_iter()
        .find(|i| i.name == name || i.friendly == name)
        .ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("interface {name} not found or has no IPv4 address"),
            )
        })
}

/// System → friendly name mapping (macOS: `networksetup -listallhardwareports`).
/// Linux has none; on Windows, the system name is already the connection name.
#[cfg(not(target_os = "macos"))]
fn friendly_names() -> BTreeMap<String, String> {
    BTreeMap::new()
}

/// System → friendly name mapping (macOS: `networksetup -listallhardwareports`).
#[cfg(target_os = "macos")]
fn friendly_names() -> BTreeMap<String, String> {
    let mut map = BTreeMap::new();
    let Ok(out) = Command::new("/usr/sbin/networksetup")
        .arg("-listallhardwareports")
        .output()
    else {
        return map;
    };
    let text = String::from_utf8_lossy(&out.stdout);
    let mut port: Option<String> = None;
    for line in text.lines() {
        if let Some(p) = line.strip_prefix("Hardware Port: ") {
            port = Some(p.trim().to_string());
        } else if let Some(dev) = line.strip_prefix("Device: ") {
            if let Some(p) = port.take() {
                map.insert(dev.trim().to_string(), p);
            }
        }
    }
    map
}

#[cfg(test)]
#[allow(clippy::indexing_slicing)]
mod tests {
    use super::*;

    #[test]
    fn loopback_is_listed() {
        let ifs = list().unwrap();
        assert!(ifs
            .iter()
            .any(|i| i.loopback && i.ipv4 == Ipv4Addr::LOCALHOST));
        let lo = ifs.iter().find(|i| i.loopback).unwrap();
        assert_eq!(find(&lo.name).unwrap().ipv4, Ipv4Addr::LOCALHOST);
        assert!(find("no-such-interface").is_err());
    }
}
