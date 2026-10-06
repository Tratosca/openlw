//! Canal Livewire ↔ groupe multicast (`docs/protocol/01-channels.md`).

use std::net::Ipv4Addr;

/// Plus petit et plus grand canal utilisables (0x7FFF est réservé).
pub const CHANNEL_MIN: u16 = 1;
pub const CHANNEL_MAX: u16 = 0x7FFE;

/// Groupe de l'horloge Livewire et de l'annonce.
pub const CLOCK_GROUP: Ipv4Addr = Ipv4Addr::new(239, 192, 255, 2);
pub const ADV_GROUP: Ipv4Addr = Ipv4Addr::new(239, 192, 255, 3);
pub const GPIO_GROUP: Ipv4Addr = Ipv4Addr::new(239, 192, 255, 4);
pub const AUDIO_PORT: u16 = 5004;
pub const CLOCK_PORT: u16 = 7000;
pub const ADV_PORT: u16 = 4001;
pub const ADV_REQUEST_PORT: u16 = 4000;

/// Famille de groupe d'un flux.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum GroupKind {
    /// Stéréo Standard, AES67, Livestream : 239.192.x.x
    Stereo,
    /// Retour vers la source (« To Source ») : 239.193.x.x
    Backfeed,
    /// Surround 8 canaux : 239.196.x.x
    Surround,
}

impl GroupKind {
    const fn prefix(self) -> u32 {
        match self {
            GroupKind::Stereo => 0xEFC0_0000,
            GroupKind::Backfeed => 0xEFC1_0000,
            GroupKind::Surround => 0xEFC4_0000,
        }
    }

    fn from_prefix(prefix: u32) -> Option<Self> {
        match prefix {
            0xEFC0_0000 => Some(GroupKind::Stereo),
            0xEFC1_0000 => Some(GroupKind::Backfeed),
            0xEFC4_0000 => Some(GroupKind::Surround),
            _ => None,
        }
    }
}

/// Numéro de canal Livewire validé.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Channel(u16);

impl Channel {
    pub const fn new(n: u16) -> Option<Self> {
        if n >= CHANNEL_MIN && n <= CHANNEL_MAX {
            Some(Self(n))
        } else {
            None
        }
    }

    pub const fn get(self) -> u16 {
        self.0
    }

    /// Groupe multicast du canal pour une famille donnée.
    pub fn group(self, kind: GroupKind) -> Ipv4Addr {
        Ipv4Addr::from(kind.prefix() | u32::from(self.0))
    }

    /// Canal et famille d'un groupe, ou `None` si le groupe n'est pas un groupe audio Livewire.
    pub fn from_group(group: Ipv4Addr) -> Option<(Self, GroupKind)> {
        let v = u32::from(group);
        let kind = GroupKind::from_prefix(v & 0xFFFF_8000)?;
        let n = u16::try_from(v & 0x7FFF).ok()?;
        Some((Self::new(n)?, kind))
    }
}

/// Groupe de l'horloge à partir de son identifiant de flux (0x00FF02 → 239.192.255.2).
pub fn clock_group(stream_id: u32) -> Ipv4Addr {
    let [_, b1, b2, b3] = stream_id.to_be_bytes();
    Ipv4Addr::new(239, 0xC0 | (b1 & 0x3F), b2, b3)
}

#[cfg(test)]
#[allow(clippy::indexing_slicing)]
mod tests {
    use super::*;

    #[test]
    fn mapping() {
        let c = Channel::new(257).unwrap();
        assert_eq!(c.group(GroupKind::Stereo), Ipv4Addr::new(239, 192, 1, 1));
        assert_eq!(
            Channel::from_group(Ipv4Addr::new(239, 196, 0, 5)),
            Some((Channel::new(5).unwrap(), GroupKind::Surround))
        );
        assert_eq!(Channel::from_group(CLOCK_GROUP), None);
        assert!(Channel::new(0).is_none() && Channel::new(0x7FFF).is_none());
        assert_eq!(clock_group(0x00FF02), CLOCK_GROUP);
    }
}
