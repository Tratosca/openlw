//! Supervision du daemon : périphérique virtuel, choix de l'interface et sessions réseau.
//!
//! [`run`] est une boucle de réconciliation :
//! 1. le périphérique (région partagée avec le plugin) est recréé quand son nombre de canaux change ;
//!    son numéro de génération augmente et le plugin se rattache (commande `geometry`) ;
//! 2. l'interface cible est relue toutes les 2 s : celle de la configuration, ou en mode `auto` celle
//!    qui entend des annonces Livewire ([`crate::detect`]) ;
//! 3. la session réseau (émission, réception, annonce, découverte) est relancée quand l'interface
//!    change ; un changement de configuration est appliqué à chaud, flux par flux ([`Session::apply`]) :
//!    patcher une entrée ne coupe pas les flux émis. Sans cible, aucune session : l'état indique la
//!    recherche.

use std::io;
use std::path::PathBuf;
use std::sync::mpsc::RecvTimeoutError;
use std::sync::PoisonError;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::advertise::Advertiser;
use crate::config::Config;
use crate::control::Shared;
use crate::device::{self, DeviceConfig, Routes, RoutesHandle};
use crate::iface::{self, Iface};
use crate::net::TxOptions;
use crate::patch;
use crate::{bus, discovery, error, info, rx, tx, Stop};

/// Thread d'une session (flux émis ou reçu, annonce, découverte), arrêtable seul.
struct Worker {
    name: String,
    stop: Stop,
    handle: JoinHandle<io::Result<()>>,
}

impl Worker {
    fn spawn(
        name: String,
        parent: &Stop,
        f: impl FnOnce(Stop) -> io::Result<()> + Send + 'static,
    ) -> io::Result<Self> {
        let stop = parent.child();
        let s = stop.clone();
        let handle = std::thread::Builder::new()
            .name(name.clone())
            .spawn(move || f(s))?;
        Ok(Self { name, stop, handle })
    }

    fn stop(self) {
        self.stop.request();
        match self.handle.join() {
            Ok(Ok(())) => {}
            Ok(Err(e)) => error!("{} : {e}", self.name),
            Err(_) => error!("{} : thread interrompu", self.name),
        }
    }
}

/// Paramètres qui imposent de relancer un flux émis. Le choix des sorties du périphérique n'en fait
/// pas partie : il ne change que la table de routes.
#[derive(Debug, Clone, PartialEq)]
struct TxKey {
    channel: u16,
    format: crate::config::Format,
    payload_type: u8,
    tone: (u64, u64),
    tos: u32,
    cushion: usize,
    with_source: bool,
}

/// Paramètres qui imposent de relancer un flux reçu.
#[derive(Debug, Clone, PartialEq)]
struct RxKey {
    group: std::net::Ipv4Addr,
    port: u16,
    channels: usize,
    target: usize,
    with_sink: bool,
}

struct TxEntry {
    key: TxKey,
    group: String,
    port: Option<bus::Port>,
    worker: Worker,
}

struct RxEntry {
    key: RxKey,
    group: String,
    port: Option<bus::Port>,
    worker: Worker,
}

/// Session en cours sur une interface : appliquée à chaud, flux par flux.
pub struct Session {
    stop: Stop,
    pub iface: Iface,
    tx: Vec<TxEntry>,
    rx: Vec<RxEntry>,
    advertiser: Option<(String, Worker)>,
    discovery: Option<Worker>,
}

impl Session {
    /// Démarre la découverte sur `nic`, puis applique `cfg` (voir [`Session::apply`]).
    pub fn start(
        cfg: &Config,
        nic: Iface,
        shared: &Shared,
        routes: Option<&RoutesHandle>,
        parent: &Stop,
    ) -> io::Result<Self> {
        let stop = parent.child();
        shared.reset_streams(&nic, cfg.auto_iface());
        let discovery = {
            let (nic, dir) = (nic.clone(), shared.directory().clone());
            Worker::spawn("découverte".into(), &stop, move |s| {
                discovery::run(&nic, &dir, &s)
            })?
        };
        let mut session = Self {
            stop,
            iface: nic,
            tx: Vec::new(),
            rx: Vec::new(),
            advertiser: None,
            discovery: Some(discovery),
        };
        if let Err(e) = session.apply(cfg, shared, routes) {
            session.stop();
            return Err(e);
        }
        Ok(session)
    }

