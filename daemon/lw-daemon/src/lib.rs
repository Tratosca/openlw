//! Service réseau d'OpenLW : flux RTP Livewire / AES67 émis et reçus sur l'interface choisie,
//! annonce et découverte des sources, périphérique virtuel partagé avec le plugin HAL (ADR 0005),
//! contrôle XPC et supervision (patch à chaud, choix de l'interface).

pub mod advertise;
pub mod bus;
pub mod config;
pub mod control;
pub mod detect;
pub mod device;
pub mod discovery;
pub mod editor;
pub mod iface;
pub mod labels;
pub mod log;
pub mod net;
pub mod patch;
pub mod rx;
pub mod supervisor;
pub mod tx;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Drapeau d'arrêt partagé entre les threads.
#[derive(Clone, Default)]
pub struct Stop {
    own: Arc<AtomicBool>,
    /// Drapeaux des arrêts parents : l'arrêt d'un parent arrête aussi celui-ci.
    parents: Vec<Arc<AtomicBool>>,
}

impl Stop {
    pub fn new() -> Self {
        Self::default()
    }

    /// Arrêt enfant : demandé seul, ou avec n'importe lequel de ses parents.
    pub fn child(&self) -> Self {
        let mut parents = self.parents.clone();
        parents.push(self.own.clone());
        Self {
            own: Arc::new(AtomicBool::new(false)),
            parents,
        }
    }

    pub fn request(&self) {
        self.own.store(true, Ordering::Relaxed);
    }

    pub fn requested(&self) -> bool {
        self.own.load(Ordering::Relaxed) || self.parents.iter().any(|p| p.load(Ordering::Relaxed))
    }

    /// Arrêt automatique après `d` (thread détaché).
    pub fn after(&self, d: Duration) {
        let me = self.clone();
        std::thread::spawn(move || {
            let end = Instant::now() + d;
            while !me.requested() {
                let left = end.saturating_duration_since(Instant::now());
                if left.is_zero() {
                    break;
                }
                std::thread::sleep(left.min(Duration::from_millis(20)));
            }
            me.request();
        });
    }
}

/// Générateur pseudo-aléatoire (xorshift) pour la gigue des annonces ; pas d'usage cryptographique.
pub(crate) struct Jitter(u64);

impl Jitter {
    pub(crate) fn seeded() -> Self {
        let t = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0x9E37_79B9);
        Self(t | 1)
    }

    /// Valeur uniforme dans [-1, 1].
    pub(crate) fn unit(&mut self) -> f64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 >> 11) as f64 / (1u64 << 52) as f64 - 1.0
    }

    /// `base` ± `spread`.
    pub(crate) fn around(&mut self, base: Duration, spread: Duration) -> Duration {
        let s = base.as_secs_f64() + spread.as_secs_f64() * self.unit();
        Duration::from_secs_f64(s.max(0.0))
    }
}
