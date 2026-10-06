//! Couche système du daemon : enveloppes sûres autour de la couche C (`csrc/`), une implémentation
//! par système (macOS, Linux, Windows).
//!
//! - [`rt`] : threads temps réel, sommeil précis, horloge hôte.
//! - [`log`] : journal du système (`os_log`, journal des événements Windows).
//! - [`shm`] : région partagée avec le client audio (anneaux, horloge), contrat `csrc/lw_shm.h`.
//! - [`ctl`] : canal de contrôle (JSON) : XPC (macOS), socket Unix (macOS, Linux), tube nommé (Windows).
//! - [`xpc`] : service XPC (macOS), utilisé par [`ctl`] et par le plugin HAL.

pub mod ctl;
pub mod shm;

mod ffi {
    use std::ffi::{c_char, c_int, c_void};

    /// Description de l'horloge hôte (`lw_host_clock`).
    #[repr(C)]
    #[derive(Debug, Clone, Copy, Default)]
    pub struct HostClock {
        pub id: u32,
        pub ns_numer: u64,
        pub ns_denom: u64,
    }

    extern "C" {
        pub fn lw_rt_promote(period_ns: u64, computation_ns: u64, constraint_ns: u64) -> c_int;
        pub fn lw_sleep_ns(ns: u64);
        pub fn lw_log(level: c_int, category: *const c_char, message: *const c_char);
        pub fn lw_host_time() -> u64;
        pub fn lw_host_time_to_ns(t: u64) -> u64;
        pub fn lw_host_clock_info(clock: *mut HostClock);
        pub fn lw_shm_alloc(size: usize, handle: *mut *mut c_void) -> *mut c_void;
        pub fn lw_shm_map(handle: *mut c_void, size: *mut usize) -> *mut c_void;
        pub fn lw_shm_unmap(base: *mut c_void, size: usize);
        pub fn lw_shm_release(handle: *mut c_void);
        #[cfg(any(target_os = "macos", windows))]
        pub fn lw_free(p: *mut c_char);
        pub fn lw_shm_size(ring_frames: u32, c0: u32, c1: u32) -> usize;
        pub fn lw_shm_init(
            base: *mut c_void,
            size: usize,
            rate: u32,
            ring_frames: u32,
            c0: u32,
            c1: u32,
            clock: *const HostClock,
        ) -> c_int;
        pub fn lw_shm_validate(base: *const c_void, size: usize) -> c_int;
        pub fn lw_shm_host_clock(base: *const c_void, clock: *mut HostClock);
        pub fn lw_ring_write(base: *mut c_void, dir: c_int, src: *const f32, frames: u32) -> u32;
        pub fn lw_ring_read(base: *mut c_void, dir: c_int, dst: *mut f32, frames: u32) -> u32;
        pub fn lw_ring_readable(base: *const c_void, dir: c_int) -> u32;
        pub fn lw_ring_writable(base: *const c_void, dir: c_int) -> u32;
        pub fn lw_ring_counters(
            base: *const c_void,
            dir: c_int,
            w: *mut u64,
            r: *mut u64,
            over: *mut u64,
            under: *mut u64,
        );
        pub fn lw_clock_publish(base: *mut c_void, host: u64, sample: u64, rate: f64);
        pub fn lw_clock_read(
            base: *const c_void,
            host: *mut u64,
            sample: *mut u64,
            rate: *mut f64,
        ) -> c_int;
    }

    #[cfg(unix)]
    extern "C" {
        pub fn lw_uid_in_group(uid: u32, group: *const c_char) -> c_int;
        pub fn lw_peer_cred(fd: c_int, uid: *mut u32, pid: *mut u32) -> c_int;
        pub fn lw_geteuid() -> u32;
    }

    #[cfg(target_os = "macos")]
    pub use mac::*;

    #[cfg(target_os = "macos")]
    mod mac {
        use std::ffi::{c_char, c_int, c_void};

