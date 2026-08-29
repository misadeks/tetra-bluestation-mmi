# Raspberry Pi setup guide

Everything that must be done **on the Pi** to run the TETRA BlueStation MMI kiosk on a
Raspberry Pi 4 + Waveshare 5" DSI panel (5-DSI-TOUCH-A, native portrait
720x1280), rendering straight to DRM/KMS with the Slint **linuxkms** backend.
No desktop / X / Wayland is required (Raspberry Pi OS Lite).

Do the steps in order. Steps 1-6 are one-time. Whether you build on the Pi or
cross-compile from WSL, the Pi always needs the apt packages in step 3.

---

## 0. Base image

- Flash **Raspberry Pi OS Lite (64-bit)** (Bookworm, aarch64).
- First-boot config (Raspberry Pi Imager or `raspi-config`):
  - **Hostname:** `tetra-ms`  -> reachable as `tetra-ms.local` (the deploy
    default). Change `PI_HOST` in `scripts/cross/pi.env` if you use another name.
  - **User:** `pi` (the deploy default; change `PI_USER` otherwise).
  - **Enable SSH.**
  - Configure Wi-Fi / Ethernet so the Pi is on the network.

`.local` name resolution uses avahi/mDNS, which Raspberry Pi OS enables by
default. If `tetra-ms.local` doesn't resolve from your dev box, use the Pi's IP
address instead.

---

## 1. Enable the DSI panel (DRM/KMS)

Edit the firmware config (`/boot/firmware/config.txt` on Bookworm; older images
use `/boot/config.txt`):

```bash
sudo nano /boot/firmware/config.txt
```

Ensure these lines are present:

```ini
# Full KMS driver (default on Bookworm)
dtoverlay=vc4-kms-v3d

# Waveshare 5" DSI panel
dtoverlay=vc4-kms-dsi-waveshare-panel-v2
```

Reboot:

```bash
sudo reboot
```

---

## 2. Verify the panel is a DRM/KMS device

After reboot, confirm the GPU exposes a card and the panel connector is
connected:

```bash
ls -l /dev/dri/            # expect card0 (or card1) and renderD128
sudo apt install -y libdrm-tests   # optional, provides modetest
modetest -c 2>/dev/null | grep -i -E 'DSI|connected'   # optional
```

If `/dev/dri/card*` is missing, the overlay didn't load - recheck step 1.

---

## 3. Install build/run prerequisites (always required)

```bash
sudo apt update
sudo apt install -y \
  build-essential pkg-config \
  libasound2-dev libdrm-dev libgbm-dev libinput-dev libudev-dev libxkbcommon-dev \
  libfontconfig1-dev fonts-dejavu-core \
  libegl-dev libgles-dev libgl1-mesa-dri \
  rsync openssh-server
```

Why each package:

| Package | Needed for |
|---|---|
| `build-essential`, `pkg-config` | C toolchain/linker + library discovery |
| `libasound2-dev` | ALSA headers for the `cpal` audio dependency |
| `libdrm-dev`, `libgbm-dev` | DRM/KMS output for the linuxkms backend |
| `libinput-dev`, `libudev-dev` | touch/keyboard input via libinput + udev |
| `libxkbcommon-dev` | keymap handling for the linuxkms backend |
| `libfontconfig1-dev` | font discovery (Slint links fontconfig on Linux) |
| `fonts-dejavu-core` | an actual font so text renders (Lite ships none) |
| `rsync`, `openssh-server` | deploy + sysroot sync from your dev box |

> These `-dev` packages must be installed **before** you run the WSL
> `sync-sysroot.sh`, so their headers and `.pc` files are copied into the
> cross-compile sysroot.

---

## 4. Install Rust (only if you build ON the Pi)

Skip this if you cross-compile from WSL (the recommended fast path) - the binary
is built off-device and copied over.

```bash
curl https://sh.rustup.rs -sSf | sh -s -- -y
source "$HOME/.cargo/env"
rustc --version    # expect >= 1.95
```

---

## 5. DRM access (run as root, or add groups)

The kiosk needs to become DRM master and open `/dev/dri/card*` and
`/dev/input/*`. Two options:

