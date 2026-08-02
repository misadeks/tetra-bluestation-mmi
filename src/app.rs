// Central app state and the single-threaded event loop that owns it.
//
// All net threads (control + telemetry) and timers marshal into here via a
// crossbeam channel; this loop is the only writer of UI state, which it pushes
// onto the Slint event loop with `upgrade_in_event_loop`. Commands to the stack
// go out through the current control connection's outbound sink.

use std::collections::{HashMap, VecDeque};
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, Sender};
use serde_json::Value;
use slint::{ModelRc, VecModel};

use crate::codeplug::Codeplug;
use crate::protocol::{self, MsRuntimeState, ServiceStatus};
use crate::{ContactRow, ConvRow, DialTarget, EntityRow, FolderRow, FormField, GroupRow, LogRow, MainWindow, MsgRow, ScanRow, Screen, SurveyRow, TreeRow};

/// Events fed into the app loop from net threads, timers, and the UI.
pub enum AppEvent {
    /// A control connection came up; carries its outbound sink (encoded frames).
    ControlConnected(Sender<Vec<u8>>),
    ControlDisconnected,
    ControlMessage(Value),
    TelemetryConnected,
    TelemetryDisconnected,
    TelemetryMessage(Value),
    /// Periodic GetState poll tick.
    PollTick,
    /// Wall clock update (pre-formatted).
    ClockTick { time: String, date: String },
    /// UI actions.
    UiRegister,
    UiDeregister,
    UiCyclePrev,
    UiCycleNext,
    UiSelectTalkgroup,
    UiCancelSelect,
    UiSelectFolder(i32),
    UiPtt,
    UiDialKey(String),
    /// Place a dialer call: (target index 0=private, 1.. = gateway; duplex).
    UiDialCall(i32, bool),
    /// Place a call to a phone-book contact: (contact index, duplex).
    UiCallContact(i32, bool),
    /// Open the detail page for a contact by index.
    UiOpenContact(i32),
    /// Start a new blank contact draft in the editor.
    UiContactNew,
    /// Open the editor on an existing contact by index.
    UiContactEdit(i32),
    /// Delete a contact by index.
    UiContactDelete(i32),
    /// Commit the current contact draft (SetConfig + ApplyConfig).
    UiContactSave,
    /// Discard the current contact draft.
    UiContactCancel,
    /// Move keyboard focus to a form field (0 name, 1 callsign, 2 issi, 3 number).
    UiEditFocus(i32),
    /// Insert a character into the focused field.
    UiEditKey(String),
    /// Remove the last character of the focused field.
    UiEditBackspace,
    /// Toggle the QWERTY shift latch.
    UiEditShift,
    /// Choose the contact target: 0 = Private (ISSI), 1.. = gateway (phone).
    UiEditTarget(i32),
    /// Send a single DTMF digit on the active in-call session.
    UiDtmf(String),
    /// Append a character to the contacts search query.
    UiContactSearchKey(String),
    /// Remove the last character of the contacts search query.
    UiContactSearchBackspace,
    /// Clear the contacts search query.
    UiContactSearchClear,
    UiCallPttDown,
    UiCallPttUp,
    UiGroupPttDown,
    UiGroupPttUp,
    UiAnswerCall,
    UiRejectCall,
    UiHangup,
    UiHangupGroup,
    UiToggleMute,
    /// Encoded uplink speech frame (call id, 274 codec bits) from the mic thread.
    UplinkAudio(u32, Vec<u8>),
    UiOpenLogs,
    UiAlertDismiss,
    AlertExpire(u64),
    UiGroupSelect(i32, i32),
    UiGroupAttach(i32, i32),
    UiGroupDetach(i32),
    UiScanlistToggle(String, bool),
    UiSurveyToggleMode,
    UiSurveyScan,
    UiSurveyStop,
    UiCampCell(u64, bool),
    UiApplyConfig,
    UiRefresh,
    /// Open the messages/conversations list screen.
    UiOpenMessages,
    /// Open the thread for a peer ssi (from a ConvRow.peer value).
    UiOpenThread(i32),
    /// Open a thread for a contact by index (ISSI contacts only).
    UiMessageContact(i32),
    /// Append a character to the open thread's draft.
    UiMsgKey(String),
    /// Remove the last character of the open thread's draft.
    UiMsgBackspace,
    /// Toggle the compose keyboard shift latch.
    UiMsgShift,
    /// Send the current draft in the open thread.
    UiMsgSend,
    /// Prepare a blank "new conversation" ISSI entry.
    UiMsgNew,
    /// Append a digit to the new-conversation ISSI.
    UiMsgNewKey(String),
    /// Remove the last digit of the new-conversation ISSI.
    UiMsgNewBackspace,
    /// Open a thread for the entered new-conversation ISSI.
    UiMsgNewStart,
    /// Delete a single stored message by its local id.
    UiMsgDelete(i32),
    /// Delete a whole conversation (peer ssi, is_group).
    UiMsgDeleteThread(i32, bool),
    /// Programming: open a codeplug section from the hub (0..6).
    UiProgSection(i32),
    /// Programming: open a row in the current section's list.
    UiProgOpen(i32),
    /// Programming: add a new entry to the current section.
    UiProgAdd,
    /// Programming: focus a form field.
    UiFormPick(i32),
    /// Programming: type into the focused field.
    UiFormKey(String),
    /// Programming: backspace the focused field.
    UiFormBackspace,
    /// Programming: toggle the QWERTY shift latch.
    UiFormShift,
    /// Programming: flip a toggle field.
    UiFormToggle(i32),
    /// Programming: advance a cycle field.
    UiFormCycle(i32),
    /// Programming: save the current form.
    UiFormSave,
    /// Programming: discard the current form.
    UiFormCancel,
    /// Programming: delete the edited entry.
    UiFormDelete,
    /// Programming: open the folders + talkgroups tree.
    UiOpenTree,
    /// Tree: edit a folder by its folder_defs index.
    UiTreeFolder(i32),
    /// Tree: edit a talkgroup by its all_talkgroups index.
    UiTreeGroup(i32),
    /// Tree: add a group into a folder (folder_defs index, -1 = Other).
    UiTreeAddGroup(i32),
    /// Tree: add a new folder.
    UiTreeAddFolder,
    /// Tree: collapse/expand a folder (folder index, -1 = Other).
    UiTreeToggle(i32),
    /// Tree: move a group up within its folder (all_talkgroups index).
    UiTreeMoveUp(i32),
    /// Tree: move a group down within its folder (all_talkgroups index).
    UiTreeMoveDown(i32),
}

struct AppState {
    control_out: Option<Sender<Vec<u8>>>,
    control_connected: bool,
    telemetry_connected: bool,
    next_handle: u32,
    have_config: bool,
    reg_type: String,
    state: MsRuntimeState,
    logged_state: bool,
    codeplug: Option<Codeplug>,
    /// Index of the selected folder in the codeplug tree.
    sel_folder: usize,
    /// GSSI currently shown by the home cycler within the selected folder.
    cycle_gssi: Option<u32>,
    /// Last talkgroup the operator switched to (TX), if still attached.
    selected_tx: Option<u32>,
    /// Number being entered on the dialer.
    dial_number: String,
    /// Outbound sink + timer scheduling: a clone of the app event sender.
    self_tx: Sender<AppEvent>,
    /// Alert generation for auto-dismiss (only the latest wins).
    alert_gen: u64,
    /// Throttle for the repeated "Radio offline" alert.
    last_offline_alert: Option<Instant>,
    /// Outstanding commands by handle, for timeout detection.
    pending: HashMap<u32, Instant>,
    /// Whether we already notified about the current stall.
    timeout_notified: bool,
    /// Last service status seen (to detect transitions to OutOfService).
    last_service: ServiceStatus,
    /// Rolling telemetry event log (newest last).
    events: VecDeque<LogEntry>,
    /// Unread event count since the log was last opened.
    unread: i32,
    /// Last full MS config TOML from GetConfig.
    last_config_toml: String,
    /// Cell survey rows collected from MsScanResult telemetry.
    scan_rows: Vec<ScanCell>,
    /// Whether a survey is currently in progress.
    scanning: bool,
    /// Whether the last survey completed (for the results footer).
    scan_complete: bool,
    /// Completion summary: (cells found, carriers scanned).
    scan_summary: (i32, i32),
    /// Live calls keyed by call identifier.
    calls: HashMap<u32, Call>,
    /// Outgoing placeholder before the call identifier is known.
    dialing: Option<Dialing>,
    /// Persistent group-call/PTT context (floor decoupled from lifecycle).
    grp_call: Option<GrpCall>,
    /// The individual call whose PTT is physically held, if any.
    ptt_held: Option<u32>,
    /// Calls we hung up ourselves (suppresses the "Call ended" toast).
    local_end: std::collections::HashSet<u32>,
    /// Microphone muted for the current call (gates uplink transmission).
    mic_muted: bool,
    /// Contact whose detail page is open (index into codeplug.contacts).
    sel_contact: Option<usize>,
    /// In-progress contact add/edit form, if the editor is open.
    contact_draft: Option<ContactDraft>,
    /// Locally-echoed DTMF digits sent during the current in-call session.
    dtmf_echo: String,
    /// Current contacts search query (case-insensitive substring filter).
    contact_query: String,
    /// Persistent SDS message store (survives UI restart).
    messages: crate::store::MessageStore,
    /// Peer (ssi, is_group) whose message thread is currently open.
    msg_thread_peer: Option<(u32, bool)>,
    /// Draft text being composed in the open thread.
    msg_draft: String,
    /// Shift state of the message compose keyboard.
    msg_shift: bool,
    /// ISSI entered on the "new conversation" screen.
    msg_new_issi: String,
    /// Bumped when the open thread should scroll to the newest message
    /// (on open + on send only, not on every update).
    msg_scroll_tick: u32,
    /// Codeplug programming: current section and open edit form draft.
    prog_section: ProgSection,
    prog_draft: Option<ProgDraft>,
    /// Tree: folder ids (or "__other__") whose groups are collapsed.
    collapsed_folders: std::collections::HashSet<String>,
}

/// Which field of the contact editor the on-screen keyboard is editing.
#[derive(Clone, Copy, PartialEq, Eq)]
enum EditField {
    Name,
    Callsign,
    Issi,
    Number,
}

impl EditField {
    /// Digit-only fields drive the numeric keypad instead of the QWERTY board.
    fn numeric(self) -> bool {
        matches!(self, EditField::Issi | EditField::Number)
    }
}

/// Editable draft backing the contact add/edit form.
struct ContactDraft {
    /// Original (unique) name when editing an existing contact; None when new.
    key_name: Option<String>,
    name: String,
    callsign: String,
    /// false = ISSI (individual) form; true = phone (number + gateway) form.
    is_phone: bool,
    issi: String,
    number: String,
    /// Selected gateway id for the phone form ("" = none chosen).
    gateway_id: String,
    focus: EditField,
    /// Shift latch for the next letter typed on the QWERTY keyboard.
    shift: bool,
}

impl ContactDraft {
    fn field_mut(&mut self) -> &mut String {
        match self.focus {
            EditField::Name => &mut self.name,
            EditField::Callsign => &mut self.callsign,
            EditField::Issi => &mut self.issi,
            EditField::Number => &mut self.number,
        }
    }
}

// --- Codeplug programming (generic list + form) ------------------------------

/// Which codeplug section the programming UI is editing.
#[derive(Clone, Copy, PartialEq, Eq)]
enum ProgSection {
    Networks,
    Folders,
    Talkgroups,
    Scanlists,
    Gateways,
    Settings,
}

/// Kind of a form field, matching the Slint `FormField.kind` int.
#[derive(Clone, Copy, PartialEq, Eq)]
enum FieldKind {
    Text = 0,
    Digits = 1,
    Dial = 2,
    Toggle = 3,
    Cycle = 4,
}

/// One editable field in the generic programming form.
struct FormFieldDraft {
    label: String,
    kind: FieldKind,
    value: String,
    on: bool,
    /// Cycle fields: display labels and their underlying values (parallel).
    options: Vec<String>,
    #[allow(dead_code)] // parallel underlying values for cycle fields
    opt_values: Vec<String>,
    opt_idx: usize,
}

impl FormFieldDraft {
    fn text(label: &str, value: String) -> FormFieldDraft {
        FormFieldDraft { label: label.into(), kind: FieldKind::Text, value, on: false, options: vec![], opt_values: vec![], opt_idx: 0 }
    }
    fn digits(label: &str, value: String) -> FormFieldDraft {
        FormFieldDraft { label: label.into(), kind: FieldKind::Digits, value, on: false, options: vec![], opt_values: vec![], opt_idx: 0 }
    }
    fn dial(label: &str, value: String) -> FormFieldDraft {
        FormFieldDraft { label: label.into(), kind: FieldKind::Dial, value, on: false, options: vec![], opt_values: vec![], opt_idx: 0 }
    }
    fn toggle(label: &str, on: bool) -> FormFieldDraft {
        FormFieldDraft { label: label.into(), kind: FieldKind::Toggle, value: String::new(), on, options: vec![], opt_values: vec![], opt_idx: 0 }
    }
    #[allow(dead_code)] // retained for future cycle-style fields
    fn cycle(label: &str, options: Vec<String>, opt_values: Vec<String>, opt_idx: usize) -> FormFieldDraft {
        let value = options.get(opt_idx).cloned().unwrap_or_default();
        FormFieldDraft { label: label.into(), kind: FieldKind::Cycle, value, on: false, options, opt_values, opt_idx }
    }
    fn is_focusable(&self) -> bool {
        matches!(self.kind, FieldKind::Text | FieldKind::Digits | FieldKind::Dial)
    }
}

/// Backing draft for the generic programming edit form.
struct ProgDraft {
    section: ProgSection,
    /// Identity of the edited row (id / name / gssi text); None when adding.
    key: Option<String>,
    /// Array index for section rows keyed only by position (networks).
    index: Option<usize>,
    fields: Vec<FormFieldDraft>,
    /// Index of the focused text/number field, or usize::MAX if none.
    focus: usize,
    shift: bool,
    can_delete: bool,
    title: String,
    /// Scanlist only: GSSI for each membership toggle (fields after the first two).
    member_gssis: Vec<u32>,
    /// Talkgroup only: the folder this group belongs to (id, or None). Set from
    /// context so the form doesn't need a folder selector.
    tg_folder: Option<String>,
}

impl ProgDraft {
    fn focused(&mut self) -> Option<&mut FormFieldDraft> {
        self.fields.get_mut(self.focus)
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Default)]
enum CallState {
    #[default]
    Setup,
    Proceeding,
    Alerting,
    Incoming,
    Connecting,
    Active,
}

#[derive(Clone, Default)]
struct Call {
    cid: u32,
    /// "mo" (outgoing) or "mt" (incoming); None until known.
    direction: Option<&'static str>,
    state: CallState,
    peer_ssi: Option<u32>,
    group: bool,
    simplex: bool,
    hook_on: bool,
    tx_status: Option<String>,
    holds_floor: bool,
    talker_ssi: Option<u32>,
    can_request_tx: bool,
    rang: bool,
    answered: bool,
    queued: bool,
    active_since: Option<Instant>,
    /// Display override for the peer name (e.g. a contact name). None = show SSI.
    peer_label: Option<String>,
    /// Display override for the sub-line (e.g. the dialled external number).
    peer_sub: Option<String>,
    /// When we last played a downlink speech frame for this call (someone is
    /// speaking now, even if the SwMI doesn't tell us who).
    rx_at: Option<Instant>,
}

#[derive(Clone)]
struct Dialing {
    peer_ssi: u32,
    group: bool,
    simplex: bool,
    /// Display override for the peer name (e.g. a contact name).
    peer_label: Option<String>,
    /// Display override for the sub-line (e.g. the dialled external number).
    peer_sub: Option<String>,
}

#[derive(Clone)]
struct GrpCall {
    gssi: u32,
    cid: Option<u32>,
    talking: bool,
}

#[derive(Clone)]
struct ScanCell {
    carrier_hz: u64,
    rssi_dbfs: Option<f32>,
    mcc: Option<i64>,
    mnc: Option<i64>,
    location_area: Option<i64>,
    registration_required: Option<bool>,
    late_entry_supported: Option<bool>,
}

struct LogEntry {
    time: String,
    variant: String,
    summary: String,
    detail: String,
}

impl AppState {
    fn next_handle(&mut self) -> u32 {
        self.next_handle = self.next_handle.wrapping_add(1);
        self.next_handle
    }

    fn send(&mut self, message: Value) -> bool {
        if let Some(h) = command_handle(&message) {
            self.pending.insert(h, Instant::now());
        }
        if let Some(tx) = &self.control_out {
            if let Ok(bytes) = serde_json::to_vec(&message) {
                return tx.send(bytes).is_ok();
            }
        }
        false
    }

    /// Show a modal alert (kind: 0 amber, 1 red, 2 green) and schedule its
    /// auto-dismiss after ~2.2s. Only the most recent alert dismisses itself.
    fn notify(&mut self, weak: &slint::Weak<MainWindow>, title: &str, message: &str, kind: i32) {
        self.alert_gen = self.alert_gen.wrapping_add(1);
        let gen = self.alert_gen;
        let (t, m) = (title.to_string(), message.to_string());
        let _ = weak.upgrade_in_event_loop(move |w| {
            w.set_alert_title(t.into());
            w.set_alert_message(m.into());
            w.set_alert_kind(kind);
            w.set_alert_visible(true);
        });
        let tx = self.self_tx.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(2200));
            let _ = tx.send(AppEvent::AlertExpire(gen));
        });
    }

    /// Guard for user actions: refuse when the control link is down and show a
    /// throttled "Radio offline" alert. Returns true when it is safe to send.
    fn require_online(&mut self, weak: &slint::Weak<MainWindow>) -> bool {
        if self.control_connected {
            return true;
        }
        let now = Instant::now();
        let recent = self
            .last_offline_alert
            .map(|t| now.duration_since(t) < Duration::from_millis(2500))
            .unwrap_or(false);
        if !recent {
            self.last_offline_alert = Some(now);
            self.notify(
                weak,
                "Radio offline",
                "Control channel is down - connect the MS before sending commands.",
                1,
            );
        }
        false
    }

    /// Guard for network actions: requires the control link up and the MS
    /// registered. Shows the appropriate alert and returns false when blocked.
    fn require_registered(&mut self, weak: &slint::Weak<MainWindow>) -> bool {
        if !self.require_online(weak) {
            return false;
        }
        if self.state.registration_state != protocol::RegistrationState::Registered {
            self.notify(weak, "Not registered", "Register the radio first.", 0);
            return false;
        }
        true
    }
}

