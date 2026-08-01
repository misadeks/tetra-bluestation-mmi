// Central app state and the single-threaded event loop that owns it.
//
// All net threads (control + telemetry) and timers marshal into here via a
// crossbeam channel; this loop is the only writer of UI state, which it pushes
// onto the Slint event loop with `upgrade_in_event_loop`. Commands to the stack
// go out through the current control connection's outbound sink.

use crossbeam_channel::{Receiver, Sender};
use serde_json::Value;
use slint::{ModelRc, VecModel};

use crate::codeplug::Codeplug;
use crate::protocol::{self, MsRuntimeState};
use crate::{FolderRow, MainWindow};

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
    UiSelectFolder(i32),
    UiPtt,
    UiDialKey(String),
    UiDialCall,
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
}

impl AppState {
    fn next_handle(&mut self) -> u32 {
        self.next_handle = self.next_handle.wrapping_add(1);
        self.next_handle
    }

    fn send(&self, message: Value) -> bool {
        if let Some(tx) = &self.control_out {
            if let Ok(bytes) = serde_json::to_vec(&message) {
                return tx.send(bytes).is_ok();
            }
        }
        false
    }
}

/// Run the app event loop until the channel closes. Blocks the calling thread.
pub fn run(rx: Receiver<AppEvent>, weak: slint::Weak<MainWindow>, reg_type: String) {
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
                tracing::info!("control: stack disconnected");
                push_ui(&app, &weak);
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
                handle_telemetry(&mut app, &value);
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
            }
            AppEvent::ClockTick { time, date } => {
                let weak = weak.clone();
                let _ = weak.upgrade_in_event_loop(move |w| {
                    w.set_clock(time.into());
                    w.set_date(date.into());
                });
            }
            AppEvent::UiRegister => {
                let h = app.next_handle();
                let st = &app.state;
                let msg = protocol::tnmm_registration(
                    h,
                    &app.reg_type,
                    st.own_issi,
                    st.home_mcc,
                    st.home_mnc,
                );
                tracing::info!(issi = st.own_issi, "UI: sending TnmmRegistration");
                app.send(msg);
            }
            AppEvent::UiDeregister => {
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
            AppEvent::UiSelectTalkgroup => {
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
                // Voice calls land in M5/M6; for now just note the press.
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
                tracing::info!(
                    number = %app.dial_number,
                    "UI: dial call (voice calls arrive in a later milestone)"
                );
            }
        }
    }
}

fn handle_control(app: &mut AppState, message: &Value, weak: &slint::Weak<MainWindow>) {
    let Some((variant, payload)) = protocol::variant_of(message) else {
        tracing::warn!(?message, "control: undecodable/none-variant frame");
        return;
    };
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
                        push_ui(app, weak);
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
                            match Codeplug::parse(toml) {
                                Some(cp) => {
                                    tracing::info!(
                                        folders = cp.folders.len(),
                                        "codeplug parsed"
                                    );
                                    app.codeplug = Some(cp);
                                    app.sel_folder = 0;
                                    app.cycle_gssi = None;
                                    push_ui(app, weak);
                                }
                                None => tracing::info!("config has no talkgroups"),
                            }
                        }
                    }
                }
                "Ack" => tracing::info!(?body, "management ack"),
                "Error" => tracing::warn!(?body, "management error"),
                other => tracing::info!(variant = other, "management response (unhandled)"),
            }
        }
        "TnmmAck" => tracing::info!(?payload, "TnmmAck"),
        "TnccAck" => tracing::info!(?payload, "TnccAck"),
        other => tracing::info!(variant = other, "control response (unhandled)"),
    }
}

fn handle_telemetry(app: &mut AppState, message: &Value) {
    let Some((variant, _payload)) = protocol::variant_of(message) else {
        tracing::warn!("telemetry: undecodable/none-variant frame");
        return;
    };
    // Downlink voice is high-rate; do not log each frame (decode lands in M5).
    if variant == "MsSpeechFrame" {
        return;
    }
    tracing::info!(variant, "telemetry event");
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
}
