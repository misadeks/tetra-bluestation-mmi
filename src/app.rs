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
use crate::{FolderRow, GroupRow, LogRow, MainWindow, ScanRow, SurveyRow};

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
    UiDialCall,
    UiCallPttDown,
    UiCallPttUp,
    UiGroupPttDown,
    UiGroupPttUp,
    UiAnswerCall,
    UiRejectCall,
    UiHangup,
    UiHangupGroup,
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
}

#[derive(Clone)]
struct Dialing {
    peer_ssi: u32,
    group: bool,
    simplex: bool,
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
                    d if app.dial_number.chars().count() < 24 => {
                        app.dial_number.push_str(d);
                    }
                    _ => {}
                }
                let n = app.dial_number.clone();
                let weak = weak.clone();
                let _ = weak.upgrade_in_event_loop(move |w| w.set_dial_number(n.into()));
            }
            AppEvent::UiDialCall => {
                if !app.require_online(&weak) {
                    continue;
                }
                if app.state.registration_state != protocol::RegistrationState::Registered {
                    app.notify(&weak, "Not registered", "Register the radio before placing a call.", 0);
                    continue;
                }
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
                tracing::info!(number = %app.dial_number, "UI: dial call");
                let ssi = app.dial_number.parse::<u32>().unwrap_or(0);
                // Individual simplex call from the dialer.
                let h = app.next_handle();
                app.send(protocol::tncc_setup(h, ssi, false, false));
                app.dialing = Some(Dialing { peer_ssi: ssi, group: false, simplex: true });
                push_calls(&app, &weak);
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
                    app.dialing = Some(Dialing { peer_ssi: sel, group: true, simplex: true });
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
        let was_group = app.calls.get(&cid).map(|c| c.group).unwrap_or(false);
        app.calls.remove(&cid);
        if app.dialing.as_ref().map(|d| d.peer_ssi).is_some() {
            app.dialing = None;
        }
        if app.grp_call.as_ref().and_then(|g| g.cid) == Some(cid) {
            app.grp_call = None;
        }
        if app.ptt_held == Some(cid) {
            app.ptt_held = None;
        }
        // Drop a group context that never bound to a real call (e.g. our setup
        // was rejected before a confirm arrived) so the panel doesn't stick.
        if active_group_call(app).is_none()
            && app.grp_call.as_ref().map(|g| g.cid.is_none()).unwrap_or(false)
        {
            app.grp_call = None;
        }
        // "Call ended" toast unless we hung up ourselves.
        if !app.local_end.remove(&cid) && !was_group {
            let cause = body
                .get("disconnect_cause")
                .and_then(Value::as_str)
                .map(pretty_cause)
                .unwrap_or_else(|| "The call was released.".to_string());
            app.notify(weak, "Call ended", &cause, 0);
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

    // Attribute the talker onto the live call (floor housekeeping).
    if let Some(t) = talker {
        if let Some(c) = app.calls.get_mut(&cid) {
            if t != app.state.own_issi {
                c.talker_ssi = Some(t);
                c.can_request_tx = false;
            }
        }
    }
    push_calls(app, weak);

    // Only 274-bit TCH/S speech is decodable.
    if !matches!(frame_bits, None | Some(274)) {
        return;
    }
    if let Some(a) = audio {
        let data: Vec<u8> = payload
            .get("data")
            .and_then(Value::as_array)
            .map(|arr| {
                arr.iter()
                    .map(|v| if v.as_u64().unwrap_or(0) != 0 { 1u8 } else { 0u8 })
                    .collect()
            })
            .unwrap_or_default();
        if data.len() == 274 {
            a.play_downlink(&data, bad);
        }
    }
}

/// Reconcile the mic uplink with the current floor: transmit only while we
/// physically hold the PTT on an active call (individual or group).
fn sync_uplink(app: &AppState, audio: Option<&crate::audio::AudioEngine>) {
    let Some(a) = audio else { return };
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
                peer_name(c.peer_ssi, false),
                c.peer_ssi.map(|s| s.to_string()).unwrap_or_default(),
                label,
                clock,
                can_ptt,
                ptt_state,
            )
        } else if let Some(d) = app.dialing.as_ref().filter(|d| !d.group) {
            (
                true,
                "Outgoing".to_string(),
                peer_name(Some(d.peer_ssi), false),
                d.peer_ssi.to_string(),
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
    let group_status = if i_am_talking {
        "TALKING - You".to_string()
    } else if let Some(o) = other_talker {
        format!("TALKING - {}", peer_name(Some(o), true))
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

    let _ = weak.upgrade_in_event_loop(move |w| {
        w.set_call_live(call_live);
        w.set_call_dir(call_dir.into());
        w.set_call_peer(call_peer.into());
        w.set_call_sub(call_sub.into());
        w.set_call_state(call_state.into());
        w.set_call_clock(call_clock.into());
        w.set_call_can_ptt(call_can_ptt);
        w.set_call_ptt_state(call_ptt_state);
        w.set_call_incoming(call_incoming);
        w.set_incoming_peer(inc_peer.into());
        w.set_incoming_sub(inc_sub.into());
        w.set_group_call_active(group_active);
        w.set_group_call_name(group_name.into());
        w.set_group_call_status(group_status.into());
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

