# TETRA TN UI - native Rust + Slint variant

A native **Rust + Slint** touchscreen radio UI for a **BlueStation MS-mode** TETRA
terminal. It is **another variant of the TN UI**, a sibling to the Python **TN web UI**
(GitHub `misadeks/tetra-tn-web-ui`, whose app is titled "TNMM Demo UI" and is checked
out locally as `tnmm_ui`). It is not a port of that browser front-end. Both variants
play the same role: they implement the **server side of the BlueStation MS external
interface** (`bluestation-ms-interface-2`) and present the operator a Classic-style
radio UI over it. This variant is native/embedded rather than browser-based.

It supersedes a prior C + LVGL attempt (`misadeks/tetra-tn-lvgl-ui`). We switched to
Rust + Slint because the whole TETRA stack is Rust, the wire protocol is serde
externally-tagged enums (so we get compile-checked wire parity), and cargo
cross-compiles cleanly to the Pi, avoiding the ARM64 C-toolchain pain of the LVGL
attempt.

The stack (or the `fake_stack.py` simulator) is the WebSocket **client** and dials
**out** to this app on two channels:

| Channel   | This app listens on | Subprotocol                | Traffic                               |
|-----------|---------------------|----------------------------|---------------------------------------|
| Control   | `9102`              | `bluestation-control-v1`   | UI to stack commands, stack to UI responses |
| Telemetry | `9101`              | `bluestation-telemetry-v1` | stack to UI events (receive-only)     |

Messages are **JSON encoded as UTF-8 inside _binary_ WebSocket frames**, using the
externally-tagged enum shape `{"Variant": {..}}`. This app does **not** reimplement any
TETRA stack, protocol, registration, or codec-negotiation logic - it drives the MS over
the fixed wire contract. See the TN web UI repo (`tetra-tn-web-ui`) and its
`PROTOCOL.md` for the message catalog.

Targets:
- **Raspberry Pi** (aarch64 embedded Linux) - deployment device.
- **Windows** - development/testing from RustRover.

## Repository layout

```
Cargo.toml        crate manifest (Slint, serde, toml, tracing)
build.rs          compiles ui/main.slint via slint-build
ui/main.slint     Slint markup (portrait 720x1280 dark hello window)
src/main.rs       init, logging, config load, Slint event loop
src/config.rs     config.toml parsing (stub for M1)
config.toml       runtime config (BlueStation MS listen ports, [audio], [ui])
DECISIONS.md      running log of decisions and deviations
```

## Prerequisites

- **Rust 1.95** or later (`cargo` / `rustc` on PATH).
- On Windows: the **MSVC** toolchain (Visual Studio 2022 Build Tools, C++ workload).
- For Pi cross-builds: the `aarch64-unknown-linux-gnu` target plus a cross linker, or
  build on the Pi directly.

No external native dependency setup is needed for the M1 spike; Slint fetches and builds
its renderer stack through cargo.

## Build and run - Windows (RustRover)

```powershell
cargo run
```

A dark portrait window titled *"TETRA TN UI"* with a scaffold card should open. The app
reads `config.toml` from the working directory; if the file is absent, built-in defaults
are used (control `9102`, telemetry `9101`). Set `RUST_LOG=debug` for more verbose logs.

Run the unit tests with:

```powershell
cargo test
```

## Build and run - Raspberry Pi (aarch64 Linux)

Add the target and build (on the Pi, or cross-compile with a linker configured):

```bash
rustup target add aarch64-unknown-linux-gnu
cargo build --release --target aarch64-unknown-linux-gnu
```

The Pi kiosk will use the Slint **linuxkms** (DRM/KMS) backend; that wiring and the
systemd autostart land in a later milestone. For dev on the Pi you can also run under
X/Wayland with the default winit backend.

## Developing with no radio hardware

The **TN web UI** repo (`tetra-tn-web-ui`, checked out locally as `tnmm_ui`) ships a
`fake_stack.py` simulator that plays the stack (the WS **client**). Because this app is
the **server**, point the simulator's control and telemetry URLs at the ports this app
listens on:

```powershell
# in the tetra-tn-web-ui repo (locally tnmm_ui), on Windows:
python fake_stack.py --control ws://127.0.0.1:9102 --telemetry ws://127.0.0.1:9101 [--chaos]
```

Leave `[command].port` / `[telemetry].port` in `config.toml` at their defaults
(`9102` / `9101`) so the simulator connects. `--chaos` randomly drops channels so you can
exercise reconnect handling. (The WebSocket servers are wired up in M2.)

## Configuration (`config.toml`)

| Section | Key | Meaning |
|---|---|---|
| `[command]` | `host` / `port` | Control-channel listen address (default `0.0.0.0:9102`). |
| `[telemetry]` | `host` / `port` | Telemetry-channel listen address (default `0.0.0.0:9101`). |
| `[command]` / `[telemetry]` | `use_tls` / `ca_cert` | `wss` + server cert PEM when TLS is enabled. |
| `[command]` / `[telemetry]` | `username` / `password` | HTTP Basic auth to accept; empty = accept all (demo). |
| `[registration]` | `registration_type` | Operator registration preference (identity comes from the MS, never configured). |
| `[audio]` | `output_device` / `input_device` | Output/input device selection. |
| `[audio]` | `sample_rate` / `frame_ms` / `jitter_ms` | Audio path timing. |
| `[ui]` | `model` | Device model to use from the catalog (built-ins: `pi-1280x720`, `pi-720x1280`, `linht`, plus any `[[device]]`). |
| `[ui]` | `width` / `height` | Explicit device-pixel size; overrides the selected model. |
| `[ui]` | `scale` | UI scale factor. In dev it overrides the host display scaling (e.g. Windows 150%) so the window renders at the target 1:1; `1.0` = one window pixel per device pixel. |
| `[ui]` | `input` | Interaction model / layout: `touch` (tap targets) or `keypad` (softkeys + Up/Down focus). Overrides the model. |
| `[ui]` | `theme` | UI theme. |
| `[[device]]` | `name` / `width` / `height` / `scale` / `input` | A device model in the catalog. Select it via `[ui].model`; a profile here overrides a built-in of the same name. |

### Device models and layouts

The window size and layout target a device model rather than a single hardcoded
size, so the same binary drives different panels (a landscape Pi touchscreen, a
portrait handheld, a LinHT-style keypad device, etc.). Select one with
`[ui].model`, or define your own under `[[device]]` and select that.
`[ui].width` / `height` / `scale` / `input` override the selected model.

Each model also declares an `input` kind that selects the layout and
interaction:

- `touch` - a touch-first layout with large tap targets.
- `keypad` - a softkey-driven layout navigated with Up/Down (focus based), for
  devices without a touchscreen.

The `scale` value also corrects the host display scaling during development: set
it to `1.0` and the dev window occupies exactly `width x height` device pixels
regardless of the Windows scaling setting.

## Status and milestones

Current: **M1 - toolchain spike.** A Slint hello window (portrait 720x1280, dark) plus a
config parsing stub. The two WebSocket servers land in M2. See `DECISIONS.md` for the
rationale behind each choice and any deviations.

- M1 toolchain spike (this milestone).
- M2 net + JSON: the two WebSocket servers, bootstrap + poll GetState, decode telemetry,
  reconnect tolerant. Test against `fake_stack.py`.
- M3+ state, status bar, home, navigation, audio, calls, editors, Pi hardware I/O, and
  kiosk polish.

## License

MIT. See `Cargo.toml`.