/// Extract the correlation handle from an outbound command or inbound response.
fn command_handle(v: &Value) -> Option<u32> {
    let (_, payload) = protocol::variant_of(v)?;
    if let Some(h) = payload.get("handle").and_then(Value::as_u64) {
        return Some(h as u32);
    }
    let (_, inner) = protocol::variant_of(payload)?;
    inner.get("handle").and_then(Value::as_u64).map(|h| h as u32)
}

/// Run the app event loop until the channel closes. Blocks the calling thread.
pub fn run(
    rx: Receiver<AppEvent>,
    self_tx: Sender<AppEvent>,
    weak: slint::Weak<MainWindow>,
    reg_type: String,
    audio_cfg: crate::config::AudioConfig,
    storage_dir: String,
) {
    // Voice engine (None when disabled, codec missing, or no audio device).
    let audio = crate::audio::AudioEngine::new(&audio_cfg, self_tx.clone());
    if audio.is_none() {
        tracing::info!("audio: running without voice (codec or device unavailable)");
    }

    let mut app = AppState {
        control_out: None,
        control_connected: false,
        telemetry_connected: false,
        next_handle: 0,
        have_config: false,
        reg_type,
        state: MsRuntimeState::default(),
        logged_state: false,
        codeplug: None,
        sel_folder: 0,
        cycle_gssi: None,
        selected_tx: None,
        dial_number: String::new(),
        self_tx,
        alert_gen: 0,
        last_offline_alert: None,
        pending: HashMap::new(),
        timeout_notified: false,
        last_service: ServiceStatus::OutOfService,
        events: VecDeque::new(),
        unread: 0,
        last_config_toml: String::new(),
        scan_rows: Vec::new(),
        scanning: false,
        scan_complete: false,
        scan_summary: (0, 0),
        calls: HashMap::new(),
        dialing: None,
        grp_call: None,
        ptt_held: None,
        local_end: std::collections::HashSet::new(),
        mic_muted: false,
        sel_contact: None,
        contact_draft: None,
        dtmf_echo: String::new(),
        contact_query: String::new(),
        messages: crate::store::MessageStore::load(&storage_dir),
        msg_thread_peer: None,
        msg_draft: String::new(),
        msg_shift: false,
        msg_new_issi: String::new(),
        msg_scroll_tick: 0,
        prog_section: ProgSection::Networks,
        prog_draft: None,
        collapsed_folders: std::collections::HashSet::new(),
    };

    for event in rx.iter() {
        match event {
            AppEvent::ControlConnected(tx) => {
                app.control_out = Some(tx);
                app.control_connected = true;
                app.have_config = false;
                tracing::info!("control: stack connected, bootstrapping");
                // Bootstrap: learn schema + initial state + config (silent).
                let h = app.next_handle();
                app.send(protocol::get_interface_version(h));
                let h = app.next_handle();
                app.send(protocol::get_state(h));
                let h = app.next_handle();
                app.send(protocol::get_config(h));
                push_ui(&app, &weak);
            }
            AppEvent::ControlDisconnected => {
                app.control_out = None;
                app.control_connected = false;
                app.pending.clear();
                app.timeout_notified = false;
                // Tear down any live calls; the link that carried them is gone.
                app.calls.clear();
                app.dialing = None;
                app.grp_call = None;
                app.ptt_held = None;
                app.local_end.clear();
                tracing::info!("control: stack disconnected");
                push_ui(&app, &weak);
                push_calls(&app, &weak);
            }
            AppEvent::ControlMessage(value) => {
                handle_control(&mut app, &value, &weak);
            }
            AppEvent::TelemetryConnected => {
                app.telemetry_connected = true;
                tracing::info!("telemetry: stack connected");
                push_ui(&app, &weak);
            }
            AppEvent::TelemetryDisconnected => {
                app.telemetry_connected = false;
                tracing::info!("telemetry: stack disconnected");
                push_ui(&app, &weak);
            }
            AppEvent::TelemetryMessage(value) => {
                handle_telemetry(&mut app, &value, &weak, audio.as_ref());
                sync_uplink(&app, audio.as_ref());
            }
            AppEvent::PollTick => {
                if app.control_connected {
                    let h = app.next_handle();
                    app.send(protocol::get_state(h));
                    if !app.have_config {
                        let h = app.next_handle();
                        app.send(protocol::get_config(h));
                    }
                }
                // Expire commands the stack never answered.
                let now = Instant::now();
                let stalled = app
                    .pending
                    .iter()
                    .any(|(_, t)| now.duration_since(*t) > Duration::from_secs(6));
                app.pending
                    .retain(|_, t| now.duration_since(*t) <= Duration::from_secs(6));
                if stalled && !app.timeout_notified {
                    app.timeout_notified = true;
                    app.notify(&weak, "No response", "The stack didn't respond in time.", 0);
                }
            }
            AppEvent::ClockTick { time, date } => {
                let weak2 = weak.clone();
                let _ = weak2.upgrade_in_event_loop(move |w| {
                    w.set_clock(time.into());
                    w.set_date(date.into());
                });
                // Refresh call durations/floor timers once a second.
                if !app.calls.is_empty() {
                    push_calls(&app, &weak);
                }
            }
            AppEvent::UiRegister => {
                if !app.require_online(&weak) {
                    continue;
                }
                let issi = app.state.own_issi;
                let mcc = app.state.home_mcc;
                let mnc = app.state.home_mnc;
                let detaching =
                    app.state.registration_state == protocol::RegistrationState::Detaching;
                if issi == 0 {
                    app.notify(
                        &weak,
                        "No identity",
                        "Waiting for the MS to report its ISSI.",
                        0,
                    );
                    continue;
                }
                if detaching {
                    app.notify(
                        &weak,
                        "Detaching",
                        "The MS is still detaching; it may reject a new registration until the network confirms.",
                        0,
                    );
                }
                let h = app.next_handle();
                let msg = protocol::tnmm_registration(h, &app.reg_type, issi, mcc, mnc);
                tracing::info!(issi, "UI: sending TnmmRegistration");
                app.send(msg);
            }
            AppEvent::UiDeregister => {
                if !app.require_online(&weak) {
                    continue;
                }
                let h = app.next_handle();
                let issi = app.state.own_issi;
                tracing::info!(issi, "UI: sending TnmmDeregistration");
                app.send(protocol::tnmm_deregistration(h, Some(issi), None, None));
            }
            AppEvent::UiCyclePrev => {
                cycle(&mut app, -1);
                push_ui(&app, &weak);
            }
            AppEvent::UiCycleNext => {
                cycle(&mut app, 1);
                push_ui(&app, &weak);
            }
            AppEvent::UiSelectFolder(i) => {
                if let Some(cp) = &app.codeplug {
                    let i = i.max(0) as usize;
                    if i < cp.folders.len() {
                        app.sel_folder = i;
                        // Reset the cycler to the folder's first talkgroup.
                        app.cycle_gssi = cp.folders[i].talkgroups.first().map(|t| t.gssi);
                    }
                }
                push_ui(&app, &weak);
            }
            AppEvent::UiCancelSelect => {
                // Snap the cycler back to the active TX group (and its folder).
                if let Some(tx) = effective_tx(&app) {
                    if let Some(cp) = &app.codeplug {
                        if let Some(fidx) = cp
                            .folders
                            .iter()
                            .position(|f| f.talkgroups.iter().any(|t| t.gssi == tx))
                        {
                            app.sel_folder = fidx;
                        }
                    }
                    app.cycle_gssi = Some(tx);
                }
                push_ui(&app, &weak);
            }
            AppEvent::UiSelectTalkgroup => {
                if !app.require_online(&weak) {
                    continue;
                }
                if let Some((gssi, cou)) = cycle_target(&app) {
                    let h = app.next_handle();
                    tracing::info!(gssi, cou, "UI: switching TX talkgroup");
                    app.send(protocol::tnmm_switch_talkgroup(h, gssi, cou));
                    app.selected_tx = Some(gssi);
                    // Nudge a GetState; telemetry will also drive an immediate one.
                    let h = app.next_handle();
                    app.send(protocol::get_state(h));
                }
            }
            AppEvent::UiPtt => {
                // Guards mirror the web UI; voice keying lands in M5/M6.
                if !app.require_online(&weak) {
                    continue;
                }
                if app.state.registration_state != protocol::RegistrationState::Registered {
                    app.notify(&weak, "Not registered", "Register the radio before transmitting.", 0);
                    continue;
                }
                if effective_tx(&app).is_none() {
                    app.notify(&weak, "No talkgroup", "Attach a talkgroup before transmitting.", 0);
                    continue;
                }
                let gssi = effective_tx(&app);
                tracing::info!(?gssi, "UI: PTT pressed (voice calls arrive in a later milestone)");
            }
            AppEvent::UiDialKey(k) => {
                match k.as_str() {
                    "back" => {
                        app.dial_number.pop();
                    }
                    "clear" => {
                        app.dial_number.clear();
                    }
                    m if m.starts_with("max") => {
                        // Trim to the cap for the current call type (see DialerScreen).
                        if let Ok(max) = m[3..].parse::<usize>() {
                            while app.dial_number.chars().count() > max {
                                app.dial_number.pop();
                            }
                        }
                    }
                    d if app.dial_number.chars().count() < 20 => {
                        app.dial_number.push_str(d);
                    }
                    _ => {}
                }
                let n = app.dial_number.clone();
                let weak = weak.clone();
                let _ = weak.upgrade_in_event_loop(move |w| w.set_dial_number(n.into()));
            }
            AppEvent::UiDialCall(target, duplex) => {
                if !app.require_online(&weak) {
                    continue;
                }
                if app.state.registration_state != protocol::RegistrationState::Registered {
                    app.notify(&weak, "Not registered", "Register the radio before placing a call.", 0);
                    continue;
                }
                if target == 0 {
                    // Private (individual ISSI) call.
                    let valid = app.dial_number.parse::<u32>().map(|n| n >= 1).unwrap_or(false);
                    if !valid {
                        app.notify(
                            &weak,
                            "Invalid number",
                            "Enter a valid private number (ISSI) to call.",
                            0,
                        );
                        continue;
                    }
                    let ssi = app.dial_number.parse::<u32>().unwrap_or(0);
                    tracing::info!(number = %app.dial_number, duplex, "UI: dial private call");
                    // Individual call: duplex (mic streams the whole call) or
                    // simplex (PTT-keyed).
                    app.mic_muted = false;
                    app.dtmf_echo.clear();
                    let h = app.next_handle();
                    app.send(protocol::tncc_setup(h, ssi, false, duplex));
                    app.dialing = Some(Dialing { peer_ssi: ssi, group: false, simplex: !duplex, peer_label: None, peer_sub: None });
                    push_calls(&app, &weak);
                    continue;
                }
                // Gateway (external PABX/PSTN) call: target index 1.. maps to
                // codeplug.gateways[target - 1]. Dial the gateway ISSI and carry
                // prefix + typed number in the external-subscriber-number IE.
                let gw = app
                    .codeplug
                    .as_ref()
                    .and_then(|cp| cp.gateways.get((target - 1) as usize))
                    .cloned();
                let Some(gw) = gw else {
                    app.notify(&weak, "No gateway", "That gateway is no longer available.", 0);
                    continue;
                };
                let digits = match crate::codeplug::normalize_dial(&format!("{}{}", gw.prefix, app.dial_number)) {
                    Ok(d) => d,
                    Err(e) => {
                        app.notify(&weak, "Invalid number", &e, 0);
                        continue;
                    }
                };
                tracing::info!(gateway = %gw.name, gateway_ssi = gw.gateway_issi, %digits, "UI: dial external call");
                // External calls are always duplex (phone-style).
                app.mic_muted = false;
                app.dtmf_echo.clear();
                let h = app.next_handle();
                app.send(protocol::tncc_setup_external(h, gw.gateway_issi, &digits, true));
                app.dialing = Some(Dialing {
                    peer_ssi: gw.gateway_issi,
                    group: false,
                    simplex: false,
                    peer_label: Some(gw.name.clone()),
                    peer_sub: Some(digits),
                });
                push_calls(&app, &weak);
            }
            AppEvent::UiCallContact(idx, duplex) => {
                if !app.require_online(&weak) {
                    continue;
                }
                if app.state.registration_state != protocol::RegistrationState::Registered {
                    app.notify(&weak, "Not registered", "Register the radio before placing a call.", 0);
                    continue;
                }
                let Some(cp) = app.codeplug.as_ref() else { continue };
                let Some(contact) = cp.contacts.get(idx as usize).cloned() else {
                    continue;
                };
                let target = match contact.resolve(cp) {
                    Ok(t) => t,
                    Err(e) => {
                        app.notify(&weak, "Cannot call contact", &e, 0);
                        continue;
                    }
                };
                let label = match &contact.callsign {
                    Some(cs) => format!("{} ({})", contact.name, cs),
                    None => contact.name.clone(),
                };
                app.mic_muted = false;
                app.dtmf_echo.clear();
                let h = app.next_handle();
                // External (phone) calls are always duplex; ISSI honors the flag.
                let (peer_ssi, sub, simplex) = match target {
                    crate::codeplug::CallTarget::Individual(ssi) => {
                        tracing::info!(name = %contact.name, ssi, duplex, "UI: call contact (individual)");
                        app.send(protocol::tncc_setup(h, ssi, false, duplex));
                        (ssi, ssi.to_string(), !duplex)
                    }
                    crate::codeplug::CallTarget::External { gateway_ssi, digits } => {
                        tracing::info!(name = %contact.name, gateway_ssi, %digits, "UI: call contact (external)");
                        app.send(protocol::tncc_setup_external(h, gateway_ssi, &digits, true));
                        (gateway_ssi, digits, false)
                    }
                };
                app.dialing = Some(Dialing {
                    peer_ssi,
                    group: false,
                    simplex,
                    peer_label: Some(label),
                    peer_sub: Some(sub),
                });
                push_calls(&app, &weak);
            }
            AppEvent::UiOpenContact(idx) => {
                let n = app.codeplug.as_ref().map(|cp| cp.contacts.len()).unwrap_or(0);
                if (idx as usize) < n {
                    app.sel_contact = Some(idx as usize);
                    push_contact_detail(&app, &weak);
                }
            }
            AppEvent::UiContactNew => {
                app.contact_draft = Some(ContactDraft {
                    key_name: None,
                    name: String::new(),
                    callsign: String::new(),
                    is_phone: false,
                    issi: String::new(),
                    number: String::new(),
                    gateway_id: String::new(),
                    focus: EditField::Name,
                    shift: true,
                });
                push_contact_editor(&app, &weak);
            }
            AppEvent::UiContactEdit(idx) => {
                if let Some(c) = app.codeplug.as_ref().and_then(|cp| cp.contacts.get(idx as usize)) {
                    app.contact_draft = Some(ContactDraft {
                        key_name: Some(c.name.clone()),
                        name: c.name.clone(),
                        callsign: c.callsign.clone().unwrap_or_default(),
                        is_phone: c.is_phone(),
                        issi: c.issi.map(|i| i.to_string()).unwrap_or_default(),
                        number: c.number.clone().unwrap_or_default(),
                        gateway_id: c.gateway.clone().unwrap_or_default(),
                        focus: EditField::Name,
                        shift: false,
                    });
                    push_contact_editor(&app, &weak);
                }
            }
            AppEvent::UiContactDelete(idx) => {
                let name = app
                    .codeplug
                    .as_ref()
                    .and_then(|cp| cp.contacts.get(idx as usize))
                    .map(|c| c.name.clone());
                let Some(name) = name else { continue };
                match crate::codeplug::delete_contact(&app.last_config_toml, &name) {
                    Ok(toml) => {
                        app.sel_contact = None;
                        write_codeplug(&mut app, &weak, toml, "Contacts", &format!("Deleted {name}"), Screen::Contacts);
                    }
                    Err(e) => app.notify(&weak, "Delete failed", &e, 0),
                }
            }
            AppEvent::UiEditFocus(field) => {
                if let Some(d) = app.contact_draft.as_mut() {
                    d.focus = match field {
                        1 => EditField::Callsign,
                        2 => EditField::Issi,
                        3 => EditField::Number,
                        _ => EditField::Name,
                    };
                    push_contact_editor(&app, &weak);
                }
            }
            AppEvent::UiEditKey(s) => {
                if let Some(d) = app.contact_draft.as_mut() {
                    let numeric = d.focus.numeric();
                    // ISSI is digits only; the phone number also allows * # +.
                    let allow_symbols = matches!(d.focus, EditField::Number);
                    for ch in s.chars() {
                        if numeric {
                            let ok = ch.is_ascii_digit()
                                || (allow_symbols && (ch == '*' || ch == '#' || ch == '+'));
                            if ok {
                                d.field_mut().push(ch);
                            }
                        } else {
                            let ch = if d.shift { ch.to_ascii_uppercase() } else { ch };
                            d.field_mut().push(ch);
                            d.shift = false;
                        }
                    }
                    push_contact_editor(&app, &weak);
                }
            }
            AppEvent::UiEditBackspace => {
                if let Some(d) = app.contact_draft.as_mut() {
                    d.field_mut().pop();
                    push_contact_editor(&app, &weak);
                }
            }
            AppEvent::UiEditShift => {
                if let Some(d) = app.contact_draft.as_mut() {
                    d.shift = !d.shift;
                    push_contact_editor(&app, &weak);
                }
            }
            AppEvent::UiEditTarget(i) => {
                // 0 = Private (ISSI); i>=1 selects codeplug.gateways[i-1] (phone).
                let gid = if i == 0 {
                    None
                } else {
                    app.codeplug
                        .as_ref()
                        .and_then(|cp| cp.gateways.get((i - 1) as usize))
                        .map(|g| g.id.clone())
                };
                if let Some(d) = app.contact_draft.as_mut() {
                    if i == 0 {
                        d.is_phone = false;
                        d.gateway_id.clear();
                        d.focus = EditField::Issi;
                    } else if let Some(gid) = gid {
                        d.is_phone = true;
                        d.gateway_id = gid;
                        d.focus = EditField::Number;
                    }
                    push_contact_editor(&app, &weak);
                }
            }
            AppEvent::UiContactCancel => {
                app.contact_draft = None;
            }            AppEvent::UiContactSave => {
                let Some(d) = app.contact_draft.as_ref() else { continue };
                let input = crate::codeplug::ContactInput {
                    name: d.name.trim().to_string(),
                    callsign: Some(d.callsign.trim().to_string()).filter(|s| !s.is_empty()),
                    issi: if d.is_phone { None } else { d.issi.parse::<u32>().ok() },
                    number: if d.is_phone { Some(d.number.clone()) } else { None },
                    gateway: if d.is_phone { Some(d.gateway_id.clone()).filter(|s| !s.is_empty()) } else { None },
                };
                let key = d.key_name.clone();
                let Some(cp) = app.codeplug.as_ref() else {
                    app.notify(&weak, "No codeplug", "Configuration not loaded yet.", 0);
                    continue;
                };
                if let Err(e) = input.validate(cp) {
                    app.notify(&weak, "Invalid contact", &e, 0);
                    continue;
                }
                match crate::codeplug::upsert_contact(&app.last_config_toml, &input, key.as_deref()) {
                    Ok(toml) => {
                        let saved = input.name.clone();
                        app.contact_draft = None;
                        write_codeplug(&mut app, &weak, toml, "Contacts", &format!("Saved {saved}"), Screen::Contacts);
                    }
                    Err(e) => app.notify(&weak, "Save failed", &e, 0),
                }
            }
            AppEvent::UiDtmf(key) => {
                let Some(cid) = in_call_individual(&app) else { continue };
                let active = app
                    .calls
                    .get(&cid)
                    .map(|c| c.state == CallState::Active)
                    .unwrap_or(false);
                if !active {
                    continue;
                }
                let ch = key.chars().next().unwrap_or(' ');
                if let Some(name) = protocol::dtmf_digit_name(ch) {
                    let h = app.next_handle();
                    app.send(protocol::tncc_dtmf(h, cid, name));
                    app.dtmf_echo.push(ch);
                    while app.dtmf_echo.chars().count() > 32 {
                        app.dtmf_echo.remove(0);
                    }
                    push_calls(&app, &weak);
                }
            }
            AppEvent::UiContactSearchKey(s) => {
                for ch in s.chars() {
                    app.contact_query.push(ch);
                }
                push_contacts(&app, &weak);
            }
            AppEvent::UiContactSearchBackspace => {
                app.contact_query.pop();
                push_contacts(&app, &weak);
            }
            AppEvent::UiContactSearchClear => {
                app.contact_query.clear();
                push_contacts(&app, &weak);
            }
            AppEvent::UiCallPttDown => {
                if !app.require_online(&weak) {
                    continue;
                }
                if let Some(cid) = live_individual_call(&app) {
                    app.ptt_held = Some(cid);
                    let h = app.next_handle();
                    app.send(protocol::tncc_tx(h, cid, true));
                    sync_uplink(&app, audio.as_ref());
                    push_calls(&app, &weak);
                }
            }
            AppEvent::UiCallPttUp => {
                if let Some(cid) = app.ptt_held.take() {
                    let h = app.next_handle();
                    app.send(protocol::tncc_tx(h, cid, false));
                    sync_uplink(&app, audio.as_ref());
                    push_calls(&app, &weak);
                }
            }
            AppEvent::UiGroupPttDown => {
                if !app.require_online(&weak) {
                    continue;
                }
                if app.state.registration_state != protocol::RegistrationState::Registered {
                    app.notify(&weak, "Not registered", "Register the radio before transmitting.", 0);
                    continue;
                }
                let Some(sel) = effective_tx(&app) else {
                    app.notify(&weak, "No talkgroup", "Attach a talkgroup before transmitting.", 0);
                    continue;
                };
                if !ptt_allowed(&app) {
                    continue;
                }
                let existing = active_group_call(&app);
                if app.grp_call.is_none() && existing.is_none() {
                    // Idle -> start the group call; TnccSetup demands the floor.
                    app.grp_call = Some(GrpCall { gssi: sel, cid: None, talking: true });
                    app.dialing = Some(Dialing { peer_ssi: sel, group: true, simplex: true, peer_label: None, peer_sub: None });
                    let h = app.next_handle();
                    app.send(protocol::tncc_setup(h, sel, true, false));
                } else {
                    // A group call exists -> adopt it and demand the floor again.
                    let cid = app
                        .grp_call
                        .as_ref()
                        .and_then(|g| g.cid)
                        .or(existing);
                    app.grp_call = Some(GrpCall { gssi: sel, cid, talking: true });
                    if let Some(cid) = cid {
                        let h = app.next_handle();
                        app.send(protocol::tncc_tx(h, cid, true));
                    }
                }
                sync_uplink(&app, audio.as_ref());
                push_calls(&app, &weak);
            }
            AppEvent::UiGroupPttUp => {
                if let Some(gc) = app.grp_call.as_mut() {
                    let was = gc.talking;
                    gc.talking = false;
                    let cid = gc.cid.or_else(|| active_group_call(&app));
                    if was {
                        if let Some(cid) = cid {
                            let h = app.next_handle();
                            app.send(protocol::tncc_tx(h, cid, false));
                        }
                    }
                }
                sync_uplink(&app, audio.as_ref());
                push_calls(&app, &weak);
            }
            AppEvent::UiAnswerCall => {
                if !app.require_online(&weak) {
                    continue;
                }
                if let Some(cid) = incoming_call(&app) {
                    app.dtmf_echo.clear();
                    let (on_hook, duplex) = app
                        .calls
                        .get(&cid)
                        .map(|c| (c.hook_on, !c.simplex))
                        .unwrap_or((false, false));
                    let h = app.next_handle();
                    if on_hook {
                        app.send(protocol::tncc_complete(h, cid, duplex));
                    } else {
                        app.send(protocol::tncc_setup_response(h, cid, duplex, false));
                    }
                    if let Some(c) = app.calls.get_mut(&cid) {
                        c.answered = true;
                        c.state = CallState::Connecting;
                    }
                    push_calls(&app, &weak);
                }
            }
            AppEvent::UiRejectCall => {
                if let Some(cid) = incoming_call(&app) {
                    let h = app.next_handle();
                    app.send(protocol::tncc_release(h, cid, true));
                    app.local_end.insert(cid);
                    app.calls.remove(&cid);
                    push_calls(&app, &weak);
                }
            }
            AppEvent::UiHangup => {
                if let Some(cid) = live_call(&app) {
                    app.local_end.insert(cid);
                    app.ptt_held = None;
                    let h = app.next_handle();
                    app.send(protocol::tncc_release(h, cid, false));
                    app.calls.remove(&cid);
                }
                app.dialing = None;
                sync_uplink(&app, audio.as_ref());
                push_calls(&app, &weak);
            }
            AppEvent::UiHangupGroup => {
                let cid = active_group_call(&app).or_else(|| app.grp_call.as_ref().and_then(|g| g.cid));
                if let Some(cid) = cid {
                    app.local_end.insert(cid);
                    let h = app.next_handle();
                    app.send(protocol::tncc_release(h, cid, false));
                    app.calls.remove(&cid);
                }
                app.grp_call = None;
                if app.dialing.as_ref().map(|d| d.group).unwrap_or(false) {
                    app.dialing = None;
                }
                sync_uplink(&app, audio.as_ref());
                push_calls(&app, &weak);
            }
            AppEvent::UiToggleMute => {
                app.mic_muted = !app.mic_muted;
                tracing::info!(muted = app.mic_muted, "UI: toggle mic mute");
                sync_uplink(&app, audio.as_ref());
                push_calls(&app, &weak);
            }
            AppEvent::UplinkAudio(cid, bits) => {
                // Fire-and-forget uplink speech (no handle, not acked).
                if app.control_connected {
                    app.send(protocol::ms_uplink_speech(cid, bits, 274));
                }
            }
            AppEvent::UiOpenLogs => {
                app.unread = 0;
                push_logs(&app, &weak);
            }
            AppEvent::UiAlertDismiss => {
                // Invalidate any pending auto-dismiss and hide now.
                app.alert_gen = app.alert_gen.wrapping_add(1);
                let _ = weak.upgrade_in_event_loop(|w| w.set_alert_visible(false));
            }
            AppEvent::AlertExpire(gen) => {
                if gen == app.alert_gen {
                    let _ = weak.upgrade_in_event_loop(|w| w.set_alert_visible(false));
                }
            }
            AppEvent::UiGroupSelect(gssi, cou) => {
                if app.require_registered(&weak) {
                    let h = app.next_handle();
                    tracing::info!(gssi, cou, "UI: group select (switch TX)");
                    app.send(protocol::tnmm_switch_talkgroup(h, gssi as u32, cou as u8));
                    app.selected_tx = Some(gssi as u32);
                    // Point the home cycler at the group we just switched to (and
                    // its folder) so returning home shows it, not a folder default.
                    if let Some(cp) = &app.codeplug {
                        if let Some(fidx) = cp
                            .folders
                            .iter()
                            .position(|f| f.talkgroups.iter().any(|t| t.gssi == gssi as u32))
                        {
                            app.sel_folder = fidx;
                        }
                    }
                    app.cycle_gssi = Some(gssi as u32);
                    push_ui(&app, &weak);
                    let h = app.next_handle();
                    app.send(protocol::get_state(h));
                }
            }
            AppEvent::UiGroupAttach(gssi, cou) => {
                if app.require_registered(&weak) {
                    let h = app.next_handle();
                    tracing::info!(gssi, cou, "UI: group attach (add to scan)");
                    app.send(protocol::tnmm_attach_group(h, gssi as u32, cou as u8));
                    let h = app.next_handle();
                    app.send(protocol::get_state(h));
                }
            }
            AppEvent::UiGroupDetach(gssi) => {
                if app.require_registered(&weak) {
                    let h = app.next_handle();
                    tracing::info!(gssi, "UI: group detach");
                    app.send(protocol::tnmm_detach_group(h, gssi as u32));
                    let h = app.next_handle();
                    app.send(protocol::get_state(h));
                }
            }
            AppEvent::UiScanlistToggle(name, active) => {
                if app.require_registered(&weak) {
                    let h = app.next_handle();
                    tracing::info!(%name, active, "UI: scanlist toggle");
                    app.send(protocol::activate_scanlist(h, &name, active));
                    let h = app.next_handle();
                    app.send(protocol::get_state(h));
                }
            }
            AppEvent::UiSurveyToggleMode => {
                if app.require_online(&weak) {
                    let manual = app.state.selection_mode_manual.unwrap_or(false);
                    let next = !manual;
                    let h = app.next_handle();
                    tracing::info!(next, "UI: set cell selection mode");
                    app.send(protocol::set_cell_selection_mode(h, next));
                    // Optimistic; GetState confirms.
                    app.state.selection_mode_manual = Some(next);
                    if !next {
                        app.scan_rows.clear();
                        app.scanning = false;
                        app.scan_complete = false;
                    }
                    let h = app.next_handle();
                    app.send(protocol::get_state(h));
                    push_survey(&app, &weak);
                    push_ui(&app, &weak);
                }
            }
            AppEvent::UiSurveyScan => {
                if app.require_online(&weak) {
                    app.scan_rows.clear();
                    app.scanning = true;
                    app.scan_complete = false;
                    let h = app.next_handle();
                    tracing::info!("UI: start cell scan");
                    app.send(protocol::start_cell_scan(h));
                    push_survey(&app, &weak);
                }
            }
            AppEvent::UiSurveyStop => {
                let h = app.next_handle();
                tracing::info!("UI: stop cell scan");
                app.send(protocol::stop_cell_scan(h));
                app.scanning = false;
                push_survey(&app, &weak);
            }
            AppEvent::UiCampCell(carrier, register) => {
                if app.require_online(&weak) {
                    let h = app.next_handle();
                    tracing::info!(carrier, register, "UI: camp on cell");
                    app.send(protocol::camp_on_cell(h, carrier, register));
                    let h = app.next_handle();
                    app.send(protocol::get_state(h));
                }
            }
            AppEvent::UiApplyConfig => {
                if app.require_online(&weak) {
                    let h = app.next_handle();
                    tracing::info!("UI: apply config");
                    app.send(protocol::apply_config(h));
                }
            }
            AppEvent::UiRefresh => {
                if app.control_connected {
                    let h = app.next_handle();
                    app.send(protocol::get_state(h));
                    let h = app.next_handle();
                    app.send(protocol::get_config(h));
                }
            }
            AppEvent::UiOpenMessages => {
                app.msg_thread_peer = None;
                push_conversations(&app, &weak);
            }
            AppEvent::UiOpenThread(peer) => {
                open_thread(&mut app, &weak, peer as u32, false);
            }
            AppEvent::UiMessageContact(idx) => {
                let ssi = app
                    .codeplug
                    .as_ref()
                    .and_then(|cp| cp.contacts.get(idx as usize))
                    .and_then(|c| c.issi);
                if let Some(ssi) = ssi {
                    open_thread(&mut app, &weak, ssi, false);
                } else {
                    app.notify(&weak, "Cannot message", "This contact has no ISSI.", 0);
                }
            }
            AppEvent::UiMsgKey(s) => {
                for ch in s.chars() {
                    let ch = if app.msg_shift { ch.to_ascii_uppercase() } else { ch };
                    app.msg_draft.push(ch);
                    app.msg_shift = false;
                }
                push_thread(&app, &weak);
            }
            AppEvent::UiMsgBackspace => {
                app.msg_draft.pop();
                push_thread(&app, &weak);
            }
            AppEvent::UiMsgShift => {
                app.msg_shift = !app.msg_shift;
                push_thread(&app, &weak);
            }
            AppEvent::UiMsgSend => {
                send_message(&mut app, &weak);
            }
            AppEvent::UiMsgNew => {
                app.msg_new_issi.clear();
                let _ = weak.upgrade_in_event_loop(|w| w.set_msg_new_issi("".into()));
            }
            AppEvent::UiMsgNewKey(s) => {
                for ch in s.chars() {
                    if ch.is_ascii_digit() && app.msg_new_issi.len() < 8 {
                        app.msg_new_issi.push(ch);
                    }
                }
                let out = app.msg_new_issi.clone();
                let _ = weak.upgrade_in_event_loop(move |w| w.set_msg_new_issi(out.into()));
            }
            AppEvent::UiMsgNewBackspace => {
                app.msg_new_issi.pop();
                let out = app.msg_new_issi.clone();
                let _ = weak.upgrade_in_event_loop(move |w| w.set_msg_new_issi(out.into()));
            }
            AppEvent::UiMsgNewStart => {
                match app.msg_new_issi.parse::<u32>() {
                    Ok(ssi) if ssi > 0 => {
                        open_thread(&mut app, &weak, ssi, false);
                        let _ = weak.upgrade_in_event_loop(|w| w.set_screen(Screen::MsgThread));
                    }
                    _ => app.notify(&weak, "Invalid ISSI", "Enter a valid ISSI number.", 0),
                }
            }
            AppEvent::UiMsgDelete(id) => {
                let id = id as u64;
                let before = app.messages.messages.len();
                app.messages.messages.retain(|m| m.id != id);
                if app.messages.messages.len() != before {
                    app.messages.save();
                    push_thread(&app, &weak);
                    push_conversations(&app, &weak);
                }
            }
            AppEvent::UiMsgDeleteThread(peer, is_group) => {
                let ssi = peer as u32;
                let before = app.messages.messages.len();
                app.messages
                    .messages
                    .retain(|m| !(m.peer_ssi == ssi && m.is_group == is_group));
                if app.messages.messages.len() != before {
                    app.messages.save();
                    // If the deleted thread was open, drop back to the list.
                    if app.msg_thread_peer == Some((ssi, is_group)) {
                        app.msg_thread_peer = None;
                        let _ = weak.upgrade_in_event_loop(|w| w.set_screen(Screen::Messages));
                    }
                    push_thread(&app, &weak);
                    push_conversations(&app, &weak);
                }
            }
            AppEvent::UiProgSection(s) => {
                prog_open_section(&mut app, &weak, s);
            }
            AppEvent::UiProgOpen(i) => {
                prog_open_entry(&mut app, &weak, i as usize);
            }
            AppEvent::UiProgAdd => {
                prog_add_entry(&mut app, &weak);
            }
            AppEvent::UiFormPick(i) => {
                if let Some(d) = app.prog_draft.as_mut() {
                    if d.fields.get(i as usize).map(|f| f.is_focusable()).unwrap_or(false) {
                        d.focus = i as usize;
                        push_form(&app, &weak);
                    }
                }
            }
            AppEvent::UiFormKey(s) => {
                if let Some(d) = app.prog_draft.as_mut() {
                    let shift = d.shift;
                    if let Some(f) = d.focused() {
                        match f.kind {
                            FieldKind::Digits => {
                                for ch in s.chars() {
                                    if ch.is_ascii_digit() {
                                        f.value.push(ch);
                                    }
                                }
                            }
                            FieldKind::Dial => {
                                for ch in s.chars() {
                                    if ch.is_ascii_digit() || ch == '*' || ch == '#' || ch == '+' {
                                        f.value.push(ch);
                                    }
                                }
                            }
                            FieldKind::Text => {
                                for ch in s.chars() {
                                    let ch = if shift { ch.to_ascii_uppercase() } else { ch };
                                    f.value.push(ch);
                                }
                                d.shift = false;
                            }
                            _ => {}
                        }
                    }
                    push_form(&app, &weak);
                }
            }
            AppEvent::UiFormBackspace => {
                if let Some(d) = app.prog_draft.as_mut() {
                    if let Some(f) = d.focused() {
                        f.value.pop();
                    }
                    push_form(&app, &weak);
                }
            }
            AppEvent::UiFormShift => {
                if let Some(d) = app.prog_draft.as_mut() {
                    d.shift = !d.shift;
                    push_form(&app, &weak);
                }
            }
            AppEvent::UiFormToggle(i) => {
                if let Some(d) = app.prog_draft.as_mut() {
                    if let Some(f) = d.fields.get_mut(i as usize) {
                        if f.kind == FieldKind::Toggle {
                            f.on = !f.on;
                        }
                    }
                    push_form(&app, &weak);
                }
            }
            AppEvent::UiFormCycle(i) => {
                if let Some(d) = app.prog_draft.as_mut() {
                    if let Some(f) = d.fields.get_mut(i as usize) {
                        if f.kind == FieldKind::Cycle && !f.options.is_empty() {
                            f.opt_idx = (f.opt_idx + 1) % f.options.len();
                            f.value = f.options[f.opt_idx].clone();
                        }
                    }
                    push_form(&app, &weak);
                }
            }
            AppEvent::UiFormSave => {
                prog_save(&mut app, &weak);
            }
            AppEvent::UiFormCancel => {
                let back = prog_return_screen(app.prog_section);
                app.prog_draft = None;
                refresh_prog(&app, &weak, back);
                let _ = weak.upgrade_in_event_loop(move |w| w.set_screen(back));
            }
            AppEvent::UiFormDelete => {
                prog_delete(&mut app, &weak);
            }
            AppEvent::UiOpenTree => {
                push_tree(&app, &weak);
                let _ = weak.upgrade_in_event_loop(|w| w.set_screen(Screen::Tree));
            }
            AppEvent::UiTreeFolder(i) => {
                app.prog_section = ProgSection::Folders;
                if let Some(d) = build_draft(&app, Some(i as usize)) {
                    app.prog_draft = Some(d);
                    push_form(&app, &weak);
                    let _ = weak.upgrade_in_event_loop(|w| w.set_screen(Screen::ProgramEdit));
                }
            }
            AppEvent::UiTreeGroup(i) => {
                app.prog_section = ProgSection::Talkgroups;
                if let Some(d) = build_draft(&app, Some(i as usize)) {
                    app.prog_draft = Some(d);
                    push_form(&app, &weak);
                    let _ = weak.upgrade_in_event_loop(|w| w.set_screen(Screen::ProgramEdit));
                }
            }
            AppEvent::UiTreeAddGroup(folder_idx) => {
                app.prog_section = ProgSection::Talkgroups;
                if let Some(mut d) = build_draft(&app, None) {
                    // The folder is set by context (which folder we added into).
                    d.tg_folder = if folder_idx >= 0 {
                        app.codeplug
                            .as_ref()
                            .and_then(|cp| cp.folder_defs.get(folder_idx as usize))
                            .map(|f| f.id.clone())
                    } else {
                        None
                    };
                    app.prog_draft = Some(d);
                    push_form(&app, &weak);
                    let _ = weak.upgrade_in_event_loop(|w| w.set_screen(Screen::ProgramEdit));
                }
            }
            AppEvent::UiTreeAddFolder => {
                app.prog_section = ProgSection::Folders;
                if let Some(d) = build_draft(&app, None) {
                    app.prog_draft = Some(d);
                    push_form(&app, &weak);
                    let _ = weak.upgrade_in_event_loop(|w| w.set_screen(Screen::ProgramEdit));
                }
            }
            AppEvent::UiTreeToggle(i) => {
                let key = tree_folder_key(&app, i);
                if !app.collapsed_folders.remove(&key) {
                    app.collapsed_folders.insert(key);
                }
                push_tree(&app, &weak);
            }
            AppEvent::UiTreeMoveUp(i) => {
                tree_reorder(&mut app, &weak, i as usize, true);
            }
            AppEvent::UiTreeMoveDown(i) => {
                tree_reorder(&mut app, &weak, i as usize, false);
            }
        }
    }
}

