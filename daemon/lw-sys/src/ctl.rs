//! Canal de contrôle du daemon : une requête JSON, une réponse JSON (ADR 0007).
//!
//! | Système | Transport du service installé                         | Peut modifier                          |
//! |---------|-------------------------------------------------------|----------------------------------------|
//! | macOS   | XPC, service Mach `fr.francois-brille.openlw.daemon`  | root, groupe `admin`                   |
//! | Linux   | socket Unix `$XDG_RUNTIME_DIR/openlw/control.sock`    | root, l'utilisateur du service         |
//! | Windows | tube nommé `\\.\pipe\fr.francois-brille.openlw.daemon` | administrateurs (session élevée), SYSTEM, groupe local `OpenLW` |
//!
//! La socket Unix sert aussi sur macOS (tests, outils). Sur les transports par flux (socket, tube),
//! chaque message tient sur une ligne terminée par `\n` ; le JSON compact n'en contient jamais.

use std::fmt;
#[cfg(unix)]
use std::path::PathBuf;
use std::sync::Arc;

use crate::shm::Region;

/// Nom du service : service Mach (macOS), tube nommé (Windows).
pub const SERVICE_NAME: &str = "fr.francois-brille.openlw.daemon";

/// Groupe local Windows dont les membres peuvent modifier la configuration (créé par l'installeur).
pub const EDIT_GROUP_WINDOWS: &str = "OpenLW";

/// Erreur du canal de contrôle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error(pub String);

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Error {}

/// Appelant d'une requête, identifié par le transport.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Caller {
    /// Identité lisible (« uid 501 », « PC\\françois »).
    pub label: String,
    /// UID effectif (macOS, Linux).
    pub uid: Option<u32>,
    /// Processus appelant, s'il est connu.
    pub pid: Option<u32>,
    /// Autorisé à modifier la configuration (voir [`edit_policy`]).
    pub may_edit: bool,
}

impl Caller {
    /// Appelant interne au processus, autorisé à tout (tests, outils).
    pub fn trusted(label: &str) -> Self {
        Self {
            label: label.into(),
            uid: None,
            pid: Some(std::process::id()),
            may_edit: true,
        }
    }

    /// Appelant identifié par son UID, avec la règle d'édition du système.
    #[cfg(unix)]
    pub fn from_uid(uid: u32, pid: Option<u32>) -> Self {
        #[cfg(target_os = "macos")]
        let may_edit = crate::auth::uid_in_group(uid, "admin");
        #[cfg(not(target_os = "macos"))]
        let may_edit = uid == 0 || uid == crate::auth::euid();
        Self {
            label: format!("uid {uid}"),
            uid: Some(uid),
            pid,
            may_edit,
        }
    }
}

/// Qui peut modifier la configuration sur ce système (message d'erreur destiné à l'utilisateur).
pub fn edit_policy() -> &'static str {
    if cfg!(target_os = "macos") {
        "réservé aux administrateurs de ce Mac (groupe admin)"
    } else if cfg!(windows) {
        "réservé aux administrateurs (application lancée en tant qu'administrateur) et aux membres du groupe local OpenLW"
    } else {
        "réservé à l'utilisateur qui exécute le service OpenLW"
    }
}

/// Gestionnaire de requêtes : JSON de la requête et appelant → JSON de la réponse.
pub type Handler = Box<dyn Fn(&str, &Caller) -> String + Send + Sync + 'static>;

/// Point d'accès du canal de contrôle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Endpoint {
    /// Service Mach.
    #[cfg(target_os = "macos")]
    Mach {
        /// Nom du service.
        name: String,
        /// Domaine système (LaunchDaemon) plutôt qu'utilisateur (LaunchAgent).
        privileged: bool,
    },
    /// Socket Unix.
    #[cfg(unix)]
    Socket(PathBuf),
    /// Tube nommé local (`\\.\pipe\<nom>`).
    #[cfg(windows)]
    Pipe(String),
}

