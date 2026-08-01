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
    tracing::info!(
        control_port = cfg.command.port,
        telemetry_port = cfg.telemetry.port,
        ui_width = cfg.ui.width,
        ui_height = cfg.ui.height,
        theme = %cfg.ui.theme,
        "loaded config (M1: servers not started yet)"
    );

    let window = MainWindow::new()?;
    window.set_status_line(
        format!(
            "M1 spike - control :{}  telemetry :{}",
            cfg.command.port, cfg.telemetry.port
        )
        .into(),
    );

    tracing::info!("starting Slint event loop");
    window.run()?;
    Ok(())
}
