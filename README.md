# TETRA BlueStation MMI

A native **Rust + Slint** touchscreen radio UI - the MMI (man-machine interface) for a
**tetra-bluestation** MS-mode TETRA terminal. It implements the **server side of the
BlueStation MS external interface** (`bluestation-ms-interface-2`) and presents the
operator a classic radio-style UI over it.

> The MS-mode radio stack this UI drives lives in
> **[misadeks/tetra-bluestation (ms-mode branch)](https://github.com/misadeks/tetra-bluestation/tree/ms-mode)**.

> **New here?** Follow the step-by-step **[Getting Started guide](docs/GETTING_STARTED.md)** -
> it walks you from a fresh machine to a running app on Windows, Linux, or the Pi.

The stack (or a simulator standing in for it) is the WebSocket **client** and dials
**out** to this app on two channels:

| Channel   | This app listens on | Subprotocol                | Traffic                               |
|-----------|---------------------|----------------------------|---------------------------------------|
| Control   | `9102`              | `bluestation-control-v1`   | UI to stack commands, stack to UI responses |
| Telemetry | `9101`              | `bluestation-telemetry-v1` | stack to UI events (receive-only)     |

Messages are **JSON encoded as UTF-8 inside _binary_ WebSocket frames**, using the
externally-tagged enum shape `{"Variant": {..}}`. This app does **not** reimplement any
TETRA stack, protocol, registration, or codec-negotiation logic - it drives the MS over
the fixed interface-2 wire contract. See `src/protocol.rs` for the serde mirror of the
wire types and the command builders.

Targets:
- **Raspberry Pi** (aarch64 embedded Linux) - deployment device.
- **Windows** - development/testing from RustRover.

## Repository layout

```
Cargo.toml        crate manifest (Slint, serde, tungstenite, crossbeam, ...)
build.rs          compiles ui/main.slint via slint-build
ui/main.slint     Slint UI (status bar + touch home + parked keypad layout)
src/main.rs       startup: config, window, spawn servers/timers, event loop
src/config.rs     config.toml parsing (channels, device models, [audio], [ui])
src/protocol.rs   serde mirror of the interface-2 wire types + command builders
src/net.rs        the two WebSocket servers (control + telemetry)
src/app.rs        central app state + the single UI-writer event loop
config.toml       runtime config (BlueStation MS listen ports, devices, [ui])
scripts/          deploy-pi.sh / deploy-pi.ps1 (build on Pi); cross/ + wsl/ (WSL cross-compile)
deploy/           tetra-bluestation-mmi.service (systemd kiosk autostart)
third_party/      git submodules (libtetra-acelp: the Rust ACELP speech codec)
```

## Prerequisites

- **Rust 1.95+** (`cargo` / `rustc` on PATH).
- **Windows:** the MSVC C++ build tools (Visual Studio 2022 Build Tools, C++ workload).
- **Linux / Pi:** a few system libraries (ALSA, fontconfig, DRM/KMS, input). The full
  lists are in the [Getting Started guide](docs/GETTING_STARTED.md) and [PI_SETUP.md](PI_SETUP.md).

### Speech codec (ACELP) - submodule + one-time populate

Two-way voice uses the **`tetra-acelp`** codec (a pure-Rust ETSI EN 300 395-2
implementation), vendored as a git submodule at `third_party/libtetra-acelp`. Its
ETSI numeric tables are copyright and **not** committed, so after cloning you
generate them once:

```bash
git submodule update --init            # if you didn't clone with --recurse-submodules
cd third_party/libtetra-acelp && cargo run -p populate && cd ../..
```

`populate` downloads the free ETSI reference archive (or pass a local `.zip`). Run
it once per checkout; the build fails early with this instruction if the tables are
missing. Details: [Getting Started](docs/GETTING_STARTED.md).

## Building and running

**Desktop (Windows / Linux):**

```bash
cargo run            # or: cargo build --release
```

A dark portrait window titled *"TETRA BlueStation MMI"* opens. It reads `config.toml`
from the working directory (built-in defaults if absent: control `9102`, telemetry
`9101`); set `RUST_LOG=debug` for verbose logs and run `cargo test` for the unit
tests. Full step-by-step: **[docs/GETTING_STARTED.md](docs/GETTING_STARTED.md)**.

**Raspberry Pi (DRM/KMS kiosk):** the Pi renders straight to a DSI panel via the
Slint **linuxkms** backend (no desktop) and can autostart as a systemd service. The
complete on-Pi checklist - panel overlay, packages, DRM access, WSL cross-compile,
autostart, and performance tuning - is in **[PI_SETUP.md](PI_SETUP.md)**.

## Developing with no radio hardware

Because this app is the **server**, any stack simulator that acts as the WebSocket
**client** can drive it - point the simulator's control and telemetry URLs at the ports
this app listens on (control `9102`, telemetry `9101`) and leave `[command].port` /
`[telemetry].port` in `config.toml` at their defaults so it connects.

On connect the app bootstraps (`GetInterfaceVersion` / `GetState` / `GetConfig`), polls
`GetState` every 2 s, and reflects telemetry live in the status bar and home screen.

## Configuration (`config.toml`)

| Section | Key | Meaning |
|---|---|---|
| `[command]` | `host` / `port` | Control-channel listen address (default `0.0.0.0:9102`). |
| `[telemetry]` | `host` / `port` | Telemetry-channel listen address (default `0.0.0.0:9101`). |
| `[command]` / `[telemetry]` | `use_tls` / `ca_cert` | `wss` + server cert PEM when TLS is enabled. |
| `[command]` / `[telemetry]` | `username` / `password` | HTTP Basic auth to accept; empty = accept all (demo). |
| `[registration]` | `registration_type` | Operator registration preference (identity comes from the MS, never configured). |
| `[audio]` | `enabled` | Enable the two-way ACELP voice path (bundled `tetra-acelp` submodule). |
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

## Features

- Two WebSocket servers (control `9102`, telemetry `9101`) implementing the interface-2
  wire contract, with bootstrap, periodic `GetState` polling, telemetry decode, and
  automatic reconnect.
- Live status bar and home screen driven by `MsRuntimeState`: registration and service
  state, signal strength, and a home talkgroup cycler with real names from the codeplug.
- Talkgroup handling from the codeplug: a searchable folders + talkgroups tree, separate
  scan lists, and attach/switch of the TX group.
- Dialer with private and group calls, a redial list, and a contacts list.
- Two-way ACELP voice with push-to-talk, threaded SDS messaging, and a master volume
  control. Actions that need the network are disabled while out of service.
- Touch and keypad layouts, selectable per device model.
- Raspberry Pi kiosk deployment: renders straight to DRM/KMS via the Slint linuxkms
  backend, with systemd autostart.

## License

This project is licensed under the **MIT License** - see the [`LICENSE`](LICENSE)
file for the full text.

### Third-party components

- **`tetra-acelp`** (git submodule at `third_party/libtetra-acelp`) - a pure-Rust
  implementation of the ETSI EN 300 395-2 TETRA ACELP speech codec, licensed
  `MIT OR Apache-2.0`. See the submodule's own `LICENSE`/`README` for details.
- **ETSI reference data tables** - the numeric tables the codec needs are ETSI
  copyright and are **not** distributed with this repo. They are downloaded and
  generated locally by the submodule's `populate` tool (see the codec section
  above) into a git-ignored file; nothing ETSI-copyrighted is committed here.
- Rust crate dependencies retain their own upstream licenses (see `Cargo.toml`
  / `cargo tree`).