impl Endpoint {
    /// Point d'accès du service installé sur ce système.
    pub fn service() -> Self {
        #[cfg(target_os = "macos")]
        {
            Self::Mach {
                name: SERVICE_NAME.into(),
                privileged: true,
            }
        }
        #[cfg(all(unix, not(target_os = "macos")))]
        {
            Self::Socket(runtime_dir().join("openlw").join("control.sock"))
        }
        #[cfg(windows)]
        {
            Self::Pipe(SERVICE_NAME.into())
        }
    }

    /// Lit `mach:NOM`, `mach-user:NOM` (macOS), `unix:CHEMIN` (macOS, Linux), `pipe:NOM` (Windows),
    /// ou `service` (point d'accès du service installé).
    pub fn parse(s: &str) -> Result<Self, Error> {
        if s == "service" {
            return Ok(Self::service());
        }
        let (scheme, rest) = s.split_once(':').ok_or_else(|| {
            Error(format!(
                "point d'accès « {s} » : forme schéma:valeur attendue"
            ))
        })?;
        if rest.is_empty() {
            return Err(Error(format!("point d'accès « {s} » : valeur vide")));
        }
        match scheme {
            #[cfg(target_os = "macos")]
            "mach" => Ok(Self::Mach {
                name: rest.into(),
                privileged: true,
            }),
            #[cfg(target_os = "macos")]
            "mach-user" => Ok(Self::Mach {
                name: rest.into(),
                privileged: false,
            }),
            #[cfg(unix)]
            "unix" => Ok(Self::Socket(PathBuf::from(rest))),
            #[cfg(windows)]
            "pipe" => Ok(Self::Pipe(rest.into())),
            other => Err(Error(format!(
                "point d'accès « {other}: » non pris en charge sur ce système"
            ))),
        }
    }
}

impl fmt::Display for Endpoint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            #[cfg(target_os = "macos")]
            Self::Mach {
                name,
                privileged: true,
            } => write!(f, "mach:{name}"),
            #[cfg(target_os = "macos")]
            Self::Mach {
                name,
                privileged: false,
            } => write!(f, "mach-user:{name}"),
            #[cfg(unix)]
            Self::Socket(p) => write!(f, "unix:{}", p.display()),
            #[cfg(windows)]
            Self::Pipe(n) => write!(f, "pipe:{n}"),
        }
    }
}

/// Répertoire d'exécution de l'utilisateur : `$XDG_RUNTIME_DIR`, sinon `/run/user/<uid>`, sinon le
/// répertoire temporaire.
#[cfg(unix)]
pub fn runtime_dir() -> PathBuf {
    if let Some(d) = std::env::var_os("XDG_RUNTIME_DIR").filter(|d| !d.is_empty()) {
        return PathBuf::from(d);
    }
    let run = PathBuf::from(format!("/run/user/{}", crate::auth::euid()));
    if run.is_dir() {
        run
    } else {
        std::env::temp_dir()
    }
}

/// Serveur du canal de contrôle. Arrêté au `drop` (attend la fin des requêtes en cours).
pub struct Server {
    inner: ServerInner,
}

enum ServerInner {
    #[cfg(target_os = "macos")]
    Xpc(crate::xpc::Server),
    #[cfg(unix)]
    Socket(#[allow(dead_code)] unix::SocketServer), // gardé pour son arrêt au drop
    #[cfg(windows)]
    Pipe(windows::PipeServer),
}

impl Server {
    /// Démarre le serveur sur `endpoint`.
    pub fn start(endpoint: &Endpoint, handler: Handler) -> Result<Self, Error> {
        let inner = match endpoint {
            #[cfg(target_os = "macos")]
            Endpoint::Mach { name, .. } => ServerInner::Xpc(
                crate::xpc::Server::start(
                    Some(name),
                    Box::new(move |req, uid| handler(req, &Caller::from_uid(uid, None))),
                )
                .map_err(|e| Error(e.0))?,
            ),
            #[cfg(unix)]
            Endpoint::Socket(path) => {
                ServerInner::Socket(unix::SocketServer::start(path, handler)?)
            }
            #[cfg(windows)]
            Endpoint::Pipe(name) => ServerInner::Pipe(windows::PipeServer::start(name, handler)?),
        };
        Ok(Self { inner })
    }

