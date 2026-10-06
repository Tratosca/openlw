//! Région partagée daemon ↔ client audio : enveloppes sûres autour de `csrc/lw_shm.c`.
//!
//! La disposition et la logique (anneaux SPSC, seqlock d'horloge) n'existent qu'en C : le plugin HAL
//! et le pilote ASIO compilent le même fichier. Partage : objet `xpc_shmem` (macOS), section
//! dupliquée dans le processus client (Windows) ; sous Linux, la région reste dans le daemon, qui sert
//! lui-même les nœuds PipeWire. Ici, on garantit côté Rust un seul producteur et un seul consommateur
//! par anneau et par processus : les extrémités ([`Producer`], [`Consumer`], [`ClockWriter`]) ne sont
//! pas clonables et ne s'obtiennent qu'une fois.

use std::ffi::{c_int, c_void};
use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use crate::ffi;

/// Sens d'un anneau.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dir {
    /// Applications → réseau (producteur : plugin ; consommateur : daemon).
    ToNet = 0,
    /// Réseau → applications (producteur : daemon ; consommateur : plugin).
    FromNet = 1,
}

/// Erreur de région partagée.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error(pub &'static str);

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.0)
    }
}

impl std::error::Error for Error {}

/// Objet de partage reçu d'un serveur : `xpc_shmem` retenu (macOS) ou handle de section (Windows).
/// Libéré au `drop` s'il n'est pas mappé.
pub struct SharedObject(*mut c_void);

// SAFETY: un objet XPC retenu ou un handle Windows peut être transféré et libéré depuis n'importe
// quel thread.
unsafe impl Send for SharedObject {}

impl SharedObject {
    #[cfg_attr(not(any(target_os = "macos", windows)), allow(dead_code))]
    pub(crate) fn from_raw(p: *mut c_void) -> Option<Self> {
        (!p.is_null()).then_some(Self(p))
    }

    /// Handle de section reçu du daemon (champ `shmem.handle` de la réponse à `attach`), déjà
    /// dupliqué dans ce processus par le daemon. Le mappage vérifie la région.
    #[cfg(windows)]
    pub fn from_handle_value(value: u64) -> Option<Self> {
        Self::from_raw(usize::try_from(value).ok()? as *mut c_void)
    }
}

impl Drop for SharedObject {
    fn drop(&mut self) {
        // SAFETY: objet retenu ou handle possédé par cette valeur, libéré une seule fois.
        unsafe { ffi::lw_shm_release(self.0) };
    }
}

struct Inner {
    base: *mut c_void,
    size: usize,
    /// Objet de partage (créateur) ou objet reçu (client), libéré au drop ; NULL sous Linux.
    handle: *mut c_void,
}

// SAFETY: la mémoire n'est accédée que par les fonctions C, atomiques pour les positions ; le partage
// des données audio est sérialisé par le protocole SPSC, que l'API garantit (une extrémité par rôle).
unsafe impl Send for Inner {}
// SAFETY: idem ; les méthodes en lecture seule (`readable`, `counters`, `clock_read`) sont atomiques.
unsafe impl Sync for Inner {}

impl Drop for Inner {
    fn drop(&mut self) {
        // SAFETY: `base`/`size` proviennent d'un mappage réussi, démappé une seule fois ; `handle` est
        // possédé par cette région.
        unsafe {
            ffi::lw_shm_unmap(self.base, self.size);
            ffi::lw_shm_release(self.handle);
        }
    }
}

/// Région partagée mappée. Les extrémités s'obtiennent une fois chacune.
pub struct Region {
    inner: Arc<Inner>,
    taken: [AtomicBool; 5],
}

/// Paramètres lus dans l'en-tête.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Geometry {
    /// Fréquence d'échantillonnage (Hz).
    pub sample_rate: u32,
    /// Taille de chaque anneau en trames (puissance de 2).
    pub ring_frames: u32,
    /// Canaux de l'anneau applications → réseau.
    pub channels_to_net: u32,
    /// Canaux de l'anneau réseau → applications.
    pub channels_from_net: u32,
}

/// Compteurs d'un anneau.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Counters {
    /// Position d'écriture (trames, croissante).
    pub write_pos: u64,
    /// Position de lecture (trames, croissante).
    pub read_pos: u64,
    /// Trames refusées faute de place.
    pub overruns: u64,
    /// Trames manquantes complétées par du silence.
    pub underruns: u64,
}

