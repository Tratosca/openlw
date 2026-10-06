//! Couche système du daemon : enveloppes sûres autour de `csrc/lw_sys.c` (macOS).
//!
//! - [`rt::promote`] : thread courant en `THREAD_TIME_CONSTRAINT_POLICY` (comme les threads IO de CoreAudio).
//! - [`log`] : journal unifié (`os_log`, sous-système `fr.francois-brille.openlw`).
//! - [`xpc`] : service de contrôle XPC, requêtes et réponses JSON.
//! - [`shm`] : région partagée avec le plugin HAL (anneaux audio, horloge), contrat `csrc/lw_shm.h`.
//!
//! Sur les autres systèmes (nodes Linux), les fonctions sont des replis sans effet ou renvoient une erreur.

#[cfg(target_os = "macos")]
pub mod shm;

#[cfg(target_os = "macos")]
mod ffi {
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
        pub fn lw_rt_promote(period_ns: u64, computation_ns: u64, constraint_ns: u64) -> c_int;
        pub fn lw_sleep_ns(ns: u64);
        pub fn lw_log(level: c_int, category: *const c_char, message: *const c_char);
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
        pub fn lw_shm_alloc(size: usize, shmem: *mut *mut c_void) -> *mut c_void;
        pub fn lw_shm_map(shmem: *mut c_void, size: *mut usize) -> *mut c_void;
        pub fn lw_shm_unmap(base: *mut c_void, size: usize);
        pub fn lw_host_time() -> u64;
        pub fn lw_host_time_to_ns(t: u64) -> u64;
        pub fn lw_shm_size(ring_frames: u32, c0: u32, c1: u32) -> usize;
        pub fn lw_shm_init(
            base: *mut c_void,
            size: usize,
            rate: u32,
            ring_frames: u32,
            c0: u32,
            c1: u32,
        ) -> c_int;
        pub fn lw_shm_validate(base: *const c_void, size: usize) -> c_int;
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
        pub fn lw_xpc_release(o: *mut c_void);
        pub fn lw_free(p: *mut c_char);
        pub fn lw_uid_in_group(uid: u32, group: *const c_char) -> c_int;
    }
}

/// Ordonnancement temps réel.
pub mod rt {
    use std::time::Duration;

    /// Passe le thread appelant en temps réel : il doit pouvoir s'exécuter `computation` au sein de
    /// chaque `period`, terminé au plus tard `constraint` après le début de la période.
    pub fn promote(
        period: Duration,
        computation: Duration,
        constraint: Duration,
    ) -> Result<(), i32> {
        #[cfg(target_os = "macos")]
        {
            let ns = |d: Duration| u64::try_from(d.as_nanos()).unwrap_or(u64::MAX);
            // SAFETY: fonction C sans pointeur ; elle ne touche que le thread appelant.
            let kr =
                unsafe { super::ffi::lw_rt_promote(ns(period), ns(computation), ns(constraint)) };
            if kr == 0 {
                Ok(())
            } else {
                Err(kr)
            }
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = (period, computation, constraint);
            Err(-1)
        }
    }

    /// Sommeil précis (`mach_wait_until`), sans attente active : seul mode d'attente admissible
    /// dans un thread temps réel. Ailleurs, repli sur `std::thread::sleep`.
    pub fn sleep(d: Duration) {
        #[cfg(target_os = "macos")]
        {
            let ns = u64::try_from(d.as_nanos()).unwrap_or(u64::MAX);
            // SAFETY: fonction C sans pointeur ; elle ne bloque que le thread appelant.
            unsafe { super::ffi::lw_sleep_ns(ns) };
        }
        #[cfg(not(target_os = "macos"))]
        std::thread::sleep(d);
    }