    /// Serveur XPC anonyme (tests, même processus) : se joindre par [`Server::xpc`] et
    /// [`crate::xpc::Client::from_endpoint`].
    #[cfg(target_os = "macos")]
    pub fn anonymous_xpc(handler: Handler) -> Result<Self, Error> {
        let server = crate::xpc::Server::start(
            None,
            Box::new(move |req, uid| handler(req, &Caller::from_uid(uid, None))),
        )
        .map_err(|e| Error(e.0))?;
        Ok(Self {
            inner: ServerInner::Xpc(server),
        })
    }

    /// Serveur XPC sous-jacent, s'il y en a un.
    #[cfg(target_os = "macos")]
    pub fn xpc(&self) -> Option<&crate::xpc::Server> {
        match &self.inner {
            ServerInner::Xpc(s) => Some(s),
            ServerInner::Socket(_) => None,
        }
    }

    /// Région partagée remise aux clients qui la demandent (requête `attach` avec la région) :
    /// objet joint à la réponse XPC (macOS), section dupliquée dans le processus du client et
    /// décrite dans le champ `shmem` de la réponse (Windows). Sans effet sur la socket Unix.
    pub fn set_region(&self, region: &Arc<Region>) {
        match &self.inner {
            #[cfg(target_os = "macos")]
            ServerInner::Xpc(s) => s.set_shmem(region),
            #[cfg(unix)]
            ServerInner::Socket(_) => {
                let _ = region;
            }
            #[cfg(windows)]
            ServerInner::Pipe(s) => s.set_region(region.clone()),
        }
    }
}

/// Client du canal de contrôle.
pub struct Client {
    inner: ClientInner,
}

enum ClientInner {
    #[cfg(target_os = "macos")]
    Xpc(crate::xpc::Client),
    #[cfg(unix)]
    Socket(unix::SocketClient),
    #[cfg(windows)]
    Pipe(windows::PipeClient),
}

impl Client {
    /// Connexion à `endpoint`.
    pub fn connect(endpoint: &Endpoint) -> Result<Self, Error> {
        let inner = match endpoint {
            #[cfg(target_os = "macos")]
            Endpoint::Mach { name, privileged } => ClientInner::Xpc(
                crate::xpc::Client::connect(name, *privileged).map_err(|e| Error(e.0))?,
            ),
            #[cfg(unix)]
            Endpoint::Socket(path) => ClientInner::Socket(unix::SocketClient::connect(path)?),
            #[cfg(windows)]
            Endpoint::Pipe(name) => ClientInner::Pipe(windows::PipeClient::connect(name)?),
        };
        Ok(Self { inner })
    }

    /// Requête synchrone : JSON envoyé (sur une ligne), JSON reçu.
    pub fn call(&self, request: &str) -> Result<String, Error> {
        match &self.inner {
            #[cfg(target_os = "macos")]
            ClientInner::Xpc(c) => c.call(request).map_err(|e| Error(e.0)),
            #[cfg(unix)]
            ClientInner::Socket(c) => c.call(request),
            #[cfg(windows)]
            ClientInner::Pipe(c) => c.call(request),
        }
    }

    /// Requête qui demande aussi la région partagée (macOS par XPC, Windows par tube nommé).
    #[cfg(any(target_os = "macos", windows))]
    pub fn call_with_region(
        &self,
        request: &str,
    ) -> Result<(String, Option<crate::shm::SharedObject>), Error> {
        match &self.inner {
            #[cfg(target_os = "macos")]
            ClientInner::Xpc(c) => c.call_with_shmem(request).map_err(|e| Error(e.0)),
            #[cfg(target_os = "macos")]
            ClientInner::Socket(_) => Err(Error(
                "la région partagée ne passe que par XPC sur macOS".into(),
            )),
            #[cfg(windows)]
            ClientInner::Pipe(c) => c.call_with_region(request),
        }
    }
}

fn check_line(request: &str) -> Result<(), Error> {
    if request.contains('\n') {
        Err(Error(
            "requête sur plusieurs lignes (JSON compact attendu)".into(),
        ))
    } else {
        Ok(())
    }
}

/// Socket Unix (macOS, Linux) : une connexion par client, un thread par connexion.
#[cfg(unix)]
mod unix {
    use std::io::{BufRead, BufReader, ErrorKind, Write};
    use std::os::unix::fs::PermissionsExt;
    use std::os::unix::io::AsRawFd;
    use std::os::unix::net::{UnixListener, UnixStream};
    use std::panic::{catch_unwind, AssertUnwindSafe};
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Condvar, Mutex, PoisonError};
    use std::thread::JoinHandle;
    use std::time::Duration;

