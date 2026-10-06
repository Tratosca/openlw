//! Codecs Livewire / AES67, sans I/O.
//!
//! Référence : `docs/protocol/`. Chaque module cite la fiche qu'il implémente.
//! Règle de conception : les décodeurs reçoivent des octets non authentifiés venus du réseau ;
//! ils ne paniquent jamais et renvoient une [`Error`] sur toute entrée invalide ou tronquée.

pub mod adv;
pub mod channel;
pub mod envelope;
pub mod format;
pub mod lwclock;
pub mod ptp;
pub mod rtp;
pub mod sdp;
pub mod tlv;

mod bytes;

pub use bytes::Error;