- **Simplest:** run it with `sudo` (the deploy scripts and the systemd unit do
  this). Nothing to configure.
- **Rootless:** add your user to the device groups and log back in:

  ```bash
  sudo usermod -aG render,video,input pi
  ```

  On a seatless system DRM master usually still wants root, so the systemd unit
  defaults to `User=root`.

---

## 6. ACELP codec libraries (only if you want two-way voice)

`config.toml` has `[audio].enabled = true` and `codec_dir = 'native'`. The ETSI
ACELP codec libraries are **copyrighted and never committed**. If you want audio,
place the aarch64 Linux builds on the Pi next to the binary:

```
<checkout-or-deploy-dir>/native/libtetra_acelp.so
<checkout-or-deploy-dir>/native/libtetra_acelp_enc.so
```

Without them, set `[audio].enabled = false` in `config.toml` (the UI still runs).

### Audio device: use a USB codec, not onboard audio

The UI's audio (ringtones + voice) must not use the Pi's onboard audio, whose DMA
contends with the SX1255 SDR's I2S and blocks registration. Use a **USB audio
codec** and route ALSA's `default` to it (full duplex):

```bash
sudo cp deploy/asound.conf /etc/asound.conf   # routes default -> USB card "Device"
# keep config.toml [audio].output_device / input_device = "default"
```

`install-service` does this automatically (if `/etc/asound.conf` doesn't already
exist). Check names with `aplay -l` / `arecord -l` or `./tetra-bluestation-mmi
--list-audio`; if your codec's card name isn't `Device`, edit `deploy/asound.conf`.
Without a mic the engine runs downlink-only (you still hear voice, just can't
transmit).

---

## 7. Build + run

Pick one path.

### How the Pi build is wired (linuxkms)

For `aarch64` Linux, `Cargo.toml` pulls Slint with `default-features = false` plus
`backend-linuxkms-noseat`, `renderer-femtovg`, and `renderer-software`: the winit
backend is dropped and the binary renders straight to DRM/KMS. `-noseat` skips
libseat/logind so it runs from a plain systemd service or `sudo`. It renders on the
**V3D GPU via FemtoVG** (OpenGL ES over EGL/GBM) and falls back to the software
renderer automatically if a GL context can't be created. Desktop (Windows/x86)
builds are unaffected and keep the default winit backend.

### A. Cross-compile from WSL on your Windows dev box (recommended)

Nothing else to run on the Pi - just make sure steps 1-3 are done and SSH works.
Cross-compiling runs on x86 with the `aarch64` cross-linker (`.cargo/config.toml`),
linking against a **sysroot rsynced from the Pi** so the linuxkms/audio C libraries
(libdrm, libinput, libxkbcommon, libudev, libasound) resolve at link time. The
sysroot is mandatory - a bare cross-linker can't find those libs.

One-time WSL setup (Debian/Ubuntu):

```bash
sudo apt update
sudo apt install -y build-essential pkg-config rsync openssh-client gcc-aarch64-linux-gnu
rustup target add aarch64-unknown-linux-gnu
```

Then, from the Windows checkout inside WSL:

```bash
cd /mnt/c/Users/<you>/RustroverProjects/tetra-bluestation-mmi
cp scripts/cross/pi.env.example scripts/cross/pi.env   # edit PI_HOST/PI_USER
bash scripts/cross/sync-sysroot.sh          # once, and after apt-installing new -dev pkgs on the Pi
bash scripts/wsl/build-deploy-run.sh        # sync -> cross-build -> deploy -> run
```

`scripts/cross/build-cross.sh` also works standalone (`--deploy` / `--run`). The
WSL wrapper reuses an existing sysroot; after you `apt install` a new `-dev`
package on the Pi, re-pull it with `-Sync` from RustRover/PowerShell
(`scripts/wsl/build-deploy-run.ps1 -Sync`) or `FORCE_SYNC=1 bash
scripts/wsl/build-deploy-run.sh` in WSL (or delete `.pi-sysroot/`). A Windows
`$env:FORCE_SYNC` does **not** reach WSL - use `-Sync`. `sync-sysroot.sh` needs the
Pi to already have the step-3 apt packages so their headers + pkg-config files come
across. The build guards against linking a binary that needs a newer glibc than the
Pi provides.

