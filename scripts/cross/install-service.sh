#!/usr/bin/env bash
#
# install-service.sh - deploy the release binary + config to the Pi and install
# the systemd kiosk unit so it autostarts on boot (and starts now). Mirrors the
# tetra-bluestation install-service-start.sh flow.
#
# Run from WSL/Linux after the cross-build (it builds first if the binary is
# missing). The unit runs the binary as root with SLINT_BACKEND=linuxkms and
# Restart=always, WorkingDirectory set to the deploy dir so it finds config.toml.
#
# Usage:
#   scripts/cross/install-service.sh            # build if needed, deploy, install, start
#   NO_START=1 scripts/cross/install-service.sh # install + enable, but don't start now
#
# Config comes from scripts/cross/pi.env / env vars (see common.sh):
#   PI_USER, PI_HOST, REMOTE_DIR, SERVICE_NAME, RUST_LOG.
set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=common.sh
source "${SCRIPT_DIR}/common.sh"

require_cmd ssh
require_cmd rsync

# Build first if we don't have a binary yet.
if [[ ! -x "${LOCAL_BINARY}" ]]; then
  echo "No binary at ${LOCAL_BINARY}; building it..."
  "${SCRIPT_DIR}/build-cross.sh"
fi

if [[ ! -f "${REPO_ROOT}/${CONFIG_FILE}" ]]; then
  echo "Missing ${REPO_ROOT}/${CONFIG_FILE}" >&2
  exit 1
fi

SERVICE_PATH="/etc/systemd/system/${SERVICE_NAME}"

echo "Preparing ${REMOTE}:${REMOTE_DIR}..."
ssh "${REMOTE}" "mkdir -p '${REMOTE_DIR}'"

# Stop the running service (if any) so the binary file isn't busy during copy.
ssh "${REMOTE}" "sudo systemctl stop '${SERVICE_NAME}' >/dev/null 2>&1 || true"

echo "Deploying binary + config..."
rsync -az "${LOCAL_BINARY}" "${REMOTE}:${REMOTE_DIR}/${BINARY_NAME}"
rsync -az "${REPO_ROOT}/${CONFIG_FILE}" "${REMOTE}:${REMOTE_DIR}/${CONFIG_FILE}"
ssh "${REMOTE}" "chmod +x '${REMOTE_DIR}/${BINARY_NAME}'"
# ACELP codec libs (gitignored, arch-specific) if present locally.
if [[ -d "${REPO_ROOT}/native" ]]; then
  rsync -az "${REPO_ROOT}/native/" "${REMOTE}:${REMOTE_DIR}/native/"
fi

# ALSA routing so "default" is the USB codec (dmix/dsnoop = shareable), keeping
# the UI's audio off the SX1255 SDR's I2S DMA. It's a managed file, so refresh it.
if [[ -f "${REPO_ROOT}/deploy/asound.conf" ]]; then
  rsync -az "${REPO_ROOT}/deploy/asound.conf" "${REMOTE}:/tmp/asound.conf"
  ssh "${REMOTE}" "sudo cp /tmp/asound.conf /etc/asound.conf"
fi

# Boot splash: show deploy/splash.png on the DSI panel from boot until the UI's
# first frame. We write the image straight to /dev/fb0 (converted to the panel's
# RGB565 framebuffer format by scripts/png_to_fb565.py, run on the Pi) - fbi is
# unreliable here because it renders on a VT and clears the screen when it exits.
# Best-effort: a failure here must not block the kiosk install.
if [[ -f "${REPO_ROOT}/deploy/splash.png" && -f "${REPO_ROOT}/scripts/png_to_fb565.py" ]]; then
  echo "Installing boot splash (raw /dev/fb0)..."
  ssh "${REMOTE}" "mkdir -p '${REMOTE_DIR}/deploy'"
  rsync -az "${REPO_ROOT}/deploy/splash.png" "${REMOTE}:${REMOTE_DIR}/deploy/splash.png"
  rsync -az "${REPO_ROOT}/scripts/png_to_fb565.py" "${REMOTE}:${REMOTE_DIR}/png_to_fb565.py"
  # Convert to the panel's framebuffer format on the Pi (reads /sys fb geometry).
  if ssh "${REMOTE}" "python3 '${REMOTE_DIR}/png_to_fb565.py' '${REMOTE_DIR}/deploy/splash.png' '${REMOTE_DIR}/deploy/splash.raw'"; then
    SPLASH_UNIT="$(cat <<EOF