    /// Applique `cfg` : démarre les flux nouveaux ou modifiés, arrête ceux qui ont disparu, garde
    /// les autres tels quels (aucune coupure), puis remplace la table de routes du périphérique.
    pub fn apply(
        &mut self,
        cfg: &Config,
        shared: &Shared,
        routes: Option<&RoutesHandle>,
    ) -> io::Result<()> {
        let nic = self.iface.clone();
        let mut table = Routes::default();
        let (mut started, mut stopped) = (0usize, 0usize);

        // Flux émis.
        let mut old_tx = std::mem::take(&mut self.tx);
        for (src, stream) in cfg.sources.iter().zip(cfg.tx_streams()) {
            let key = TxKey {
                channel: src.channel,
                format: src.format,
                payload_type: src.payload_type,
                tone: (src.tone_hz.to_bits(), src.level_dbfs.to_bits()),
                tos: cfg.tos,
                cushion: cfg.latency.tx_cushion(),
                with_source: src.device_channels.is_some(),
            };
            let entry = match old_tx.iter().position(|e| e.key == key) {
                Some(i) => old_tx.swap_remove(i),
                None => {
                    started += 1;
                    let ch = usize::from(stream.format.channels());
                    let port = key.with_source.then(|| bus::port(ch, patch::BUS_FRAMES));
                    let spp = stream.format.samples_per_packet() as usize;
                    let target = 2 * spp + key.cushion;
                    let source = port
                        .as_ref()
                        .map(|p| bus::JitterReader::new(p.reader(), target, target + 2048));
                    let group = stream.group().to_string();
                    let (nic, tos, shared) = (nic.clone(), cfg.tos, shared.clone());
                    let worker = Worker::spawn(format!("TX {group}"), &self.stop, move |s| {
                        let opts = TxOptions {
                            ttl: 128,
                            tos,
                            realtime: true,
                        };
                        tx::run_from(&nic, &stream, opts, &s, source, |r| shared.update_tx(r))
                            .map(|r| shared.update_tx(&r))
                    })?;
                    TxEntry {
                        key: key.clone(),
                        group,
                        port,
                        worker,
                    }
                }
            };
            if let (Some(chs), Some(port)) = (&src.device_channels, &entry.port) {
                table.outputs.push(device::OutRoute {
                    label: format!("{} → canal {}", src.name, src.channel),
                    device_channels: chs.iter().map(|&c| usize::from(c) - 1).collect(),
                    writer: port.writer(),
                });
            }
            self.tx.push(entry);
        }
        for e in old_tx {
            stopped += 1;
            e.worker.stop();
            shared.forget_stream(&e.group);
        }

        // Flux reçus.
        let mut old_rx = std::mem::take(&mut self.rx);
        for (d, (group, port_no)) in cfg.destinations.iter().zip(cfg.rx_groups()) {
            let key = RxKey {
                group,
                port: port_no,
                channels: d.stream_channels(),
                target: cfg.latency.rx_target(),
                with_sink: d.device_channels.is_some(),
            };
            let entry = match old_rx.iter().position(|e| e.key == key) {
                Some(i) => old_rx.swap_remove(i),
                None => {
                    started += 1;
                    let port = key
                        .with_sink
                        .then(|| bus::port(key.channels, patch::BUS_FRAMES));
                    let sink = port.as_ref().map(bus::Port::writer);
                    let (nic, shared) = (nic.clone(), shared.clone());
                    let worker =
                        Worker::spawn(format!("RX {}", d.label()), &self.stop, move |s| {
                            rx::run_into(
                                &nic,
                                group,
                                port_no,
                                &s,
                                Duration::from_secs(1),
                                sink,
                                |st| shared.update_rx(st),
                            )
                            .map(|st| shared.update_rx(&st))
                        })?;
                    RxEntry {
                        key: key.clone(),
                        group: group.to_string(),
                        port,
                        worker,
                    }
                }
            };
            if let (Some(chs), Some(port)) = (&d.device_channels, &entry.port) {
                let t = key.target;
                table.inputs.push(device::InRoute {
                    label: format!("{} → entrées {:?}", d.label(), chs),
                    device_channels: chs.iter().map(|&c| usize::from(c) - 1).collect(),
                    reader: bus::JitterReader::new(port.reader(), t, 4 * t),
                });
            }
            self.rx.push(entry);
        }
        for e in old_rx {
            stopped += 1;
            e.worker.stop();
            shared.forget_stream(&e.group);
        }

        // Table de routes du périphérique (remplacée d'un bloc par son thread).
        let patched = table.outputs.len() + table.inputs.len();
        match routes {
            Some(h) => h.set(table),
            None if patched > 0 => error!("patch ignoré : aucun périphérique virtuel actif"),
            None => {}
        }

        // Annonce : relancée seulement si ce qui est annoncé change.
        let adv_key = (cfg.advertise && !cfg.sources.is_empty())
            .then(|| format!("{}|{:?}", cfg.terminal(), cfg.adv_sources()));
        if self.advertiser.as_ref().map(|(k, _)| Some(k)) != Some(adv_key.as_ref()) {
            if let Some((_, w)) = self.advertiser.take() {
                w.stop();
            }
            if let Some(k) = adv_key {
                let mut adv = Advertiser::new(&nic, &cfg.terminal(), cfg.adv_sources())?;
                let w = Worker::spawn("annonce".into(), &self.stop, move |s| adv.run(&s))?;
                self.advertiser = Some((k, w));
            }
        }
        shared.set_advertised(if self.advertiser.is_some() {
            cfg.sources.len()
        } else {
            0
        });

        info!(
            "session sur {} ({}, {}) : {} flux émis, {} reçus, {patched} route(s) vers le périphérique ; {started} flux démarré(s), {stopped} arrêté(s)",
            nic.name,
            nic.friendly,
            nic.ipv4,
            self.tx.len(),
            self.rx.len(),
        );
        Ok(())
    }

