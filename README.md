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
Cargo.toml        crate manifest (Slint, serde, tungstenite, crossbeam, ...)
build.rs          compiles ui/main.slint via slint-build
ui/main.slint     Slint UI (status bar + touch home + parked keypad layout)
src/main.rs       startup: config, window, spawn servers/timers, event loop
src/config.rs     config.toml parsing (channels, device models, [audio], [ui])
src/protocol.rs   serde mirror of the interface-2 wire types + command builders
src/net.rs        the two WebSocket servers (control + telemetry)
src/app.rs        central app state + the single UI-writer event loop
config.toml       runtime config (BlueStation MS listen ports, devices, [ui])
scripts/          deploy-pi.sh / deploy-pi.ps1 (sync + build + run on the Pi)
deploy/           tetra-tn-ui.service (systemd kiosk autostart)
DECISIONS.md      running log of decisions and deviations
```

## Prerequisites

- **Rust 1.95** or later (`cargo` / `rustc` on PATH).
- On Windows: the **MSVC** toolchain (Visual Studio 2022 Build Tools, C++ workload).
- For Pi cross-builds: the `aarch64-unknown-linux-gnu` target plus a cross linker, or
  build on the Pi directly (the supported path - see *Build and run - Raspberry Pi*).

No external native dependency setup is needed for Windows dev; Slint fetches and builds
its renderer stack through cargo. The Pi (linuxkms) build needs a few apt packages -
see the Raspberry Pi section below.

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

## Build and run - Raspberry Pi (aarch64 Linux, DRM/KMS kiosk)

The Pi is a **Raspberry Pi 4** running **Raspberry Pi OS Lite** (no desktop / X /
Wayland) driving a **Waveshare 5" DSI panel** (5-DSI-TOUCH-A, native portrait
720x1280). The panel is enabled in `/boot/firmware/config.txt` via
`dtoverlay=vc4-kms-dsi-waveshare-panel-v2`, so the Pi exposes it as a DRM/KMS
device (`/dev/dri/card*`). The app renders **straight to DRM/KMS** using the
Slint **linuxkms** backend - no compositor involved.

### apt prerequisites (on the Pi)

```bash
sudo apt update
sudo apt install build-essential pkg-config \
  libasound2-dev libdrm-dev libgbm-dev libinput-dev libudev-dev libxkbcommon-dev
```

- `build-essential` + `pkg-config` - C toolchain/linker and lib discovery.
- `libasound2-dev` - ALSA headers for the `cpal` audio dependency.
- `libdrm-dev` / `libgbm-dev` - DRM/KMS output for the linuxkms backend.
- `libinput-dev` / `libudev-dev` - touch/keyboard input via libinput + udev.
- `libxkbcommon-dev` - keymap handling for the linuxkms backend.

Install Rust with [rustup](https://rustup.rs) (>= 1.95).

### The linuxkms backend

The Pi build is target-scoped in `Cargo.toml`: for `aarch64` Linux, Slint is
pulled in with `default-features = false` plus `backend-linuxkms-noseat` and
`renderer-software`, so the winit backend is dropped and the binary renders to
DRM/KMS. `-noseat` skips libseat/logind so it runs from a plain systemd service
or `sudo` with no seat manager. The software renderer is used for a reliable
first bring-up; FemtoVG/Skia GPU rendering can be enabled later (swap
`renderer-software` for `renderer-femtovg` and add the matching GL/EGL packages).
Windows/x86 dev is unaffected and keeps the default winit backend.

### Build and run on the Pi

Build natively on the Pi (the primary path - see cross-compiling below):

```bash
cargo build --release
sudo SLINT_BACKEND=linuxkms RUST_LOG=info ./target/release/tetra-tn-ui
```

`sudo` is used so the process can become DRM master and open `/dev/dri/card*`
and `/dev/input/*`. Alternatively add your user to the `render`, `video`, and
`input` groups. Run from the checkout so it finds `config.toml` (which already
selects `model = "pi-720x1280"`).

### Deploy from your dev box

`scripts/deploy-pi.sh` (bash) and `scripts/deploy-pi.ps1` (PowerShell, for
RustRover on Windows) sync the source to the Pi, run `cargo build --release`
there, and launch it with `SLINT_BACKEND=linuxkms`. Host/user are parameterized:

```bash
PI_HOST=tetra-ms.local PI_USER=pi ./scripts/deploy-pi.sh
```

```powershell
./scripts/deploy-pi.ps1 -PiHost tetra-ms.local -PiUser pi
```

RustRover ships two shared run configs under `.idea/runConfigurations/`:
**Run (Windows dev)** (local `cargo run`, winit) and **Deploy + run on Pi
(linuxkms)** (invokes the PowerShell deploy script).

### Kiosk autostart (systemd)

`deploy/tetra-tn-ui.service` autostarts the kiosk on boot with
`SLINT_BACKEND=linuxkms` and `Restart=always`:

```bash
sudo cp deploy/tetra-tn-ui.service /etc/systemd/system/tetra-tn-ui.service
# edit WorkingDirectory/ExecStart if your checkout isn't /home/pi/tetra-tn-ui
sudo systemctl daemon-reload
sudo systemctl enable --now tetra-tn-ui.service
journalctl -u tetra-tn-ui -f
```

### Optional: cross-compile from Windows/x86

Building on the Pi is the supported path. To cross-compile instead you need the
aarch64 target plus a cross-linker and a Pi sysroot for `libasound`/`libdrm`
(e.g. via [`cross`](https://github.com/cross-rs/cross) or a configured
`aarch64-linux-gnu-gcc` linker in `.cargo/config.toml`):

```bash
rustup target add aarch64-unknown-linux-gnu
cargo build --release --target aarch64-unknown-linux-gnu
```

For dev on a Pi that *does* run X/Wayland you can also run under the default
winit backend by not setting `SLINT_BACKEND`.

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
exercise reconnect handling. On connect the app bootstraps
(GetInterfaceVersion/GetState/GetConfig), polls GetState every 2s, and reflects
telemetry live in the status bar and home screen.

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

Current: **M4 (Pi touch look) + codeplug.** The touch UI is the web-UI device
frame - status bar, header, a home talkgroup **cycler** (real names from the
codeplug, folder selector, prev/next arrows), a Select-talkgroup action, a docked
PTT, a 3-column softkey bar, and a Radio Info screen - all styled from the browser
tetra-tn-web-ui CSS and driven by live MsRuntimeState. The two WebSocket servers,
protocol layer, bootstrap + 2s polling, telemetry decode, and reconnect are
underneath. Tested against `fake_stack.py` and the real BlueStation MS stack. See
`DECISIONS.md`.

- M1 toolchain spike (done).
- M2 net + JSON (done).
- M3 state + status bar + home (done).
- M4 home look + navigation (done for touch/Pi): device frame, talkgroup cycler,
  PTT dock, softkeys, Radio Info.
- M4b codeplug parsing (done): real talkgroup names, folder selector + cycling,
  Select-talkgroup (attach/switch TX).
- M5+ audio, calls (M6), codeplug editor (M7), Pi hardware I/O (M8), kiosk polish.

## License

MIT. See `Cargo.toml`.
