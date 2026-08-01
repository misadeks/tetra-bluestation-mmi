# TETRA TN UI (native Rust + Slint)

Another variant of the TN UI: a native, touchscreen radio UI for a BlueStation
MS-mode TETRA terminal, built with Rust + Slint. This app implements the server
side of the BlueStation MS external interface and presents a Classic-style
radio UI over it.

It is a peer of the Python TNMM Demo UI (GitHub `misadeks/tetra-tn-web-ui`, the
browser variant). Both variants play the same role; this one is native/embedded,
the other is browser-based. This is NOT a port of the browser UI and NOT a
dashboard client.

## Topology (get this right first)

- The stack is the WebSocket CLIENT and dials OUT to this UI.
- This UI is the WebSocket SERVER and listens on two ports:
  - Control `9102`, subprotocol `bluestation-control-v1` (UI to stack commands
    and stack to UI responses on the same socket).
  - Telemetry `9101`, subprotocol `bluestation-telemetry-v1` (stack to UI
    events, receive-only).
- App messages are JSON encoded as UTF-8 inside binary WebSocket frames.
- Enums are externally tagged: `{"VariantName": { ...fields... }}`.

## Targets

- Raspberry Pi (aarch64 Linux) for deployment (Slint linuxkms backend).
- Windows / RustRover for development (`cargo run`).

## Status

M1 (toolchain spike): a Slint hello window (portrait 720x1280, dark) plus a
config parsing stub. The two WebSocket servers land in M2.

## Build and run (Windows dev)

```powershell
cargo run
```

The window reads `config.toml` from the working directory. If the file is
absent, built-in defaults are used (control `9102`, telemetry `9101`).

## Configuration

See `config.toml`. This app is the server side of the interface, so `host` and
`port` are the addresses the stack dials into. See DECISIONS.md for the rationale
behind the current choices.

## Milestones

- M1 toolchain spike (this milestone).
- M2 net + JSON: the two WebSocket servers, bootstrap + poll GetState, decode
  telemetry, reconnect tolerant. Test against `fake_stack.py`.
- M3+ state, status bar, home, navigation, audio, calls, editors, Pi hardware
  I/O, and kiosk polish.