    /// Un thread de la session s'est-il arrêté sur erreur ? (retourne la première erreur rencontrée)
    pub fn failed(&mut self) -> Option<String> {
        let finished = |w: &Worker| w.handle.is_finished();
        if self.discovery.as_ref().is_some_and(finished) {
            return self.discovery.take().and_then(join_error);
        }
        if let Some(i) = self.tx.iter().position(|e| finished(&e.worker)) {
            return join_error(self.tx.swap_remove(i).worker);
        }
        if let Some(i) = self.rx.iter().position(|e| finished(&e.worker)) {
            return join_error(self.rx.swap_remove(i).worker);
        }
        if self.advertiser.as_ref().is_some_and(|(_, w)| finished(w)) {
            return self.advertiser.take().and_then(|(_, w)| join_error(w));
        }
        None
    }

    /// Arrête la session et attend ses threads.
    pub fn stop(self) {
        self.stop.request();
        let workers = self
            .tx
            .into_iter()
            .map(|e| e.worker)
            .chain(self.rx.into_iter().map(|e| e.worker))
            .chain(self.advertiser.map(|(_, w)| w))
            .chain(self.discovery);
        for w in workers {
            w.stop();
        }
    }
}

fn join_error(w: Worker) -> Option<String> {
    match w.handle.join() {
        Ok(Ok(())) => None,
        Ok(Err(e)) => Some(format!("{} : {e}", w.name)),
        Err(_) => Some(format!("{} : thread interrompu", w.name)),
    }
}

/// Erreur de démarrage du daemon.
pub type Error = Box<dyn std::error::Error>;

/// Fait tourner le daemon jusqu'à l'arrêt : canal de contrôle (`control`), détecteur, périphérique,
/// sessions. `path` : fichier où enregistrer les modifications de configuration reçues.
pub fn run(
    cfg: Config,
    path: Option<PathBuf>,
    stop: &Stop,
    control: Option<&lw_sys::ctl::Endpoint>,
) -> Result<(), Error> {
    let shared = Shared::new(None, 0);
    let server = match control {
        Some(ep) => {
            let server = shared.serve(ep)?;
            info!("canal de contrôle publié : {ep}");
            Some(server)
        }
        None => None,
    };
    let want_device = cfg.device.is_some() || control.is_some();
    supervise(&shared, server.as_ref(), cfg, path, stop, want_device)?;
    drop(server);
    Ok(())
}