[Unit]
Description=Boot splash on DSI panel (raw -> /dev/fb0)
Before=${SERVICE_NAME}
Conflicts=getty@tty1.service

[Service]
Type=oneshot
RemainAfterExit=yes
ExecStart=/bin/dd if=${REMOTE_DIR}/deploy/splash.raw of=/dev/fb0 bs=1843200 count=1 conv=notrunc status=none

[Install]
WantedBy=multi-user.target
EOF
)"
    printf '%s\n' "${SPLASH_UNIT}" | ssh "${REMOTE}" "sudo tee /etc/systemd/system/splash.service >/dev/null"
    ssh "${REMOTE}" "sudo systemctl daemon-reload && sudo systemctl enable splash.service >/dev/null 2>&1 || true"
    echo "  NOTE: for a clean splash, silence the boot console (keep console=tty1)."
    echo "  In /boot/firmware/cmdline.txt add: quiet loglevel=0 logo.nologo \\"
    echo "  vt.global_cursor_default=0 systemd.show_status=0 ; config.txt: disable_splash=1"
    echo "  (see PI_SETUP.md). cmdline changes need a reboot."
  else
    echo "  (warning: splash conversion failed; skipping splash)"
  fi
fi

echo "Installing ${SERVICE_NAME}..."
UNIT_CONTENT="$(cat <<EOF
[Unit]
Description=TETRA TN UI kiosk (Slint linuxkms / DRM-KMS)
After=network-online.target systemd-user-sessions.service getty@tty1.service
Wants=network-online.target
Conflicts=getty@tty1.service

[Service]
Type=simple
User=root
WorkingDirectory=${REMOTE_DIR}
Environment=SLINT_BACKEND=linuxkms
Environment=RUST_LOG=${RUST_LOG}
ExecStart=${REMOTE_DIR}/${BINARY_NAME}
TTYPath=/dev/tty1
TTYReset=yes
TTYVHangup=yes
StandardInput=tty
StandardOutput=journal
StandardError=journal
${CPU_AFFINITY:+CPUAffinity=${CPU_AFFINITY}}
Restart=always
RestartSec=2
StartLimitIntervalSec=60
StartLimitBurst=5

[Install]
WantedBy=multi-user.target
EOF
)"
printf '%s\n' "${UNIT_CONTENT}" | ssh "${REMOTE}" "sudo tee '${SERVICE_PATH}' >/dev/null"

# Kill any stray non-service instance (e.g. left by build-cross.sh --run) so it
# doesn't keep the DRM master or the 9101/9102 ports from the service.
ssh "${REMOTE}" "sudo pkill -x '${BINARY_NAME}' >/dev/null 2>&1 || true"

if [[ "${NO_START:-0}" == "1" ]]; then
  ssh "${REMOTE}" "sudo systemctl daemon-reload && sudo systemctl enable '${SERVICE_NAME}'"
  echo "Installed and enabled ${SERVICE_NAME} (not started; NO_START=1)."
else
  echo "Enabling + starting ${SERVICE_NAME}..."
  ssh "${REMOTE}" "
    set -e
    sudo systemctl daemon-reload
    sudo systemctl enable '${SERVICE_NAME}'
    sudo systemctl restart '${SERVICE_NAME}'
    sleep 3
    sudo systemctl --no-pager --full status '${SERVICE_NAME}' || true
  "
fi

cat <<EOF

Done. ${SERVICE_NAME} is installed and enabled (autostarts on boot).

Useful commands:
  ssh ${REMOTE} "sudo systemctl status ${SERVICE_NAME}"
  ssh ${REMOTE} "sudo journalctl -u ${SERVICE_NAME} -f"
  ssh ${REMOTE} "sudo systemctl restart ${SERVICE_NAME}"
  ssh ${REMOTE} "sudo systemctl stop ${SERVICE_NAME}"
EOF