        pub type HandlerFn = extern "C" fn(*const c_char, u32, *mut c_void) -> *mut c_char;
        pub type FreeFn = extern "C" fn(*mut c_char);

        #[repr(C)]
        pub struct Server {
            _private: [u8; 0],
        }
        #[repr(C)]
        pub struct Client {
            _private: [u8; 0],
        }

        extern "C" {
            pub fn lw_xpc_server_start(
                name: *const c_char,
                h: HandlerFn,
                f: FreeFn,
                ctx: *mut c_void,
            ) -> *mut Server;
            pub fn lw_xpc_server_endpoint(s: *mut Server) -> *mut c_void;
            pub fn lw_xpc_server_stop(s: *mut Server);
            pub fn lw_xpc_client_mach(name: *const c_char, privileged: c_int) -> *mut Client;
            pub fn lw_xpc_client_endpoint(endpoint: *mut c_void) -> *mut Client;
            pub fn lw_xpc_call(
                c: *mut Client,
                req: *const c_char,
                err: *mut *const c_char,
            ) -> *mut c_char;
            pub fn lw_xpc_client_close(c: *mut Client);
            pub fn lw_xpc_server_set_shmem(s: *mut Server, shmem: *mut c_void);
            pub fn lw_xpc_call_shmem(
                c: *mut Client,
                req: *const c_char,
                err: *mut *const c_char,
                shmem: *mut *mut c_void,
            ) -> *mut c_char;
            pub fn lw_xpc_release(o: *mut c_void);
        }
    }

    #[cfg(windows)]
    pub use win::*;

    #[cfg(windows)]
    mod win {
        use std::ffi::{c_char, c_int, c_void};

        /// Appelant d'un tube nommé (`lw_caller`).
        #[repr(C)]
        pub struct Caller {
            pub pid: u32,
            pub may_edit: c_int,
            pub user: [c_char; 256],
        }

        pub type PipeHandlerFn =
            extern "C" fn(*const c_char, *const Caller, *mut c_void) -> *mut c_char;
        pub type PipeFreeFn = extern "C" fn(*mut c_char);

        #[repr(C)]
        pub struct PipeServer {
            _private: [u8; 0],
        }
        #[repr(C)]
        pub struct PipeClient {
            _private: [u8; 0],
        }

        extern "C" {
            pub fn lw_pipe_server_start(
                name: *const c_char,
                edit_group: *const c_char,
                h: PipeHandlerFn,
                f: PipeFreeFn,
                ctx: *mut c_void,
            ) -> *mut PipeServer;
            pub fn lw_pipe_server_stop(s: *mut PipeServer);
            pub fn lw_pipe_client_connect(
                name: *const c_char,
                timeout_ms: u32,
                err: *mut *const c_char,
            ) -> *mut PipeClient;
            pub fn lw_pipe_call(
                c: *mut PipeClient,
                req: *const c_char,
                err: *mut *const c_char,
            ) -> *mut c_char;
            pub fn lw_pipe_client_close(c: *mut PipeClient);
            pub fn lw_shm_share_with(handle: *mut c_void, pid: u32) -> u64;
            pub fn lw_qos_dscp(
                sock: u64,
                dest_ip: *const u8,
                dest_port: u16,
                dscp: u32,
                error: *mut c_int,
            ) -> *mut c_void;
            pub fn lw_qos_close(flow: *mut c_void);
        }
    }
}

/// Ordonnancement temps réel et horloge hôte.
pub mod rt {
    use std::time::Duration;