The plain [`cross`](https://github.com/cross-rs/cross) (Docker) route also works but
needs a custom image with the `arm64` dpkg architecture and `*-dev:arm64` packages -
more setup than the sysroot path, so it isn't wired up here.

### B. Build on the Pi

Copy the source over and build natively (needs step 4). From your dev box:

```bash
PI_HOST=tetra-ms.local PI_USER=pi ./scripts/deploy-pi.sh
```

or, directly on the Pi in the checkout:

```bash
cargo build --release
sudo SLINT_BACKEND=linuxkms RUST_LOG=info ./target/release/tetra-bluestation-mmi
```

The app reads `config.toml` from its working directory, so run it from the
checkout / deploy directory.

---

## 8. Kiosk autostart (systemd)

**Easiest (from your dev box):** the install script cross-builds if needed,
deploys the binary + `config.toml`, and installs + enables + starts the service:

```powershell
./scripts/wsl/install-service.ps1        # RustRover/Windows (add -Sync after new Pi -dev pkgs)
```
```bash
bash scripts/wsl/install-service.sh      # from WSL; NO_START=1 to not start now
```

**Manual**, once a release binary is on the Pi (default dir `/home/pi/tetra-tn-ui`):

```bash
sudo cp deploy/tetra-bluestation-mmi.service /etc/systemd/system/tetra-bluestation-mmi.service
# edit WorkingDirectory/ExecStart if your dir differs
sudo systemctl daemon-reload
sudo systemctl enable --now tetra-bluestation-mmi.service
journalctl -u tetra-bluestation-mmi -f            # follow logs
```

The unit runs the binary as root with `SLINT_BACKEND=linuxkms` and
`Restart=always`.

---

## 8b. Boot splash (optional)

Show a logo on the DSI panel from boot until the UI's first frame, instead of a
blank screen or scrolling kernel logs. For most of the wait the app isn't
running yet, so the splash is drawn at the framebuffer level.

We write the image **straight to `/dev/fb0`** (not via `fbi`). `fbi` renders on a
virtual terminal, so it's only visible when its VT is the foreground console and
it clears the screen to black when it exits (~1 s after painting, under
systemd) — which is why an `fbi` splash tends to flash and vanish. A raw
`/dev/fb0` write shows regardless of the foreground VT and stays in the
framebuffer until the UI's DRM modeset scans out over it — a clean handoff.

`scripts/png_to_fb565.py` (stdlib-only; the Pi has `python3` but no
Pillow/ImageMagick) converts `deploy/splash.png` to the panel's exact
framebuffer format (RGB565, read from `/sys/class/graphics/fb0`), producing
`deploy/splash.raw`. `splash.service` then `dd`s that onto `/dev/fb0`.

The `install-service` scripts handle all of this: they deploy `deploy/splash.png`
**if it exists locally**, run the converter on the Pi, and install + enable
`splash.service`. The image is not committed (PNGs are gitignored) — drop your
own logo at `deploy/splash.png` (native panel size, 720x1280 portrait) before
deploying, or the splash step is skipped.

**Silence the boot console — REQUIRED.** Keep the panel's console on `tty1` (so
the panel shows that VT) but suppress its output, otherwise kernel/systemd text
shows behind the splash. In the single line of `/boot/firmware/cmdline.txt`
**keep `console=tty1`** and append:

```
quiet loglevel=0 logo.nologo vt.global_cursor_default=0 systemd.show_status=0
```

Then in `/boot/firmware/config.txt` add `disable_splash=1` (drops the rainbow
square). Reboot to apply — cmdline changes only take effect on boot. (Do **not**
move the console to `tty3`: the panel would then show a different, empty VT and
the boot text is already silenced by the flags above.)

The `splash.service` is `oneshot` + `RemainAfterExit` with `Conflicts=getty@tty1`:
`dd` writes the image and exits, the framebuffer keeps it, and the unit stays
active (holding tty1 off the login getty) until the UI's modeset takes over.

**Manual install** (if not using the deploy script):

