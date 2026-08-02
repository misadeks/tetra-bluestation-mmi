// Local persistence for SDS message history so conversations survive a UI
// restart. A single JSON file is rewritten on every change (human-rate events),
// written to a temp file and atomically renamed so a crash can't corrupt it.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

/// Message delivery state (also the int the UI renders).
pub mod state {
    pub const SENDING: u8 = 0;
    pub const DELIVERED: u8 = 1;
    pub const READ: u8 = 2;
    pub const FAILED: u8 = 3;
    /// Inbound messages have no send-state; use this so the UI hides the ticks.
    pub const INBOUND: u8 = 4;
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct StoredMessage {
    pub id: u64,
    pub peer_ssi: u32,
    pub is_group: bool,
    /// true = we sent it; false = we received it.
    pub outgoing: bool,
    pub text: String,
    /// SDS-TL message reference (0..255) we assigned (outgoing) or received.
    pub reference: u8,
    /// See `state` module. Inbound messages use `INBOUND`.
    pub state: u8,
    /// Failure cause byte (>=0x40) when `state == FAILED`.
    #[serde(default)]
    pub fail_code: u8,
    /// Unix milliseconds when the message was added.
    pub at_ms: u64,
    /// Inbound only: whether the user has opened/read it yet.
    #[serde(default)]
    pub read: bool,
    /// Inbound only: a consumed report is owed to the sender once read.
    #[serde(default)]
    pub wants_consumed: bool,
}

#[derive(Serialize, Deserialize, Default, Debug)]
pub struct MessageStore {
    /// Rolling 0..255 reference to assign to the next outgoing message.
    pub next_ref: u8,
    /// Monotonic local id for messages.
    pub next_id: u64,
    pub messages: Vec<StoredMessage>,
    #[serde(skip)]
    path: PathBuf,
}

pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

impl MessageStore {
    /// Load the store from `<dir>/messages.json`, or start empty if it's
    /// missing or unreadable. Never panics.
    pub fn load(dir: &str) -> MessageStore {
        let path = Path::new(dir).join("messages.json");
        let mut store = std::fs::read_to_string(&path)
            .ok()
            .and_then(|s| serde_json::from_str::<MessageStore>(&s).ok())
            .unwrap_or_default();
        store.path = path;
        store
    }

    /// Persist the store (atomic temp-file + rename). Errors are logged only.
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
                tracing::warn!(error = %e, "messages: serialize failed");
                return;
            }
        };
        let tmp = self.path.with_extension("json.tmp");
        if std::fs::write(&tmp, json).is_ok() {
            if let Err(e) = std::fs::rename(&tmp, &self.path) {
                tracing::warn!(error = %e, "messages: persist rename failed");
            }
        }
    }

    /// Assign the next outgoing message reference (rolling 0..255).
    pub fn next_reference(&mut self) -> u8 {
        let r = self.next_ref;
        self.next_ref = self.next_ref.wrapping_add(1);
        r
    }

    /// Assign the next local message id.
    pub fn next_local_id(&mut self) -> u64 {
        let id = self.next_id;
        self.next_id = self.next_id.wrapping_add(1);
        id
    }
}
