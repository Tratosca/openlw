//! Journal : stderr horodaté, journal du système (`lw_sys::log` : `os_log` sous macOS, journal des
//! événements sous Windows ; journald recueille stderr sous Linux) et, en option, un fichier
//! (service Windows, qui n'a pas de stderr).

use std::io::Write;
use std::path::Path;
use std::sync::{Mutex, PoisonError};
use std::time::{SystemTime, UNIX_EPOCH};

/// Fichier journal, s'il est demandé.
static FILE: Mutex<Option<std::fs::File>> = Mutex::new(None);

/// Au-delà, le fichier journal repart de zéro au démarrage.
const MAX_FILE_BYTES: u64 = 10 * 1024 * 1024;

pub fn stamp() -> String {
    let d = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let s = d.as_secs();
    format!(
        "{:02}:{:02}:{:02}.{:03}",
        (s / 3600) % 24,
        (s / 60) % 60,
        s % 60,
        d.subsec_millis()
    )
}

/// Copie aussi le journal dans `path` (ajout ; remis à zéro au-delà de 10 Mo).
pub fn to_file(path: &Path) -> std::io::Result<()> {
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir)?;
    }
    let big = std::fs::metadata(path).is_ok_and(|m| m.len() > MAX_FILE_BYTES);
    let f = std::fs::OpenOptions::new()
        .create(true)
        .append(!big)
        .write(true)
        .truncate(big)
        .open(path)?;
    *FILE.lock().unwrap_or_else(PoisonError::into_inner) = Some(f);
    Ok(())
}

/// Écrit sur stderr, dans le journal du système et dans le fichier journal.
pub fn emit(level: lw_sys::log::Level, message: &str) {
    let line = format!("{} {message}", stamp());
    eprintln!("{line}");
    if let Some(f) = FILE.lock().unwrap_or_else(PoisonError::into_inner).as_mut() {
        let _ = writeln!(f, "{line}");
    }
    lw_sys::log::write(level, "daemon", message);
}

#[macro_export]
macro_rules! info {
    ($($arg:tt)*) => { $crate::log::emit(lw_sys::log::Level::Info, &format!($($arg)*)) };
}

#[macro_export]
macro_rules! error {
    ($($arg:tt)*) => { $crate::log::emit(lw_sys::log::Level::Error, &format!($($arg)*)) };
}
