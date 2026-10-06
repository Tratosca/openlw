//! Côté daemon du périphérique virtuel : région partagée avec le plugin HAL (ADR 0005).
//!
//! Un thread temps réel cadencé à 1 ms :
//! - publie l'horloge dans la région (horloge hôte, rapport 1,0 tant qu'aucune horloge réseau n'asservit) ;
//! - consomme l'anneau applications → réseau et mesure les crêtes par canal ;
//! - en mode `loopback` (test), recopie cet audio dans l'anneau réseau → applications.
//!
//! Le routage vers les flux RTP (grille de patch) viendra ensuite.

use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use lw_sys::shm::{Dir, Region};
use serde::{Deserialize, Serialize};

use crate::bus::{BusCounters, BusWriter, JitterReader};
use crate::Stop;

/// Paramètres du périphérique virtuel.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DeviceConfig {
    /// Canaux de sortie des applications (vers le réseau).
    #[serde(default = "default_channels")]
    pub channels_to_net: u32,
    /// Canaux d'entrée des applications (depuis le réseau).
    #[serde(default = "default_channels")]
    pub channels_from_net: u32,
    /// Taille de chaque anneau en trames (puissance de 2) ; 8192 = 170 ms à 48 kHz.
    #[serde(default = "default_ring")]
    pub ring_frames: u32,
    /// Recopie la sortie des applications vers leur entrée (boucle interne, mode de test).
    /// Mode de test : prioritaire sur les entrées patchées.
    #[serde(default)]
    pub loopback: bool,
}

fn default_channels() -> u32 {
    2
}
fn default_ring() -> u32 {
    8192
}

impl Default for DeviceConfig {
    fn default() -> Self {
        Self {
            channels_to_net: 2,
            channels_from_net: 2,
            ring_frames: 8192,
            loopback: false,
        }
    }
}

/// État du périphérique, exposé par XPC.
#[derive(Debug, Clone, Default, Serialize)]
pub struct DeviceStatus {
    /// Numéro de la région partagée : change à chaque recréation (le plugin doit se rattacher).
    pub generation: u64,
    pub channels_to_net: u32,
    pub channels_from_net: u32,
    pub ring_frames: u32,
    pub loopback: bool,
    /// Position d'échantillon publiée dans l'horloge partagée.
    pub clock_sample_time: u64,
    pub to_net_frames: u64,
    pub to_net_overruns: u64,
    pub from_net_frames: u64,
    pub from_net_overruns: u64,
    pub from_net_underruns: u64,
    /// Crête par canal de l'audio des applications sur les 100 dernières ms (dBFS).
    pub to_net_peak_dbfs: Vec<f64>,
    /// Crête par entrée du périphérique (audio venu du réseau) sur les 100 dernières ms (dBFS).
    pub from_net_peak_dbfs: Vec<f64>,
    /// Routes actives : canaux du périphérique (1-based) et compteurs de bus.
    pub outputs: Vec<RouteStatus>,
    pub inputs: Vec<RouteStatus>,
}

/// État d'une route.
#[derive(Debug, Clone, Default, Serialize)]
pub struct RouteStatus {
    pub label: String,
    /// Canaux du périphérique, numérotés à partir de 1 (comme dans la configuration).
    pub device_channels: Vec<usize>,
    /// Entrées : tampon de gigue amorcé (audio en cours).
    pub primed: bool,
    pub bus: BusCounters,
}

/// Sorties du périphérique vers un flux émis.
pub struct OutRoute {
    pub label: String,
    /// Canaux du périphérique (0-based), dans l'ordre des canaux du flux.
    pub device_channels: Vec<usize>,
    pub writer: BusWriter,
}

/// Flux reçu vers des entrées du périphérique.
pub struct InRoute {
    pub label: String,
    pub device_channels: Vec<usize>,
    pub reader: JitterReader,
}

/// Table de routes, remplacée à chaud.
#[derive(Default)]
pub struct Routes {
    pub outputs: Vec<OutRoute>,
    pub inputs: Vec<InRoute>,
}