    /// Passe le thread appelant en temps réel : il doit pouvoir s'exécuter `computation` au sein de
    /// chaque `period`, terminé au plus tard `constraint` après le début de la période.
    /// macOS : `THREAD_TIME_CONSTRAINT_POLICY` ; Linux : `SCHED_FIFO` (exige `CAP_SYS_NICE` ou une
    /// limite `RLIMIT_RTPRIO`) ; Windows : MMCSS « Pro Audio ». L'erreur est le code du système.
    pub fn promote(
        period: Duration,
        computation: Duration,
        constraint: Duration,
    ) -> Result<(), i32> {
        let ns = |d: Duration| u64::try_from(d.as_nanos()).unwrap_or(u64::MAX);
        // SAFETY: fonction C sans pointeur ; elle ne touche que le thread appelant.
        let rc =
            unsafe { super::ffi::lw_rt_promote(ns(period), ns(computation), ns(constraint)) };
        if rc == 0 {
            Ok(())
        } else {
            Err(rc)
        }
    }

    /// Sommeil précis, sans attente active : seul mode d'attente admissible dans un thread temps réel.
    pub fn sleep(d: Duration) {
        let ns = u64::try_from(d.as_nanos()).unwrap_or(u64::MAX);
        // SAFETY: fonction C sans pointeur ; elle ne bloque que le thread appelant.
        unsafe { super::ffi::lw_sleep_ns(ns) };
    }

    /// Horloge hôte en nanosecondes (base de temps du client audio).
    pub fn host_time_ns() -> u64 {
        // SAFETY: fonctions C sans pointeur.
        unsafe { super::ffi::lw_host_time_to_ns(super::ffi::lw_host_time()) }
    }

    /// Convertit une durée en ticks hôte bruts en nanosecondes.
    pub fn host_time_ns_of(raw: u64) -> u64 {
        // SAFETY: fonction C pure.
        unsafe { super::ffi::lw_host_time_to_ns(raw) }
    }

    /// Horloge hôte brute (`mach_absolute_time`, `QueryPerformanceCounter`, `CLOCK_MONOTONIC`),
    /// unité attendue dans la région partagée.
    pub fn host_time() -> u64 {
        // SAFETY: fonction C sans pointeur.
        unsafe { super::ffi::lw_host_time() }
    }

    /// Paramètres usuels pour un thread qui émet un paquet par `interval` :
    /// calcul ≤ 1/8 de la période (au moins 100 µs), contrainte à la moitié de la période.
    pub fn promote_for_packet_interval(interval: Duration) -> Result<(), i32> {
        let computation = (interval / 8).max(Duration::from_micros(100));
        let constraint = (interval / 2).max(computation);
        promote(interval, computation, constraint)
    }
}

/// Journal du système.
pub mod log {
    /// Niveau d'un message.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum Level {
        /// Débogage (non conservé par défaut).
        Debug = 0,
        /// Information.
        Info = 1,
        /// Défaut (conservé).
        Default = 2,
        /// Erreur.
        Error = 3,
        /// Faute.
        Fault = 4,
    }

    /// Écrit `message` dans le journal du système : journal unifié sur macOS
    /// (`log stream --predicate 'subsystem == "fr.francois-brille.openlw"'`), débogueur et journal des
    /// événements (source OpenLW) sur Windows. Sans effet sur Linux, où journald recueille stderr.
    pub fn write(level: Level, category: &str, message: &str) {
        use std::ffi::CString;
        let cat = CString::new(category.replace('\0', " ")).unwrap_or_default();
        let msg = CString::new(message.replace('\0', " ")).unwrap_or_default();
        // SAFETY: deux chaînes C valides et terminées, vivantes pendant l'appel ; la couche C les copie.
        unsafe { super::ffi::lw_log(level as i32, cat.as_ptr(), msg.as_ptr()) };
    }
}

/// Autorisation des appelants identifiés par leur UID (macOS, Linux).
#[cfg(unix)]
pub mod auth {
    /// `true` si `uid` est root ou membre du groupe `group` (« admin » : administrateurs macOS).
    pub fn uid_in_group(uid: u32, group: &str) -> bool {
        let Ok(g) = std::ffi::CString::new(group) else {
            return false;
        };
        // SAFETY: chaîne C valide pendant l'appel ; la fonction C n'utilise que des tampons locaux.
        unsafe { super::ffi::lw_uid_in_group(uid, g.as_ptr()) == 1 }
    }