    use super::{check_line, Caller, Error, Handler};

    /// Intervalle de vérification de l'arrêt (attente d'une connexion, d'une requête).
    const POLL: Duration = Duration::from_millis(100);
    const MAX_REQUEST_BYTES: usize = 1 << 20;

    struct Shared {
        handler: Handler,
        stop: AtomicBool,
        active: Mutex<usize>,
        idle: Condvar,
    }

    pub(super) struct SocketServer {
        path: PathBuf,
        shared: Arc<Shared>,
        accept: Option<JoinHandle<()>>,
    }

    fn peer(stream: &UnixStream) -> Caller {
        let (mut uid, mut pid) = (u32::MAX, 0u32);
        // SAFETY: descripteur valide (socket connectée possédée par `stream`) ; pointeurs de sortie valides.
        let rc = unsafe { crate::ffi::lw_peer_cred(stream.as_raw_fd(), &mut uid, &mut pid) };
        if rc != 0 {
            return Caller {
                label: "pair inconnu".into(),
                uid: None,
                pid: None,
                may_edit: false,
            };
        }
        Caller::from_uid(uid, (pid != 0).then_some(pid))
    }

    fn serve_connection(shared: &Shared, stream: UnixStream) {
        let caller = peer(&stream);
        if stream.set_read_timeout(Some(POLL)).is_err() {
            return;
        }
        let Ok(mut writer) = stream.try_clone() else {
            return;
        };
        let mut reader = BufReader::new(stream);
        let mut line = Vec::new();
        while !shared.stop.load(Ordering::Relaxed) {
            match reader.read_until(b'\n', &mut line) {
                Ok(0) => break,
                Ok(_) if line.last() == Some(&b'\n') => {
                    line.pop();
                    let request = String::from_utf8_lossy(&line).into_owned();
                    line.clear();
                    let response =
                        catch_unwind(AssertUnwindSafe(|| (shared.handler)(&request, &caller)))
                            .unwrap_or_else(|_| {
                                r#"{"ok":false,"error":"panique dans le gestionnaire"}"#.to_string()
                            });
                    let mut out = response.replace('\n', " ");
                    out.push('\n');
                    if writer.write_all(out.as_bytes()).is_err() {
                        break;
                    }
                }
                Ok(_) => break, // fin de flux au milieu d'une ligne
                Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {
                    if line.len() > MAX_REQUEST_BYTES {
                        break;
                    }
                }
                Err(e) if e.kind() == ErrorKind::Interrupted => {}
                Err(_) => break,
            }
        }
    }