impl Region {
    /// Crée une région (côté daemon) : anneaux de `ring_frames` trames (puissance de 2).
    pub fn create(
        sample_rate: u32,
        ring_frames: u32,
        channels_to_net: u32,
        channels_from_net: u32,
    ) -> Result<Self, Error> {
        // SAFETY: fonction C pure.
        let size = unsafe { ffi::lw_shm_size(ring_frames, channels_to_net, channels_from_net) };
        if size == 0 {
            return Err(Error(
                "géométrie invalide (anneau puissance de 2 entre 64 et 65536, ≤ 64 canaux)",
            ));
        }
        let mut handle: *mut c_void = std::ptr::null_mut();
        // SAFETY: `handle` est un pointeur de sortie valide ; la couche C alloue `size` octets.
        let base = unsafe { ffi::lw_shm_alloc(size, &mut handle) };
        if base.is_null() {
            return Err(Error("allocation de la région partagée impossible"));
        }
        let mut clock = ffi::HostClock::default();
        // SAFETY: pointeur de sortie valide.
        unsafe { ffi::lw_host_clock_info(&mut clock) };
        // SAFETY: `base` pointe vers `size` octets fraîchement mappés, non partagés à ce stade.
        let rc = unsafe {
            ffi::lw_shm_init(
                base,
                size,
                sample_rate,
                ring_frames,
                channels_to_net,
                channels_from_net,
                &clock,
            )
        };
        let region = Self::wrap(base, size, handle);
        if rc != 0 {
            return Err(Error("initialisation de la région impossible"));
        }
        Ok(region)
    }

    /// Mappe une région reçue du daemon (côté client, ou test) et la valide.
    pub fn map(object: SharedObject) -> Result<Self, Error> {
        let mut size = 0usize;
        // SAFETY: `object.0` est un objet de partage possédé ; `size` est un pointeur de sortie valide.
        let base = unsafe { ffi::lw_shm_map(object.0, &mut size) };
        if base.is_null() {
            return Err(Error("mappage de la région impossible"));
        }
        let obj = object.0;
        std::mem::forget(object); // la propriété de l'objet passe à `Inner`
        let region = Self::wrap(base, size, obj);
        // SAFETY: `base` pointe vers `size` octets mappés.
        if unsafe { ffi::lw_shm_validate(base, size) } != 0 {
            return Err(Error("région invalide (magie, version ou taille)"));
        }
        Ok(region)
    }

    fn wrap(base: *mut c_void, size: usize, handle: *mut c_void) -> Self {
        Self {
            inner: Arc::new(Inner { base, size, handle }),
            taken: Default::default(),
        }
    }

    #[cfg(target_os = "macos")]
    pub(crate) fn handle(&self) -> *mut c_void {
        self.inner.handle
    }

    /// Duplique la section dans le processus `pid` ; renvoie la valeur du handle dans ce processus.
    #[cfg(windows)]
    pub fn share_with(&self, pid: u32) -> Option<u64> {
        // SAFETY: `handle` est la section de cette région, valide tant qu'elle vit.
        let h = unsafe { ffi::lw_shm_share_with(self.inner.handle, pid) };
        (h != 0).then_some(h)
    }

    /// Horloge hôte déclarée dans l'en-tête : (identifiant `lw_host_clock_id`, numérateur,
    /// dénominateur de la conversion en ns).
    pub fn host_clock(&self) -> (u32, u64, u64) {
        let mut c = ffi::HostClock::default();
        // SAFETY: région valide ; pointeur de sortie valide.
        unsafe { ffi::lw_shm_host_clock(self.inner.base, &mut c) };
        (c.id, c.ns_numer, c.ns_denom)
    }

    /// Taille de la région en octets.
    pub fn size(&self) -> usize {
        self.inner.size
    }

    /// Géométrie lue dans l'en-tête.
    pub fn geometry(&self) -> Geometry {
        // Lecture des champs immuables après initialisation (écrits avant la magie, publiée en release).
        let base = self.inner.base.cast::<u32>();
        // SAFETY: l'en-tête fait 4096 octets ; offsets fixés par lw_shm_header : sample_rate @12,
        // ring_frames @16, channels @20 et @24 (u32 alignés).
        unsafe {
            Geometry {
                sample_rate: base.add(3).read_volatile(),
                ring_frames: base.add(4).read_volatile(),
                channels_to_net: base.add(5).read_volatile(),
                channels_from_net: base.add(6).read_volatile(),
            }
        }
    }

