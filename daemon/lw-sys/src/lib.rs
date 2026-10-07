//! Daemon system layer: safe wrappers around C layer (`csrc/`), one implementation
//! per system (macOS, Linux, Windows).
//!
//! - [`rt`]: real-time threads, precise sleep, host clock.
//! - [`log`]: system log (`os_log`, Windows Event Log).
//! - [`shm`]: region shared with audio client (rings, clock), `csrc/lw_shm.h` contract.
//! - [`ctl`]: JSON control channel: XPC (macOS), Unix socket (macOS/Linux), named pipe (Windows).
//! - [`xpc`]: XPC service (macOS), used by [`ctl`] and HAL plugin.

pub mod ctl;
pub mod shm;

mod ffi {
    use std::ffi::{c_char, c_int, c_void};

    /// Host clock description (`lw_host_clock`).
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
        pub fn lw_ring_skip(base: *mut c_void, dir: c_int, frames: u32) -> u32;
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

        /// Named-pipe caller (`lw_caller`).
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
            pub fn lw_process_alive(pid: u32, name: *mut c_char, cap: usize) -> c_int;
        }
    }
}

/// Real-time scheduling and host clock.
pub mod rt {
    use std::time::Duration;

    /// Promote calling thread to real-time: it must execute `computation` within
    /// each `period`, finishing no later than `constraint` after period start.
    /// macOS: `THREAD_TIME_CONSTRAINT_POLICY`; Linux: `SCHED_FIFO` (requires `CAP_SYS_NICE` or
    /// `RLIMIT_RTPRIO`); Windows: MMCSS “Pro Audio”. Error is system code.
    pub fn promote(
        period: Duration,
        computation: Duration,
        constraint: Duration,
    ) -> Result<(), i32> {
        let ns = |d: Duration| u64::try_from(d.as_nanos()).unwrap_or(u64::MAX);
        // SAFETY: pointer-free C function; affects only calling thread.
        let rc = unsafe { super::ffi::lw_rt_promote(ns(period), ns(computation), ns(constraint)) };
        if rc == 0 {
            Ok(())
        } else {
            Err(rc)
        }
    }

    /// Precise sleep without busy-waiting: only acceptable waiting mode in a real-time thread.
    pub fn sleep(d: Duration) {
        let ns = u64::try_from(d.as_nanos()).unwrap_or(u64::MAX);
        // SAFETY: pointer-free C function; blocks only calling thread.
        unsafe { super::ffi::lw_sleep_ns(ns) };
    }

    /// Host clock in nanoseconds (audio client timebase).
    pub fn host_time_ns() -> u64 {
        // SAFETY: pointer-free C functions.
        unsafe { super::ffi::lw_host_time_to_ns(super::ffi::lw_host_time()) }
    }

    /// Convert raw host-tick duration to nanoseconds.
    pub fn host_time_ns_of(raw: u64) -> u64 {
        // SAFETY: pure C function.
        unsafe { super::ffi::lw_host_time_to_ns(raw) }
    }

    /// Raw host clock (`mach_absolute_time`, `QueryPerformanceCounter`, `CLOCK_MONOTONIC`),
    /// unit expected in shared region.
    pub fn host_time() -> u64 {
        // SAFETY: pointer-free C function.
        unsafe { super::ffi::lw_host_time() }
    }

    /// Typical parameters for a thread transmitting one packet per `interval`:
    /// compute ≤ 1/8 period (at least 100 µs), deadline at half-period.
    pub fn promote_for_packet_interval(interval: Duration) -> Result<(), i32> {
        let computation = (interval / 8).max(Duration::from_micros(100));
        let constraint = (interval / 2).max(computation);
        promote(interval, computation, constraint)
    }
}