    impl SocketServer {
        pub(super) fn start(path: &Path, handler: Handler) -> Result<Self, Error> {
            let err =
                |what: &str, e: std::io::Error| Error(format!("{what} {} : {e}", path.display()));
            if let Some(dir) = path.parent() {
                if !dir.as_os_str().is_empty() && !dir.exists() {
                    std::fs::create_dir_all(dir)
                        .map_err(|e| err("création du répertoire de", e))?;
                    // Répertoire privé : seul l'utilisateur du service y accède.
                    let _ = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700));
                }
            }
            if path.exists() {
                if UnixStream::connect(path).is_ok() {
                    return Err(Error(format!(
                        "{} : déjà servi par un autre processus",
                        path.display()
                    )));
                }
                std::fs::remove_file(path)
                    .map_err(|e| err("suppression de la socket orpheline", e))?;
            }
            let listener = UnixListener::bind(path).map_err(|e| err("écoute sur", e))?;
            listener
                .set_nonblocking(true)
                .map_err(|e| err("socket", e))?;
            let shared = Arc::new(Shared {
                handler,
                stop: AtomicBool::new(false),
                active: Mutex::new(0),
                idle: Condvar::new(),
            });
            let s = shared.clone();
            let accept = std::thread::Builder::new()
                .name("contrôle".into())
                .spawn(move || {
                    while !s.stop.load(Ordering::Relaxed) {
                        match listener.accept() {
                            Ok((stream, _)) => {
                                if stream.set_nonblocking(false).is_err() {
                                    continue;
                                }
                                *s.active.lock().unwrap_or_else(PoisonError::into_inner) += 1;
                                let s2 = s.clone();
                                let spawned = std::thread::Builder::new()
                                    .name("contrôle-client".into())
                                    .spawn(move || {
                                        serve_connection(&s2, stream);
                                        let mut n = s2
                                            .active
                                            .lock()
                                            .unwrap_or_else(PoisonError::into_inner);
                                        *n -= 1;
                                        s2.idle.notify_all();
                                    });
                                if spawned.is_err() {
                                    *s.active.lock().unwrap_or_else(PoisonError::into_inner) -= 1;
                                }
                            }
                            Err(e) if e.kind() == ErrorKind::WouldBlock => std::thread::sleep(POLL),
                            Err(_) => std::thread::sleep(POLL),
                        }
                    }
                })
                .map_err(|e| err("thread d'écoute de", e))?;
            Ok(Self {
                path: path.to_path_buf(),
                shared,
                accept: Some(accept),
            })
        }
    }

    impl Drop for SocketServer {
        fn drop(&mut self) {
            self.shared.stop.store(true, Ordering::Relaxed);
            if let Some(t) = self.accept.take() {
                let _ = t.join();
            }
            let mut n = self
                .shared
                .active
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            while *n > 0 {
                n = self
                    .shared
                    .idle
                    .wait(n)
                    .unwrap_or_else(PoisonError::into_inner);
            }
            drop(n);
            let _ = std::fs::remove_file(&self.path);
        }
    }

    pub(super) struct SocketClient {
        stream: Mutex<BufReader<UnixStream>>,
    }

    impl SocketClient {
        pub(super) fn connect(path: &Path) -> Result<Self, Error> {
            let stream = UnixStream::connect(path).map_err(|e| {
                Error(match e.kind() {
                    ErrorKind::NotFound | ErrorKind::ConnectionRefused => {
                        format!("service OpenLW introuvable ({})", path.display())
                    }
                    ErrorKind::PermissionDenied => {
                        format!("accès au service OpenLW refusé ({})", path.display())
                    }
                    _ => format!("connexion à {} : {e}", path.display()),
                })
            })?;
            Ok(Self {
                stream: Mutex::new(BufReader::new(stream)),
            })
        }

        pub(super) fn call(&self, request: &str) -> Result<String, Error> {
            check_line(request)?;
            let mut s = self.stream.lock().unwrap_or_else(PoisonError::into_inner);
            let lost =
                |e: std::io::Error| Error(format!("connexion au service OpenLW interrompue : {e}"));
            let mut out = request.as_bytes().to_vec();
            out.push(b'\n');
            s.get_mut().write_all(&out).map_err(lost)?;
            let mut line = Vec::new();
            s.read_until(b'\n', &mut line).map_err(lost)?;
            if line.pop() != Some(b'\n') {
                return Err(Error("connexion au service OpenLW interrompue".into()));
            }
            String::from_utf8(line).map_err(|_| Error("réponse non UTF-8".into()))
        }
    }
}

/// Tube nommé (Windows) : couche C `lw_pipe_*`.
#[cfg(windows)]
mod windows {
    use std::ffi::{c_char, c_void, CStr, CString};
    use std::panic::{catch_unwind, AssertUnwindSafe};
    use std::sync::{Arc, Mutex, PoisonError};

    use serde_json::{json, Value};

    use super::{check_line, Caller, Error, Handler, EDIT_GROUP_WINDOWS};
    use crate::shm::{Region, SharedObject};

    /// Attente maximale d'une instance libre du tube.
    const CONNECT_TIMEOUT_MS: u32 = 2000;

    struct Ctx {
        handler: Handler,
        region: Mutex<Option<Arc<Region>>>,
    }

