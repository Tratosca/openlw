//! App-only preferences (manually entered channels, advanced settings visibility, dismissed
//! warning), stored in `~/.config/openlw/app.json`. Network settings live in the service
//! configuration.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManualSource {
    pub channel: u16,
    #[serde(default = "stereo")]
    pub kind: String,
}

fn stereo() -> String {
    "stereo".into()
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Settings {
    #[serde(default)]
    pub manual: Vec<ManualSource>,
    #[serde(default)]
    pub advanced_visible: bool,
    /// Warning before a device width change (brief audio cut) not to be shown again.
    #[serde(default)]
    pub skip_width_warning: bool,
}

fn path() -> PathBuf {
    gtk::glib::user_config_dir().join("openlw").join("app.json")
}

impl Settings {
    pub fn load() -> Self {
        std::fs::read(path())
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default()
    }

    /// Preferences only: losing them is harmless, errors are ignored.
    pub fn save(&self) {
        let p = path();
        let tmp = p.with_extension("json.tmp");
        let ok = p
            .parent()
            .map(std::fs::create_dir_all)
            .is_some_and(|r| r.is_ok())
            && serde_json::to_vec(self)
                .ok()
                .is_some_and(|b| std::fs::write(&tmp, b).is_ok());
        if ok {
            let _ = std::fs::rename(&tmp, &p);
        }
    }
}