/// System logging.
pub mod log {
    /// Message level.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum Level {
        /// Debug (not retained by default).
        Debug = 0,
        /// Information.
        Info = 1,
        /// Default (retained).
        Default = 2,
        /// Error.
        Error = 3,
        /// Fault.
        Fault = 4,
    }

    /// Write `message` to system log: unified logging on macOS
    /// (`log stream --predicate 'subsystem == "fr.francois-brille.openlw"'`), debugger and Event Log
    /// (OpenLW source) on Windows. No effect on Linux, where journald collects stderr.
    pub fn write(level: Level, category: &str, message: &str) {
        use std::ffi::CString;
        let cat = CString::new(category.replace('\0', " ")).unwrap_or_default();
        let msg = CString::new(message.replace('\0', " ")).unwrap_or_default();
        // SAFETY: two valid terminated C strings, alive during call; C layer copies them.
        unsafe { super::ffi::lw_log(level as i32, cat.as_ptr(), msg.as_ptr()) };
    }
}

/// Authorization of UID-identified callers (macOS/Linux).
#[cfg(unix)]
pub mod auth {
    /// `true` if `uid` is root or belongs to `group` (“admin”: macOS administrators).
    pub fn uid_in_group(uid: u32, group: &str) -> bool {
        let Ok(g) = std::ffi::CString::new(group) else {
            return false;
        };
        // SAFETY: C string valid during call; C function uses only local buffers.
        unsafe { super::ffi::lw_uid_in_group(uid, g.as_ptr()) == 1 }
    }

    /// Current process effective UID.
    pub fn euid() -> u32 {
        // SAFETY: C function with no arguments.
        unsafe { super::ffi::lw_geteuid() }
    }
}

/// DSCP marking through qWAVE (Windows ignores `IP_TOS`).
#[cfg(windows)]
pub mod qos {
    use std::net::{SocketAddrV4, UdpSocket};
    use std::os::windows::io::AsRawSocket;

    /// Socket qWAVE flow: marking lasts while value lives (release before socket).
    pub struct Flow(*mut std::ffi::c_void);

    // SAFETY: flow manipulated only at closure, from any thread.
    unsafe impl Send for Flow {}

    /// Mark `dscp` (0–63) on `sock` sends to `dest`. Error is Windows code
    /// (access denied outside administrator/service context).
    pub fn set_dscp(sock: &UdpSocket, dest: SocketAddrV4, dscp: u8) -> Result<Flow, i32> {
        let ip = dest.ip().octets();
        let mut err = 0;
        // SAFETY: valid socket borrowed during call; `ip` contains four bytes; output
        // pointer valid.
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
            // SAFETY: flow created by `lw_qos_dscp`, closed once.
            unsafe { super::ffi::lw_qos_close(self.0) };
        }
    }
}

/// Windows service: Service Control Manager (SCM) integration.
#[cfg(windows)]
pub mod service {
    use std::ffi::OsString;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex, OnceLock, PoisonError};
    use std::time::Duration;

    use windows_service::service::{
        ServiceControl, ServiceControlAccept, ServiceExitCode, ServiceState, ServiceStatus,
        ServiceType,
    };
    use windows_service::service_control_handler::{self, ServiceControlHandlerResult};
    use windows_service::{define_windows_service, service_dispatcher};

    /// Service body: receives stop flag, set when SCM requests stop.
    pub type Body = Box<dyn FnOnce(Arc<AtomicBool>) -> Result<(), String> + Send>;

    static NAME: OnceLock<String> = OnceLock::new();
    static BODY: Mutex<Option<Body>> = Mutex::new(None);

    define_windows_service!(ffi_service_main, service_main);

    fn service_main(_args: Vec<OsString>) {
        let name = NAME.get().cloned().unwrap_or_default();
        let stop = Arc::new(AtomicBool::new(false));
        let flag = stop.clone();
        let handler = move |ev| match ev {
            ServiceControl::Stop | ServiceControl::Shutdown => {
                flag.store(true, Ordering::Relaxed);
                ServiceControlHandlerResult::NoError
            }
            ServiceControl::Interrogate => ServiceControlHandlerResult::NoError,
            _ => ServiceControlHandlerResult::NotImplemented,
        };
        let Ok(status) = service_control_handler::register(&name, handler) else {
            return;
        };
        let report = |state, accept, code| {
            let _ = status.set_service_status(ServiceStatus {
                service_type: ServiceType::OWN_PROCESS,
                current_state: state,
                controls_accepted: accept,
                exit_code: ServiceExitCode::Win32(code),
                checkpoint: 0,
                wait_hint: Duration::from_secs(5),
                process_id: None,
            });
        };
        report(
            ServiceState::Running,
            ServiceControlAccept::STOP | ServiceControlAccept::SHUTDOWN,
            0,
        );
        let body = BODY.lock().unwrap_or_else(PoisonError::into_inner).take();
        let result = body.map_or(Ok(()), |b| b(stop));
        report(
            ServiceState::Stopped,
            ServiceControlAccept::empty(),
            u32::from(result.is_err()),
        );
    }

    /// Run `body` as service `name`; return only when service stops. Fail if
    /// process was not started by Service Control Manager.
    pub fn run(name: &str, body: Body) -> Result<(), String> {
        let _ = NAME.set(name.to_string());
        *BODY.lock().unwrap_or_else(PoisonError::into_inner) = Some(body);
        service_dispatcher::start(name, ffi_service_main)
            .map_err(|e| format!("Windows Service Control Manager: {e}"))
    }
}