    impl Ctx {
        fn handle(&self, request: &str, caller: &Caller) -> String {
            let response = (self.handler)(request, caller);
            let wants_region = serde_json::from_str::<Value>(request)
                .ok()
                .and_then(|v| v.get("want_shmem").and_then(Value::as_bool))
                .unwrap_or(false);
            if !wants_region {
                return response;
            }
            let region = self
                .region
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .clone();
            let (Some(region), Some(pid)) = (region, caller.pid) else {
                return response;
            };
            let Ok(mut v) = serde_json::from_str::<Value>(&response) else {
                return response;
            };
            if v.get("ok") != Some(&Value::Bool(true)) {
                return response;
            }
            if let (Some(h), Some(o)) = (region.share_with(pid), v.as_object_mut()) {
                o.insert(
                    "shmem".into(),
                    json!({ "handle": h, "size": region.size() }),
                );
            }
            v.to_string()
        }
    }

    extern "C" fn trampoline(
        req: *const c_char,
        caller: *const crate::ffi::Caller,
        ctx: *mut c_void,
    ) -> *mut c_char {
        let run = || {
            // SAFETY: `ctx` est le `Box<Ctx>` créé par `PipeServer::start`, libéré seulement après
            // `lw_pipe_server_stop`, qui attend la fin de tous les appels.
            let ctx = unsafe { &*(ctx as *const Ctx) };
            // SAFETY: la couche C passe une chaîne terminée et un appelant valides pendant l'appel.
            let (request, c) = unsafe { (CStr::from_ptr(req).to_string_lossy(), &*caller) };
            // SAFETY: `user` est une chaîne terminée (tampon de la couche C, mis à zéro puis rempli).
            let user = unsafe { CStr::from_ptr(c.user.as_ptr()) }.to_string_lossy();
            let caller = Caller {
                label: user.into_owned(),
                uid: None,
                pid: (c.pid != 0).then_some(c.pid),
                may_edit: c.may_edit != 0,
            };
            ctx.handle(&request, &caller)
        };
        let response = catch_unwind(AssertUnwindSafe(run)).unwrap_or_else(|_| {
            r#"{"ok":false,"error":"panique dans le gestionnaire"}"#.to_string()
        });
        CString::new(response.replace(['\0', '\n'], " "))
            .unwrap_or_default()
            .into_raw()
    }

    extern "C" fn free_response(p: *mut c_char) {
        if !p.is_null() {
            // SAFETY: `p` provient de `CString::into_raw` dans `trampoline`, libéré une seule fois.
            drop(unsafe { CString::from_raw(p) });
        }
    }

    pub(super) struct PipeServer {
        raw: *mut crate::ffi::PipeServer,
        ctx: *mut Ctx,
    }

    // SAFETY: la couche C protège son état ; `ctx` n'est partagé qu'en lecture (`Handler` est
    // `Send + Sync`, la région est sous mutex).
    unsafe impl Send for PipeServer {}
    // SAFETY: idem.
    unsafe impl Sync for PipeServer {}

    impl PipeServer {
        pub(super) fn start(name: &str, handler: Handler) -> Result<Self, Error> {
            let cname = CString::new(name).map_err(|_| Error("nom de tube invalide".into()))?;
            let group = CString::new(EDIT_GROUP_WINDOWS).unwrap_or_default();
            let ctx = Box::into_raw(Box::new(Ctx {
                handler,
                region: Mutex::new(None),
            }));
            // SAFETY: chaînes valides pendant l'appel (copiées par la couche C) ; rappels `extern "C"` ;
            // `ctx` reste valide jusqu'à `lw_pipe_server_stop` (voir `Drop`).
            let raw = unsafe {
                crate::ffi::lw_pipe_server_start(
                    cname.as_ptr(),
                    group.as_ptr(),
                    trampoline,
                    free_response,
                    ctx.cast(),
                )
            };
            if raw.is_null() {
                // SAFETY: le serveur n'a pas été créé, `ctx` n'est référencé nulle part ailleurs.
                drop(unsafe { Box::from_raw(ctx) });
                return Err(Error(format!(
                    "tube \\\\.\\pipe\\{name} indisponible (déjà servi par un autre processus ?)"
                )));
            }
            Ok(Self { raw, ctx })
        }