fn handle_control(app: &mut AppState, message: &Value, weak: &slint::Weak<MainWindow>) {
    let Some((variant, payload)) = protocol::variant_of(message) else {
        tracing::warn!(?message, "control: undecodable/none-variant frame");
        return;
    };
    // A response clears its pending command and any active stall.
    if let Some(h) = command_handle(message) {
        app.pending.remove(&h);
        app.timeout_notified = false;
    }
    match variant {
        "Management" => {
            let Some((inner, body)) = protocol::variant_of(payload) else {
                return;
            };
            match inner {
                "State" => match serde_json::from_value::<MsRuntimeState>(body["state"].clone()) {
                    Ok(state) => {
                        let changed = !app.logged_state
                            || state.registration_state != app.state.registration_state
                            || state.service_status != app.state.service_status
                            || state.attached_groups != app.state.attached_groups
                            || protocol::rssi_to_bars(state.rssi_dbfs)
                                != protocol::rssi_to_bars(app.state.rssi_dbfs);
                        if changed {
                            tracing::info!(
                                reg = state.registration_state.label(),
                                service = state.service_status.label(),
                                issi = state.own_issi,
                                groups = ?state.attached_groups,
                                bars = protocol::rssi_to_bars(state.rssi_dbfs),
                                "state updated"
                            );
                            app.logged_state = true;
                        }
                        app.state = state;
                        reconcile_home_view(app);
                        push_ui(app, weak);
                        push_survey(app, weak);
                    }
                    Err(e) => tracing::warn!(error = %e, "failed to parse MsRuntimeState"),
                },
                "InterfaceVersion" => {
                    tracing::info!(version = ?body.get("version"), "interface version");
                }
                "Config" => {
                    if let Some(toml) = body.get("toml").and_then(Value::as_str) {
                        if !toml.trim().is_empty() {
                            app.have_config = true;
                            app.last_config_toml = toml.to_string();
                            let t = toml.to_string();
                            let weak2 = weak.clone();
                            let _ = weak2
                                .upgrade_in_event_loop(move |w| w.set_config_toml(t.into()));
                            match Codeplug::parse(toml) {
                                Some(cp) => {
                                    tracing::info!(
                                        folders = cp.folders.len(),
                                        "codeplug parsed"
                                    );
                                    app.codeplug = Some(cp);
                                    app.sel_folder = 0;
                                    app.cycle_gssi = None;
                                    reconcile_home_view(app);
                                    push_ui(app, weak);
                                }
                                None => tracing::info!("config has no talkgroups"),
                            }
                        }
                    }
                }
                "Ack" => {
                    tracing::info!(?body, "management ack");
                    if body.get("accepted").and_then(Value::as_bool) == Some(false) {
                        let detail = body
                            .get("message")
                            .and_then(Value::as_str)
                            .unwrap_or("The stack rejected the request.");
                        app.notify(weak, "Command rejected", detail, 0);
                    } else if body.get("restart_required").and_then(Value::as_bool) == Some(true) {
                        app.notify(
                            weak,
                            "Restart required",
                            "Configuration staged - restart the stack to apply.",
                            0,
                        );
                    }
                }
                "Error" => {
                    tracing::warn!(?body, "management error");
                    let msg = body
                        .get("message")
                        .and_then(Value::as_str)
                        .unwrap_or("Management command failed.");
                    app.notify(weak, "Error", msg, 1);
                }
                other => tracing::info!(variant = other, "management response (unhandled)"),
            }
        }
        "TnmmAck" => {
            tracing::info!(?payload, "TnmmAck");
            if payload.get("accepted").and_then(Value::as_bool) == Some(false) {
                let detail = payload
                    .get("detail")
                    .and_then(Value::as_str)
                    .unwrap_or("The stack rejected the request.");
                app.notify(weak, "Command rejected", detail, 0);
            }
        }
        "TnccAck" => {
            tracing::info!(?payload, "TnccAck");
            if payload.get("accepted").and_then(Value::as_bool) == Some(false) {
                let detail = payload
                    .get("detail")
                    .and_then(Value::as_str)
                    .unwrap_or("The stack rejected the call request.");
                app.notify(weak, "Call rejected", detail, 0);
            }
        }
        other => tracing::info!(variant = other, "control response (unhandled)"),
    }
}