    /// UID effectif du processus courant.
    pub fn euid() -> u32 {
        // SAFETY: fonction C sans argument.
        unsafe { super::ffi::lw_geteuid() }
    }
}

/// Marquage DSCP par qWAVE (Windows ignore `IP_TOS`).
#[cfg(windows)]
pub mod qos {
    use std::net::{SocketAddrV4, UdpSocket};
    use std::os::windows::io::AsRawSocket;

    /// Flux qWAVE d'une socket : le marquage dure tant que la valeur vit (à libérer avant la socket).
    pub struct Flow(*mut std::ffi::c_void);

    // SAFETY: le flux n'est manipulé qu'à sa fermeture, depuis n'importe quel thread.
    unsafe impl Send for Flow {}

    /// Marque `dscp` (0 à 63) sur les envois de `sock` vers `dest`. L'erreur est le code Windows
    /// (accès refusé hors administrateur ou service).
    pub fn set_dscp(sock: &UdpSocket, dest: SocketAddrV4, dscp: u8) -> Result<Flow, i32> {
        let ip = dest.ip().octets();
        let mut err = 0;
        // SAFETY: socket valide (empruntée pendant l'appel) ; `ip` contient 4 octets ; pointeur de
        // sortie valide.
        let f = unsafe {
            super::ffi::lw_qos_dscp(
                sock.as_raw_socket(),
                ip.as_ptr(),
                dest.port(),
                u32::from(dscp.min(63)),
                &mut err,
            )
        };
        if f.is_null() {
            Err(err)
        } else {
            Ok(Flow(f))
        }
    }

    impl Drop for Flow {
        fn drop(&mut self) {
            // SAFETY: flux créé par `lw_qos_dscp`, fermé une seule fois.
            unsafe { super::ffi::lw_qos_close(self.0) };
        }
    }
}

/// Contrôle XPC (requêtes et réponses JSON), macOS.
#[cfg(target_os = "macos")]
pub mod xpc {
    use std::ffi::{c_char, c_void, CStr, CString};
    use std::fmt;
    use std::panic::{catch_unwind, AssertUnwindSafe};

    /// Gestionnaire de requêtes : reçoit le JSON de la requête et l'UID effectif de l'appelant,
    /// renvoie le JSON de la réponse.
    pub type Handler = Box<dyn Fn(&str, u32) -> String + Send + Sync + 'static>;

    /// Erreur XPC.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct Error(pub String);