        pub(super) fn set_region(&self, region: Arc<Region>) {
            // SAFETY: `ctx` est valide tant que le serveur vit.
            let ctx = unsafe { &*self.ctx };
            *ctx.region.lock().unwrap_or_else(PoisonError::into_inner) = Some(region);
        }
    }

    impl Drop for PipeServer {
        fn drop(&mut self) {
            // SAFETY: serveur actif arrêté une seule fois ; au retour, plus aucun appel en cours.
            unsafe { crate::ffi::lw_pipe_server_stop(self.raw) };
            // SAFETY: `ctx` vient de `Box::into_raw` et n'est plus utilisé (barrière ci-dessus).
            drop(unsafe { Box::from_raw(self.ctx) });
        }
    }

    pub(super) struct PipeClient {
        raw: Mutex<*mut crate::ffi::PipeClient>,
    }

    // SAFETY: le handle de tube peut être utilisé depuis n'importe quel thread ; les appels sont
    // sérialisés par le mutex.
    unsafe impl Send for PipeClient {}
    // SAFETY: idem.
    unsafe impl Sync for PipeClient {}

    fn error_from(err: *const c_char, fallback: &str) -> Error {
        if err.is_null() {
            Error(fallback.into())
        } else {
            // SAFETY: `err` pointe vers une chaîne statique de la couche C.
            Error(
                unsafe { CStr::from_ptr(err) }
                    .to_string_lossy()
                    .into_owned(),
            )
        }
    }

    impl PipeClient {
        pub(super) fn connect(name: &str) -> Result<Self, Error> {
            let cname = CString::new(name).map_err(|_| Error("nom de tube invalide".into()))?;
            let mut err: *const c_char = std::ptr::null();
            // SAFETY: chaîne valide pendant l'appel ; pointeur de sortie valide.
            let raw = unsafe {
                crate::ffi::lw_pipe_client_connect(cname.as_ptr(), CONNECT_TIMEOUT_MS, &mut err)
            };
            if raw.is_null() {
                return Err(error_from(err, "connexion au service OpenLW impossible"));
            }
            Ok(Self {
                raw: Mutex::new(raw),
            })
        }

        pub(super) fn call(&self, request: &str) -> Result<String, Error> {
            check_line(request)?;
            let req = CString::new(request)
                .map_err(|_| Error("requête contenant un octet nul".into()))?;
            let raw = self.raw.lock().unwrap_or_else(PoisonError::into_inner);
            let mut err: *const c_char = std::ptr::null();
            // SAFETY: client ouvert, utilisé par un seul thread à la fois (mutex) ; chaînes valides.
            let out = unsafe { crate::ffi::lw_pipe_call(*raw, req.as_ptr(), &mut err) };
            if out.is_null() {
                return Err(error_from(err, "erreur du tube"));
            }
            // SAFETY: chaîne allouée par la couche C, terminée ; libérée juste après la copie.
            let s = unsafe { CStr::from_ptr(out) }
                .to_string_lossy()
                .into_owned();
            // SAFETY: `out` vient de malloc dans la couche C, libéré une seule fois.
            unsafe { crate::ffi::lw_free(out) };
            Ok(s)
        }

        pub(super) fn call_with_region(
            &self,
            request: &str,
        ) -> Result<(String, Option<SharedObject>), Error> {
            let mut v: Value = serde_json::from_str(request)
                .map_err(|e| Error(format!("requête JSON invalide : {e}")))?;
            if let Some(o) = v.as_object_mut() {
                o.insert("want_shmem".into(), Value::Bool(true));
            }
            let response = self.call(&v.to_string())?;
            let handle = serde_json::from_str::<Value>(&response)
                .ok()
                .and_then(|r| r.get("shmem")?.get("handle")?.as_u64());
            Ok((response, handle.and_then(SharedObject::from_handle_value)))
        }
    }