/// Périphérique en fonctionnement.
pub struct Device {
    pub region: Arc<Region>,
    pub status: Arc<Mutex<DeviceStatus>>,
    pub thread: std::thread::JoinHandle<()>,
    pending: Arc<Mutex<Option<Routes>>>,
}

const SAMPLE_RATE: u32 = 48_000;
/// Retard de lecture de l'anneau de sortie rattrapable d'un coup (trames) : absorbe le cadencement
/// de l'hôte sans transmettre ses blocs en rafale aux flux émis.
const OUT_CARRY_MAX: usize = 512;
const TICK: Duration = Duration::from_millis(1);

/// Crée la région et lance le thread du périphérique.
pub fn start(cfg: &DeviceConfig, stop: &Stop) -> Result<Device, lw_sys::shm::Error> {
    let region = Arc::new(Region::create(
        SAMPLE_RATE,
        cfg.ring_frames,
        cfg.channels_to_net,
        cfg.channels_from_net,
    )?);
    let mut clock = region.clock_writer()?;
    let mut from_apps = region.consumer(Dir::ToNet)?;
    let mut to_apps = region.producer(Dir::FromNet)?;
    let status = Arc::new(Mutex::new(DeviceStatus {
        channels_to_net: cfg.channels_to_net,
        channels_from_net: cfg.channels_from_net,
        ring_frames: cfg.ring_frames,
        loopback: cfg.loopback,
        to_net_peak_dbfs: vec![f64::NEG_INFINITY; cfg.channels_to_net as usize],
        ..DeviceStatus::default()
    }));
    let pending: Arc<Mutex<Option<Routes>>> = Arc::new(Mutex::new(None));
    let (stop, st, reg, loopback, slot) = (
        stop.clone(),
        status.clone(),
        region.clone(),
        cfg.loopback,
        pending.clone(),
    );
    let thread = std::thread::Builder::new()
        .name("lw-device".into())
        .spawn(move || {
            if let Err(kr) = lw_sys::rt::promote_for_packet_interval(TICK) {
                crate::error!("périphérique : temps réel refusé (kern_return {kr})");
            }
            let (ch_in, ch_out) = (from_apps.channels() as usize, to_apps.channels() as usize);
            let ring = reg.geometry().ring_frames as usize;
            let t0_host = lw_sys::rt::host_time();
            let t0_ns = lw_sys::rt::host_time_ns();
            // Tampons préalloués : aucune allocation dans la boucle.
            let mut buf = vec![0f32; ch_in * ring];
            let mut out = vec![0f32; ch_out * ring];
            let mut scratch = vec![0f32; 8 * ring];
            let mut peaks = vec![0f32; ch_in];
            let mut in_peaks = vec![0f32; ch_out];
            let mut routes = Routes::default();
            let mut produced: u64 = 0;
            let mut out_clock: u64 = 0;
            let mut owed: usize = 0;
            let start = Instant::now();
            let mut k: u32 = 0;
            clock.publish(t0_host, 0, 1.0);
            while !stop.requested() {
                k = k.wrapping_add(1);
                let deadline = start + TICK * k;
                loop {
                    let left = deadline.saturating_duration_since(Instant::now());
                    if left.is_zero() {
                        break;
                    }
                    lw_sys::rt::sleep(left);
                }
                // Nouvelle table de routes (sans bloquer : un essai par tick).
                if let Ok(mut s) = slot.try_lock() {
                    if let Some(r) = s.take() {
                        routes = r;
                    }
                }
                // Horloge hôte libre : position = temps écoulé × 48 kHz.
                let now_host = lw_sys::rt::host_time();
                let elapsed_ns = lw_sys::rt::host_time_ns().saturating_sub(t0_ns);
                let sample = elapsed_ns * u64::from(SAMPLE_RATE) / 1_000_000_000;
                if k % 10 == 0 {
                    clock.publish(now_host, sample, 1.0);
                }
                // 1. Audio des applications : crêtes, sorties patchées, boucle interne.
                //    Lu au rythme de l'horloge (trames dues, retard reporté jusqu'à OUT_CARRY_MAX) :
                //    l'hôte écrit par blocs (jusqu'à 4096 trames et plus), que l'anneau partagé lisse ;
                //    les transmettre d'un coup ferait déborder puis vider les tampons d'émission.
                owed = (owed + sample.saturating_sub(out_clock) as usize).min(OUT_CARRY_MAX);
                out_clock = sample;
                let n = owed.min(from_apps.readable() as usize).min(ring);
                owed -= n;
                if let (true, Some(block)) = (n > 0 && ch_in > 0, buf.get_mut(..n * ch_in)) {
                    let _ = from_apps.read(block);
                    for frame in block.chunks_exact(ch_in) {
                        for (p, s) in peaks.iter_mut().zip(frame) {
                            *p = p.max(s.abs());
                        }
                    }
                    for route in &mut routes.outputs {
                        let sc = route.device_channels.len();
                        if let Some(dst) = scratch.get_mut(..n * sc) {
                            for (frame, o) in
                                block.chunks_exact(ch_in).zip(dst.chunks_exact_mut(sc))
                            {
                                for (v, &c) in o.iter_mut().zip(&route.device_channels) {
                                    *v = frame.get(c).copied().unwrap_or(0.0);
                                }
                            }
                            route.writer.push(dst);
                        }
                    }
                    if loopback && ch_out > 0 {
                        if let Some(dst) = out.get_mut(..n * ch_out) {
                            for (frame, o) in
                                block.chunks_exact(ch_in).zip(dst.chunks_exact_mut(ch_out))
                            {
                                for (c, v) in o.iter_mut().enumerate() {
                                    *v = frame.get(c).copied().unwrap_or(0.0);
                                }
                            }
                            let _ = to_apps.write(dst);
                        }
                    }
                }
                // 2. Entrées des applications : trames dues selon l'horloge. L'anneau est rempli tant
                //    qu'il a de la place ; la latence est bornée par le plugin, qui connaît la taille
                //    de bloc de l'hôte (il jette l'excédent avant de lire).
                let due = sample.saturating_sub(produced) as usize;
                produced = sample;
                if !loopback && !routes.inputs.is_empty() && ch_out > 0 && due > 0 {
                    let frames = due.min(ring);
                    let free = reg.writable(Dir::FromNet) as usize;
                    if free >= frames {
                        if let Some(dst) = out.get_mut(..frames * ch_out) {
                            dst.iter_mut().for_each(|v| *v = 0.0);
                            for route in &mut routes.inputs {
                                let sc = route.reader.channels();
                                if let Some(src) = scratch.get_mut(..frames * sc) {
                                    route.reader.pull(src);
                                    for (i, o) in
                                        src.chunks_exact(sc).zip(dst.chunks_exact_mut(ch_out))
                                    {
                                        for (v, &c) in i.iter().zip(&route.device_channels) {
                                            if let Some(slot) = o.get_mut(c) {
                                                *slot = *v;
                                            }
                                        }
                                    }
                                }
                            }
                            for frame in dst.chunks_exact(ch_out) {
                                for (p, s) in in_peaks.iter_mut().zip(frame) {
                                    *p = p.max(s.abs());
                                }
                            }
                            let _ = to_apps.write(dst);
                        }
                    } else {
                        // Anneau plein : personne ne lit (IO du plugin arrêtée). On consomme quand même
                        // les tampons de gigue pour ne pas accumuler de retard. Les crêtes restent
                        // mesurées : l'app affiche l'audio reçu avant tout enregistrement.
                        for route in &mut routes.inputs {
                            let sc = route.reader.channels();
                            if let Some(src) = scratch.get_mut(..frames * sc) {
                                route.reader.pull(src);
                                for i in src.chunks_exact(sc) {
                                    for (v, &c) in i.iter().zip(&route.device_channels) {
                                        if let Some(p) = in_peaks.get_mut(c) {
                                            *p = p.max(v.abs());
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
                if k % 100 == 0 {
                    let mut s = st.lock().unwrap_or_else(PoisonError::into_inner);
                    s.clock_sample_time = sample;
                    let a = reg.counters(Dir::ToNet);
                    let b = reg.counters(Dir::FromNet);
                    s.to_net_frames = a.write_pos;
                    s.to_net_overruns = a.overruns;
                    s.from_net_frames = b.write_pos;
                    s.from_net_overruns = b.overruns;
                    s.from_net_underruns = b.underruns;
                    s.to_net_peak_dbfs = peaks
                        .iter()
                        .map(|&p| {
                            if p > 0.0 {
                                20.0 * f64::from(p).log10()
                            } else {
                                f64::NEG_INFINITY
                            }
                        })
                        .collect();
                    peaks.iter_mut().for_each(|p| *p = 0.0);
                    s.from_net_peak_dbfs = in_peaks
                        .iter()
                        .map(|&p| {
                            if p > 0.0 {
                                20.0 * f64::from(p).log10()
                            } else {
                                f64::NEG_INFINITY
                            }
                        })
                        .collect();
                    in_peaks.iter_mut().for_each(|p| *p = 0.0);
                    // Rafraîchissement de l'état des routes (vecteurs réutilisés : pas d'allocation
                    // tant que le nombre de routes ne change pas).
                    s.outputs.truncate(routes.outputs.len());
                    s.inputs.truncate(routes.inputs.len());
                    for (i, r) in routes.outputs.iter().enumerate() {
                        let st = RouteStatus {
                            label: String::new(),
                            device_channels: Vec::new(),
                            primed: true,
                            bus: r.writer.counters(),
                        };
                        match s.outputs.get_mut(i) {
                            Some(e) => e.bus = st.bus,
                            None => s.outputs.push(RouteStatus {
                                label: r.label.clone(),
                                device_channels: r.device_channels.iter().map(|c| c + 1).collect(),
                                ..st
                            }),
                        }
                    }
                    for (i, r) in routes.inputs.iter().enumerate() {
                        match s.inputs.get_mut(i) {
                            Some(e) => {
                                e.bus = r.reader.counters();
                                e.primed = r.reader.primed();
                            }
                            None => s.inputs.push(RouteStatus {
                                label: r.label.clone(),
                                device_channels: r.device_channels.iter().map(|c| c + 1).collect(),
                                primed: r.reader.primed(),
                                bus: r.reader.counters(),
                            }),
                        }
                    }
                }
            }
        });
    let thread =
        thread.map_err(|_| lw_sys::shm::Error("création du thread du périphérique impossible"))?;
    Ok(Device {
        region,
        status,
        thread,
        pending,
    })
}

impl Device {
    /// Remplace la table de routes ; prise en compte au tick suivant du thread du périphérique.
    pub fn set_routes(&self, routes: Routes) {
        let mut st = self.status.lock().unwrap_or_else(PoisonError::into_inner);
        st.outputs.clear();
        st.inputs.clear();
        drop(st);
        *self.pending.lock().unwrap_or_else(PoisonError::into_inner) = Some(routes);
    }

    /// Accès à l'emplacement de routes, pour le transmettre à un superviseur.
    pub fn routes_handle(&self) -> RoutesHandle {
        RoutesHandle {
            pending: self.pending.clone(),
            status: self.status.clone(),
        }
    }

    pub fn snapshot(&self) -> DeviceStatus {
        self.status
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

/// Poignée clonable pour remplacer les routes d'un périphérique en marche.
#[derive(Clone)]
pub struct RoutesHandle {
    pending: Arc<Mutex<Option<Routes>>>,
    status: Arc<Mutex<DeviceStatus>>,
}

impl RoutesHandle {
    pub fn set(&self, routes: Routes) {
        let mut st = self.status.lock().unwrap_or_else(PoisonError::into_inner);
        st.outputs.clear();
        st.inputs.clear();
        drop(st);
        *self.pending.lock().unwrap_or_else(PoisonError::into_inner) = Some(routes);
    }
}
