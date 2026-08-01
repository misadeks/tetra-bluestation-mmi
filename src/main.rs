// TETRA TN UI - native Rust + Slint variant.
//
// Another variant of the TN UI: this app implements the server side of the
// BlueStation MS external interface and presents a Classic-style radio UI over
// it. It is a native/embedded peer of the browser tetra-tn-web-ui (Python TNMM
// Demo UI), not a port of it.
//
// M1: toolchain spike. Bring up a Slint hello window (portrait, dark) and parse
// the config stub. Networking (the two WebSocket servers) lands in M2.

mod config;

use config::Config;

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
        theme = %ui.theme,
        "loaded config (M1: servers not started yet)"
    );

    // Override the host display scaling (e.g. Windows 150%) so the window renders
    // at the device's own scale factor. Must be set before the window is created.
    std::env::set_var("SLINT_SCALE_FACTOR", ui.scale.to_string());

    let window = MainWindow::new()?;
    // Size the window so it occupies width x height device pixels on screen,
    // regardless of the host monitor's scaling. With SLINT_SCALE_FACTOR forced to
    // `scale`, physical = logical * scale, so request logical = width/scale.
    window.window().set_size(slint::LogicalSize::new(
        ui.width as f32 / ui.scale,
        ui.height as f32 / ui.scale,
    ));
    tracing::info!(
        width = ui.width,
        height = ui.height,
        scale = ui.scale,
        "applied window size from config"
    );
    window.set_status_line(
        format!(
            "M1 spike - {} {}x{} @{}x  control :{}  telemetry :{}",
            ui.model.as_deref().unwrap_or("custom"),
            ui.width,
            ui.height,
            ui.scale,
            cfg.command.port,
            cfg.telemetry.port
        )
        .into(),
    );

    tracing::info!("starting Slint event loop");
    window.run()?;
    Ok(())
}
