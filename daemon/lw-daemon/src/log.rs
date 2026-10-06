//! Journal : stderr horodaté (usage interactif) et journal unifié `os_log`
//! (`log stream --predicate 'subsystem == "fr.francois-brille.openlw"'`).

use std::time::{SystemTime, UNIX_EPOCH};

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

/// Écrit sur stderr et dans le journal unifié.
pub fn emit(level: lw_sys::log::Level, message: &str) {
    eprintln!("{} {message}", stamp());
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
