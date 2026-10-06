//! Livewire / AES67 codecs, no I/O.
//!
//! Reference: `docs/protocol/`. Each module cites the document it implements.
//! Design rule: decoders receive unauthenticated network bytes;
//! they never panic and return [`Error`] for invalid or truncated inputs.

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
