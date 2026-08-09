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

fn default_volume() -> f32 {
    1.0
}

/// Which ringtone applies to a given incoming call.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum RingCategory {
    /// Private simplex (PTT) call.
    Simplex,
    /// Private duplex (full-duplex) call.
    Duplex,
    /// External call arriving through a gateway.
    Gateway,
}

#[derive(Serialize, Deserialize)]
pub struct UiPrefs {
    /// Play a ringtone when an individual call arrives.
    #[serde(default = "default_true")]
    pub ring_enabled: bool,
    /// Ringtone for private simplex (PTT) calls.
    #[serde(default = "default_ringtone")]
    pub ring_simplex: String,
    /// Ringtone for private duplex calls.
    #[serde(default = "default_ringtone")]
    pub ring_duplex: String,
    /// Ringtone for external (gateway) calls.
    #[serde(default = "default_ringtone")]
    pub ring_gateway: String,
    /// Whether the Event Log entry is shown in the main menu (UI-only setting).
    #[serde(default = "default_true")]
    pub show_event_log: bool,
    /// Master playback volume (0.0..1.0) for call audio and ringtones.
    #[serde(default = "default_volume")]
    pub volume: f32,
    /// Legacy single-ringtone field (pre per-type); migrated on load.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    ringtone: Option<String>,
    #[serde(skip)]
    path: PathBuf,
}

impl Default for UiPrefs {
    fn default() -> UiPrefs {
        UiPrefs {
            ring_enabled: true,
            ring_simplex: default_ringtone(),
            ring_duplex: default_ringtone(),
            ring_gateway: default_ringtone(),
            show_event_log: true,
            volume: 1.0,
            ringtone: None,
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
        // Migrate an older single-ringtone file to the per-type fields.
        if let Some(old) = prefs.ringtone.take() {
            if crate::ringtone::is_valid(&old) {
                prefs.ring_simplex = old.clone();
                prefs.ring_duplex = old.clone();
                prefs.ring_gateway = old;
            }
        }
        // Guard against stale/unknown ringtone ids.
        for r in [&mut prefs.ring_simplex, &mut prefs.ring_duplex, &mut prefs.ring_gateway] {
            if !crate::ringtone::is_valid(r) {
                *r = default_ringtone();
            }
        }
        // Clamp volume to a sane range (guards NaN / out-of-range files).
        if !(prefs.volume.is_finite()) {
            prefs.volume = 1.0;
        }
        prefs.volume = prefs.volume.clamp(0.0, 1.0);
        prefs.path = path;
        prefs
    }

    /// The selected ringtone id for a call category.
    pub fn ringtone_for(&self, cat: RingCategory) -> &str {
        match cat {
            RingCategory::Simplex => &self.ring_simplex,
            RingCategory::Duplex => &self.ring_duplex,
            RingCategory::Gateway => &self.ring_gateway,
        }
    }

    /// Set the ringtone id for a call category.
    pub fn set_ringtone(&mut self, cat: RingCategory, id: String) {
        match cat {
            RingCategory::Simplex => self.ring_simplex = id,
            RingCategory::Duplex => self.ring_duplex = id,
            RingCategory::Gateway => self.ring_gateway = id,
        }
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