fn handle_telemetry(
    app: &mut AppState,
    message: &Value,
    weak: &slint::Weak<MainWindow>,
    audio: Option<&crate::audio::AudioEngine>,
) {
    let Some((variant, payload)) = protocol::variant_of(message) else {
        tracing::warn!("telemetry: undecodable/none-variant frame");
        return;
    };
    // Downlink voice is high-rate: decode + play it, attribute the talker, and
    // refresh the call UI, but never log each frame.
    if variant == "MsSpeechFrame" {
        handle_speech_frame(app, payload, weak, audio);
        return;
    }
    tracing::info!(variant, "telemetry event");

    // Record it in the event log (newest last, capped) and bump unread.
    let time = chrono::Local::now().format("%H:%M:%S").to_string();
    app.events.push_back(LogEntry {
        time,
        variant: variant.to_string(),
        summary: summarize_event(variant, payload),
        detail: serde_json::to_string_pretty(message).unwrap_or_else(|_| message.to_string()),
    });
    while app.events.len() > 200 {
        app.events.pop_front();
    }
    app.unread = (app.unread + 1).min(999);
    push_logs(app, weak);

    // Important, actionable telemetry raises a notification (routine attach/detach
    // and registration success are reflected by the UI state itself).
    match variant {
        "TnmmRegistrationConfirm" | "TnmmRegistrationIndication" => {
            let status = payload.get("registration_status").and_then(Value::as_str);
            if status != Some("Success") {
                let cause = payload
                    .get("registration_reject_cause")
                    .and_then(Value::as_str)
                    .unwrap_or("The network rejected registration.");
                app.notify(weak, "Registration failed", cause, 1);
            }
        }
        "TnmmServiceIndication" => {
            if let Some(now) = payload
                .get("service_status")
                .and_then(|v| serde_json::from_value::<ServiceStatus>(v.clone()).ok())
            {
                if now == ServiceStatus::OutOfService && app.last_service != ServiceStatus::OutOfService
                {
                    app.notify(
                        weak,
                        "No service",
                        "Downlink lost - the radio has left coverage.",
                        1,
                    );
                }
                app.last_service = now;
            }
        }
        "MsScanResult" => {
            app.scanning = true;
            app.scan_complete = false;
            app.scan_rows.push(ScanCell {
                carrier_hz: payload.get("carrier_hz").and_then(Value::as_u64).unwrap_or(0),
                rssi_dbfs: payload
                    .get("rssi_dbfs")
                    .and_then(Value::as_f64)
                    .map(|v| v as f32),
                mcc: payload.get("mcc").and_then(Value::as_i64),
                mnc: payload.get("mnc").and_then(Value::as_i64),
                location_area: payload.get("location_area").and_then(Value::as_i64),
                registration_required: payload
                    .get("registration_required")
                    .and_then(Value::as_bool),
                late_entry_supported: payload
                    .get("late_entry_supported")
                    .and_then(Value::as_bool),
            });
            push_survey(app, weak);
        }
        "MsScanComplete" => {
            app.scanning = false;
            app.scan_complete = true;
            let found = payload
                .get("found")
                .and_then(Value::as_i64)
                .unwrap_or(app.scan_rows.len() as i64) as i32;
            let scanned = payload.get("scanned").and_then(Value::as_i64).unwrap_or(0) as i32;
            app.scan_summary = (found, scanned);
            push_survey(app, weak);
        }
        "TnsdsMessageIndication" => {
            handle_sds_message_in(app, payload, weak);
        }
        "TnsdsReportIndication" => {
            handle_sds_report_in(app, payload, weak);
        }
        "TnsdsStatusIndication" => {
            let from = payload.get("calling_party_ssi").and_then(Value::as_u64).unwrap_or(0);
            let num = payload.get("status_number").and_then(Value::as_u64).unwrap_or(0);
            app.notify(weak, "Status received", &format!("From {from}: status {num}"), 0);
        }
        "TnsdsUnitdataIndication" => {
            let from = payload.get("calling_party_ssi").and_then(Value::as_u64).unwrap_or(0);
            tracing::info!(from, "SDS unitdata received (opaque, not shown)");
        }
        _ => {}
    }

    // Call-control telemetry drives the call state machine + call UI.
    if variant.starts_with("Tncc") {
        apply_call_event(app, variant, payload, weak);
        push_calls(app, weak);
    }

    // A state change means MsRuntimeState just moved; pull it right away instead
    // of waiting for the next poll tick.
    if protocol::is_state_changing_event(variant) && app.control_connected {
        let h = app.next_handle();
        app.send(protocol::get_state(h));
        if !app.have_config {
            let h = app.next_handle();
            app.send(protocol::get_config(h));
        }
    }
}

// --- Call state machine (M6) -------------------------------------------------

/// Nested `indication`/`confirm`/`request`/`response` body of a call event.
fn call_body(payload: &Value) -> &Value {
    for k in ["indication", "confirm", "request", "response"] {
        if let Some(b) = payload.get(k) {
            if b.is_object() {
                return b;
            }
        }
    }
    payload
}

/// Set a call's floor state from a TNCC transmission status + talker SSI.
fn apply_floor(c: &mut Call, status: Option<&str>, talker: Option<u32>, own_issi: u32) {
    let Some(status) = status else { return };
    c.tx_status = Some(status.to_string());
    let own_echo = talker.is_some() && own_issi != 0 && talker == Some(own_issi);
    if status == "TransmissionGrantedToAnotherUser" && !own_echo {
        c.talker_ssi = talker;
        c.can_request_tx = false;
        c.holds_floor = false;
    } else {
        c.talker_ssi = None;
        c.can_request_tx = true;
        c.holds_floor = status == "TransmissionGranted" || own_echo;
    }
}

fn apply_call_event(
    app: &mut AppState,
    variant: &str,
    payload: &Value,
    weak: &slint::Weak<MainWindow>,
) {
    let Some(cid) = payload.get("call_identifier").and_then(Value::as_u64) else {
        return;
    };
    let cid = cid as u32;
    let own_issi = app.state.own_issi;
    let body = call_body(payload).clone();

    if variant == "TnccReleaseIndication" || variant == "TnccReleaseConfirm" {
        // Take the call out of the map (if still present). The stack sends
        // several release frames per call, so only the first - when the call is
        // still known - is a real end; later duplicates find nothing and are
        // ignored, preventing repeated "Call ended" toasts.
        let removed = app.calls.remove(&cid);
        if app.dialing.as_ref().map(|d| d.peer_ssi).is_some() {
            app.dialing = None;
        }
        if app.grp_call.as_ref().and_then(|g| g.cid) == Some(cid) {
            app.grp_call = None;
        }
        if app.ptt_held == Some(cid) {
            app.ptt_held = None;
        }
        // Clear the mic-mute latch once no individual call remains.
        if !app.calls.values().any(|c| !c.group) {
            app.mic_muted = false;
        }
        // Drop a group context that never bound to a real call (e.g. our setup
        // was rejected before a confirm arrived) so the panel doesn't stick.
        if active_group_call(app).is_none()
            && app.grp_call.as_ref().map(|g| g.cid.is_none()).unwrap_or(false)
        {
            app.grp_call = None;
        }
        // "Call ended" toast only when a known individual call ends and we did
        // NOT hang it up ourselves (we clear local_end here either way).
        let local = app.local_end.remove(&cid);
        if let Some(c) = removed {
            if !local && !c.group {
                let cause = body
                    .get("disconnect_cause")
                    .and_then(Value::as_str)
                    .map(pretty_cause)
                    .unwrap_or_else(|| "The call was released.".to_string());
                app.notify(weak, "Call ended", &cause, 0);
            }
        }
        return;
    }

    let c = app.calls.entry(cid).or_insert_with(|| Call {
        cid,
        can_request_tx: true,
        simplex: true,
        ..Call::default()
    });

    match variant {
        "TnccSetupIndication" => {
            let bsi = body.get("basic_service_information").cloned().unwrap_or(Value::Null);
            let comm = bsi.get("communication_type").and_then(Value::as_str);
            let is_group = comm.map(|t| t != "PointToPoint").unwrap_or(false);
            c.group = c.group || is_group;
            if c.group {
                if let Some(g) = body.get("called_party_ssi").and_then(Value::as_u64) {
                    c.peer_ssi = Some(g as u32);
                }
            } else {
                c.direction = Some("mt");
                if let Some(p) = body.get("calling_party_ssi").and_then(Value::as_u64) {
                    c.peer_ssi = Some(p as u32);
                }
            }
            c.simplex = body.get("simplex_duplex_selection").and_then(Value::as_str)
                != Some("DuplexOperation");
            c.hook_on = body.get("hook_method_selection").and_then(Value::as_str)
                == Some("HookOnHookOffSignallingOrCallAcceptanceSignalling");
            let caller = body.get("calling_party_ssi").and_then(Value::as_u64).map(|v| v as u32);
            if caller.is_some() && caller != Some(own_issi) {
                let grant = body.get("transmission_grant").and_then(Value::as_str);
                apply_floor(c, grant, caller, own_issi);
            }
            c.state = if c.group { CallState::Active } else { CallState::Incoming };
        }
        "TnccProceedIndication" => {
            if c.direction.is_none() {
                c.direction = Some("mo");
            }
            c.state = CallState::Proceeding;
            bind_dialing(app, cid);
        }
        "TnccAlertIndication" => {
            if c.direction.is_none() {
                c.direction = Some("mo");
            }
            c.state = CallState::Alerting;
            c.queued = body.get("call_queued").and_then(Value::as_str) == Some("CallIsQueued");
            bind_dialing(app, cid);
        }
        "TnccSetupConfirm" | "TnccCompleteConfirm" => {
            if c.direction.is_none() {
                c.direction = Some("mo");
            }
            c.state = CallState::Active;
            let bsi = body.get("basic_service_information").cloned().unwrap_or(Value::Null);
            if let Some(t) = bsi.get("communication_type").and_then(Value::as_str) {
                if t != "PointToPoint" {
                    c.group = true;
                }
            }
            let grant = body
                .get("transmission_grant")
                .or_else(|| body.get("transmission_status"))
                .and_then(Value::as_str);
            apply_floor(c, grant, None, own_issi);
            bind_dialing(app, cid);
        }
        "TnccTxIndication" | "TnccTxConfirm" => {
            let st = body
                .get("transmission_status")
                .or_else(|| body.get("transmission_grant"))
                .and_then(Value::as_str);
            let who = body.get("transmitting_party_ssi").and_then(Value::as_u64).map(|v| v as u32);
            apply_floor(c, st, who, own_issi);
            if matches!(c.state, CallState::Setup) {
                c.state = CallState::Active;
            }
        }
        _ => {}
    }

    if let Some(c) = app.calls.get_mut(&cid) {
        if c.state == CallState::Active && c.active_since.is_none() {
            c.active_since = Some(Instant::now());
        }
        if c.state == CallState::Incoming {
            c.rang = true;
        }
        if c.group {
            // Keep the group PTT context bound to this call id.
            if let Some(g) = app.grp_call.as_mut() {
                if g.cid.is_none() {
                    g.cid = Some(cid);
                }
            }
        }
    }
}