    /// Nombre de canaux de l'anneau `dir`.
    pub fn channels(&self, dir: Dir) -> u32 {
        let g = self.geometry();
        match dir {
            Dir::ToNet => g.channels_to_net,
            Dir::FromNet => g.channels_from_net,
        }
    }

    /// Trames lisibles dans l'anneau `dir`.
    pub fn readable(&self, dir: Dir) -> u32 {
        // SAFETY: région valide ; lecture atomique.
        unsafe { ffi::lw_ring_readable(self.inner.base, dir as c_int) }
    }

    /// Place libre (trames) dans l'anneau `dir`.
    pub fn writable(&self, dir: Dir) -> u32 {
        // SAFETY: région valide ; lecture atomique.
        unsafe { ffi::lw_ring_writable(self.inner.base, dir as c_int) }
    }

    /// Positions et compteurs de l'anneau `dir`.
    pub fn counters(&self, dir: Dir) -> Counters {
        let mut c = Counters::default();
        // SAFETY: région valide ; pointeurs de sortie valides.
        unsafe {
            ffi::lw_ring_counters(
                self.inner.base,
                dir as c_int,
                &mut c.write_pos,
                &mut c.read_pos,
                &mut c.overruns,
                &mut c.underruns,
            );
        }
        c
    }

    /// Lecture cohérente de l'horloge publiée : (temps hôte brut, position d'échantillon, rapport).
    pub fn clock(&self) -> Option<(u64, u64, f64)> {
        let (mut h, mut s, mut r) = (0u64, 0u64, 0f64);
        // SAFETY: région valide ; pointeurs de sortie valides.
        let rc = unsafe { ffi::lw_clock_read(self.inner.base, &mut h, &mut s, &mut r) };
        (rc == 0).then_some((h, s, r))
    }

    fn take(&self, slot: usize) -> Result<(), Error> {
        let flag = self.taken.get(slot).ok_or(Error("extrémité inconnue"))?;
        if flag.swap(true, Ordering::AcqRel) {
            Err(Error("extrémité déjà attribuée"))
        } else {
            Ok(())
        }
    }

    /// Producteur de l'anneau `dir` (une seule fois par région).
    pub fn producer(&self, dir: Dir) -> Result<Producer, Error> {
        self.take(dir as usize * 2)?;
        Ok(Producer {
            inner: self.inner.clone(),
            dir,
            channels: self.channels(dir),
        })
    }

    /// Consommateur de l'anneau `dir` (une seule fois par région).
    pub fn consumer(&self, dir: Dir) -> Result<Consumer, Error> {
        self.take(dir as usize * 2 + 1)?;
        Ok(Consumer {
            inner: self.inner.clone(),
            dir,
            channels: self.channels(dir),
        })
    }

    /// Écrivain de l'horloge (daemon, une seule fois).
    pub fn clock_writer(&self) -> Result<ClockWriter, Error> {
        self.take(4)?;
        Ok(ClockWriter {
            inner: self.inner.clone(),
        })
    }
}

fn frames_of(len: usize, channels: u32) -> Result<u32, Error> {
    if channels == 0 {
        return Ok(0);
    }
    let ch = channels as usize;
    if len % ch != 0 {
        return Err(Error("longueur non multiple du nombre de canaux"));
    }
    u32::try_from(len / ch).map_err(|_| Error("trop de trames"))
}

/// Producteur d'un anneau.
pub struct Producer {
    inner: Arc<Inner>,
    dir: Dir,
    channels: u32,
}

impl Producer {
    /// Nombre de canaux par trame.
    pub fn channels(&self) -> u32 {
        self.channels
    }

    /// Écrit des trames entrelacées ; renvoie le nombre de trames acceptées (le reste compte en overruns).
    pub fn write(&mut self, interleaved: &[f32]) -> Result<u32, Error> {
        let frames = frames_of(interleaved.len(), self.channels)?;
        // SAFETY: producteur unique de cet anneau (garanti par `Region::producer`) ; `interleaved`
        // contient `frames × channels` échantillons.
        Ok(unsafe {
            ffi::lw_ring_write(
                self.inner.base,
                self.dir as c_int,
                interleaved.as_ptr(),
                frames,
            )
        })
    }
}