    impl Drop for PipeClient {
        fn drop(&mut self) {
            let raw = *self.raw.get_mut().unwrap_or_else(PoisonError::into_inner);
            // SAFETY: client ouvert par `connect`, fermé une seule fois.
            unsafe { crate::ffi::lw_pipe_client_close(raw) };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn echo() -> Handler {
        Box::new(|req: &str, c: &Caller| {
            format!(
                "{{\"echo\":{req},\"may_edit\":{},\"pid\":{}}}",
                c.may_edit,
                c.pid.unwrap_or(0)
            )
        })
    }

    #[cfg(unix)]
    fn test_endpoint(tag: &str) -> Endpoint {
        Endpoint::Socket(
            std::env::temp_dir().join(format!("openlw-test-{}-{tag}.sock", std::process::id())),
        )
    }

    #[cfg(windows)]
    fn test_endpoint(tag: &str) -> Endpoint {
        Endpoint::Pipe(format!(
            "fr.francois-brille.openlw.test.{}.{tag}",
            std::process::id()
        ))
    }

    #[test]
    fn endpoint_parse_and_display() {
        assert_eq!(Endpoint::parse("service").unwrap(), Endpoint::service());
        assert!(Endpoint::parse("nimporte").is_err());
        assert!(Endpoint::parse("ftp:x").is_err());
        let ep = test_endpoint("parse");
        assert_eq!(Endpoint::parse(&ep.to_string()).unwrap(), ep);
    }

    #[test]
    fn roundtrip_identifies_caller() {
        let ep = test_endpoint("roundtrip");
        let server = Server::start(&ep, echo()).unwrap();
        assert!(Server::start(&ep, echo()).is_err(), "nom déjà servi");
        let client = Client::connect(&ep).unwrap();
        for i in 0..50 {
            let resp = client.call(&format!("{{\"n\":{i}}}")).unwrap();
            let v: serde_json::Value = serde_json::from_str(&resp).unwrap();
            assert_eq!(v["echo"]["n"], i);
            assert_eq!(v["pid"], std::process::id(), "processus appelant identifié");
        }
        assert!(
            client.call("{\n}").is_err(),
            "requête sur plusieurs lignes refusée"
        );
        drop(client);
        drop(server);
        assert!(Client::connect(&ep).is_err(), "serveur arrêté");
    }

    #[test]
    fn concurrent_clients() {
        let ep = test_endpoint("concurrent");
        let _server = Server::start(&ep, echo()).unwrap();
        let threads: Vec<_> = (0..8)
            .map(|t| {
                let ep = ep.clone();
                std::thread::spawn(move || {
                    let c = Client::connect(&ep).unwrap();
                    for i in 0..20 {
                        let r = c.call(&format!("{{\"t\":{t},\"i\":{i}}}")).unwrap();
                        assert!(r.contains(&format!("\"t\":{t},\"i\":{i}")));
                    }
                })
            })
            .collect();
        for t in threads {
            t.join().unwrap();
        }
    }

    #[test]
    fn handler_panic_is_contained() {
        let ep = test_endpoint("panic");
        let _server = Server::start(&ep, Box::new(|_: &str, _: &Caller| panic!("test"))).unwrap();
        let client = Client::connect(&ep).unwrap();
        assert!(client.call("{}").unwrap().contains("panique"));
    }

    /// Sous Linux, l'utilisateur du service peut modifier ; sous macOS, seuls les administrateurs.
    #[cfg(unix)]
    #[test]
    fn edit_rule_follows_uid() {
        let me = crate::auth::euid();
        let c = Caller::from_uid(me, None);
        if cfg!(target_os = "macos") {
            assert_eq!(c.may_edit, crate::auth::uid_in_group(me, "admin"));
        } else {
            assert!(c.may_edit);
        }
        assert!(Caller::from_uid(0, None).may_edit, "root toujours autorisé");
    }

    #[cfg(windows)]
    #[test]
    fn region_travels_with_attach() {
        let ep = test_endpoint("region");
        let server = Server::start(
            &ep,
            Box::new(|_: &str, _: &Caller| r#"{"ok":true,"device":{}}"#.to_string()),
        )
        .unwrap();
        let region = Arc::new(Region::create(48_000, 256, 2, 2).unwrap());
        server.set_region(&region);
        let client = Client::connect(&ep).unwrap();
        let (resp, obj) = client.call_with_region(r#"{"cmd":"attach"}"#).unwrap();
        assert!(resp.contains("\"shmem\""));
        let mapped = Region::map(obj.expect("section reçue")).unwrap();
        assert_eq!(mapped.geometry(), region.geometry());
    }
}
