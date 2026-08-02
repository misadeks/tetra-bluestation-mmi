// Local, UI-only preferences that must survive a UI restart but are NEVER sent
// to the stack (unlike the codeplug, which round-trips through GetConfig /
// SetConfig). Stored as a small JSON file next to the message store in the
// configured storage directory.
//
// This is the answer to "how to make it UI-only but changeable from the UI":
// keep it out of the codeplug entirely and persist it here. The UI reads it at
// startup and rewrites it whenever the user changes a setting.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

fn default_true() -> bool {
    true
}

fn default_ringtone() -> String {
    crate::ringtone::default_id().to_string()
}

#[derive(Serialize, Deserialize)]
pub struct UiPrefs {
    /// Play a ringtone when an individual call arrives.
    #[serde(default = "default_true")]
    pub ring_enabled: bool,
    /// Selected ringtone id (see `ringtone::RINGTONES`).
    #[serde(default = "default_ringtone")]
    pub ringtone: String,
    #[serde(skip)]
    path: PathBuf,
}

impl Default for UiPrefs {
    fn default() -> UiPrefs {
        UiPrefs {
            ring_enabled: true,
            ringtone: default_ringtone(),
            path: PathBuf::new(),
        }
    }
}

impl UiPrefs {
    /// Load from `<dir>/ui_prefs.json`, or defaults if missing/unreadable.
    pub fn load(dir: &str) -> UiPrefs {
        let path = Path::new(dir).join("ui_prefs.json");
        let mut prefs = std::fs::read_to_string(&path)
            .ok()
            .and_then(|s| serde_json::from_str::<UiPrefs>(&s).ok())
            .unwrap_or_default();
        // Guard against a stale/unknown ringtone id.
        if !crate::ringtone::is_valid(&prefs.ringtone) {
            prefs.ringtone = default_ringtone();
        }
        prefs.path = path;
        prefs
    }

    /// Persist (atomic temp-file + rename). Errors are logged only.
    pub fn save(&self) {
        if self.path.as_os_str().is_empty() {
            return;
        }
        if let Some(parent) = self.path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let json = match serde_json::to_string_pretty(self) {
            Ok(j) => j,
            Err(e) => {
                tracing::warn!(error = %e, "prefs: serialize failed");
                return;
            }
        };
        let tmp = self.path.with_extension("json.tmp");
        if std::fs::write(&tmp, json).is_ok() {
            if let Err(e) = std::fs::rename(&tmp, &self.path) {
                tracing::warn!(error = %e, "prefs: persist rename failed");
            }
        }
    }
}