/// Consommateur d'un anneau.
pub struct Consumer {
    inner: Arc<Inner>,
    dir: Dir,
    channels: u32,
}

impl Consumer {
    /// Nombre de canaux par trame.
    pub fn channels(&self) -> u32 {
        self.channels
    }

    /// Trames lisibles.
    pub fn readable(&self) -> u32 {
        // SAFETY: région valide ; lecture atomique.
        unsafe { ffi::lw_ring_readable(self.inner.base, self.dir as c_int) }
    }

    /// Remplit `out` (trames entrelacées) ; complète par du silence s'il manque des trames.
    /// Renvoie le nombre de trames réellement lues.
    pub fn read(&mut self, out: &mut [f32]) -> Result<u32, Error> {
        let frames = frames_of(out.len(), self.channels)?;
        // SAFETY: consommateur unique de cet anneau ; `out` peut recevoir `frames × channels` échantillons.
        Ok(unsafe {
            ffi::lw_ring_read(self.inner.base, self.dir as c_int, out.as_mut_ptr(), frames)
        })
    }
}

/// Écrivain de l'horloge partagée.
pub struct ClockWriter {
    inner: Arc<Inner>,
}

impl ClockWriter {
    /// Publie : à l'instant hôte `host_time` (brut, [`crate::rt::host_time`]), la position vaut `sample_time`.
    pub fn publish(&mut self, host_time: u64, sample_time: u64, rate_scalar: f64) {
        // SAFETY: écrivain unique (garanti par `Region::clock_writer`).
        unsafe { ffi::lw_clock_publish(self.inner.base, host_time, sample_time, rate_scalar) };
    }
}

#[cfg(test)]
#[allow(clippy::indexing_slicing)]
mod tests {
    use super::*;

    #[test]
    fn geometry_and_invalid_parameters() {
        let r = Region::create(48_000, 1024, 8, 2).unwrap();
        assert_eq!(
            r.geometry(),
            Geometry {
                sample_rate: 48_000,
                ring_frames: 1024,
                channels_to_net: 8,
                channels_from_net: 2
            }
        );
        assert_eq!(r.size(), 4096 + 1024 * 10 * 4);
        assert!(
            Region::create(48_000, 1000, 2, 2).is_err(),
            "anneau non puissance de 2"
        );
        assert!(
            Region::create(48_000, 1024, 65, 2).is_err(),
            "trop de canaux"
        );
    }

    #[test]
    fn ends_are_unique() {
        let r = Region::create(48_000, 256, 2, 2).unwrap();
        let _p = r.producer(Dir::ToNet).unwrap();
        assert!(r.producer(Dir::ToNet).is_err());
        let _c = r.consumer(Dir::ToNet).unwrap();
        let _w = r.clock_writer().unwrap();
        assert!(r.clock_writer().is_err());
    }

    #[test]
    fn ring_wraps_and_counts() {
        let r = Region::create(48_000, 64, 2, 2).unwrap();
        let (mut p, mut c) = (
            r.producer(Dir::FromNet).unwrap(),
            r.consumer(Dir::FromNet).unwrap(),
        );
        let mut out = vec![0f32; 2 * 40];
        let mut next = 0f32;
        for _ in 0..10 {
            let block: Vec<f32> = (0..80).map(|i| next + i as f32).collect();
            assert_eq!(p.write(&block).unwrap(), 40);
            assert_eq!(c.read(&mut out).unwrap(), 40);
            assert_eq!(out, block, "contenu identique après rebouclage");
            next += 80.0;
        }
        // Débordement : 64 trames de place, 100 demandées.
        let big = vec![1f32; 2 * 100];
        assert_eq!(p.write(&big).unwrap(), 64);
        assert_eq!(r.counters(Dir::FromNet).overruns, 36);
        // Sous-alimentation : 64 lisibles, 70 demandées → 6 trames de silence.
        let mut out = vec![9f32; 2 * 70];
        assert_eq!(c.read(&mut out).unwrap(), 64);
        assert_eq!(r.counters(Dir::FromNet).underruns, 6);
        assert!(out[128..].iter().all(|&s| s == 0.0));
        assert!(p.write(&[1.0]).is_err(), "longueur non multiple des canaux");
    }