/// Absorb the outgoing placeholder into the real call once its id is known.
fn bind_dialing(app: &mut AppState, cid: u32) {
    let Some(d) = app.dialing.take() else { return };
    if let Some(c) = app.calls.get_mut(&cid) {
        if c.peer_ssi.is_none() {
            c.peer_ssi = Some(d.peer_ssi);
        }
        if c.peer_label.is_none() {
            c.peer_label = d.peer_label.clone();
        }
        if c.peer_sub.is_none() {
            c.peer_sub = d.peer_sub.clone();
        }
        if d.group {
            c.group = true;
        }
        c.simplex = d.simplex;
    }
    if d.group {
        if let Some(g) = app.grp_call.as_mut() {
            if g.cid.is_none() {
                g.cid = Some(cid);
            }
        }
    }
}

fn pretty_cause(cause: &str) -> String {
    // Split CamelCase into words.
    let mut out = String::new();
    for (i, ch) in cause.chars().enumerate() {
        if i > 0 && ch.is_uppercase() {
            out.push(' ');
        }
        out.push(ch);
    }
    out
}

/// First call in a live state, preferring a non-group individual call.
fn live_call(app: &AppState) -> Option<u32> {
    let live = |c: &&Call| {
        matches!(
            c.state,
            CallState::Proceeding | CallState::Alerting | CallState::Connecting | CallState::Active
        )
    };
    app.calls
        .values()
        .filter(live)
        .find(|c| !c.group)
        .or_else(|| app.calls.values().find(live))
        .map(|c| c.cid)
}

/// The individual call shown on the in-call screen (excludes an unanswered ring).
fn in_call_individual(app: &AppState) -> Option<u32> {
    app.calls
        .values()
        .find(|c| {
            !c.group
                && matches!(
                    c.state,
                    CallState::Proceeding
                        | CallState::Alerting
                        | CallState::Connecting
                        | CallState::Active
                )
                && !(c.rang && !c.answered)
        })
        .map(|c| c.cid)
}

/// The active individual simplex call whose floor the PTT controls.
fn live_individual_call(app: &AppState) -> Option<u32> {
    app.calls
        .values()
        .find(|c| !c.group && c.simplex && c.state == CallState::Active)
        .map(|c| c.cid)
}

/// An incoming individual call still waiting to be answered.
fn incoming_call(app: &AppState) -> Option<u32> {
    app.calls
        .values()
        .find(|c| !c.group && c.rang && !c.answered)
        .map(|c| c.cid)
}

/// The active group call (network- or self-established).
fn active_group_call(app: &AppState) -> Option<u32> {
    if let Some(g) = &app.grp_call {
        if let Some(cid) = g.cid {
            if app.calls.contains_key(&cid) {
                return Some(cid);
            }
        }
    }
    app.calls
        .values()
        .find(|c| {
            c.group
                && matches!(
                    c.state,
                    CallState::Active | CallState::Proceeding | CallState::Alerting
                )
        })
        .map(|c| c.cid)
}

/// Whether the SwMI currently permits us to key the active group call.
fn ptt_allowed(app: &AppState) -> bool {
    match active_group_call(app).and_then(|cid| app.calls.get(&cid)) {
        Some(c) => c.can_request_tx,
        None => true,
    }
}

/// Diagnostic counter for downlink speech frames (rate-limits the logging).
static SPEECH_SEEN: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Decode a downlink `MsSpeechFrame`, play it, and attribute the talker so the
/// "other talker" UI lights up even independently of the audio path.
fn handle_speech_frame(
    app: &mut AppState,
    payload: &Value,
    weak: &slint::Weak<MainWindow>,
    audio: Option<&crate::audio::AudioEngine>,
) {
    let Some(cid) = payload.get("call_identifier").and_then(Value::as_u64) else {
        return;
    };
    let cid = cid as u32;
    let talker = payload.get("transmitting_party_ssi").and_then(Value::as_u64).map(|v| v as u32);
    let bad = payload.get("bad_frame").and_then(Value::as_bool).unwrap_or(false);
    let frame_bits = payload.get("frame_bits").and_then(Value::as_u64);
    let own = app.state.own_issi;

    let transmitting = we_are_transmitting(app);
    let tx_block = tx_blocks_playback(app, cid);
    let is_own_echo = transmitting && own != 0 && talker == Some(own);
    let blocked = tx_block || is_own_echo;

    let data: Vec<u8> = payload
        .get("data")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .map(|v| if v.as_u64().unwrap_or(0) != 0 { 1u8 } else { 0u8 })
                .collect()
        })
        .unwrap_or_default();

    // Diagnostics: dump the first few raw frames (to see the real field names/
    // values) and then a periodic one-liner of the decode/gating decision.
    let n = SPEECH_SEEN.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    if n < 5 {
        tracing::info!(raw = %payload, "speech-frame raw (first 5)");
    }
    if n < 5 || n % 50 == 0 {
        tracing::info!(
            n,
            cid,
            ?talker,
            own,
            ?frame_bits,
            data_len = data.len(),
            transmitting,
            tx_block,
            is_own_echo,
            blocked,
            grp_talking = app.grp_call.as_ref().map(|g| g.talking).unwrap_or(false),
            grp_cid = ?app.grp_call.as_ref().and_then(|g| g.cid),
            ptt_held = ?app.ptt_held,
            "speech-frame decision"
        );
    }

    // Decode + play FIRST so the audio path is never delayed by UI work.
    if !blocked && matches!(frame_bits, None | Some(274)) && data.len() == 274 {
        if let Some(a) = audio {
            a.play_downlink(&data, bad);
        }
    }

    // Attribute the talker onto the live call (floor housekeeping). Only refresh
    // the call UI when it actually changes, not on every 60 ms frame. Our own
    // echo is not a real other talker, so ignore it here too.
    let mut changed = false;
    if !blocked {
        // Downlink audio is flowing for this call -> someone is speaking now.
        if let Some(c) = app.calls.get_mut(&cid) {
            let was_receiving = c.rx_at.map(|t| t.elapsed() < Duration::from_millis(700)).unwrap_or(false);
            c.rx_at = Some(Instant::now());
            if !was_receiving {
                changed = true;
            }
        }
    }
    if let Some(t) = talker {
        if let Some(c) = app.calls.get_mut(&cid) {
            if t != own && (c.talker_ssi != Some(t) || c.can_request_tx) {
                c.talker_ssi = Some(t);
                c.can_request_tx = false;
                changed = true;
            }
        }
    }
    if changed {
        push_calls(app, weak);
    }
}

/// True while our mic is (or should be) keying the network: keying a group call,
/// holding an individual PTT, or in an active duplex call (continuous mic). Used
/// to decide whether a talker==own-ISSI downlink frame is our own echo.
fn we_are_transmitting(app: &AppState) -> bool {
    if app.grp_call.as_ref().map(|g| g.talking).unwrap_or(false) {
        return true;
    }
    if app.ptt_held.is_some() {
        return true;
    }
    app.calls
        .values()
        .any(|c| !c.group && !c.simplex && c.state == CallState::Active)
}

/// While WE transmit, the SwMI echoes our own voice back as downlink frames.
/// Block playback then: keying a group call (half-duplex floor) or a simplex
/// individual call while PTT is held. Duplex calls stay full-duplex (play on).
fn tx_blocks_playback(app: &AppState, cid: u32) -> bool {
    if app.grp_call.as_ref().map(|g| g.talking).unwrap_or(false) {
        return true;
    }
    if app.ptt_held.is_some() {
        // simplex (true) blocks; a duplex call keeps playing the far end.
        if app.calls.get(&cid).map(|c| c.simplex).unwrap_or(true) {
            return true;
        }
    }
    false
}

/// Reconcile the mic uplink with the current floor: transmit only while we
/// physically hold the PTT on an active call (individual or group).
fn sync_uplink(app: &AppState, audio: Option<&crate::audio::AudioEngine>) {
    let Some(a) = audio else { return };
    // Muted mic: never transmit, whatever the floor state.
    if app.mic_muted {
        a.set_uplink(false, 0);
        return;
    }
    if let Some(cid) = app.ptt_held {
        if app.calls.get(&cid).map(|c| c.state == CallState::Active).unwrap_or(false) {
            a.set_uplink(true, cid);
            return;
        }
    }
    if app.grp_call.as_ref().map(|g| g.talking).unwrap_or(false) {
        if let Some(cid) = active_group_call(app) {
            a.set_uplink(true, cid);
            return;
        }
    }
    // A duplex individual call is through-connected both ways: stream the mic for
    // the whole call (no PTT), mirroring the web UI's syncDuplexMic.
    if let Some(c) = app
        .calls
        .values()
        .find(|c| !c.group && !c.simplex && c.state == CallState::Active)
    {
        a.set_uplink(true, c.cid);
        return;
    }
    a.set_uplink(false, 0);
}

/// A short human summary of a telemetry event for the log.
fn summarize_event(_variant: &str, payload: &Value) -> String {
    let mut parts: Vec<String> = Vec::new();
    if let Some(issi) = payload.get("issi").and_then(Value::as_u64) {
        parts.push(format!("issi {issi}"));
    }
    if let Some(gssis) = payload.get("gssis").and_then(Value::as_array) {
        let list: Vec<String> = gssis
            .iter()
            .filter_map(Value::as_u64)
            .map(|g| g.to_string())
            .collect();
        if !list.is_empty() {
            parts.push(format!("gssis [{}]", list.join(", ")));
        }
    }
    if let Some(s) = payload.get("service_status").and_then(Value::as_str) {
        parts.push(s.to_string());
    }
    if let Some(s) = payload.get("registration_status").and_then(Value::as_str) {
        parts.push(s.to_string());
    }
    parts.join(" - ")
}

/// Push the event log + unread count onto the Slint event loop (newest first).
fn push_logs(app: &AppState, weak: &slint::Weak<MainWindow>) {
    let rows: Vec<LogRow> = app
        .events
        .iter()
        .rev()
        .map(|e| LogRow {
            time: e.time.clone().into(),
            variant: e.variant.clone().into(),
            summary: e.summary.clone().into(),
            detail: e.detail.clone().into(),
        })
        .collect();
    let unread = app.unread;
    let _ = weak.upgrade_in_event_loop(move |w| {
        w.set_logs(ModelRc::new(VecModel::from(rows)));
        w.set_unread(unread);
    });
}

/// The effective TX talkgroup: the last one switched to if still attached,
/// otherwise the first attached group.
fn effective_tx(app: &AppState) -> Option<u32> {
    let groups = &app.state.attached_groups;
    if let Some(tx) = app.selected_tx {
        if groups.contains(&tx) {
            return Some(tx);
        }
    }
    groups.first().copied()
}

/// (gssi, on-air class of usage) of the talkgroup the cycler currently shows.
fn cycle_target(app: &AppState) -> Option<(u32, u8)> {
    let cp = app.codeplug.as_ref()?;
    let folder = cp.folders.get(app.sel_folder.min(cp.folders.len().saturating_sub(1)))?;
    let tgs = &folder.talkgroups;
    if tgs.is_empty() {
        return None;
    }
    let i = app
        .cycle_gssi
        .and_then(|g| tgs.iter().position(|t| t.gssi == g))
        .unwrap_or(0);
    tgs.get(i).map(|t| (t.gssi, t.class_of_usage))
}

/// Move the cycler within the selected folder by `dir` (+/-1), wrapping.
fn cycle(app: &mut AppState, dir: isize) {
    let Some(cp) = &app.codeplug else { return };
    if app.sel_folder >= cp.folders.len() {
        app.sel_folder = 0;
    }
    let Some(folder) = cp.folders.get(app.sel_folder) else { return };
    let tgs = &folder.talkgroups;
    if tgs.is_empty() {
        return;
    }
    let cur = app
        .cycle_gssi
        .and_then(|g| tgs.iter().position(|t| t.gssi == g))
        .unwrap_or(0);
    let i = (cur as isize + dir).rem_euclid(tgs.len() as isize) as usize;
    app.cycle_gssi = Some(tgs[i].gssi);
}

/// Keep the home cycler pointed at the attached TX group by default. When the
/// cycler has no selection (or points outside the current folder), snap it to
/// the effective TX group and its folder, else fall back to the folder's first
/// talkgroup. Mirrors the web UI's renderHomeCycler reconciliation and never
/// overrides a valid in-folder selection the operator browsed to.
fn reconcile_home_view(app: &mut AppState) {
    let folders_len = match &app.codeplug {
        Some(cp) => cp.folders.len(),
        None => return,
    };
    if folders_len == 0 {
        return;
    }
    if app.sel_folder >= folders_len {
        app.sel_folder = 0;
    }
    let cur_folder = app.sel_folder;
    let in_folder = {
        let cp = app.codeplug.as_ref().unwrap();
        app.cycle_gssi
            .map(|g| cp.folders[cur_folder].talkgroups.iter().any(|t| t.gssi == g))
            .unwrap_or(false)
    };
    if in_folder {
        return;
    }
    let tx = effective_tx(app);
    let (new_folder, new_gssi) = {
        let cp = app.codeplug.as_ref().unwrap();
        let owner = tx.and_then(|g| {
            cp.folders
                .iter()
                .position(|f| f.talkgroups.iter().any(|t| t.gssi == g))
                .map(|fidx| (fidx, g))
        });
        match owner {
            Some((fidx, g)) => (fidx, Some(g)),
            None => (
                cur_folder,
                cp.folders[cur_folder].talkgroups.first().map(|t| t.gssi),
            ),
        }
    };
    app.sel_folder = new_folder;
    app.cycle_gssi = new_gssi;
}

/// Snapshot the state into plain values and push them onto the Slint event loop.
fn push_ui(app: &AppState, weak: &slint::Weak<MainWindow>) {
    let s = &app.state;
    let control_connected = app.control_connected;
    let telemetry_connected = app.telemetry_connected;
    let reg_state = s.registration_state.label().to_string();
    let service_status = s.service_status.label().to_string();
    let in_service = s.service_status.in_service();
    let registered = s.registration_state == protocol::RegistrationState::Registered;
    let signal_bars = protocol::rssi_to_bars(s.rssi_dbfs);
    let rssi = match s.rssi_dbfs {
        Some(v) => format!("{v:.0} dBFS"),
        None => "--".to_string(),
    };
    let issi = if s.own_issi != 0 {
        s.own_issi.to_string()
    } else {
        "--".to_string()
    };
    let network = format!("{} / {}", s.home_mcc, s.home_mnc);
    let serving_la = s.serving_la.to_string();
    let colour_code = s.colour_code.to_string();
    let attached = &s.attached_groups;
    let attached_count = attached.len() as i32;
    let scan_active = attached.len() > 1;
    let tx = effective_tx(app);

    let has_codeplug = app.codeplug.is_some();
    let mut folder_name = "Talkgroups".to_string();
    let tg_name;
    let mut tg_id = "--".to_string();
    let mut tg_sub = String::new();
    let mut badge = String::new();
    let mut badge_kind: i32 = 0; // 0 not-selected, 1 scanning, 2 on-air
    let mut can_cycle = false;
    let mut select_enabled = false;
    let mut show_cancel = false;
    let mut sel_folder_idx: i32 = 0;
    let mut folder_rows: Vec<(String, i32, bool)> = Vec::new();

    if let Some(cp) = &app.codeplug {
        let fidx = if app.sel_folder < cp.folders.len() {
            app.sel_folder
        } else {
            0
        };
        sel_folder_idx = fidx as i32;
        let folder = &cp.folders[fidx];
        folder_name = folder.name.clone();
        let tgs = &folder.talkgroups;
        can_cycle = tgs.len() > 1;
        let i = app
            .cycle_gssi
            .and_then(|g| tgs.iter().position(|t| t.gssi == g))
            .unwrap_or(0);
        if let Some(t) = tgs.get(i) {
            tg_name = t.name.clone();
            tg_id = t.gssi.to_string();
            tg_sub = format!("GSSI {} ({}/{})", t.gssi, i + 1, tgs.len());
            let is_attached = attached.contains(&t.gssi);
            let is_tx = is_attached && Some(t.gssi) == tx;
            badge_kind = if is_tx {
                2
            } else if is_attached {
                1
            } else {
                0
            };
            badge = if is_tx {
                "ON AIR"
            } else if is_attached {
                "SCANNING"
            } else {
                "NOT SELECTED"
            }
            .to_string();
            select_enabled = registered && !is_tx;
            // Show Cancel only when a TX group is active and the cycler has
            // browsed away from it, so it can snap back.
            show_cancel = tx.is_some() && !is_tx;
        } else {
            tg_name = "No talkgroups".to_string();
        }
        folder_rows = cp
            .folders
            .iter()
            .enumerate()
            .map(|(idx, f)| (f.name.clone(), f.talkgroups.len() as i32, idx == fidx))
            .collect();
    } else {
        // Fallback (no codeplug yet): show the attached TX group by number.
        match tx {
            Some(gssi) => {
                tg_name = format!("TG {gssi}");
                tg_id = gssi.to_string();
                tg_sub = format!("GSSI {gssi}");
                badge_kind = if registered { 2 } else { 0 };
                badge = if registered { "ON AIR" } else { "SELECT" }.to_string();
            }
            None => {
                tg_name = if registered { "No group" } else { "--" }.to_string();
            }
        }
    }

    let ptt_enabled = registered && tx.is_some();
    let ptt_label = if ptt_enabled {
        "Push to talk"
    } else if registered {
        "No talkgroup"
    } else {
        "Register to talk"
    }
    .to_string();
    let restart_required = s.restart_required;

    let _ = weak.upgrade_in_event_loop(move |w| {
        w.set_control_connected(control_connected);
        w.set_telemetry_connected(telemetry_connected);
        w.set_reg_state(reg_state.into());
        w.set_service_status(service_status.into());
        w.set_in_service(in_service);
        w.set_registered(registered);
        w.set_signal_bars(signal_bars);
        w.set_rssi(rssi.into());
        w.set_issi(issi.into());
        w.set_network(network.into());
        w.set_serving_la(serving_la.into());
        w.set_colour_code(colour_code.into());
        w.set_attached_count(attached_count);
        w.set_scan_active(scan_active);
        w.set_has_codeplug(has_codeplug);
        w.set_folder_name(folder_name.into());
        w.set_talkgroup_name(tg_name.into());
        w.set_talkgroup_id(tg_id.into());
        w.set_tg_sub(tg_sub.into());
        w.set_badge(badge.into());
        w.set_badge_kind(badge_kind);
        w.set_can_cycle(can_cycle);
        w.set_select_enabled(select_enabled);
        w.set_show_cancel_select(show_cancel);
        w.set_sel_folder(sel_folder_idx);
        w.set_ptt_enabled(ptt_enabled);
        w.set_ptt_label(ptt_label.into());
        w.set_restart_required(restart_required);

        let rows: Vec<FolderRow> = folder_rows
            .into_iter()
            .map(|(name, count, selected)| FolderRow {
                name: name.into(),
                count,
                selected,
            })
            .collect();
        w.set_folders(ModelRc::new(VecModel::from(rows)));
    });

    push_groups(app, weak);
    push_contacts(app, weak);
    push_dial_targets(app, weak);
    push_conversations(app, weak);
    push_thread(app, weak);
}

