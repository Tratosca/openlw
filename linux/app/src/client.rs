//! Control channel client: one JSON request per line over the user service Unix socket
//! `$XDG_RUNTIME_DIR/openlw/control.sock`. The socket directory belongs to the user;
//! the service identifies the caller through `SO_PEERCRED`.

use std::io::{self, BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::{Mutex, PoisonError};
use std::time::Duration;

use serde_json::Value;

use crate::i18n::{tr, trf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DaemonError {
    Unreachable,
    Decoding,
    Refused(String),
}

impl DaemonError {
    /// Displayed message: what happened, then what the user can do.
    pub fn message(&self) -> String {
        match self {
            DaemonError::Unreachable => tr(
                "The OpenLW service is not responding. If the problem persists, reinstall OpenLW.",
            )
            .into(),
            DaemonError::Decoding => tr(
                "Unexpected response from the OpenLW service. Reinstall OpenLW to update the app and the service.",
            )
            .into(),
            // `reason`: daemon message (English) or app reason (translated by the caller).
            // Daemon reasons may lack the final period of an error message.
            DaemonError::Refused(reason) => {
                let mut m = trf("Could not apply the change: {reason}", &[("reason", reason)]);
                if !m.ends_with(['.', '!', '?']) {
                    m.push('.');
                }
                m
            }
        }
    }
}

/// Service socket: `$XDG_RUNTIME_DIR/openlw/control.sock`, as published by `lw-daemon run --control`.
pub fn socket_path() -> PathBuf {
    let runtime = std::env::var_os("XDG_RUNTIME_DIR")
        .filter(|d| !d.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(gtk::glib::user_runtime_dir);
    runtime.join("openlw").join("control.sock")
}

type Connection = (UnixStream, BufReader<UnixStream>);

/// Connection kept between requests, reopened when the service restarts.
#[derive(Default)]
pub struct Client {
    conn: Mutex<Option<Connection>>,
}

impl Client {
    /// Sends `request` and returns the reply (`"ok": true`). Blocking: call off the UI thread.
    pub fn call(&self, request: &Value) -> Result<Value, DaemonError> {
        let mut conn = self.conn.lock().unwrap_or_else(PoisonError::into_inner);
        // One reconnection attempt: the service may have restarted since the last call.
        for _ in 0..2 {
            match exchange(&mut conn, request) {
                Ok(line) => return parse(&line),
                Err(_) => *conn = None,
            }
        }
        Err(DaemonError::Unreachable)
    }
}

fn exchange(conn: &mut Option<Connection>, request: &Value) -> io::Result<String> {
    if conn.is_none() {
        let s = UnixStream::connect(socket_path())?;
        s.set_read_timeout(Some(Duration::from_secs(5)))?;
        s.set_write_timeout(Some(Duration::from_secs(5)))?;
        let reader = BufReader::new(s.try_clone()?);
        *conn = Some((s, reader));
    }
    let Some((stream, reader)) = conn.as_mut() else {
        return Err(io::ErrorKind::NotConnected.into());
    };
    let mut line = request.to_string();
    line.push('\n');
    stream.write_all(line.as_bytes())?;
    let mut reply = String::new();
    if reader.read_line(&mut reply)? == 0 {
        return Err(io::ErrorKind::UnexpectedEof.into());
    }
    Ok(reply)
}

fn parse(line: &str) -> Result<Value, DaemonError> {
    let v: Value = serde_json::from_str(line).map_err(|_| DaemonError::Decoding)?;
    if !v.is_object() {
        return Err(DaemonError::Decoding);
    }
    if v.get("ok").and_then(Value::as_bool) == Some(true) {
        Ok(v)
    } else {
        let reason = v
            .get("error")
            .and_then(Value::as_str)
            .unwrap_or("unknown error");
        Err(DaemonError::Refused(reason.into()))
    }
}

#[cfg(test)]
#[allow(clippy::indexing_slicing)]
mod tests {
    use super::*;
    use std::os::unix::net::UnixListener;

    #[test]
    fn replies_and_errors() {
        assert!(parse(r#"{"ok":true,"status":{}}"#).is_ok());
        assert_eq!(
            parse(r#"{"ok":false,"error":"refused"}"#),
            Err(DaemonError::Refused("refused".into()))
        );
        assert_eq!(parse("not json"), Err(DaemonError::Decoding));
        assert_eq!(parse("[1]"), Err(DaemonError::Decoding));
    }

    #[test]
    fn round_trip_and_reconnection() {
        let dir = std::env::temp_dir().join(format!("openlw-app-test-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("openlw")).unwrap();
        let path = dir.join("openlw").join("control.sock");
        let _ = std::fs::remove_file(&path);
        let listener = UnixListener::bind(&path).unwrap();
        // Server: answers one request per connection, then closes it (forces a reconnection).
        let server = std::thread::spawn(move || {
            for _ in 0..2 {
                let (s, _) = listener.accept().unwrap();
                let mut r = BufReader::new(s.try_clone().unwrap());
                let mut line = String::new();
                r.read_line(&mut line).unwrap();
                let req: Value = serde_json::from_str(&line).unwrap();
                let reply = serde_json::json!({"ok": true, "echo": req["cmd"]});
                (&s).write_all(format!("{reply}\n").as_bytes()).unwrap();
            }
        });
        std::env::set_var("XDG_RUNTIME_DIR", &dir);
        let c = Client::default();
        let a = c.call(&serde_json::json!({"cmd": "status"})).unwrap();
        assert_eq!(a["echo"], "status");
        let b = c.call(&serde_json::json!({"cmd": "config"})).unwrap();
        assert_eq!(b["echo"], "config");
        server.join().unwrap();
        assert_eq!(
            c.call(&serde_json::json!({"cmd": "status"})),
            Err(DaemonError::Unreachable)
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