    #[test]
    fn host_clock_is_declared() {
        let r = Region::create(48_000, 256, 2, 2).unwrap();
        let (id, numer, denom) = r.host_clock();
        let expected = if cfg!(target_os = "macos") {
            1
        } else if cfg!(windows) {
            2
        } else {
            3
        };
        assert_eq!(id, expected);
        assert!(numer > 0 && denom > 0);
    }

    /// Le daemon crée, le client mappe l'objet de partage : deux adresses, même mémoire.
    #[cfg(target_os = "macos")]
    fn second_mapping(daemon: &Region) -> Region {
        let server =
            crate::xpc::Server::start(None, Box::new(|_, _| "{\"ok\":true}".into())).unwrap();
        server.set_shmem(daemon);
        let client = crate::xpc::Client::from_endpoint(&server.endpoint()).unwrap();
        let (_, obj) = client.call_with_shmem("{}").unwrap();
        Region::map(obj.expect("objet xpc_shmem reçu")).unwrap()
    }

    #[cfg(windows)]
    fn second_mapping(daemon: &Region) -> Region {
        let h = daemon.share_with(std::process::id()).expect("section dupliquée");
        Region::map(SharedObject::from_handle_value(h).unwrap()).unwrap()
    }

    #[cfg(any(target_os = "macos", windows))]
    #[test]
    fn spsc_across_two_mappings() {
        let daemon = Region::create(48_000, 1024, 2, 2).unwrap();
        let plugin = second_mapping(&daemon);
        assert_eq!(plugin.geometry(), daemon.geometry());
        assert_eq!(plugin.host_clock(), daemon.host_clock());

        let mut producer = plugin.producer(Dir::ToNet).unwrap();
        let mut consumer = daemon.consumer(Dir::ToNet).unwrap();
        const TOTAL: u32 = 200_000;
        let writer = std::thread::spawn(move || {
            let mut n = 0u32;
            while n < TOTAL {
                let k = 37.min(TOTAL - n);
                let block: Vec<f32> = (n..n + k).flat_map(|i| [i as f32, -(i as f32)]).collect();
                let w = producer.write(&block).unwrap();
                n += w;
                if w < k {
                    std::thread::yield_now();
                    // Les trames refusées sont comptées en overrun : on renvoie la suite, pas le reste.
                    let lost = k - w;
                    n += lost;
                }
            }
        });
        let mut got = 0u64;
        let mut expect = 0f32;
        let mut buf = vec![0f32; 2 * 29];
        let mut idle = 0;
        while idle < 200_000 {
            let avail = consumer.readable().min(29);
            if avail == 0 {
                idle += 1;
                std::thread::yield_now();
                continue;
            }
            idle = 0;
            let slice = &mut buf[..2 * avail as usize];
            assert_eq!(consumer.read(slice).unwrap(), avail);
            for f in slice.chunks_exact(2) {
                assert!(f[0] >= expect, "ordre conservé");
                assert_eq!(f[1], -f[0], "trame intacte (pas de déchirure)");
                expect = f[0] + 1.0;
                got += 1;
            }
        }
        writer.join().unwrap();
        let c = daemon.counters(Dir::ToNet);
        assert_eq!(
            got + c.overruns,
            u64::from(TOTAL),
            "chaque trame est soit lue, soit comptée perdue"
        );
    }

    #[test]
    fn clock_seqlock_is_consistent() {
        let r = Region::create(48_000, 256, 2, 2).unwrap();
        assert!(r.clock().is_none(), "aucune horloge avant publication");
        let mut w = r.clock_writer().unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let s2 = stop.clone();
        let writer = std::thread::spawn(move || {
            let mut i = 1u64;
            while !s2.load(Ordering::Relaxed) {
                w.publish(i, i * 2, i as f64 * 0.5);
                i += 1;
            }
        });
        let mut reads = 0;
        while reads < 200_000 {
            if let Some((h, s, rate)) = r.clock() {
                assert_eq!(s, h * 2, "triplet cohérent");
                assert_eq!(rate, h as f64 * 0.5);
                reads += 1;
            }
        }
        stop.store(true, Ordering::Relaxed);
        writer.join().unwrap();
    }
}
