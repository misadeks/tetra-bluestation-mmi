# Raspberry Pi setup guide

Everything that must be done **on the Pi** to run the TETRA TN UI kiosk on a
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
exist). Check names with `aplay -l` / `arecord -l` or `./tetra-tn-ui
--list-audio`; if your codec's card name isn't `Device`, edit `deploy/asound.conf`.
Without a mic the engine runs downlink-only (you still hear voice, just can't
transmit).

---

## 7. Build + run

Pick one path.

### A. Cross-compile from WSL on your Windows dev box (recommended)

Nothing else to run on the Pi - just make sure steps 1-3 are done and SSH works.
From the Windows checkout, see the README "Cross-compile from Windows via WSL"
section. In short (in WSL):

```bash
bash scripts/cross/sync-sysroot.sh          # once (and after new apt -dev pkgs)
bash scripts/wsl/build-deploy-run.sh        # sync -> cross-build -> deploy -> run
```

### B. Build on the Pi

Copy the source over and build natively (needs step 4). From your dev box:

```bash
PI_HOST=tetra-ms.local PI_USER=pi ./scripts/deploy-pi.sh
```

or, directly on the Pi in the checkout:

```bash
cargo build --release
sudo SLINT_BACKEND=linuxkms RUST_LOG=info ./target/release/tetra-tn-ui
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
sudo cp deploy/tetra-tn-ui.service /etc/systemd/system/tetra-tn-ui.service
# edit WorkingDirectory/ExecStart if your dir differs
sudo systemctl daemon-reload
sudo systemctl enable --now tetra-tn-ui.service
journalctl -u tetra-tn-ui -f            # follow logs
```

The unit runs the binary as root with `SLINT_BACKEND=linuxkms` and
`Restart=always`.

---

## 8b. Boot splash (optional)

Show a logo on the DSI panel from early boot until the UI's first frame,
instead of a blank screen or scrolling kernel logs. For most of the wait the
app isn't running yet, so the splash is drawn at the framebuffer level with
`fbi`: it paints `deploy/splash.png` onto `/dev/fb0` (the fbdev emulation that
`vc4-kms-v3d` exposes) and stays up until the kiosk's DRM modeset scans out its
own frame over it - an automatic handoff.

The `install-service` scripts handle the wiring: they install `fbi` and enable
`splash.service`, and deploy `deploy/splash.png` **if it exists locally**. The
image itself is not committed (PNGs are gitignored) - drop your own logo at
`deploy/splash.png` (native panel size, 720x1280 portrait) before deploying,
or the splash step is simply skipped.

**Silence the boot console** so the panel is clean behind the splash (not
scrolling logs). Append to the single line in `/boot/firmware/cmdline.txt`:

```
quiet loglevel=0 logo.nologo vt.global_cursor_default=0 console=tty3
```

`console=tty3` moves kernel/login text to a hidden VT; the kiosk still owns
tty1. And in `/boot/firmware/config.txt` add `disable_splash=1` to drop the
rainbow square.

**Manual install** (if not using the deploy script):

```bash
sudo apt install -y fbi
sudo cp deploy/splash.service /etc/systemd/system/splash.service
# edit the splash.png path in the unit if your dir isn't /home/pi/tetra-tn-ui
sudo systemctl daemon-reload
sudo systemctl enable splash.service      # shows on next boot
```

---

## 9. Performance (CPU pinning, throttling)

On a Pi shared with the TETRA radio/stack, keep the UI off the radio's cores:

- The installed service pins the UI with `CPUAffinity=0 1`; keep the radio on
  cores 2-3 (its own `CPUAffinity` or kernel `isolcpus`). Tune via `CPU_AFFINITY`
  in `scripts/cross/pi.env`.
- For a manual run: `sudo SLINT_BACKEND=linuxkms taskset -c 0-1 ./tetra-tn-ui`.
- Check for under-voltage/thermal throttling (a common cause of a sluggish UI):

  ```bash
  vcgencmd get_throttled     # 0x0 = healthy; non-zero = check PSU/cooling
  ```

- To offload drawing to the GPU, switch the aarch64 build from `renderer-software`
  to `renderer-femtovg` (see README "Performance" - needs `libgles2-mesa-dev
  libegl-dev` on the Pi + a sysroot re-sync).

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
