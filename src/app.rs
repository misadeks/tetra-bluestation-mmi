// Central app state and the single-threaded event loop that owns it.
//
// All net threads (control + telemetry) and timers marshal into here via a
// crossbeam channel; this loop is the only writer of UI state, which it pushes
// onto the Slint event loop with `upgrade_in_event_loop`. Commands to the stack
// go out through the current control connection's outbound sink.

use crossbeam_channel::{Receiver, Sender};
use serde_json::Value;

use crate::protocol::{self, MsRuntimeState};
use crate::MainWindow;

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
    /// Wall clock update (pre-formatted HH:MM:SS).
    ClockTick(String),
    /// UI actions.
    UiRegister,
    UiDeregister,
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
            AppEvent::ClockTick(text) => {
                let weak = weak.clone();
                let _ = weak.upgrade_in_event_loop(move |w| w.set_clock(text.into()));
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
                    if body
                        .get("toml")
                        .and_then(Value::as_str)
                        .map(|s| !s.trim().is_empty())
                        .unwrap_or(false)
                    {
                        app.have_config = true;
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
    let attached_count = s.attached_groups.len() as i32;
    let scan_active = s.attached_groups.len() > 1;
    let (talkgroup_name, talkgroup_id) = match s.attached_groups.first() {
        Some(gssi) => (format!("TG {gssi}"), gssi.to_string()),
        None => ("No group".to_string(), "--".to_string()),
    };
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
        w.set_attached_count(attached_count);
        w.set_scan_active(scan_active);
        w.set_talkgroup_name(talkgroup_name.into());
        w.set_talkgroup_id(talkgroup_id.into());
        w.set_restart_required(restart_required);
    });
}