/// XPC control (JSON requests/responses), macOS.
#[cfg(target_os = "macos")]
pub mod xpc {
    use std::ffi::{c_char, c_void, CStr, CString};
    use std::fmt;
    use std::panic::{catch_unwind, AssertUnwindSafe};

    /// Request handler: receives request JSON and caller effective UID,
    /// returns response JSON.
    pub type Handler = Box<dyn Fn(&str, u32) -> String + Send + Sync + 'static>;

    /// XPC error.
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
            // SAFETY: `ctx` is the `Box<Handler>` created by `Server::start`, freed only after
            // `lw_xpc_server_stop`, which guarantees no active calls.
            let handler = unsafe { &*(ctx as *const Handler) };
            // SAFETY: C layer passes a terminated string valid during call.
            let request = unsafe { CStr::from_ptr(req) }.to_string_lossy();
            handler(&request, uid)
        };
        // A panic must never cross the C boundary.
        let response = catch_unwind(AssertUnwindSafe(run))
            .unwrap_or_else(|_| r#"{"ok":false,"error":"panic in handler"}"#.to_string());
        CString::new(response.replace('\0', " "))
            .unwrap_or_default()
            .into_raw()
    }

    extern "C" fn free_response(p: *mut c_char) {
        if !p.is_null() {
            // SAFETY: `p` comes from `CString::into_raw` in `trampoline`, freed only here, once.
            drop(unsafe { CString::from_raw(p) });
        }
    }

    /// XPC listener. Stopped and freed on `drop`.
    pub struct Server {
        raw: *mut super::ffi::Server,
        ctx: *mut Handler,
    }

    // SAFETY: C layer serializes calls on its queue; `ctx` is read only by handler,
    // which is `Send + Sync`.
    unsafe impl Send for Server {}
    // SAFETY: `&self` methods are thread-safe: `set_shmem` uses
    // `dispatch_sync` on listener queue, `endpoint` uses thread-safe `xpc_endpoint_create`.
    unsafe impl Sync for Server {}

    impl Server {
        /// Start named Mach listener (`Some`, service declared in launchd plist) or anonymous (`None`).
        pub fn start(mach_name: Option<&str>, handler: Handler) -> Result<Self, Error> {
            let name = mach_name
                .map(CString::new)
                .transpose()
                .map_err(|_| Error("invalid service name".into()))?;
            let ctx = Box::into_raw(Box::new(handler));
            let name_ptr = name.as_ref().map_or(std::ptr::null(), |n| n.as_ptr());
            // SAFETY: valid or null string; `extern "C"` callbacks; `ctx` stays valid
            // until `lw_xpc_server_stop` (see `Drop`).
            let raw = unsafe {
                super::ffi::lw_xpc_server_start(name_ptr, trampoline, free_response, ctx.cast())
            };
            if raw.is_null() {
                // SAFETY: listener was not created; `ctx` is referenced nowhere else.
                drop(unsafe { Box::from_raw(ctx) });
                return Err(Error("cannot create the XPC listener".into()));
            }
            Ok(Self { raw, ctx })
        }

        /// Shared region returned to requesting clients ([`Client::call_with_shmem`]).
        pub fn set_shmem(&self, region: &crate::shm::Region) {
            // SAFETY: `raw` is an active listener; region xpc_shmem object is valid and C layer
            // retains it (releases on stop or replacement).
            unsafe { super::ffi::lw_xpc_server_set_shmem(self.raw, region.handle()) };
        }

        /// Listener endpoint for a same-process client (tests).
        pub fn endpoint(&self) -> Endpoint {
            // SAFETY: `raw` is an active listener; returned object retained, freed by `Endpoint::drop`.
            Endpoint(unsafe { super::ffi::lw_xpc_server_endpoint(self.raw) })
        }
    }

    impl Drop for Server {
        fn drop(&mut self) {
            // SAFETY: `raw` is valid and stopped once; no handlers run after return,
            // so `ctx` may be freed.
            unsafe { super::ffi::lw_xpc_server_stop(self.raw) };
            // SAFETY: `ctx` comes from `Box::into_raw` in `start`, no longer used (barrier above).
            drop(unsafe { Box::from_raw(self.ctx) });
        }
    }

    /// XPC endpoint (retained object).
    pub struct Endpoint(*mut c_void);

    impl Drop for Endpoint {
        fn drop(&mut self) {
            // SAFETY: XPC object retained by `lw_xpc_server_endpoint`, released once.
            unsafe { super::ffi::lw_xpc_release(self.0) };
        }
    }

    /// Client connection.
    pub struct Client {
        raw: *mut super::ffi::Client,
    }

    // SAFETY: an XPC connection can be used from any thread.
    unsafe impl Send for Client {}
    // SAFETY: `xpc_connection_send_message_with_reply_sync` can be called from multiple threads.
    unsafe impl Sync for Client {}

    fn error_from(err: *const c_char) -> Error {
        if err.is_null() {
            Error("XPC error".into())
        } else {
            // SAFETY: `err` points to a static C-layer string.
            Error(
                unsafe { CStr::from_ptr(err) }
                    .to_string_lossy()
                    .into_owned(),
            )
        }
    }

    fn take_string(out: *mut c_char) -> String {
        // SAFETY: `out` is a terminated strdup-allocated string; freed immediately after copying.
        let s = unsafe { CStr::from_ptr(out) }
            .to_string_lossy()
            .into_owned();
        // SAFETY: `out` comes from strdup in C layer, freed once by `lw_free`.
        unsafe { super::ffi::lw_free(out) };
        s
    }

    impl Client {
        /// Connect to Mach service (`privileged`: system-domain LaunchDaemon service).
        pub fn connect(mach_name: &str, privileged: bool) -> Result<Self, Error> {
            let name = CString::new(mach_name).map_err(|_| Error("invalid service name".into()))?;
            // SAFETY: string valid during call; C layer copies name.
            let raw =
                unsafe { super::ffi::lw_xpc_client_mach(name.as_ptr(), i32::from(privileged)) };
            if raw.is_null() {
                return Err(Error("cannot open the XPC connection".into()));
            }
            Ok(Self { raw })
        }

        /// Connect to endpoint (same process).
        pub fn from_endpoint(endpoint: &Endpoint) -> Result<Self, Error> {
            // SAFETY: `endpoint` is a valid XPC object while `Endpoint` lives; C layer retains it.
            let raw = unsafe { super::ffi::lw_xpc_client_endpoint(endpoint.0) };
            if raw.is_null() {
                return Err(Error("cannot connect to the endpoint".into()));
            }
            Ok(Self { raw })
        }

        /// Synchronous request also asking for server shared region.
        pub fn call_with_shmem(
            &self,
            request: &str,
        ) -> Result<(String, Option<crate::shm::SharedObject>), Error> {
            let req =
                CString::new(request).map_err(|_| Error("request contains a NUL byte".into()))?;
            let mut err: *const c_char = std::ptr::null();
            let mut obj: *mut c_void = std::ptr::null_mut();
            // SAFETY: open connection; output pointers valid during call.
            let out = unsafe {
                super::ffi::lw_xpc_call_shmem(self.raw, req.as_ptr(), &mut err, &mut obj)
            };
            let shm = crate::shm::SharedObject::from_raw(obj);
            if out.is_null() {
                return Err(error_from(err));
            }
            Ok((take_string(out), shm))
        }

        /// Synchronous request: JSON sent, JSON received.
        pub fn call(&self, request: &str) -> Result<String, Error> {
            let req =
                CString::new(request).map_err(|_| Error("request contains a NUL byte".into()))?;
            let mut err: *const c_char = std::ptr::null();
            // SAFETY: `raw` is an open connection; `req` and `err` valid during call.
            let out = unsafe { super::ffi::lw_xpc_call(self.raw, req.as_ptr(), &mut err) };
            if out.is_null() {
                return Err(error_from(err));
            }
            Ok(take_string(out))
        }
    }

    impl Drop for Client {
        fn drop(&mut self) {
            // SAFETY: connection opened by `connect` or `from_endpoint`, closed once.
            unsafe { super::ffi::lw_xpc_client_close(self.raw) };
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    /// macOS/Windows allow unprivileged real-time scheduling; Linux requires `CAP_SYS_NICE` or
    /// `RLIMIT_RTPRIO`, rarely available in CI.
    #[test]
    fn rt_promotion() {
        let r =
            std::thread::spawn(|| super::rt::promote_for_packet_interval(Duration::from_millis(1)))
                .join()
                .unwrap();
        if cfg!(target_os = "linux") {
            assert!(
                r.is_ok() || r == Err(1),
                "only EPERM is accepted on Linux: {r:?}"
            );
        } else {
            r.expect("real-time promotion refused");
        }
    }

    #[test]
    fn host_clock_advances_in_nanoseconds() {
        let (t0, n0) = (super::rt::host_time(), super::rt::host_time_ns());
        std::thread::sleep(Duration::from_millis(50));
        let (t1, n1) = (super::rt::host_time(), super::rt::host_time_ns());
        let dt = super::rt::host_time_ns_of(t1 - t0);
        assert!(
            (45_000_000..500_000_000).contains(&dt),
            "host delta {dt} ns"
        );
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
        // Linux VMs/containers (CI, OrbStack) have coarse timers: measured median lateness of
        // 2.5 ms for a 500 µs sleep in pure C, with/without SCHED_FIFO. Same tolerance
        // under emulated Wine (OPENLW_COARSE_TIMERS, set by tools/ci/wine.Dockerfile).
        let coarse = std::env::var_os("OPENLW_COARSE_TIMERS").is_some();
        let bound = if cfg!(target_os = "linux") || coarse {
            Duration::from_millis(50)
        } else {
            Duration::from_millis(5)
        };
        assert!(worst < bound, "late wake-up by {worst:?}");
    }

    #[test]
    fn log_does_not_crash() {
        super::log::write(super::log::Level::Info, "test", "lw-sys: test message");
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
        assert!(client.call("{}").unwrap().contains("panic"));
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
            super::auth::uid_in_group(0, "nogroup-nonexistent"),
            "root always allowed"
        );
        assert!(
            !super::auth::uid_in_group(4_000_000, "admin"),
            "unknown UID refused"
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn xpc_unknown_service_is_an_error() {
        let client =
            super::xpc::Client::connect("fr.francois-brille.openlw.nonexistent", false).unwrap();
        assert!(client.call("{}").is_err());
    }
}