    /// Horloge hôte en nanosecondes (`mach_absolute_time` converti), base de temps de CoreAudio.
    pub fn host_time_ns() -> u64 {
        #[cfg(target_os = "macos")]
        {
            // SAFETY: fonctions C sans pointeur.
            unsafe { super::ffi::lw_host_time_to_ns(super::ffi::lw_host_time()) }
        }
        #[cfg(not(target_os = "macos"))]
        {
            0
        }
    }

    /// Convertit une durée en ticks hôte bruts (`mach_absolute_time`) en nanosecondes.
    pub fn host_time_ns_of(raw: u64) -> u64 {
        #[cfg(target_os = "macos")]
        {
            // SAFETY: fonction C pure.
            unsafe { super::ffi::lw_host_time_to_ns(raw) }
        }
        #[cfg(not(target_os = "macos"))]
        {
            raw
        }
    }

    /// Horloge hôte brute (`mach_absolute_time`), unité attendue dans la région partagée.
    pub fn host_time() -> u64 {
        #[cfg(target_os = "macos")]
        {
            // SAFETY: fonction C sans pointeur.
            unsafe { super::ffi::lw_host_time() }
        }
        #[cfg(not(target_os = "macos"))]
        {
            0
        }
    }

    /// Paramètres usuels pour un thread qui émet un paquet par `interval` :
    /// calcul ≤ 1/8 de la période (au moins 100 µs), contrainte à la moitié de la période.
    pub fn promote_for_packet_interval(interval: Duration) -> Result<(), i32> {
        let computation = (interval / 8).max(Duration::from_micros(100));
        let constraint = (interval / 2).max(computation);
        promote(interval, computation, constraint)
    }
}

/// Journal unifié.
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

    /// Écrit `message` dans le journal unifié (`log stream --predicate 'subsystem == "fr.francois-brille.openlw"'`).
    pub fn write(level: Level, category: &str, message: &str) {
        #[cfg(target_os = "macos")]
        {
            use std::ffi::CString;
            let cat = CString::new(category.replace('\0', " ")).unwrap_or_default();
            let msg = CString::new(message.replace('\0', " ")).unwrap_or_default();
            // SAFETY: deux chaînes C valides et terminées, vivantes pendant l'appel ; os_log copie le message.
            unsafe { super::ffi::lw_log(level as i32, cat.as_ptr(), msg.as_ptr()) };
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = (level, category, message);
        }
    }
}

/// Autorisation des appelants XPC.
pub mod auth {
    /// `true` si `uid` est root ou membre du groupe `group` (« admin » : administrateurs macOS).
    pub fn uid_in_group(uid: u32, group: &str) -> bool {
        #[cfg(target_os = "macos")]
        {
            let Ok(g) = std::ffi::CString::new(group) else {
                return false;
            };
            // SAFETY: chaîne C valide pendant l'appel ; la fonction C n'utilise que des tampons locaux.
            unsafe { super::ffi::lw_uid_in_group(uid, g.as_ptr()) == 1 }
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = group;
            uid == 0
        }
    }
}