/// Build the dialer target selector: "Private" (ISSI) plus one entry per
/// codeplug gateway, in order. Each carries the digit cap for the typed number
/// (private ISSI = 8; gateway = 24 - prefix length, since prefix + number must
/// fit the 24-digit external-subscriber-number IE).
fn push_dial_targets(app: &AppState, weak: &slint::Weak<MainWindow>) {
    let mut targets: Vec<DialTarget> = vec![DialTarget {
        label: "Private".into(),
        max_len: 8,
    }];
    if let Some(cp) = &app.codeplug {
        for g in &cp.gateways {
            let cap = (crate::codeplug::MAX_EXTERNAL_DIGITS as i32
                - g.prefix.chars().count() as i32)
                .max(1);
            targets.push(DialTarget {
                label: g.name.clone().into(),
                max_len: cap,
            });
        }
    }
    let _ = weak.upgrade_in_event_loop(move |w| {
        w.set_dial_targets(ModelRc::new(VecModel::from(targets)));
    });
}

// --- SDS messaging (interface-5) --------------------------------------------

/// Human-readable name for a peer SSI: a contact/talkgroup name if known,
/// otherwise "ISSI n" (individual) or "Group n".
fn peer_name(app: &AppState, ssi: u32, is_group: bool) -> String {
    if let Some(cp) = &app.codeplug {
        if is_group {
            return cp.name_of(ssi);
        }
        if let Some(c) = cp.contacts.iter().find(|c| c.issi == Some(ssi)) {
            return match &c.callsign {
                Some(cs) if !cs.is_empty() => format!("{} - {}", c.name, cs),
                _ => c.name.clone(),
            };
        }
    }
    if is_group {
        format!("Group {ssi}")
    } else {
        format!("ISSI {ssi}")
    }
}

/// Format a unix-millis timestamp as a short local time string.
fn fmt_ts(ms: u64) -> String {
    chrono::DateTime::from_timestamp_millis(ms as i64)
        .map(|t| t.with_timezone(&chrono::Local).format("%H:%M").to_string())
        .unwrap_or_default()
}

/// Total count of unread inbound messages, for the menu badge.
fn unread_messages(app: &AppState) -> i32 {
    app.messages
        .messages
        .iter()
        .filter(|m| !m.outgoing && !m.read)
        .count() as i32
}

/// Mark every inbound message from `(ssi, is_group)` as read, sending a consumed
/// report for any that requested one. Returns true if anything changed.
fn mark_peer_read(app: &mut AppState, ssi: u32, is_group: bool) -> bool {
    let mut reports: Vec<u8> = Vec::new();
    let mut changed = false;
    for m in app.messages.messages.iter_mut() {
        if m.outgoing || m.peer_ssi != ssi || m.is_group != is_group {
            continue;
        }
        if !m.read {
            m.read = true;
            changed = true;
        }
        if m.wants_consumed {
            reports.push(m.reference);
            m.wants_consumed = false;
        }
    }
    // Consumed reports are only meaningful for individual messages.
    if !is_group {
        for reference in reports {
            let h = app.next_handle();
            app.send(protocol::tnsds_send_report(h, ssi, reference, 0x02));
        }
    }
    changed
}

/// Open (or switch to) the message thread for a peer, marking it read.
fn open_thread(app: &mut AppState, weak: &slint::Weak<MainWindow>, ssi: u32, is_group: bool) {
    app.msg_thread_peer = Some((ssi, is_group));
    app.msg_draft.clear();
    app.msg_shift = false;
    app.msg_scroll_tick = app.msg_scroll_tick.wrapping_add(1);
    if mark_peer_read(app, ssi, is_group) {
        app.messages.save();
    }
    push_thread(app, weak);
    push_conversations(app, weak);
}

/// Compose + send the current draft in the open thread; store it locally.
fn send_message(app: &mut AppState, weak: &slint::Weak<MainWindow>) {
    let Some((ssi, is_group)) = app.msg_thread_peer else {
        return;
    };
    let text = app.msg_draft.trim().to_string();
    if text.is_empty() {
        return;
    }
    if !app.require_online(weak) {
        return;
    }
    // Groups get no per-recipient delivery reports; individuals get both ticks.
    let report = if is_group { "None" } else { "ReceivedAndConsumed" };
    let reference = app.messages.next_reference();
    let sdu = protocol::encode_text_sdu(&text);
    let h = app.next_handle();
    app.send(protocol::tnsds_send_message(h, ssi, is_group, reference, report, sdu));

    let id = app.messages.next_local_id();
    app.messages.messages.push(crate::store::StoredMessage {
        id,
        peer_ssi: ssi,
        is_group,
        outgoing: true,
        text,
        reference,
        state: crate::store::state::SENDING,
        fail_code: 0,
        at_ms: crate::store::now_ms(),
        read: true,
        wants_consumed: false,
    });
    app.messages.save();
    app.msg_draft.clear();
    app.msg_shift = false;
    app.msg_scroll_tick = app.msg_scroll_tick.wrapping_add(1);
    push_thread(app, weak);
    push_conversations(app, weak);
}

/// Store an inbound text message and surface it.
fn handle_sds_message_in(app: &mut AppState, payload: &Value, weak: &slint::Weak<MainWindow>) {
    // Only Text Messaging (SDS-TL, protocol_id 0x82) is a "message". Other
    // protocol IDs (proprietary/other SDS-TL services) aren't shown in the
    // messages app.
    let protocol_id = payload.get("protocol_id").and_then(Value::as_u64).unwrap_or(0) as u32;
    if protocol_id != protocol::SDS_TEXT_PROTOCOL_ID {
        tracing::info!(protocol_id, "SDS message with non-text protocol id ignored");
        return;
    }
    let ssi = payload.get("calling_party_ssi").and_then(Value::as_u64).unwrap_or(0) as u32;
    let is_group = payload
        .get("called_party_is_group")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let reference = payload.get("message_reference").and_then(Value::as_u64).unwrap_or(0) as u8;
    let drr = payload
        .get("delivery_report_request")
        .and_then(Value::as_str)
        .unwrap_or("None");
    let bytes: Vec<u8> = payload
        .get("user_data")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(|v| v.as_u64().map(|n| n as u8)).collect())
        .unwrap_or_default();
    let text = protocol::decode_text_sdu(&bytes);
    let wants_consumed =
        !is_group && matches!(drr, "Consumed" | "ReceivedAndConsumed");

    let open = app.msg_thread_peer == Some((ssi, is_group));
    let id = app.messages.next_local_id();
    app.messages.messages.push(crate::store::StoredMessage {
        id,
        peer_ssi: ssi,
        is_group,
        outgoing: false,
        text: text.clone(),
        reference,
        state: crate::store::state::INBOUND,
        fail_code: 0,
        at_ms: crate::store::now_ms(),
        read: open,
        wants_consumed,
    });

    if open {
        // Thread is on-screen: mark read and send any consumed report now.
        mark_peer_read(app, ssi, is_group);
        app.messages.save();
        push_thread(app, weak);
    } else {
        app.messages.save();
        let who = peer_name(app, ssi, is_group);
        let preview: String = text.chars().take(40).collect();
        app.notify(weak, &format!("Message from {who}"), &preview, 0);
    }
    push_conversations(app, weak);
}

/// Match an inbound delivery/read report to the outgoing message it refers to.
fn handle_sds_report_in(app: &mut AppState, payload: &Value, weak: &slint::Weak<MainWindow>) {
    let ssi = payload.get("calling_party_ssi").and_then(Value::as_u64).unwrap_or(0) as u32;
    let reference = payload.get("message_reference").and_then(Value::as_u64).unwrap_or(0) as u8;
    let ds = payload.get("delivery_status").and_then(Value::as_u64).unwrap_or(0) as u8;
    let new_state = if ds >= 0x40 {
        crate::store::state::FAILED
    } else if ds == 0x02 {
        crate::store::state::READ
    } else {
        crate::store::state::DELIVERED
    };
    let mut hit = false;
    for m in app.messages.messages.iter_mut() {
        if m.outgoing && m.peer_ssi == ssi && m.reference == reference {
            if new_state == crate::store::state::FAILED {
                m.state = crate::store::state::FAILED;
                m.fail_code = ds;
            } else if m.state != crate::store::state::FAILED && new_state > m.state {
                m.state = new_state;
            }
            hit = true;
        }
    }
    if hit {
        app.messages.save();
        push_thread(app, weak);
        push_conversations(app, weak);
    }
}

/// Build the conversation list: one row per peer, newest activity first, with
/// the last message snippet and unread count.
fn push_conversations(app: &AppState, weak: &slint::Weak<MainWindow>) {
    // Group by (peer_ssi, is_group), tracking last timestamp + snippet + unread.
    let mut order: Vec<(u32, bool)> = Vec::new();
    let mut last_ts: HashMap<(u32, bool), u64> = HashMap::new();
    let mut snippet: HashMap<(u32, bool), String> = HashMap::new();
    let mut unread: HashMap<(u32, bool), i32> = HashMap::new();
    for m in &app.messages.messages {
        let key = (m.peer_ssi, m.is_group);
        if !order.contains(&key) {
            order.push(key);
        }
        let cur = last_ts.entry(key).or_insert(0);
        if m.at_ms >= *cur {
            *cur = m.at_ms;
            snippet.insert(key, m.text.chars().take(48).collect());
        }
        if !m.outgoing && !m.read {
            *unread.entry(key).or_insert(0) += 1;
        }
    }
    order.sort_by(|a, b| last_ts[b].cmp(&last_ts[a]));
    let rows: Vec<ConvRow> = order
        .iter()
        .map(|&(ssi, is_group)| ConvRow {
            peer: ssi as i32,
            name: peer_name(app, ssi, is_group).into(),
            snippet: snippet.get(&(ssi, is_group)).cloned().unwrap_or_default().into(),
            unread: *unread.get(&(ssi, is_group)).unwrap_or(&0),
            is_group,
            ts: fmt_ts(*last_ts.get(&(ssi, is_group)).unwrap_or(&0)).into(),
        })
        .collect();
    let badge = unread_messages(app);
    let _ = weak.upgrade_in_event_loop(move |w| {
        w.set_conversations(ModelRc::new(VecModel::from(rows)));
        w.set_msg_unread(badge);
    });
}

/// Build the open thread's message bubbles + title + draft state.
fn push_thread(app: &AppState, weak: &slint::Weak<MainWindow>) {
    let mut rows: Vec<MsgRow> = Vec::new();
    let mut title = String::new();
    if let Some((ssi, is_group)) = app.msg_thread_peer {
        title = peer_name(app, ssi, is_group);
        for m in &app.messages.messages {
            if m.peer_ssi == ssi && m.is_group == is_group {
                rows.push(MsgRow {
                    id: m.id as i32,
                    outgoing: m.outgoing,
                    text: m.text.clone().into(),
                    state: m.state as i32,
                    ts: fmt_ts(m.at_ms).into(),
                });
            }
        }
    }
    let draft = app.msg_draft.clone();
    let shift = app.msg_shift;
    let scroll_tick = app.msg_scroll_tick as i32;
    let _ = weak.upgrade_in_event_loop(move |w| {
        w.set_msg_thread(ModelRc::new(VecModel::from(rows)));
        w.set_msg_thread_title(title.into());
        w.set_msg_draft(draft.into());
        w.set_msg_shift(shift);
        w.set_msg_scroll_tick(scroll_tick);
    });
}

fn push_contacts(app: &AppState, weak: &slint::Weak<MainWindow>) {
    let query = app.contact_query.trim().to_lowercase();
    let mut rows: Vec<ContactRow> = Vec::new();
    if let Some(cp) = &app.codeplug {
        for (i, c) in cp.contacts.iter().enumerate() {
            // Case-insensitive substring filter over name, callsign, ISSI, number.
            if !query.is_empty() {
                let hay = format!(
                    "{} {} {} {}",
                    c.name.to_lowercase(),
                    c.callsign.as_deref().unwrap_or("").to_lowercase(),
                    c.issi.map(|x| x.to_string()).unwrap_or_default(),
                    c.number.as_deref().unwrap_or("")
                );
                if !hay.contains(&query) {
                    continue;
                }
            }
            // kind: 0 = individual (ISSI), 1 = external phone (via gateway).
            let (kind, sub) = if let Some(issi) = c.issi {
                (0, format!("ISSI {issi}"))
            } else if let (Some(num), Some(gw_id)) = (c.number.as_ref(), c.gateway.as_ref()) {
                let gw = cp.gateway_by_id(gw_id);
                let gw_name = gw.map(|g| g.name.clone()).unwrap_or_else(|| gw_id.clone());
                (1, format!("{num} via {gw_name}"))
            } else {
                (0, "Invalid contact".to_string())
            };
            let title = match &c.callsign {
                Some(cs) if !cs.is_empty() => format!("{} - {}", c.name, cs),
                _ => c.name.clone(),
            };
            rows.push(ContactRow {
                index: i as i32,
                name: title.into(),
                sub: sub.into(),
                kind,
            });
        }
    }
    let query_out = app.contact_query.clone();
    let _ = weak.upgrade_in_event_loop(move |w| {
        w.set_contacts(ModelRc::new(VecModel::from(rows)));
        w.set_contact_query(query_out.into());
    });
}

/// Stage + commit an edited codeplug TOML: SetConfig, ApplyConfig, then GetConfig
/// to pull back the canonical version. Applies the change locally at once for
/// instant UI feedback (the stack commits contacts/gateways live, no restart).
/// Stage + commit an edited codeplug TOML (SetConfig + ApplyConfig + GetConfig)
/// with an optimistic local update. No toast, no navigation.
fn apply_codeplug(app: &mut AppState, weak: &slint::Weak<MainWindow>, toml: String) {
    let h = app.next_handle();
    app.send(protocol::set_config(h, &toml));
    let h2 = app.next_handle();
    app.send(protocol::apply_config(h2));

    app.last_config_toml = toml.clone();
    if let Some(cp) = Codeplug::parse(&toml) {
        app.codeplug = Some(cp);
        reconcile_home_view(app);
    }
    push_ui(app, weak);

    let h3 = app.next_handle();
    app.send(protocol::get_config(h3));
}

fn write_codeplug(
    app: &mut AppState,
    weak: &slint::Weak<MainWindow>,
    toml: String,
    title: &str,
    msg: &str,
    return_screen: Screen,
) {
    apply_codeplug(app, weak, toml);
    app.notify(weak, title, msg, 0);
    let _ = weak.upgrade_in_event_loop(move |w| w.set_screen(return_screen));
}

// --- Codeplug programming: section lists, edit form, save/delete -------------

fn section_from_code(s: i32) -> Option<ProgSection> {
    match s {
        0 => Some(ProgSection::Networks),
        1 => Some(ProgSection::Folders),
        2 => Some(ProgSection::Talkgroups),
        3 => Some(ProgSection::Scanlists),
        4 => Some(ProgSection::Gateways),
        6 => Some(ProgSection::Settings),
        _ => None,
    }
}

/// Open a codeplug section from the hub: settings jumps straight to the form,
/// others show the generic list.
fn prog_open_section(app: &mut AppState, weak: &slint::Weak<MainWindow>, s: i32) {
    let Some(section) = section_from_code(s) else { return };
    app.prog_section = section;
    if section == ProgSection::Settings {
        app.prog_draft = Some(build_settings_draft(app));
        push_form(app, weak);
        let _ = weak.upgrade_in_event_loop(|w| w.set_screen(Screen::ProgramEdit));
        return;
    }
    push_prog_list(app, weak);
    let _ = weak.upgrade_in_event_loop(|w| w.set_screen(Screen::ProgramList));
}

fn has_home(cp: &Codeplug) -> bool {
    cp.networks.first().map(|n| n.home).unwrap_or(false)
}

fn new_draft(
    section: ProgSection,
    key: Option<String>,
    index: Option<usize>,
    can_delete: bool,
    title: String,
    fields: Vec<FormFieldDraft>,
    member_gssis: Vec<u32>,
) -> ProgDraft {
    let focus = fields.iter().position(|f| f.is_focusable()).unwrap_or(usize::MAX);
    ProgDraft { section, key, index, fields, focus, shift: false, can_delete, title, member_gssis, tg_folder: None }
}

fn build_settings_draft(app: &AppState) -> ProgDraft {
    let hd = app.codeplug.as_ref().and_then(|cp| cp.settings.home_display.clone());
    let enabled = hd.as_ref().map(|h| h.enabled).unwrap_or(false);
    let pid = hd.as_ref().map(|h| h.pid).unwrap_or(130);
    new_draft(
        ProgSection::Settings,
        None,
        None,
        false,
        "Home mode display".to_string(),
        vec![
            FormFieldDraft::toggle("Enabled", enabled),
            FormFieldDraft::digits("PID", pid.to_string()),
        ],
        vec![],
    )
}

/// Turn a folder name into a URL-ish slug, unique against `existing` ids.
fn unique_folder_slug(name: &str, existing: &[String]) -> String {
    let mut base = String::new();
    let mut prev_dash = false;
    for ch in name.trim().to_lowercase().chars() {
        if ch.is_ascii_alphanumeric() {
            base.push(ch);
            prev_dash = false;
        } else if !prev_dash {
            base.push('-');
            prev_dash = true;
        }
    }
    let base = base.trim_matches('-').to_string();
    let base = if base.is_empty() { "folder".to_string() } else { base };
    if !existing.iter().any(|e| e == &base) {
        return base;
    }
    let mut n = 2;
    loop {
        let candidate = format!("{base}-{n}");
        if !existing.iter().any(|e| e == &candidate) {
            return candidate;
        }
        n += 1;
    }
}