/// Boucle de supervision, avec un état partagé et un canal de contrôle fournis par l'appelant (tests).
/// `want_device` : crée le périphérique virtuel même si la configuration n'en décrit pas.
pub fn supervise(
    shared: &Shared,
    server: Option<&lw_sys::ctl::Server>,
    cfg: Config,
    path: Option<PathBuf>,
    stop: &Stop,
    want_device: bool,
) -> Result<(), Error> {
    let shared = shared.clone();
    let detector = {
        let (heard, stop) = (shared.heard().clone(), stop.clone());
        std::thread::Builder::new()
            .name("détection".into())
            .spawn(move || crate::detect::run(&heard, &stop))?
    };
    let (reload_tx, reload_rx) = std::sync::mpsc::channel::<Config>();
    shared.set_config(cfg.clone(), path, reload_tx);

    let mut current = cfg;
    let mut dev: Option<(DeviceConfig, Stop, device::Device)> = None;
    let mut generation = 0u64;
    let mut session: Option<Session> = None;
    let mut restart = true;
    let mut reconfigure = false;
    let mut last_check: Option<Instant> = None;
    let mut last_error = String::new();
    while !stop.requested() {
        // 1. Périphérique.
        let dc = current.device_config();
        if want_device && dev.as_ref().is_none_or(|(c, _, _)| *c != dc) {
            if let Some(s) = session.take() {
                s.stop();
            }
            if let Some((_, ds, d)) = dev.take() {
                ds.request();
                let _ = d.thread.join();
            }
            let ds = Stop::new();
            match device::start(&dc, &ds) {
                Ok(d) => {
                    generation += 1;
                    d.status
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .generation = generation;
                    if let Some(s) = server {
                        s.set_region(&d.region);
                    }
                    shared.set_device(d.status.clone());
                    info!(
                        "périphérique virtuel n° {generation} : {} sorties vers le réseau, {} entrées depuis le réseau{}",
                        dc.channels_to_net,
                        dc.channels_from_net,
                        if dc.loopback { ", boucle interne" } else { "" }
                    );
                    dev = Some((dc, ds, d));
                }
                Err(e) => {
                    error!("périphérique virtuel : {e}");
                    std::thread::sleep(Duration::from_secs(1));
                    continue;
                }
            }
            restart = true;
        }
        let routes = dev.as_ref().map(|(_, _, d)| d.routes_handle());

        // 2. Interface cible (relue toutes les 2 s : `iface::list` interroge networksetup).
        if restart
            || reconfigure
            || last_check.is_none_or(|t| t.elapsed() >= Duration::from_secs(2))
        {
            last_check = Some(Instant::now());
            let auto = current.auto_iface();
            let target = if auto {
                let now_on = session.as_ref().map(|s| s.iface.name.clone());
                shared
                    .heard()
                    .choose(now_on.as_deref(), Instant::now())
                    .and_then(|n| iface::find(&n).ok())
            } else {
                iface::find(&current.iface).ok()
            };
            let same = match (&session, &target) {
                (Some(s), Some(t)) => s.iface.name == t.name && s.iface.ipv4 == t.ipv4,
                (None, None) => true,
                _ => false,
            };
            if restart || !same {
                if let Some(s) = session.take() {
                    s.stop();
                }
                match target {
                    Some(nic) => {
                        match Session::start(&current, nic, &shared, routes.as_ref(), stop) {
                            Ok(s) => {
                                session = Some(s);
                                last_error.clear();
                            }
                            Err(e) => {
                                let msg = format!("session réseau impossible : {e}");
                                if msg != last_error {
                                    error!("{msg}");
                                    last_error = msg;
                                }
                                shared.set_searching(auto);
                            }
                        }
                    }
                    None => {
                        if let Some(r) = &routes {
                            r.set(Routes::default());
                        }
                        shared.set_searching(auto);
                        if auto {
                            info!("recherche du réseau Livewire (annonces sur 239.192.255.3)");
                        } else {
                            info!("interface {} absente : en attente", current.iface);
                        }
                    }
                }
                restart = false;
            } else if reconfigure {
                // Même interface : modification appliquée à chaud, flux par flux.
                if let Some(s) = session.as_mut() {
                    if let Err(e) = s.apply(&current, &shared, routes.as_ref()) {
                        error!("modification inapplicable : {e}");
                    }
                }
            }
            reconfigure = false;
        }

        // 3. Configuration modifiée par XPC.
        match reload_rx.recv_timeout(Duration::from_millis(200)) {
            Ok(next) => {
                current = next;
                reconfigure = true;
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }
        // Un thread en erreur (interface perdue, port occupé…) est journalisé ; le daemon continue.
        if let Some(s) = session.as_mut() {
            while let Some(e) = s.failed() {
                error!("{e}");
            }
        }
    }
    if let Some(s) = session.take() {
        s.stop();
    }
    if let Some((_, ds, d)) = dev.take() {
        ds.request();
        d.thread
            .join()
            .map_err(|_| "thread du périphérique interrompu")?;
    }
    let _ = detector.join();
    Ok(())
}
