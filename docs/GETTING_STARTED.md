# Getting Started

A step-by-step, copy-paste guide to build and run **TETRA BlueStation MMI** from
scratch. If you follow this top to bottom you'll have it running. No prior Rust
experience needed.

---

## 0. What this is (30 seconds)

This app is the **operator screen (MMI)** for a `tetra-bluestation` TETRA radio.
It talks to the radio stack over two local WebSocket connections (the stack
connects *to* this app). You don't need the radio to try the UI - a simulator can
stand in for it (see step 6).

---

## 1. Install the tools

You need **Git** and **Rust** (1.95 or newer). Pick your OS:

### Windows

1. Install **Git**: https://git-scm.com/download/win (accept the defaults).
2. Install the **MSVC C++ build tools**: get "Visual Studio 2022 Build Tools"
   from https://visualstudio.microsoft.com/downloads/ and, in the installer,
   tick **"Desktop development with C++"**.
3. Install **Rust**: https://rustup.rs -> run the installer, choose the default.
4. Close and reopen your terminal so `cargo` is on the PATH. Check:
   ```powershell
   cargo --version
   ```

### Linux (Ubuntu/Debian, x64 or arm64)

```bash
# Git + Rust
sudo apt update && sudo apt install -y git curl build-essential pkg-config
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh   # accept defaults
source "$HOME/.cargo/env"

# System libraries the UI + audio need
sudo apt install -y \
  libasound2-dev libfontconfig1-dev libxkbcommon-dev \
  libdrm-dev libgbm-dev libinput-dev libudev-dev \
  libegl-dev libgles-dev libgl1-mesa-dri \
  libxcb1-dev libxcb-render0-dev libxcb-shape0-dev libxcb-xfixes0-dev \
  libwayland-dev
```

### Raspberry Pi (kiosk on a DSI panel)

The Pi has extra steps (panel overlay, DRM access, autostart). Do steps 2-3 here
first to get the code and codec, then follow the dedicated **[PI_SETUP.md](../PI_SETUP.md)**.

---

## 2. Get the code (with the codec submodule)

The speech codec lives in a **git submodule**, so clone with `--recurse-submodules`:

```bash
git clone --recurse-submodules https://github.com/misadeks/tetra-bluestation-mmi.git
cd tetra-bluestation-mmi
```

Already cloned without submodules? Pull them in:

```bash
git submodule update --init --recursive
```

**How to tell it worked:** the folder `third_party/libtetra-acelp/` should NOT be
empty (it should contain `Cargo.toml`, `src/`, etc.).

---

## 3. Generate the speech-codec tables (one time)

The codec needs ETSI reference tables that are **copyright and not shipped** with
the repo. Generate them once - this downloads a free archive from ETSI and writes
a local (git-ignored) `tables.rs`:

```bash
cd third_party/libtetra-acelp
cargo run -p populate
cd ../..
```

- This needs **internet access** the first time.
- Have the archive already? `cargo run -p populate -- path/to/en_30039502v010301p0.zip`
- You only do this **once per checkout**. The generated file persists.
- If you skip it, the build stops early with a message telling you to run this.

---

## 4. Build and run

```bash
cargo run
```

The first build downloads and compiles dependencies (several minutes - normal).
When it's done a dark portrait window titled **"TETRA BlueStation MMI"** opens.

- For a faster, optimized binary: `cargo build --release`
  (it lands in `target/release/tetra-bluestation-mmi[.exe]`).
- More logs: set `RUST_LOG=debug` before running.

Run the tests to sanity-check your setup:

```bash
cargo test
```

---

## 5. Configure (optional)

Settings live in `config.toml` next to the binary. The defaults work out of the
box. The ones you're most likely to touch:

| Setting | What it does | Default |
|---|---|---|
| `[command].port` | Control-channel port the stack connects to | `9102` |
| `[telemetry].port` | Telemetry-channel port | `9101` |
| `[audio].enabled` | Two-way voice on/off | `true` |
| `[ui].model` | Device screen model (e.g. `pi-720x1280`) | selected in file |

Full reference: see the **Configuration** table in [README.md](../README.md).

---

## 6. Try it without a radio

This app is the **server**; the radio stack is the **client** that connects in.
To exercise the UI without hardware, run any stack **simulator** that acts as the
WebSocket client and point it at this app's ports:

- Control: `ws://127.0.0.1:9102`
- Telemetry: `ws://127.0.0.1:9101`

Leave `[command].port` / `[telemetry].port` at their defaults so the simulator
connects. On connect, the app bootstraps and starts reflecting live state in the
status bar and home screen.

---

## 7. Deploy to a Raspberry Pi

The Pi renders straight to the screen (DRM/KMS, no desktop) and can autostart as a
systemd service. Everything Pi-specific - panel overlay, packages, DRM
permissions, autostart, cross-compiling from your PC - is in **[PI_SETUP.md](../PI_SETUP.md)**.

Quick version once the code + codec tables are on the Pi:

```bash
cargo build --release
sudo SLINT_BACKEND=linuxkms RUST_LOG=info ./target/release/tetra-bluestation-mmi
```

---

## Troubleshooting

**"tables.rs missing" / build fails in `tetra-acelp`**
You skipped step 3. Run `cargo run -p populate` inside `third_party/libtetra-acelp`.

**`third_party/libtetra-acelp` is empty**
You cloned without submodules. Run `git submodule update --init --recursive`.

**Linux build fails with `pkg-config`/`.pc not found` or missing `-l` libraries**
Install the system libraries from step 1 (Linux). The error usually names the
missing package (e.g. `alsa`, `fontconfig`, `libdrm`).

**Windows: `link.exe` not found / linker errors**
The MSVC C++ build tools aren't installed - redo step 1 (Windows) and tick
"Desktop development with C++".

**No sound / no microphone**
Voice needs a working audio device. On the Pi see the audio section of
[PI_SETUP.md](../PI_SETUP.md). Missing a mic just disables uplink - you still hear
incoming audio.

**Pi: "presenting framebuffer: Permission denied"**
The linuxkms backend needs a VT / DRM master - run under the provided systemd unit
or with `sudo`, and make sure nothing else owns tty1. See [PI_SETUP.md](../PI_SETUP.md).