```bash
python3 scripts/png_to_fb565.py deploy/splash.png deploy/splash.raw   # run on the Pi
sudo cp deploy/splash.service /etc/systemd/system/splash.service
# edit the splash.raw path in the unit if your dir isn't /home/pi/tetra-tn-ui
sudo systemctl daemon-reload
sudo systemctl enable splash.service      # shows on next boot
```

---

## 9. Performance (CPU pinning, throttling)

On a Pi shared with the TETRA radio/stack, keep the UI off the radio's cores:

- The installed service pins the UI with `CPUAffinity=0 1`; keep the radio on
  cores 2-3 (its own `CPUAffinity` or kernel `isolcpus`). Tune via `CPU_AFFINITY`
  in `scripts/cross/pi.env`.
- For a manual run: `sudo SLINT_BACKEND=linuxkms taskset -c 0-1 ./tetra-bluestation-mmi`.
- Check for under-voltage/thermal throttling (a common cause of a sluggish UI):

  ```bash
  vcgencmd get_throttled     # 0x0 = healthy; non-zero = check PSU/cooling
  ```

- To offload drawing to the GPU, switch the aarch64 build from `renderer-software`
  to `renderer-femtovg` (see README "Performance" - needs `libgles2-mesa-dev
  libegl-dev` on the Pi + a sysroot re-sync).

---

## 10. Landscape orientation (rotated display + touch)

The Waveshare 5" DSI panel is **natively portrait (720x1280)**. To run the UI in
**landscape**, select the landscape model in `config.toml`:

```toml
[ui]
model = "pi-1280x720"     # 1280x720 landscape; rotation defaults to 90
# rotation = 270          # uncomment/flip if the image is upside down for your mount
```

The `pi-1280x720` model carries `rotation = 90`, which the app passes to the
linuxkms backend via `SLINT_KMS_ROTATION`. That rotates the 1280x720 UI a quarter
turn so it fills the physical panel instead of being clipped. Without it the
window is drawn at 1280x720 onto the 720x1280 panel, so it looks portrait and the
right side is cut off. If landscape comes out upside down, set `rotation = 270`.

### Touch calibration for the rotated panel

`SLINT_KMS_ROTATION` rotates the **image only** - the touchscreen still reports
coordinates in the panel's native (portrait) orientation, so taps land in the
wrong place. Fix it with a libinput calibration matrix via a udev rule on the Pi:

```bash
# 90 rotation (matches [ui].rotation = 90). Use the 270 matrix instead if you
# set rotation = 270. Applies to any touchscreen (ID_INPUT_TOUCHSCREEN).
sudo tee /etc/udev/rules.d/99-touch-rotate.rules >/dev/null <<'EOF'
# 90 clockwise:
ENV{ID_INPUT_TOUCHSCREEN}=="1", ENV{LIBINPUT_CALIBRATION_MATRIX}="0 -1 1 1 0 0"
# 270 clockwise (90 counter-clockwise): use instead of the line above
# ENV{ID_INPUT_TOUCHSCREEN}=="1", ENV{LIBINPUT_CALIBRATION_MATRIX}="0 1 0 -1 0 1"
# 180 (upside down): "-1 0 1 0 -1 1"
EOF
sudo udevadm control --reload-rules
sudo reboot
```

After reboot the display and touch are both landscape. To go back to portrait,
use `model = "pi-720x1280"` (rotation 0) and remove the udev rule.

---

## Quick copy-paste (steps 1-5, fresh Pi)

```bash
# 1. panel overlay
echo 'dtoverlay=vc4-kms-dsi-waveshare-panel-v2' | sudo tee -a /boot/firmware/config.txt

# 3. prerequisites
sudo apt update
sudo apt install -y build-essential pkg-config \
  libasound2-dev libdrm-dev libgbm-dev libinput-dev libudev-dev libxkbcommon-dev \
  libfontconfig1-dev fonts-dejavu-core rsync openssh-server

# 4. Rust (only if building on the Pi)
curl https://sh.rustup.rs -sSf | sh -s -- -y && source "$HOME/.cargo/env"

# 5. optional rootless DRM
sudo usermod -aG render,video,input "$USER"

sudo reboot
```

After reboot, verify `ls /dev/dri/card*` shows a device, then proceed to build
(step 7).