/// Build the edit draft for entry `idx` (Some) or a new entry (None).
fn build_draft(app: &AppState, idx: Option<usize>) -> Option<ProgDraft> {
    let cp = app.codeplug.as_ref()?;
    let d = match app.prog_section {
        ProgSection::Networks => {
            let home_present = has_home(cp);
            match idx {
                Some(i) => {
                    let n = cp.networks.get(i)?;
                    if n.home {
                        new_draft(
                            ProgSection::Networks,
                            Some("__home__".into()),
                            None,
                            false,
                            "Home network".to_string(),
                            vec![
                                FormFieldDraft::digits("MCC", n.mcc.to_string()),
                                FormFieldDraft::digits("MNC", n.mnc.to_string()),
                            ],
                            vec![],
                        )
                    } else {
                        let net_idx = i - home_present as usize;
                        new_draft(
                            ProgSection::Networks,
                            None,
                            Some(net_idx),
                            true,
                            "Edit network".to_string(),
                            vec![
                                FormFieldDraft::digits("MCC", n.mcc.to_string()),
                                FormFieldDraft::digits("MNC", n.mnc.to_string()),
                                FormFieldDraft::text("Name", n.name.clone().unwrap_or_default()),
                                FormFieldDraft::digits("Priority", n.priority.to_string()),
                            ],
                            vec![],
                        )
                    }
                }
                None => new_draft(
                    ProgSection::Networks,
                    None,
                    None,
                    false,
                    "New network".to_string(),
                    vec![
                        FormFieldDraft::digits("MCC", String::new()),
                        FormFieldDraft::digits("MNC", String::new()),
                        FormFieldDraft::text("Name", String::new()),
                        FormFieldDraft::digits("Priority", "0".to_string()),
                    ],
                    vec![],
                ),
            }
        }
        ProgSection::Folders => {
            let (key, title, name) = match idx {
                Some(i) => {
                    let f = cp.folder_defs.get(i)?;
                    (Some(f.id.clone()), "Edit folder".to_string(), f.name.clone())
                }
                None => (None, "New folder".to_string(), String::new()),
            };
            new_draft(
                ProgSection::Folders,
                key,
                None,
                idx.is_some(),
                title,
                vec![FormFieldDraft::text("Name", name)],
                vec![],
            )
        }
        ProgSection::Talkgroups => match idx {
            Some(i) => {
                let t = cp.all_talkgroups.get(i)?;
                let mut d = new_draft(
                    ProgSection::Talkgroups,
                    Some(t.gssi.to_string()),
                    None,
                    true,
                    "Edit talkgroup".to_string(),
                    vec![
                        FormFieldDraft::digits("GSSI", t.gssi.to_string()),
                        FormFieldDraft::text("Name", t.name.clone()),
                        FormFieldDraft::digits("Class of usage (0-7)", t.class_of_usage.to_string()),
                    ],
                    vec![],
                );
                d.tg_folder = t.folder.clone();
                d
            }
            None => new_draft(
                ProgSection::Talkgroups,
                None,
                None,
                false,
                "New talkgroup".to_string(),
                vec![
                    FormFieldDraft::digits("GSSI", String::new()),
                    FormFieldDraft::text("Name", String::new()),
                    FormFieldDraft::digits("Class of usage (0-7)", "0".to_string()),
                ],
                vec![],
            ),
        },
        ProgSection::Scanlists => {
            let (key, title, name, active, members): (Option<String>, String, String, bool, Vec<u32>) =
                match idx {
                    Some(i) => {
                        let s = cp.scanlists.get(i)?;
                        (Some(s.name.clone()), "Edit scan list".to_string(), s.name.clone(), s.active, s.talkgroups.clone())
                    }
                    None => (None, "New scan list".to_string(), String::new(), true, vec![]),
                };
            let mut fields = vec![
                FormFieldDraft::text("Name", name),
                FormFieldDraft::toggle("Active", active),
            ];
            let mut member_gssis = Vec::new();
            for t in &cp.all_talkgroups {
                fields.push(FormFieldDraft::toggle(
                    &format!("{} ({})", t.name, t.gssi),
                    members.contains(&t.gssi),
                ));
                member_gssis.push(t.gssi);
            }
            new_draft(ProgSection::Scanlists, key, None, idx.is_some(), title, fields, member_gssis)
        }
        ProgSection::Gateways => match idx {
            Some(i) => {
                let g = cp.gateways.get(i)?;
                new_draft(
                    ProgSection::Gateways,
                    Some(g.id.clone()),
                    None,
                    true,
                    "Edit gateway".to_string(),
                    vec![
                        FormFieldDraft::text("Id", g.id.clone()),
                        FormFieldDraft::text("Name", g.name.clone()),
                        FormFieldDraft::digits("Gateway ISSI", g.gateway_issi.to_string()),
                        FormFieldDraft::dial("Prefix", g.prefix.clone()),
                    ],
                    vec![],
                )
            }
            None => new_draft(
                ProgSection::Gateways,
                None,
                None,
                false,
                "New gateway".to_string(),
                vec![
                    FormFieldDraft::text("Id", String::new()),
                    FormFieldDraft::text("Name", String::new()),
                    FormFieldDraft::digits("Gateway ISSI", String::new()),
                    FormFieldDraft::dial("Prefix", String::new()),
                ],
                vec![],
            ),
        },
        ProgSection::Settings => build_settings_draft(app),
    };
    Some(d)
}

fn prog_open_entry(app: &mut AppState, weak: &slint::Weak<MainWindow>, idx: usize) {
    if let Some(d) = build_draft(app, Some(idx)) {
        app.prog_draft = Some(d);
        push_form(app, weak);
        let _ = weak.upgrade_in_event_loop(|w| w.set_screen(Screen::ProgramEdit));
    }
}

fn prog_add_entry(app: &mut AppState, weak: &slint::Weak<MainWindow>) {
    if let Some(d) = build_draft(app, None) {
        app.prog_draft = Some(d);
        push_form(app, weak);
        let _ = weak.upgrade_in_event_loop(|w| w.set_screen(Screen::ProgramEdit));
    }
}

fn fval(d: &ProgDraft, i: usize) -> String {
    d.fields.get(i).map(|f| f.value.trim().to_string()).unwrap_or_default()
}

/// Commit the current programming form via the matching codeplug writer.
fn prog_save(app: &mut AppState, weak: &slint::Weak<MainWindow>) {
    let Some(d) = app.prog_draft.as_ref() else { return };
    let toml = &app.last_config_toml;
    let section = d.section;
    let result: Result<(String, String), String> = (|| match section {
        ProgSection::Networks => {
            if d.key.as_deref() == Some("__home__") {
                let mcc = fval(d, 0).parse::<u16>().map_err(|_| "MCC must be a number".to_string())?;
                let mnc = fval(d, 1).parse::<u16>().map_err(|_| "MNC must be a number".to_string())?;
                Ok((crate::codeplug::set_net_info(toml, mcc, mnc)?, "Saved home network".to_string()))
            } else {
                let mcc = fval(d, 0).parse::<u16>().map_err(|_| "MCC must be a number".to_string())?;
                let mnc = fval(d, 1).parse::<u16>().map_err(|_| "MNC must be a number".to_string())?;
                let name = fval(d, 2);
                let priority = fval(d, 3).parse::<i64>().unwrap_or(0);
                let input = crate::codeplug::NetworkInput {
                    mcc,
                    mnc,
                    name: Some(name).filter(|s| !s.is_empty()),
                    priority,
                };
                Ok((crate::codeplug::upsert_network(toml, &input, d.index)?, "Saved network".to_string()))
            }
        }
        ProgSection::Folders => {
            let name = fval(d, 0);
            if name.is_empty() {
                return Err("Folder name is required".to_string());
            }
            let id = match d.key.as_deref() {
                // Editing: keep the existing id.
                Some(existing) => existing.to_string(),
                // Adding: generate a unique slug from the name.
                None => {
                    let existing: Vec<String> = app
                        .codeplug
                        .as_ref()
                        .map(|cp| cp.folder_defs.iter().map(|f| f.id.clone()).collect())
                        .unwrap_or_default();
                    unique_folder_slug(&name, &existing)
                }
            };
            let input = crate::codeplug::FolderInput { id, name };
            Ok((crate::codeplug::upsert_folder(toml, &input, d.key.as_deref())?, "Saved folder".to_string()))
        }
        ProgSection::Talkgroups => {
            let gssi = fval(d, 0).parse::<u32>().map_err(|_| "GSSI must be a number".to_string())?;
            let class = fval(d, 2).parse::<u8>().map_err(|_| "Class of usage must be 0-7".to_string())?;
            let input = crate::codeplug::TalkgroupInput {
                gssi,
                name: fval(d, 1),
                folder: d.tg_folder.clone(),
                class_of_usage: class,
            };
            let key = d.key.as_deref().and_then(|s| s.parse::<u32>().ok());
            Ok((crate::codeplug::upsert_talkgroup(toml, &input, key)?, "Saved talkgroup".to_string()))
        }
        ProgSection::Scanlists => {
            let name = fval(d, 0);
            let active = d.fields.get(1).map(|f| f.on).unwrap_or(true);
            let mut talkgroups = Vec::new();
            for (j, g) in d.member_gssis.iter().enumerate() {
                if d.fields.get(2 + j).map(|f| f.on).unwrap_or(false) {
                    talkgroups.push(*g);
                }
            }
            let input = crate::codeplug::ScanlistInput { name, talkgroups, active };
            Ok((crate::codeplug::upsert_scanlist(toml, &input, d.key.as_deref())?, "Saved scan list".to_string()))
        }
        ProgSection::Gateways => {
            let gssi = fval(d, 2).parse::<u32>().map_err(|_| "Gateway ISSI must be a number".to_string())?;
            let input = crate::codeplug::GatewayInput {
                id: fval(d, 0),
                name: fval(d, 1),
                gateway_issi: gssi,
                prefix: fval(d, 3),
            };
            Ok((crate::codeplug::upsert_gateway(toml, &input, d.key.as_deref())?, "Saved gateway".to_string()))
        }
        ProgSection::Settings => {
            let enabled = d.fields.first().map(|f| f.on).unwrap_or(false);
            let pid = fval(d, 1).parse::<u8>().unwrap_or(130);
            Ok((crate::codeplug::set_home_display(toml, enabled, pid)?, "Saved settings".to_string()))
        }
    })();

    match result {
        Ok((new_toml, msg)) => {
            let back = prog_return_screen(section);
            app.prog_draft = None;
            write_codeplug(app, weak, new_toml, "Programming", &msg, back);
            refresh_prog(app, weak, back);
        }
        Err(e) => app.notify(weak, "Invalid entry", &e, 0),
    }
}

/// Where to return after a save/delete/cancel in a given section.
fn prog_return_screen(section: ProgSection) -> Screen {
    match section {
        ProgSection::Settings => Screen::Program,
        ProgSection::Folders | ProgSection::Talkgroups => Screen::Tree,
        _ => Screen::ProgramList,
    }
}

/// Refresh whichever section view we are returning to.
fn refresh_prog(app: &AppState, weak: &slint::Weak<MainWindow>, back: Screen) {
    match back {
        Screen::Tree => push_tree(app, weak),
        Screen::ProgramList => push_prog_list(app, weak),
        _ => {}
    }
}

/// Delete the entry currently open in the programming form.
fn prog_delete(app: &mut AppState, weak: &slint::Weak<MainWindow>) {
    let Some(d) = app.prog_draft.as_ref() else { return };
    let section = d.section;
    let toml = &app.last_config_toml;
    let result: Result<(String, String), String> = match d.section {
        ProgSection::Networks => match d.index {
            Some(i) => crate::codeplug::delete_network(toml, i).map(|t| (t, "Deleted network".to_string())),
            None => Err("Cannot delete this network".to_string()),
        },
        ProgSection::Folders => match d.key.as_deref() {
            Some(id) => crate::codeplug::delete_folder(toml, id).map(|t| (t, "Deleted folder".to_string())),
            None => Err("Nothing to delete".to_string()),
        },
        ProgSection::Talkgroups => match d.key.as_deref().and_then(|s| s.parse::<u32>().ok()) {
            Some(g) => crate::codeplug::delete_talkgroup(toml, g).map(|t| (t, "Deleted talkgroup".to_string())),
            None => Err("Nothing to delete".to_string()),
        },
        ProgSection::Scanlists => match d.key.as_deref() {
            Some(n) => crate::codeplug::delete_scanlist(toml, n).map(|t| (t, "Deleted scan list".to_string())),
            None => Err("Nothing to delete".to_string()),
        },
        ProgSection::Gateways => match d.key.as_deref() {
            Some(id) => crate::codeplug::delete_gateway(toml, id).map(|t| (t, "Deleted gateway".to_string())),
            None => Err("Nothing to delete".to_string()),
        },
        ProgSection::Settings => Err("Nothing to delete".to_string()),
    };
    match result {
        Ok((new_toml, msg)) => {
            let back = prog_return_screen(section);
            app.prog_draft = None;
            write_codeplug(app, weak, new_toml, "Programming", &msg, back);
            refresh_prog(app, weak, back);
        }
        Err(e) => app.notify(weak, "Delete failed", &e, 0),
    }
}

/// Build the current section's list rows + header/labels.
fn push_prog_list(app: &AppState, weak: &slint::Weak<MainWindow>) {
    let mut rows: Vec<EntityRow> = Vec::new();
    let (title, empty, add): (&str, &str, &str) = match app.prog_section {
        ProgSection::Networks => ("Networks", "No networks", "Add network"),
        ProgSection::Folders => ("Folders", "No folders", "Add folder"),
        ProgSection::Talkgroups => ("Talkgroups", "No talkgroups", "Add talkgroup"),
        ProgSection::Scanlists => ("Scan lists", "No scan lists", "Add scan list"),
        ProgSection::Gateways => ("Gateways", "No gateways", "Add gateway"),
        ProgSection::Settings => ("Settings", "", ""),
    };
    if let Some(cp) = &app.codeplug {
        match app.prog_section {
            ProgSection::Networks => {
                for (i, n) in cp.networks.iter().enumerate() {
                    let name = if n.home {
                        "Home network".to_string()
                    } else {
                        n.name.clone().unwrap_or_else(|| "Network".to_string())
                    };
                    rows.push(EntityRow {
                        index: i as i32,
                        title: name.into(),
                        sub: format!("MCC {} / MNC {}", n.mcc, n.mnc).into(),
                    });
                }
            }
            ProgSection::Folders => {
                for (i, f) in cp.folder_defs.iter().enumerate() {
                    rows.push(EntityRow {
                        index: i as i32,
                        title: f.name.clone().into(),
                        sub: format!("id {}", f.id).into(),
                    });
                }
            }
            ProgSection::Talkgroups => {
                for (i, t) in cp.all_talkgroups.iter().enumerate() {
                    let folder = t.folder.clone().unwrap_or_else(|| "Other".to_string());
                    rows.push(EntityRow {
                        index: i as i32,
                        title: t.name.clone().into(),
                        sub: format!("GSSI {} - {}", t.gssi, folder).into(),
                    });
                }
            }
            ProgSection::Scanlists => {
                for (i, s) in cp.scanlists.iter().enumerate() {
                    let state = if s.active { "on" } else { "off" };
                    rows.push(EntityRow {
                        index: i as i32,
                        title: s.name.clone().into(),
                        sub: format!("{} groups - {}", s.talkgroups.len(), state).into(),
                    });
                }
            }
            ProgSection::Gateways => {
                for (i, g) in cp.gateways.iter().enumerate() {
                    let pfx = if g.prefix.is_empty() {
                        String::new()
                    } else {
                        format!(" - prefix {}", g.prefix)
                    };
                    rows.push(EntityRow {
                        index: i as i32,
                        title: g.name.clone().into(),
                        sub: format!("ISSI {}{}", g.gateway_issi, pfx).into(),
                    });
                }
            }
            ProgSection::Settings => {}
        }
    }
    let (title, empty, add) = (title.to_string(), empty.to_string(), add.to_string());
    let _ = weak.upgrade_in_event_loop(move |w| {
        w.set_prog_title(title.into());
        w.set_prog_empty(empty.into());
        w.set_prog_add_label(add.into());
        w.set_prog_rows(ModelRc::new(VecModel::from(rows)));
    });
}

/// Push the open edit form's fields + header/state.
fn push_form(app: &AppState, weak: &slint::Weak<MainWindow>) {
    let Some(d) = app.prog_draft.as_ref() else { return };
    let fields: Vec<FormField> = d
        .fields
        .iter()
        .map(|f| FormField {
            label: f.label.clone().into(),
            kind: f.kind as i32,
            value: f.value.clone().into(),
            on: f.on,
        })
        .collect();
    let title = d.title.clone();
    let focus = if d.focus == usize::MAX { -1 } else { d.focus as i32 };
    let shift = d.shift;
    let can_delete = d.can_delete;
    let from_tree = matches!(d.section, ProgSection::Folders | ProgSection::Talkgroups);
    let _ = weak.upgrade_in_event_loop(move |w| {
        w.set_form_title(title.into());
        w.set_form_fields(ModelRc::new(VecModel::from(fields)));
        w.set_form_focus(focus);
        w.set_form_shift(shift);
        w.set_form_can_delete(can_delete);
        w.set_form_from_tree(from_tree);
    });
}

/// Folder ids present in the codeplug (for Other-bucket detection).
fn folder_id_set(cp: &Codeplug) -> std::collections::HashSet<&str> {
    cp.folder_defs.iter().map(|f| f.id.as_str()).collect()
}

/// Ordered all_talkgroups indices belonging to a folder (Some(id)) or the Other
/// bucket (None = no folder or an unknown folder id).
fn folder_members(cp: &Codeplug, folder_key: Option<&str>) -> Vec<usize> {
    let ids = folder_id_set(cp);
    cp.all_talkgroups
        .iter()
        .enumerate()
        .filter(|(_, t)| match folder_key {
            Some(id) => t.folder.as_deref() == Some(id),
            None => match t.folder.as_deref() {
                None => true,
                Some(fid) => !ids.contains(fid),
            },
        })
        .map(|(i, _)| i)
        .collect()
}

