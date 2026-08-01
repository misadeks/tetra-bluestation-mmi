// TETRA TN UI - native Rust + Slint variant.
//
// Another variant of the TN UI: this app implements the server side of the
// BlueStation MS external interface and presents a Classic-style radio UI over
// it. It is a native/embedded peer of the browser tetra-tn-web-ui (Python TNMM
// Demo UI), not a port of it.
//
// M2/M3: the two WebSocket servers (control 9102, telemetry 9101), a serde
// protocol layer, a central app state, and a status bar + home screen driven by
// live MsRuntimeState. Topology: the stack is the WS client and dials out; this
// app is the server.

mod app;
mod audio;
mod codeplug;
mod config;
mod net;
mod protocol;

use std::thread;
use std::time::Duration;

use config::{Config, InputKind};

slint::include_modules!();

fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let cfg = Config::load("config.toml")?;
    let ui = cfg.resolve_ui();
    tracing::info!(
        control_port = cfg.command.port,
        telemetry_port = cfg.telemetry.port,
        model = ui.model.as_deref().unwrap_or("<none>"),
        width = ui.width,
        height = ui.height,
        scale = ui.scale,
        input = ?ui.input,
        theme = %ui.theme,
        "loaded config"
    );

    // Override the host display scaling (e.g. Windows 150%) so the window renders
    // at the device's own scale factor. Must be set before the window is created.
    std::env::set_var("SLINT_SCALE_FACTOR", ui.scale.to_string());

    let window = MainWindow::new()?;
    // Fixed window size in logical pixels (physical = logical * scale). Binding
    // the Window width/height makes it non-resizable so screen content never
    // stretches it. SLINT_SCALE_FACTOR (set above) forces the device scale.
    window.set_win_width(ui.width as f32 / ui.scale);
    window.set_win_height(ui.height as f32 / ui.scale);
    window.set_device_input(match ui.input {
        InputKind::Touch => DeviceInput::Touch,
        InputKind::Keypad => DeviceInput::Keypad,
    });
    window.set_show_event_log(ui.show_event_log);

    // --- Wire the network + app event loop --------------------------------
    let (events_tx, events_rx) = crossbeam_channel::unbounded::<app::AppEvent>();

    let control_auth = net::ChannelAuth {
        username: cfg.command.username.clone(),
        password: cfg.command.password.clone(),
    };
    let telemetry_auth = net::ChannelAuth {
        username: cfg.telemetry.username.clone(),
        password: cfg.telemetry.password.clone(),
    };
    net::spawn_control_server(
        cfg.command.host.clone(),
        cfg.command.port,
        control_auth,
        events_tx.clone(),
    );
    net::spawn_telemetry_server(
        cfg.telemetry.host.clone(),
        cfg.telemetry.port,
        telemetry_auth,
        events_tx.clone(),
    );

    // Poll GetState every ~2s (the app loop only acts on it while connected).
    {
        let tx = events_tx.clone();
        thread::spawn(move || loop {
            thread::sleep(Duration::from_secs(2));
            if tx.send(app::AppEvent::PollTick).is_err() {
                break;
            }
        });
    }

    // Wall clock + date for the status bar/header, once a second.
    {
        let tx = events_tx.clone();
        thread::spawn(move || loop {
            let now = chrono::Local::now();
            let tick = app::AppEvent::ClockTick {
                time: now.format("%H:%M").to_string(),
                date: now.format("%a %d %b").to_string(),
            };
            if tx.send(tick).is_err() {
                break;
            }
            thread::sleep(Duration::from_secs(1));
        });
    }

    // UI actions -> app loop.
    {
        let tx = events_tx.clone();
        window.on_register(move || {
            let _ = tx.send(app::AppEvent::UiRegister);
        });
    }
    {
        let tx = events_tx.clone();
        window.on_deregister(move || {
            let _ = tx.send(app::AppEvent::UiDeregister);
        });
    }
    {
        let tx = events_tx.clone();
        window.on_cycle_prev(move || {
            let _ = tx.send(app::AppEvent::UiCyclePrev);
        });
    }
    {
        let tx = events_tx.clone();
        window.on_cycle_next(move || {
            let _ = tx.send(app::AppEvent::UiCycleNext);
        });
    }
    {
        let tx = events_tx.clone();
        window.on_ptt(move || {
            let _ = tx.send(app::AppEvent::UiPtt);
        });
    }
    {
        let tx = events_tx.clone();
        window.on_select_talkgroup(move || {
            let _ = tx.send(app::AppEvent::UiSelectTalkgroup);
        });
    }
    {
        let tx = events_tx.clone();
        window.on_cancel_select(move || {
            let _ = tx.send(app::AppEvent::UiCancelSelect);
        });
    }
    {
        let tx = events_tx.clone();
        window.on_select_folder(move |i| {
            let _ = tx.send(app::AppEvent::UiSelectFolder(i));
        });
    }
    {
        let tx = events_tx.clone();
        window.on_dial_key(move |k| {
            let _ = tx.send(app::AppEvent::UiDialKey(k.to_string()));
        });
    }
    {
        let tx = events_tx.clone();
        window.on_dial_call(move || {
            let _ = tx.send(app::AppEvent::UiDialCall);
        });
    }
    {
        let tx = events_tx.clone();
        window.on_open_logs(move || {
            let _ = tx.send(app::AppEvent::UiOpenLogs);
        });
    }
    {
        let tx = events_tx.clone();
        window.on_alert_dismiss(move || {
            let _ = tx.send(app::AppEvent::UiAlertDismiss);
        });
    }
    {
        let tx = events_tx.clone();
        window.on_group_select(move |gssi, cou| {
            let _ = tx.send(app::AppEvent::UiGroupSelect(gssi, cou));
        });
    }
    {
        let tx = events_tx.clone();
        window.on_group_attach(move |gssi, cou| {
            let _ = tx.send(app::AppEvent::UiGroupAttach(gssi, cou));
        });
    }
    {
        let tx = events_tx.clone();
        window.on_group_detach(move |gssi| {
            let _ = tx.send(app::AppEvent::UiGroupDetach(gssi));
        });
    }
    {
        let tx = events_tx.clone();
        window.on_scanlist_toggle(move |name, active| {
            let _ = tx.send(app::AppEvent::UiScanlistToggle(name.to_string(), active));
        });
    }
    {
        let tx = events_tx.clone();
        window.on_apply_config(move || {
            let _ = tx.send(app::AppEvent::UiApplyConfig);
        });
    }
    {
        let tx = events_tx.clone();
        window.on_refresh(move || {
            let _ = tx.send(app::AppEvent::UiRefresh);
        });
    }
    {
        let tx = events_tx.clone();
        window.on_survey_toggle_mode(move || {
            let _ = tx.send(app::AppEvent::UiSurveyToggleMode);
        });
    }
    {
        let tx = events_tx.clone();
        window.on_survey_scan(move || {
            let _ = tx.send(app::AppEvent::UiSurveyScan);
        });
    }
    {
        let tx = events_tx.clone();
        window.on_survey_stop(move || {
            let _ = tx.send(app::AppEvent::UiSurveyStop);
        });
    }
    {
        let tx = events_tx.clone();
        window.on_survey_camp(move |carrier, register| {
            let _ = tx.send(app::AppEvent::UiCampCell(carrier as u64, register));
        });
    }
    {
        let tx = events_tx.clone();
        window.on_call_ptt_down(move || {
            let _ = tx.send(app::AppEvent::UiCallPttDown);
        });
    }
    {
        let tx = events_tx.clone();
        window.on_call_ptt_up(move || {
            let _ = tx.send(app::AppEvent::UiCallPttUp);
        });
    }
    {
        let tx = events_tx.clone();
        window.on_group_ptt_down(move || {
            let _ = tx.send(app::AppEvent::UiGroupPttDown);
        });
    }
    {
        let tx = events_tx.clone();
        window.on_group_ptt_up(move || {
            let _ = tx.send(app::AppEvent::UiGroupPttUp);
        });
    }
    {
        let tx = events_tx.clone();
        window.on_answer_call(move || {
            let _ = tx.send(app::AppEvent::UiAnswerCall);
        });
    }
    {
        let tx = events_tx.clone();
        window.on_reject_call(move || {
            let _ = tx.send(app::AppEvent::UiRejectCall);
        });
    }
    {
        let tx = events_tx.clone();
        window.on_hangup_call(move || {
            let _ = tx.send(app::AppEvent::UiHangup);
        });
    }
    {
        let tx = events_tx.clone();
        window.on_hangup_group(move || {
            let _ = tx.send(app::AppEvent::UiHangupGroup);
        });
    }

    // The app loop owns state and is the sole UI writer.
    {
        let weak = window.as_weak();
        let reg_type = cfg.registration.registration_type.clone();
        let audio_cfg = cfg.audio.clone();
        let rx = events_rx;
        let self_tx = events_tx.clone();
        thread::spawn(move || app::run(rx, self_tx, weak, reg_type, audio_cfg));
    }

    tracing::info!("starting Slint event loop");
    window.run()?;
    Ok(())
}
