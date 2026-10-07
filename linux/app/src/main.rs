//! OpenLW app for Linux: GTK4 and libadwaita configuration app (ADR 0006), same sections as the
//! macOS and Windows apps. Talks to the user service through its Unix socket (ADR 0007).

mod client;
mod grid;
mod i18n;
mod listener;
mod meter;
mod models;
mod settings;
mod window;

use adw::prelude::*;
use gtk::glib;

/// GTK and D-Bus application ID (hyphens are not valid there: `francois_brille`).
pub const APP_ID: &str = "fr.francois_brille.openlw";

fn main() -> glib::ExitCode {
    let app = adw::Application::builder().application_id(APP_ID).build();
    app.connect_activate(|app| {
        // Single instance: a second launch brings the existing window forward.
        if let Some(w) = app.active_window() {
            w.present();
            return;
        }
        window::Window::build(app);
    });
    app.run()
}
