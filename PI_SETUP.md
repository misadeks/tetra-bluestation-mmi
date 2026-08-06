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

Once a release binary is on the Pi (default deploy dir `/home/pi/tetra-tn-ui`):

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
