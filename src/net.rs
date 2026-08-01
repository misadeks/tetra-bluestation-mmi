// The two WebSocket servers (control + telemetry), mirroring the stack's own
// bins: sync tungstenite, one accept thread per channel, one thread per
// connection, decoded inbound pushed to the app over crossbeam. The control
// connection also carries an outbound sink so the app can send commands; we
// interleave reads and writes using a short socket read timeout (the same
// pattern the stack uses).
//
// Topology reminder: the STACK is the client and dials out; THIS app is the
// server. Frames are binary UTF-8 JSON; text is tolerated on receive.

use std::io::{ErrorKind, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use base64::Engine;
use crossbeam_channel::{unbounded, Sender};
use serde_json::Value;
use tungstenite::handshake::server::{ErrorResponse, Request, Response};
use tungstenite::Message;

use crate::app::AppEvent;
use crate::protocol::{CONTROL_SUBPROTOCOL, TELEMETRY_SUBPROTOCOL};

/// Optional wire log of raw WebSocket frames (both channels), for diagnostics.
pub struct WireLog {
    file: Option<Mutex<std::fs::File>>,
    speech: bool,
}

impl WireLog {
    /// Open the wire log if enabled; a disabled log is a cheap no-op.
    pub fn new(enabled: bool, path: &str, speech: bool) -> Arc<WireLog> {
        let file = if enabled {
            match std::fs::OpenOptions::new().create(true).append(true).open(path) {
                Ok(f) => {
                    tracing::info!(%path, "wire log: enabled");
                    Some(Mutex::new(f))
                }
                Err(e) => {
                    tracing::warn!(%path, error = %e, "wire log: could not open file");
                    None
                }
            }
        } else {
            None
        };
        Arc::new(WireLog { file, speech })
    }

    /// Record one frame. `out` = we sent it; `channel` is "ctrl"/"tele".
    fn record(&self, out: bool, channel: &str, data: &[u8]) {
        let Some(file) = &self.file else { return };
        let text = std::str::from_utf8(data).unwrap_or("<binary>");
        // MsSpeechFrame is high-rate voice; skip unless explicitly requested.
        if !self.speech && text.contains("MsSpeechFrame") {
            return;
        }
        let ts = chrono::Local::now().format("%H:%M:%S%.3f");
        let dir = if out { "OUT" } else { "IN " };
        let line = format!("{ts} {dir} {channel} {text}\n");
        if let Ok(mut f) = file.lock() {
            let _ = f.write_all(line.as_bytes());
        }
    }
}

/// Optional HTTP Basic auth for a channel. Empty username = accept all (demo).
#[derive(Clone)]
pub struct ChannelAuth {
    pub username: String,
    pub password: String,
}

impl ChannelAuth {
    fn accepts(&self, req: &Request) -> bool {
        if self.username.is_empty() {
            return true;
        }
        let expected = format!(
            "Basic {}",
            base64::engine::general_purpose::STANDARD
                .encode(format!("{}:{}", self.username, self.password))
        );
        req.headers()
            .get("Authorization")
            .and_then(|v| v.to_str().ok())
            .map(|h| h == expected)
            .unwrap_or(false)
    }
}

/// Handshake callback: enforce auth, then accept and echo the subprotocol.
fn negotiate(
    auth: &ChannelAuth,
    subprotocol: &str,
    req: &Request,
    mut response: Response,
) -> Result<Response, ErrorResponse> {
    if !auth.accepts(req) {
        let mut err = ErrorResponse::new(Some("Unauthorized".to_string()));
        *err.status_mut() = tungstenite::http::StatusCode::UNAUTHORIZED;
        err.headers_mut().insert(
            "WWW-Authenticate",
            "Basic realm=\"bluestation\"".parse().unwrap(),
        );
        return Err(err);
    }
    let offered = req
        .headers()
        .get("Sec-WebSocket-Protocol")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    if offered.split(',').map(str::trim).any(|s| s == subprotocol) {
        response
            .headers_mut()
            .insert("Sec-WebSocket-Protocol", subprotocol.parse().unwrap());
        Ok(response)
    } else {
        Err(ErrorResponse::new(Some(format!(
            "unsupported subprotocol; expected {subprotocol}"
        ))))
    }
}

fn decode_and_forward(events: &Sender<AppEvent>, data: &[u8], control: bool) {
    match serde_json::from_slice::<Value>(data) {
        Ok(value) => {
            let event = if control {
                AppEvent::ControlMessage(value)
            } else {
                AppEvent::TelemetryMessage(value)
            };
            let _ = events.send(event);
        }
        Err(e) => tracing::warn!(error = %e, control, "undecodable frame, ignoring"),
    }
}

fn is_would_block(err: &tungstenite::Error) -> bool {
    matches!(
        err,
        tungstenite::Error::Io(e)
            if e.kind() == ErrorKind::WouldBlock || e.kind() == ErrorKind::TimedOut
    )
}

pub fn spawn_control_server(
    host: String,
    port: u16,
    auth: ChannelAuth,
    events: Sender<AppEvent>,
    wire: Arc<WireLog>,
) -> JoinHandle<()> {
    thread::spawn(move || {
        let addr = format!("{host}:{port}");
        let listener = match TcpListener::bind(&addr) {
            Ok(l) => l,
            Err(e) => {
                tracing::error!(%addr, error = %e, "control: bind failed");
                return;
            }
        };
        tracing::info!(%addr, subprotocol = CONTROL_SUBPROTOCOL, "control server listening");
        for stream in listener.incoming() {
            match stream {
                Ok(s) => {
                    let peer = s.peer_addr().map(|a| a.to_string()).unwrap_or_else(|_| "?".into());
                    let auth = auth.clone();
                    let events = events.clone();
                    let wire = wire.clone();
                    thread::spawn(move || handle_control(s, peer, auth, events, wire));
                }
                Err(e) => tracing::warn!(error = %e, "control: accept error"),
            }
        }
    })
}

pub fn spawn_telemetry_server(
    host: String,
    port: u16,
    auth: ChannelAuth,
    events: Sender<AppEvent>,
    wire: Arc<WireLog>,
) -> JoinHandle<()> {
    thread::spawn(move || {
        let addr = format!("{host}:{port}");
        let listener = match TcpListener::bind(&addr) {
            Ok(l) => l,
            Err(e) => {
                tracing::error!(%addr, error = %e, "telemetry: bind failed");
                return;
            }
        };
        tracing::info!(%addr, subprotocol = TELEMETRY_SUBPROTOCOL, "telemetry server listening");
        for stream in listener.incoming() {
            match stream {
                Ok(s) => {
                    let peer = s.peer_addr().map(|a| a.to_string()).unwrap_or_else(|_| "?".into());
                    let auth = auth.clone();
                    let events = events.clone();
                    let wire = wire.clone();
                    thread::spawn(move || handle_telemetry(s, peer, auth, events, wire));
                }
                Err(e) => tracing::warn!(error = %e, "telemetry: accept error"),
            }
        }
    })
}

fn handle_control(
    stream: TcpStream,
    peer: String,
    auth: ChannelAuth,
    events: Sender<AppEvent>,
    wire: Arc<WireLog>,
) {
    let mut ws = match tungstenite::accept_hdr(stream, |req: &Request, resp: Response| {
        negotiate(&auth, CONTROL_SUBPROTOCOL, req, resp)
    }) {
        Ok(ws) => ws,
        Err(e) => {
            tracing::warn!(%peer, error = %e, "control: handshake failed");
            return;
        }
    };
    // Short read timeout so we can interleave outbound command sends.
    if let Err(e) = ws.get_mut().set_read_timeout(Some(Duration::from_millis(100))) {
        tracing::warn!(error = %e, "control: set_read_timeout failed");
    }
    tracing::info!(%peer, "control: connected");

    let (out_tx, out_rx) = unbounded::<Vec<u8>>();
    if events.send(AppEvent::ControlConnected(out_tx)).is_err() {
        return;
    }

    'conn: loop {
        // Flush any pending outbound commands first.
        while let Ok(bytes) = out_rx.try_recv() {
            wire.record(true, "ctrl", &bytes);
            if let Err(e) = ws.send(Message::Binary(bytes.into())) {
                tracing::warn!(%peer, error = %e, "control: send failed");
                break 'conn;
            }
        }
        let _ = ws.flush();

        match ws.read() {
            Ok(Message::Binary(data)) => {
                wire.record(false, "ctrl", &data);
                decode_and_forward(&events, &data, true);
            }
            Ok(Message::Text(text)) => {
                wire.record(false, "ctrl", text.as_str().as_bytes());
                decode_and_forward(&events, text.as_str().as_bytes(), true);
            }
            Ok(Message::Ping(_)) => {
                let _ = ws.flush();
            }
            Ok(Message::Pong(_)) | Ok(Message::Frame(_)) => {}
            Ok(Message::Close(_)) => break,
            Err(ref e) if is_would_block(e) => continue,
            Err(tungstenite::Error::ConnectionClosed)
            | Err(tungstenite::Error::AlreadyClosed) => break,
            Err(e) => {
                tracing::info!(%peer, error = %e, "control: read ended");
                break;
            }
        }
    }

    let _ = events.send(AppEvent::ControlDisconnected);
    tracing::info!(%peer, "control: closed");
}