    impl fmt::Display for Error {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str(&self.0)
        }
    }

    impl std::error::Error for Error {}

    extern "C" fn trampoline(req: *const c_char, uid: u32, ctx: *mut c_void) -> *mut c_char {
        let run = || {
            // SAFETY: `ctx` est le `Box<Handler>` créé par `Server::start`, libéré seulement après
            // `lw_xpc_server_stop`, qui garantit qu'aucun appel n'est en cours.
            let handler = unsafe { &*(ctx as *const Handler) };
            // SAFETY: la couche C passe une chaîne terminée, valide pendant l'appel.
            let request = unsafe { CStr::from_ptr(req) }.to_string_lossy();
            handler(&request, uid)
        };
        // Une panique ne doit jamais traverser la frontière C.
        let response = catch_unwind(AssertUnwindSafe(run)).unwrap_or_else(|_| {
            r#"{"ok":false,"error":"panique dans le gestionnaire"}"#.to_string()
        });
        CString::new(response.replace('\0', " "))
            .unwrap_or_default()
            .into_raw()
    }

    extern "C" fn free_response(p: *mut c_char) {
        if !p.is_null() {
            // SAFETY: `p` provient de `CString::into_raw` dans `trampoline` et n'est libéré qu'ici, une fois.
            drop(unsafe { CString::from_raw(p) });
        }
    }

    /// Écouteur XPC. Arrêté et libéré au `drop`.
    pub struct Server {
        raw: *mut super::ffi::Server,
        ctx: *mut Handler,
    }

    // SAFETY: la couche C sérialise les appels sur sa file ; `ctx` n'est lu que par le gestionnaire,
    // lui-même `Send + Sync`.
    unsafe impl Send for Server {}
    // SAFETY: les méthodes prenant `&self` sont sûres entre threads : `set_shmem` passe par
    // `dispatch_sync` sur la file de l'écouteur, `endpoint` par `xpc_endpoint_create` (thread-safe).
    unsafe impl Sync for Server {}

    impl Server {
        /// Démarre un écouteur Mach nommé (`Some`, service déclaré dans le plist launchd) ou anonyme (`None`).
        pub fn start(mach_name: Option<&str>, handler: Handler) -> Result<Self, Error> {
            let name = mach_name
                .map(CString::new)
                .transpose()
                .map_err(|_| Error("nom de service invalide".into()))?;
            let ctx = Box::into_raw(Box::new(handler));
            let name_ptr = name.as_ref().map_or(std::ptr::null(), |n| n.as_ptr());
            // SAFETY: chaîne valide ou nulle ; fonctions de rappel `extern "C"` ; `ctx` reste valide
            // jusqu'à `lw_xpc_server_stop` (voir `Drop`).
            let raw = unsafe {
                super::ffi::lw_xpc_server_start(name_ptr, trampoline, free_response, ctx.cast())
            };
            if raw.is_null() {
                // SAFETY: l'écouteur n'a pas été créé, `ctx` n'est référencé nulle part ailleurs.
                drop(unsafe { Box::from_raw(ctx) });
                return Err(Error("création de l'écouteur XPC impossible".into()));
            }
            Ok(Self { raw, ctx })
        }

        /// Région partagée remise aux clients qui la demandent ([`Client::call_with_shmem`]).
        pub fn set_shmem(&self, region: &crate::shm::Region) {
            // SAFETY: `raw` est un écouteur actif ; l'objet xpc_shmem de la région est valide et la couche C
            // le retient (le relâche à l'arrêt ou au remplacement).
            unsafe { super::ffi::lw_xpc_server_set_shmem(self.raw, region.handle()) };
        }

        /// Point d'accès de l'écouteur, pour un client du même processus (tests).
        pub fn endpoint(&self) -> Endpoint {
            // SAFETY: `raw` est un écouteur actif ; l'objet renvoyé est retenu, libéré par `Endpoint::drop`.
            Endpoint(unsafe { super::ffi::lw_xpc_server_endpoint(self.raw) })
        }
    }

    impl Drop for Server {
        fn drop(&mut self) {
            // SAFETY: `raw` est valide et arrêté une seule fois ; après le retour, plus aucun gestionnaire
            // ne s'exécute, `ctx` peut donc être libéré.
            unsafe { super::ffi::lw_xpc_server_stop(self.raw) };
            // SAFETY: `ctx` vient de `Box::into_raw` dans `start` et n'est plus utilisé (barrière ci-dessus).
            drop(unsafe { Box::from_raw(self.ctx) });
        }
    }

    /// Point d'accès XPC (objet retenu).
    pub struct Endpoint(*mut c_void);

    impl Drop for Endpoint {
        fn drop(&mut self) {
            // SAFETY: objet XPC retenu par `lw_xpc_server_endpoint`, relâché une seule fois.
            unsafe { super::ffi::lw_xpc_release(self.0) };
        }
    }

    /// Connexion cliente.
    pub struct Client {
        raw: *mut super::ffi::Client,
    }

    // SAFETY: une connexion XPC peut être utilisée depuis n'importe quel thread.
    unsafe impl Send for Client {}
    // SAFETY: `xpc_connection_send_message_with_reply_sync` peut être appelée depuis plusieurs threads.
    unsafe impl Sync for Client {}

    fn error_from(err: *const c_char) -> Error {
        if err.is_null() {
            Error("erreur XPC".into())
        } else {
            // SAFETY: `err` pointe vers une chaîne statique de la couche C.
            Error(unsafe { CStr::from_ptr(err) }.to_string_lossy().into_owned())
        }
    }

    fn take_string(out: *mut c_char) -> String {
        // SAFETY: `out` est une chaîne allouée par strdup, terminée ; libérée juste après la copie.
        let s = unsafe { CStr::from_ptr(out) }.to_string_lossy().into_owned();
        // SAFETY: `out` vient de strdup dans la couche C, libéré une seule fois par `lw_free`.
        unsafe { super::ffi::lw_free(out) };
        s
    }

    impl Client {
        /// Connexion à un service Mach (`privileged` : service du domaine système, LaunchDaemon).
        pub fn connect(mach_name: &str, privileged: bool) -> Result<Self, Error> {
            let name =
                CString::new(mach_name).map_err(|_| Error("nom de service invalide".into()))?;
            // SAFETY: chaîne valide pendant l'appel ; la couche C copie le nom.
            let raw =
                unsafe { super::ffi::lw_xpc_client_mach(name.as_ptr(), i32::from(privileged)) };
            if raw.is_null() {
                return Err(Error("connexion XPC impossible".into()));
            }
            Ok(Self { raw })
        }

        /// Connexion à un point d'accès (même processus).
        pub fn from_endpoint(endpoint: &Endpoint) -> Result<Self, Error> {
            // SAFETY: `endpoint` est un objet XPC valide tant que l'`Endpoint` vit ; la couche C le retient.
            let raw = unsafe { super::ffi::lw_xpc_client_endpoint(endpoint.0) };
            if raw.is_null() {
                return Err(Error("connexion au point d'accès impossible".into()));
            }
            Ok(Self { raw })
        }

        /// Requête synchrone qui demande aussi la région partagée du serveur.
        pub fn call_with_shmem(
            &self,
            request: &str,
        ) -> Result<(String, Option<crate::shm::SharedObject>), Error> {
            let req = CString::new(request)
                .map_err(|_| Error("requête contenant un octet nul".into()))?;
            let mut err: *const c_char = std::ptr::null();
            let mut obj: *mut c_void = std::ptr::null_mut();
            // SAFETY: connexion ouverte ; pointeurs de sortie valides pendant l'appel.
            let out = unsafe {
                super::ffi::lw_xpc_call_shmem(self.raw, req.as_ptr(), &mut err, &mut obj)
            };
            let shm = crate::shm::SharedObject::from_raw(obj);
            if out.is_null() {
                return Err(error_from(err));
            }
            Ok((take_string(out), shm))
        }

        /// Requête synchrone : JSON envoyé, JSON reçu.
        pub fn call(&self, request: &str) -> Result<String, Error> {
            let req = CString::new(request)
                .map_err(|_| Error("requête contenant un octet nul".into()))?;
            let mut err: *const c_char = std::ptr::null();
            // SAFETY: `raw` est une connexion ouverte ; `req` et `err` sont valides pendant l'appel.
            let out = unsafe { super::ffi::lw_xpc_call(self.raw, req.as_ptr(), &mut err) };
            if out.is_null() {
                return Err(error_from(err));
            }
            Ok(take_string(out))
        }
    }

    impl Drop for Client {
        fn drop(&mut self) {
            // SAFETY: connexion ouverte par `connect` ou `from_endpoint`, fermée une seule fois.
            unsafe { super::ffi::lw_xpc_client_close(self.raw) };
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    /// macOS et Windows accordent le temps réel sans privilège ; Linux exige `CAP_SYS_NICE` ou une
    /// limite `RLIMIT_RTPRIO`, rarement présentes en CI.
    #[test]
    fn rt_promotion() {
        let r =
            std::thread::spawn(|| super::rt::promote_for_packet_interval(Duration::from_millis(1)))
                .join()
                .unwrap();
        if cfg!(target_os = "linux") {
            assert!(r.is_ok() || r == Err(1), "seul EPERM est admis sous Linux : {r:?}");
        } else {
            r.expect("passage en temps réel refusé");
        }
    }

    #[test]
    fn host_clock_advances_in_nanoseconds() {
        let (t0, n0) = (super::rt::host_time(), super::rt::host_time_ns());
        std::thread::sleep(Duration::from_millis(50));
        let (t1, n1) = (super::rt::host_time(), super::rt::host_time_ns());
        let dt = super::rt::host_time_ns_of(t1 - t0);
        assert!((45_000_000..500_000_000).contains(&dt), "écart hôte {dt} ns");
        assert!((45_000_000..500_000_000).contains(&(n1 - n0)));
    }

    #[test]
    fn rt_thread_sleeps_precisely() {
        let worst = std::thread::spawn(|| {
            let _ = super::rt::promote_for_packet_interval(Duration::from_millis(1));
            let mut worst = Duration::ZERO;
            for _ in 0..200 {
                let t0 = std::time::Instant::now();
                super::rt::sleep(Duration::from_micros(500));
                worst = worst.max(t0.elapsed().saturating_sub(Duration::from_micros(500)));
            }
            worst
        })
        .join()
        .unwrap();
        // Les VM et conteneurs Linux (CI, OrbStack) ont des minuteurs grossiers : retard médian de
        // 2,5 ms mesuré pour un sommeil de 500 µs, en C pur, avec ou sans SCHED_FIFO. Même tolérance
        // sous Wine émulé (OPENLW_COARSE_TIMERS, posée par tools/ci/wine.Dockerfile).
        let coarse = std::env::var_os("OPENLW_COARSE_TIMERS").is_some();
        let bound = if cfg!(target_os = "linux") || coarse {
            Duration::from_millis(50)
        } else {
            Duration::from_millis(5)
        };
        assert!(worst < bound, "réveil tardif de {worst:?}");
    }

    #[test]
    fn log_does_not_crash() {
        super::log::write(super::log::Level::Info, "test", "lw-sys : message de test");
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn xpc_anonymous_roundtrip() {
        let server = super::xpc::Server::start(
            None,
            Box::new(|req: &str, _uid: u32| format!("{{\"echo\":{req}}}")),
        )
        .unwrap();
        let client = super::xpc::Client::from_endpoint(&server.endpoint()).unwrap();
        for i in 0..50 {
            let resp = client.call(&format!("{{\"n\":{i}}}")).unwrap();
            assert_eq!(resp, format!("{{\"echo\":{{\"n\":{i}}}}}"));
        }
        drop(client);
        drop(server);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn xpc_handler_panic_is_contained() {
        let server =
            super::xpc::Server::start(None, Box::new(|_req: &str, _uid: u32| panic!("test")))
                .unwrap();
        let client = super::xpc::Client::from_endpoint(&server.endpoint()).unwrap();
        assert!(client.call("{}").unwrap().contains("panique"));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn xpc_passes_caller_uid() {
        let server =
            super::xpc::Server::start(None, Box::new(|_req: &str, uid: u32| uid.to_string()))
                .unwrap();
        let client = super::xpc::Client::from_endpoint(&server.endpoint()).unwrap();
        let me = std::process::Command::new("id").arg("-u").output().unwrap();
        assert_eq!(
            client.call("{}").unwrap(),
            String::from_utf8_lossy(&me.stdout).trim()
        );
    }

    #[cfg(unix)]
    #[test]
    fn group_membership() {
        assert!(
            super::auth::uid_in_group(0, "nogroup-inexistant"),
            "root toujours autorisé"
        );
        assert!(
            !super::auth::uid_in_group(4_000_000, "admin"),
            "UID inconnu refusé"
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn xpc_unknown_service_is_an_error() {
        let client =
            super::xpc::Client::connect("fr.francois-brille.openlw.inexistant", false).unwrap();
        assert!(client.call("{}").is_err());
    }
}
