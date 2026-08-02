// Local call log ("Recents") persistence, so the last calls survive a UI
// restart. Like the message store and UI prefs, this is UI-only: it is written
// to a small JSON file in the storage directory and never sent to the stack.
//
// One entry is recorded per finished individual call (group calls are excluded
// - they are broadcast/floor based, not point-to-point calls).

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// How a call ended (also the int the UI renders for its icon/colour).
pub mod outcome {
    /// Answered and connected (has a duration).
    pub const ANSWERED: u8 = 0;
    /// Incoming call we never answered (caller gave up).
    pub const MISSED: u8 = 1;
    /// Incoming call we rejected.
    pub const REJECTED: u8 = 2;
    /// Outgoing call the other side never answered.
    pub const NO_ANSWER: u8 = 3;
    /// Call setup failed (network/other error).
    pub const FAILED: u8 = 4;
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct CallLogEntry {
    pub id: u64,
    /// The peer ISSI (for an external call, the gateway ISSI).
    pub peer_ssi: u32,
    /// A resolved display label snapshot (contact "Name - Callsign"), if known.
    #[serde(default)]
    pub peer_label: Option<String>,
    /// For an external (gateway) call, the external subscriber number.
    #[serde(default)]
    pub external_number: Option<String>,
    /// true = we placed it (outgoing); false = it came to us (incoming).
    pub outgoing: bool,
    /// See the `outcome` module.
    pub outcome: u8,
    /// Simplex (PTT) vs duplex.
    pub simplex: bool,
    /// External (gateway) call.
    #[serde(default)]
    pub is_external: bool,
    /// Call duration in seconds (0 when it never connected).
    #[serde(default)]
    pub duration_s: u64,
    /// Unix milliseconds when the call started.
    pub at_ms: u64,
}

#[derive(Serialize, Deserialize, Default)]
pub struct CallLog {
    /// Monotonic id for entries.
    pub next_id: u64,
    /// Missed calls not yet seen by the user (menu badge).
    #[serde(default)]
    pub missed_unread: u32,
    /// Newest last.
    pub entries: Vec<CallLogEntry>,
    #[serde(skip)]
    path: PathBuf,
}

/// Keep at most this many recent calls.
const MAX_ENTRIES: usize = 200;

impl CallLog {
    /// Load from `<dir>/call_log.json`, or start empty if missing/unreadable.
    pub fn load(dir: &str) -> CallLog {
        let path = Path::new(dir).join("call_log.json");
        let mut log = std::fs::read_to_string(&path)
            .ok()
            .and_then(|s| serde_json::from_str::<CallLog>(&s).ok())
            .unwrap_or_default();
        log.path = path;
        log
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
                tracing::warn!(error = %e, "call log: serialize failed");
                return;
            }
        };
        let tmp = self.path.with_extension("json.tmp");
        if std::fs::write(&tmp, json).is_ok() {
            if let Err(e) = std::fs::rename(&tmp, &self.path) {
                tracing::warn!(error = %e, "call log: persist rename failed");
            }
        }
    }

    /// Add an entry (assigns its id) and prune to the newest `MAX_ENTRIES`.
    pub fn add(&mut self, mut entry: CallLogEntry) {
        entry.id = self.next_id;
        self.next_id = self.next_id.wrapping_add(1);
        if entry.outcome == outcome::MISSED {
            self.missed_unread = self.missed_unread.saturating_add(1);
        }
        self.entries.push(entry);
        if self.entries.len() > MAX_ENTRIES {
            let overflow = self.entries.len() - MAX_ENTRIES;
            self.entries.drain(0..overflow);
        }
    }

    /// Clear the missed-call badge (called when the Recents screen is opened).
    pub fn clear_missed(&mut self) {
        self.missed_unread = 0;
    }
}