fn handle_telemetry(
    stream: TcpStream,
    peer: String,
    auth: ChannelAuth,
    events: Sender<AppEvent>,
    wire: Arc<WireLog>,
) {
    let mut ws = match tungstenite::accept_hdr(stream, |req: &Request, resp: Response| {
        negotiate(&auth, TELEMETRY_SUBPROTOCOL, req, resp)
    }) {
        Ok(ws) => ws,
        Err(e) => {
            tracing::warn!(%peer, error = %e, "telemetry: handshake failed");
            return;
        }
    };
    tracing::info!(%peer, "telemetry: connected");
    let _ = events.send(AppEvent::TelemetryConnected);

    loop {
        match ws.read() {
            Ok(Message::Binary(data)) => {
                wire.record(false, "tele", &data);
                decode_and_forward(&events, &data, false);
            }
            Ok(Message::Text(text)) => {
                wire.record(false, "tele", text.as_str().as_bytes());
                decode_and_forward(&events, text.as_str().as_bytes(), false);
            }
            Ok(Message::Ping(_)) => {
                let _ = ws.flush();
            }
            Ok(Message::Pong(_)) | Ok(Message::Frame(_)) => {}
            Ok(Message::Close(_)) => break,
            Err(tungstenite::Error::ConnectionClosed)
            | Err(tungstenite::Error::AlreadyClosed) => break,
            Err(e) => {
                tracing::info!(%peer, error = %e, "telemetry: read ended");
                break;
            }
        }
    }

    let _ = events.send(AppEvent::TelemetryDisconnected);
    tracing::info!(%peer, "telemetry: closed");
}