/// The folder key (Some(id) / None for Other) that a group belongs to.
fn group_folder_key(cp: &Codeplug, i: usize) -> Option<String> {
    let ids = folder_id_set(cp);
    match cp.all_talkgroups.get(i).and_then(|t| t.folder.as_deref()) {
        Some(id) if ids.contains(id) => Some(id.to_string()),
        _ => None,
    }
}

/// Collapsed-state key for a tree folder header index (-1 = Other).
fn tree_folder_key(app: &AppState, header_index: i32) -> String {
    if header_index < 0 {
        return "__other__".to_string();
    }
    app.codeplug
        .as_ref()
        .and_then(|cp| cp.folder_defs.get(header_index as usize))
        .map(|f| f.id.clone())
        .unwrap_or_else(|| format!("__folder_{header_index}"))
}

/// Append a folder header and (unless collapsed) its groups + add-group row.
fn push_folder_rows(
    rows: &mut Vec<TreeRow>,
    cp: &Codeplug,
    collapsed_set: &std::collections::HashSet<String>,
    header_index: i32,
    key: &str,
    name: &str,
    members: &[usize],
) {
    let collapsed = collapsed_set.contains(key);
    rows.push(TreeRow {
        kind: 0,
        index: header_index,
        title: name.into(),
        sub: format!("{} groups", members.len()).into(),
        collapsed,
        can_up: false,
        can_down: false,
    });
    if collapsed {
        return;
    }
    let n = members.len();
    for (pos, &i) in members.iter().enumerate() {
        let t = &cp.all_talkgroups[i];
        rows.push(TreeRow {
            kind: 1,
            index: i as i32,
            title: t.name.clone().into(),
            sub: format!("GSSI {}", t.gssi).into(),
            collapsed: false,
            can_up: pos > 0,
            can_down: pos + 1 < n,
        });
    }
    rows.push(TreeRow {
        kind: 2,
        index: header_index,
        title: "Add group".into(),
        sub: "".into(),
        collapsed: false,
        can_up: false,
        can_down: false,
    });
}

/// Build the folders + talkgroups tree: each folder as a header with its groups
/// nested beneath, an "Other" bucket for unfiled groups, and add-group actions.
fn push_tree(app: &AppState, weak: &slint::Weak<MainWindow>) {
    let mut rows: Vec<TreeRow> = Vec::new();
    if let Some(cp) = &app.codeplug {
        for (fi, f) in cp.folder_defs.iter().enumerate() {
            let members = folder_members(cp, Some(f.id.as_str()));
            push_folder_rows(&mut rows, cp, &app.collapsed_folders, fi as i32, &f.id, &f.name, &members);
        }
        let others = folder_members(cp, None);
        if !others.is_empty() || cp.folder_defs.is_empty() {
            push_folder_rows(&mut rows, cp, &app.collapsed_folders, -1, "__other__", "Other", &others);
        }
    } else {
        rows.push(TreeRow {
            kind: 3,
            index: -1,
            title: "No codeplug loaded yet".into(),
            sub: "".into(),
            collapsed: false,
            can_up: false,
            can_down: false,
        });
    }
    let _ = weak.upgrade_in_event_loop(move |w| {
        w.set_tree_rows(ModelRc::new(VecModel::from(rows)));
    });
}

/// Move a group up/down within its folder by renumbering the folder's `order`s.
fn tree_reorder(app: &mut AppState, weak: &slint::Weak<MainWindow>, i: usize, up: bool) {
    let Some(cp) = app.codeplug.as_ref() else { return };
    let key = group_folder_key(cp, i);
    let members = folder_members(cp, key.as_deref());
    let Some(pos) = members.iter().position(|&m| m == i) else { return };
    let swap_with = if up {
        if pos == 0 {
            return;
        }
        pos - 1
    } else {
        if pos + 1 >= members.len() {
            return;
        }
        pos + 1
    };
    let mut seq: Vec<u32> = members.iter().map(|&m| cp.all_talkgroups[m].gssi).collect();
    seq.swap(pos, swap_with);
    let orders: Vec<(u32, i64)> = seq.iter().enumerate().map(|(idx, g)| (*g, idx as i64)).collect();
    match crate::codeplug::set_talkgroup_orders(&app.last_config_toml, &orders) {
        Ok(new_toml) => {
            apply_codeplug(app, weak, new_toml);
            push_tree(app, weak);
        }
        Err(e) => app.notify(weak, "Reorder failed", &e, 0),
    }
}

/// Push the open contact's detail-page fields.
fn push_contact_detail(app: &AppState, weak: &slint::Weak<MainWindow>) {
    let mut name = String::new();
    let mut callsign = String::new();
    let mut sub = String::new();
    let mut is_phone = false;
    let mut idx: i32 = -1;
    if let (Some(cp), Some(i)) = (app.codeplug.as_ref(), app.sel_contact) {
        if let Some(c) = cp.contacts.get(i) {
            idx = i as i32;
            name = c.name.clone();
            callsign = c.callsign.clone().unwrap_or_default();
            is_phone = c.is_phone();
            sub = if let Some(issi) = c.issi {
                format!("Private call - ISSI {issi}")
            } else if let (Some(num), Some(gw_id)) = (c.number.as_ref(), c.gateway.as_ref()) {
                let gw = cp.gateway_by_id(gw_id);
                let gw_name = gw.map(|g| g.name.clone()).unwrap_or_else(|| gw_id.clone());
                format!("{num} via {gw_name}")
            } else {
                "Invalid contact".to_string()
            };
        }
    }
    let _ = weak.upgrade_in_event_loop(move |w| {
        w.set_detail_index(idx);
        w.set_detail_name(name.into());
        w.set_detail_callsign(callsign.into());
        w.set_detail_sub(sub.into());
        w.set_detail_is_phone(is_phone);
    });
}

/// Push the contact-editor draft fields (and the gateway picker model).
fn push_contact_editor(app: &AppState, weak: &slint::Weak<MainWindow>) {
    let Some(d) = app.contact_draft.as_ref() else { return };
    let title = if d.key_name.is_some() { "Edit contact" } else { "New contact" }.to_string();
    let name = d.name.clone();
    let callsign = d.callsign.clone();
    let issi = d.issi.clone();
    let number = d.number.clone();
    let is_phone = d.is_phone;
    let numeric = d.focus.numeric();
    let shift = d.shift;
    let focus = match d.focus {
        EditField::Name => 0,
        EditField::Callsign => 1,
        EditField::Issi => 2,
        EditField::Number => 3,
    };
    // Target selector index: 0 = Private (ISSI); a gateway maps to its index + 1.
    let target_sel: i32 = if !d.is_phone {
        0
    } else {
        app.codeplug
            .as_ref()
            .and_then(|cp| cp.gateways.iter().position(|g| g.id == d.gateway_id))
            .map(|i| i as i32 + 1)
            .unwrap_or(0)
    };
    let _ = weak.upgrade_in_event_loop(move |w| {
        w.set_edit_title(title.into());
        w.set_edit_name(name.into());
        w.set_edit_callsign(callsign.into());
        w.set_edit_issi(issi.into());
        w.set_edit_number(number.into());
        w.set_edit_is_phone(is_phone);
        w.set_edit_focus(focus);
        w.set_edit_numeric(numeric);
        w.set_edit_shift(shift);
        w.set_edit_target_sel(target_sel);
    });
}

/// Build the Groups + scan-list models (all codeplug talkgroups tagged
/// TX/SCAN/none, plus programmed scan lists with their active state).
fn push_groups(app: &AppState, weak: &slint::Weak<MainWindow>) {
    let tx = effective_tx(app);
    let attached = &app.state.attached_groups;
    let active_scanlists = app.state.active_scanlists.clone().unwrap_or_default();

    let mut groups: Vec<GroupRow> = Vec::new();
    let mut scanlists: Vec<ScanRow> = Vec::new();
    if let Some(cp) = &app.codeplug {
        for folder in &cp.folders {
            for t in &folder.talkgroups {
                let is_attached = attached.contains(&t.gssi);
                let is_tx = is_attached && Some(t.gssi) == tx;
                let tag = if is_tx {
                    2
                } else if is_attached {
                    1
                } else {
                    0
                };
                groups.push(GroupRow {
                    gssi: t.gssi as i32,
                    name: t.name.clone().into(),
                    sub: format!("GSSI {} - {}", t.gssi, folder.name).into(),
                    tag,
                    cou: t.class_of_usage as i32,
                    attached: is_attached,
                    is_tx,
                });
            }
        }
        for sl in &cp.scanlists {
            let names: Vec<String> = sl.talkgroups.iter().map(|g| cp.name_of(*g)).collect();
            scanlists.push(ScanRow {
                name: sl.name.clone().into(),
                sub: names.join(", ").into(),
                active: active_scanlists.iter().any(|n| n == &sl.name),
            });
        }
    }

    let _ = weak.upgrade_in_event_loop(move |w| {
        w.set_groups(ModelRc::new(VecModel::from(groups)));
        w.set_scanlists(ModelRc::new(VecModel::from(scanlists)));
    });
}

fn push_survey(app: &AppState, weak: &slint::Weak<MainWindow>) {
    let manual = app.state.selection_mode_manual.unwrap_or(false);
    let scanning = app.scanning;
    let complete = app.scan_complete;
    let fmt_opt = |v: Option<i64>| v.map(|n| n.to_string()).unwrap_or_else(|| "-".to_string());
    let ynd = |v: Option<bool>| match v {
        Some(true) => "Yes",
        Some(false) => "No",
        None => "-",
    };
    let rows: Vec<SurveyRow> = app
        .scan_rows
        .iter()
        .map(|r| {
            let mhz = r.carrier_hz as f64 / 1_000_000.0;
            let rssi = match r.rssi_dbfs {
                Some(v) => format!("{v:.1} dBFS"),
                None => "-".to_string(),
            };
            SurveyRow {
                carrier: r.carrier_hz as i32,
                title: format!("{mhz:.4} MHz").into(),
                sub1: format!(
                    "MCC {} - MNC {} - LA {}",
                    fmt_opt(r.mcc),
                    fmt_opt(r.mnc),
                    fmt_opt(r.location_area)
                )
                .into(),
                sub2: format!(
                    "{} - Reg-req {} - Late-entry {}",
                    rssi,
                    ynd(r.registration_required),
                    ynd(r.late_entry_supported)
                )
                .into(),
            }
        })
        .collect();
    let count = rows.len() as i32;
    let (found, scanned) = app.scan_summary;
    let status = if scanning {
        format!("Scanning... {count} found so far")
    } else if complete {
        format!(
            "{found} cell{} - {scanned} carrier{} scanned",
            if found == 1 { "" } else { "s" },
            if scanned == 1 { "" } else { "s" }
        )
    } else {
        String::new()
    };
    let _ = weak.upgrade_in_event_loop(move |w| {
        w.set_survey_manual(manual);
        w.set_survey_scanning(scanning);
        w.set_survey_status(status.into());
        w.set_survey_rows(ModelRc::new(VecModel::from(rows)));
    });
}

fn fmt_dur(d: Duration) -> String {
    let s = d.as_secs();
    format!("{:02}:{:02}", s / 60, s % 60)
}

/// Push the call/PTT UI state derived from the call map.
fn push_calls(app: &AppState, weak: &slint::Weak<MainWindow>) {
    let peer_name = |ssi: Option<u32>, group: bool| -> String {
        match ssi {
            Some(s) => {
                if group {
                    app.codeplug
                        .as_ref()
                        .map(|cp| cp.name_of(s))
                        .unwrap_or_else(|| s.to_string())
                } else {
                    s.to_string()
                }
            }
            None => "-".to_string(),
        }
    };

    // Individual in-call screen.
    let indiv = in_call_individual(app).and_then(|cid| app.calls.get(&cid));
    let (call_live, call_dir, call_peer, call_sub, call_state, call_clock, call_can_ptt, call_ptt_state) =
        if let Some(c) = indiv {
            let label = call_state_label(c);
            let clock = if c.state == CallState::Active {
                c.active_since.map(|t| fmt_dur(t.elapsed())).unwrap_or_default()
            } else {
                String::new()
            };
            let can_ptt = c.state == CallState::Active && c.simplex;
            let holding = app.ptt_held == Some(c.cid);
            let ptt_state = if holding && c.holds_floor {
                2
            } else if holding
                && matches!(
                    c.tx_status.as_deref(),
                    Some("TransmissionRequestQueued") | Some("TransmissionWait") | None
                )
            {
                1
            } else {
                0
            };
            let dir = if c.direction == Some("mt") { "Incoming" } else { "Outgoing" };
            (
                true,
                dir.to_string(),
                c.peer_label.clone().unwrap_or_else(|| peer_name(c.peer_ssi, false)),
                c.peer_sub.clone().unwrap_or_else(|| c.peer_ssi.map(|s| s.to_string()).unwrap_or_default()),
                label,
                clock,
                can_ptt,
                ptt_state,
            )
        } else if let Some(d) = app.dialing.as_ref().filter(|d| !d.group) {
            (
                true,
                "Outgoing".to_string(),
                d.peer_label.clone().unwrap_or_else(|| peer_name(Some(d.peer_ssi), false)),
                d.peer_sub.clone().unwrap_or_else(|| d.peer_ssi.to_string()),
                "Setting up...".to_string(),
                String::new(),
                false,
                0,
            )
        } else {
            (false, String::new(), String::new(), String::new(), String::new(), String::new(), false, 0)
        };

    // Incoming ring overlay.
    let inc = incoming_call(app).and_then(|cid| app.calls.get(&cid));
    let (call_incoming, inc_peer, inc_sub) = match inc {
        Some(c) => (
            true,
            peer_name(c.peer_ssi, false),
            format!(
                "{}{}",
                c.peer_ssi.map(|s| s.to_string()).unwrap_or_default(),
                if c.simplex { " - PTT" } else { " - duplex" }
            ),
        ),
        None => (false, String::new(), String::new()),
    };

    // Group call panel + PTT.
    let gcall = active_group_call(app).and_then(|cid| app.calls.get(&cid));
    let group_active = gcall.is_some()
        || app.grp_call.is_some()
        || app.dialing.as_ref().map(|d| d.group).unwrap_or(false);
    let i_am_talking = app.grp_call.as_ref().map(|g| g.talking).unwrap_or(false);
    // Someone is speaking on the call right now if downlink audio is flowing.
    let receiving = gcall
        .and_then(|c| c.rx_at)
        .map(|t| t.elapsed() < Duration::from_millis(700))
        .unwrap_or(false);
    let other_talker = if i_am_talking {
        None
    } else {
        gcall.and_then(|c| c.talker_ssi).filter(|s| *s != app.state.own_issi)
    };
    let group_gssi = app
        .grp_call
        .as_ref()
        .map(|g| g.gssi)
        .or_else(|| gcall.and_then(|c| c.peer_ssi))
        .or_else(|| effective_tx(app));
    let group_name = group_gssi
        .map(|g| peer_name(Some(g), true))
        .unwrap_or_else(|| "Group call".to_string());
    // Talker line: a programmed name when known, otherwise just "TALKING"; the
    // raw talker id (if any) goes on its own line below (group_talker_id) instead
    // of an inline "TG {id}" label.
    let talker_name = other_talker
        .and_then(|o| app.codeplug.as_ref().and_then(|cp| cp.known_name(o)));
    let group_status = if i_am_talking {
        "TALKING - You".to_string()
    } else if other_talker.is_some() {
        match &talker_name {
            Some(n) => format!("TALKING - {n}"),
            None => "TALKING".to_string(),
        }
    } else if receiving {
        // Audio is flowing but the SwMI didn't give us a usable talker SSI.
        "TALKING".to_string()
    } else {
        let floor = gcall.and_then(|c| c.tx_status.as_deref());
        match floor {
            Some("TransmissionRequestQueued") | Some("TransmissionWait") => {
                "Requesting floor...".to_string()
            }
            _ if gcall.is_none() => "Connecting...".to_string(),
            _ => "Floor free - push to talk".to_string(),
        }
    };
    let group_talker_id = if i_am_talking {
        String::new()
    } else {
        other_talker.map(|o| o.to_string()).unwrap_or_default()
    };
    let group_ptt_state: i32 = if i_am_talking {
        let req = matches!(
            gcall.and_then(|c| c.tx_status.as_deref()),
            Some("TransmissionRequestQueued") | Some("TransmissionWait")
        );
        if req {
            1
        } else {
            2
        }
    } else if !ptt_allowed(app) {
        3
    } else {
        0
    };

    let call_muted = app.mic_muted;
    // DTMF pad: only on an active individual duplex call. Echo shows locally.
    let (call_dtmf, call_digits) = match in_call_individual(app).and_then(|cid| app.calls.get(&cid)) {
        Some(c) if c.state == CallState::Active && !c.simplex => (true, app.dtmf_echo.clone()),
        _ => (false, String::new()),
    };
    let _ = weak.upgrade_in_event_loop(move |w| {
        w.set_call_live(call_live);
        w.set_call_dir(call_dir.into());
        w.set_call_peer(call_peer.into());
        w.set_call_sub(call_sub.into());
        w.set_call_state(call_state.into());
        w.set_call_clock(call_clock.into());
        w.set_call_can_ptt(call_can_ptt);
        w.set_call_ptt_state(call_ptt_state);
        w.set_call_muted(call_muted);
        w.set_call_dtmf(call_dtmf);
        w.set_call_digits(call_digits.into());
        w.set_call_incoming(call_incoming);
        w.set_incoming_peer(inc_peer.into());
        w.set_incoming_sub(inc_sub.into());
        w.set_group_call_active(group_active);
        w.set_group_call_name(group_name.into());
        w.set_group_call_status(group_status.into());
        w.set_group_talker_id(group_talker_id.into());
        w.set_group_ptt_state(group_ptt_state);
    });
}

fn call_state_label(c: &Call) -> String {
    match c.state {
        CallState::Setup => "Setting up...".to_string(),
        CallState::Proceeding => "Calling...".to_string(),
        CallState::Alerting => {
            if c.queued {
                "Queued...".to_string()
            } else {
                "Ringing...".to_string()
            }
        }
        CallState::Connecting => "Connecting...".to_string(),
        CallState::Incoming => "Incoming...".to_string(),
        CallState::Active => {
            if c.group {
                "Group call active".to_string()
            } else {
                "Connected".to_string()
            }
        }
    }
}