/// Contrôle XPC (requêtes et réponses JSON).
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

    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
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

    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    extern "C" fn free_response(p: *mut c_char) {
        if !p.is_null() {
            // SAFETY: `p` provient de `CString::into_raw` dans `trampoline` et n'est libéré qu'ici, une fois.
            drop(unsafe { CString::from_raw(p) });
        }
    }

    /// Écouteur XPC. Arrêté et libéré au `drop`.
    pub struct Server {
        #[cfg(target_os = "macos")]
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
            let ctx = Box::into_raw(Box::new(handler));
            #[cfg(target_os = "macos")]
            {
                let name = match mach_name.map(CString::new).transpose() {
                    Ok(n) => n,
                    Err(_) => {
                        // SAFETY: `ctx` vient de `Box::into_raw` juste au-dessus et n'a pas été transmis.
                        drop(unsafe { Box::from_raw(ctx) });
                        return Err(Error("nom de service invalide".into()));
                    }
                };
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
            #[cfg(not(target_os = "macos"))]
            {
                let _ = mach_name;
                // SAFETY: `ctx` vient de `Box::into_raw` et n'a pas été transmis.
                drop(unsafe { Box::from_raw(ctx) });
                Err(Error("XPC indisponible sur ce système".into()))
            }
        }

        /// Région partagée remise aux clients qui la demandent ([`Client::call_with_shmem`]).
        #[cfg(target_os = "macos")]
        pub fn set_shmem(&self, region: &crate::shm::Region) {
            // SAFETY: `raw` est un écouteur actif ; l'objet xpc_shmem de la région est valide et la couche C
            // le retient (le relâche à l'arrêt ou au remplacement).
            unsafe { super::ffi::lw_xpc_server_set_shmem(self.raw, region.xpc_object()) };
        }

        /// Point d'accès de l'écouteur, pour un client du même processus (tests).
        pub fn endpoint(&self) -> Endpoint {
            #[cfg(target_os = "macos")]
            {
                // SAFETY: `raw` est un écouteur actif ; l'objet renvoyé est retenu, libéré par `Endpoint::drop`.
                Endpoint(unsafe { super::ffi::lw_xpc_server_endpoint(self.raw) })
            }
            #[cfg(not(target_os = "macos"))]
            {
                Endpoint(std::ptr::null_mut())
            }
        }
    }

    impl Drop for Server {
        fn drop(&mut self) {
            #[cfg(target_os = "macos")]
            // SAFETY: `raw` est valide et arrêté une seule fois ; après le retour, plus aucun gestionnaire
            // ne s'exécute, `ctx` peut donc être libéré.
            unsafe {
                super::ffi::lw_xpc_server_stop(self.raw);
            }
            // SAFETY: `ctx` vient de `Box::into_raw` dans `start` et n'est plus utilisé (barrière ci-dessus).
            drop(unsafe { Box::from_raw(self.ctx) });
        }
    }

    /// Point d'accès XPC (objet retenu).
    pub struct Endpoint(*mut c_void);

    impl Drop for Endpoint {
        fn drop(&mut self) {
            #[cfg(target_os = "macos")]
            // SAFETY: objet XPC retenu par `lw_xpc_server_endpoint`, relâché une seule fois.
            unsafe {
                super::ffi::lw_xpc_release(self.0);
            }
        }
    }

    /// Connexion cliente.
    pub struct Client {
        #[cfg(target_os = "macos")]
        raw: *mut super::ffi::Client,
    }

    // SAFETY: une connexion XPC peut être utilisée depuis n'importe quel thread.
    unsafe impl Send for Client {}

    impl Client {
        /// Connexion à un service Mach (`privileged` : service du domaine système, LaunchDaemon).
        pub fn connect(mach_name: &str, privileged: bool) -> Result<Self, Error> {
            #[cfg(target_os = "macos")]
            {
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
            #[cfg(not(target_os = "macos"))]
            {
                let _ = (mach_name, privileged);
                Err(Error("XPC indisponible sur ce système".into()))
            }
        }

        /// Connexion à un point d'accès (même processus).
        pub fn from_endpoint(endpoint: &Endpoint) -> Result<Self, Error> {
            #[cfg(target_os = "macos")]
            {
                // SAFETY: `endpoint` est un objet XPC valide tant que l'`Endpoint` vit ; la couche C le retient.
                let raw = unsafe { super::ffi::lw_xpc_client_endpoint(endpoint.0) };
                if raw.is_null() {
                    return Err(Error("connexion au point d'accès impossible".into()));
                }
                Ok(Self { raw })
            }
            #[cfg(not(target_os = "macos"))]
            {
                let _ = endpoint;
                Err(Error("XPC indisponible sur ce système".into()))
            }
        }

        /// Requête synchrone qui demande aussi la région partagée du serveur.
        #[cfg(target_os = "macos")]
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
            let shm = crate::shm::SharedObject::from_retained(obj);
            if out.is_null() {
                let msg = if err.is_null() {
                    "erreur XPC".to_string()
                } else {
                    // SAFETY: `err` pointe vers une chaîne statique de la couche C.
                    unsafe { CStr::from_ptr(err) }
                        .to_string_lossy()
                        .into_owned()
                };
                return Err(Error(msg));
            }
            // SAFETY: chaîne allouée par strdup, terminée.
            let s = unsafe { CStr::from_ptr(out) }
                .to_string_lossy()
                .into_owned();
            // SAFETY: `out` vient de strdup, libéré une seule fois.
            unsafe { super::ffi::lw_free(out) };
            Ok((s, shm))
        }

        /// Requête synchrone : JSON envoyé, JSON reçu.
        pub fn call(&self, request: &str) -> Result<String, Error> {
            #[cfg(target_os = "macos")]
            {
                let req = CString::new(request)
                    .map_err(|_| Error("requête contenant un octet nul".into()))?;
                let mut err: *const c_char = std::ptr::null();
                // SAFETY: `raw` est une connexion ouverte ; `req` et `err` sont valides pendant l'appel.
                let out = unsafe { super::ffi::lw_xpc_call(self.raw, req.as_ptr(), &mut err) };
                if out.is_null() {
                    let msg = if err.is_null() {
                        "erreur XPC".to_string()
                    } else {
                        // SAFETY: `err` pointe vers une chaîne statique de la couche C.
                        unsafe { CStr::from_ptr(err) }
                            .to_string_lossy()
                            .into_owned()
                    };
                    return Err(Error(msg));
                }
                // SAFETY: `out` est une chaîne allouée par strdup, terminée ; libérée juste après la copie.
                let s = unsafe { CStr::from_ptr(out) }
                    .to_string_lossy()
                    .into_owned();
                // SAFETY: `out` vient de strdup dans la couche C, libéré une seule fois par `lw_free`.
                unsafe { super::ffi::lw_free(out) };
                Ok(s)
            }
            #[cfg(not(target_os = "macos"))]
            {
                let _ = request;
                Err(Error("XPC indisponible sur ce système".into()))
            }
        }
    }

    impl Drop for Client {
        fn drop(&mut self) {
            #[cfg(target_os = "macos")]
            // SAFETY: connexion ouverte par `connect` ou `from_endpoint`, fermée une seule fois.
            unsafe {
                super::ffi::lw_xpc_client_close(self.raw);
            }
        }
    }
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use std::time::Duration;

    #[test]
    fn rt_promotion_succeeds() {
        std::thread::spawn(|| super::rt::promote_for_packet_interval(Duration::from_millis(1)))
            .join()
            .unwrap()
            .expect("THREAD_TIME_CONSTRAINT_POLICY refusée");
    }

    #[test]
    fn rt_thread_sleeps_precisely() {
        let worst = std::thread::spawn(|| {
            super::rt::promote_for_packet_interval(Duration::from_millis(1)).unwrap();
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
        assert!(
            worst < Duration::from_millis(5),
            "réveil tardif de {worst:?}"
        );
    }

    #[test]
    fn os_log_does_not_crash() {
        super::log::write(super::log::Level::Info, "test", "lw-sys : message de test");
    }

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

    #[test]
    fn xpc_handler_panic_is_contained() {
        let server =
            super::xpc::Server::start(None, Box::new(|_req: &str, _uid: u32| panic!("test")))
                .unwrap();
        let client = super::xpc::Client::from_endpoint(&server.endpoint()).unwrap();
        assert!(client.call("{}").unwrap().contains("panique"));
    }

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

    #[test]
    fn xpc_unknown_service_is_an_error() {
        let client =
            super::xpc::Client::connect("fr.francois-brille.openlw.inexistant", false).unwrap();
        assert!(client.call("{}").is_err());
    }
}
