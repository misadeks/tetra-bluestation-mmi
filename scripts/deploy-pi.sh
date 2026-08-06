#!/usr/bin/env bash
#
# deploy-pi.sh - sync this repo to a Raspberry Pi, build it there, and run the
# kiosk straight to DRM/KMS via the Slint linuxkms backend.
#
# Building happens ON THE PI (native aarch64). This is the primary path because
# cross-compiling aarch64 from a Windows/x86 dev box needs a cross-linker + Pi
# sysroot; see README.md for the optional cross-compile route.
#
# Usage:
#   PI_HOST=tetra-ms.local PI_USER=pi ./scripts/deploy-pi.sh
#   ./scripts/deploy-pi.sh --host 192.168.1.42 --user pi
#
# Config (env vars, overridable by flags):
#   PI_HOST   Pi hostname/IP           (default: tetra-ms.local)
#   PI_USER   SSH user on the Pi       (default: pi)
#   PI_DIR    remote checkout dir      (default: ~/tetra-tn-ui)
#   NO_RUN    if set to 1, sync+build only, don't run
#
# Prereqs on the Pi (see README.md): build-essential pkg-config libasound2-dev
# libdrm-dev libgbm-dev libinput-dev libudev-dev libxkbcommon-dev, plus the Rust toolchain (rustup) and the DSI panel enabled via the
# vc4-kms-dsi-waveshare-panel-v2 overlay so /dev/dri/card* exists.
set -euo pipefail

PI_HOST="${PI_HOST:-tetra-ms.local}"
PI_USER="${PI_USER:-pi}"
PI_DIR="${PI_DIR:-~/tetra-tn-ui}"
NO_RUN="${NO_RUN:-0}"

while [[ $# -gt 0 ]]; do
  case "$1" in
    --host) PI_HOST="$2"; shift 2 ;;
    --user) PI_USER="$2"; shift 2 ;;
    --dir)  PI_DIR="$2";  shift 2 ;;
    --no-run) NO_RUN=1; shift ;;
    -h|--help) sed -n '2,25p' "$0"; exit 0 ;;
    *) echo "unknown arg: $1" >&2; exit 2 ;;
  esac
done

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
REMOTE="${PI_USER}@${PI_HOST}"

echo ">> syncing $REPO_ROOT -> ${REMOTE}:${PI_DIR}"
# Exclude build artifacts and never-committed codec libs; --delete keeps the Pi
# checkout in lockstep with the source tree.
if command -v rsync >/dev/null 2>&1; then
  rsync -az --delete \
    --exclude '/target' \
    --exclude '/.git' \
    --exclude '/.idea' \
    --exclude '/data' \
    --exclude '*.log' \
    "$REPO_ROOT/" "${REMOTE}:${PI_DIR}/"
else
  echo ">> rsync not found, falling back to scp (no --delete)"
  ssh "$REMOTE" "mkdir -p ${PI_DIR}"
  scp -r "$REPO_ROOT/." "${REMOTE}:${PI_DIR}/"
fi

echo ">> building on the Pi (cargo build --release)"
ssh "$REMOTE" "cd ${PI_DIR} && cargo build --release"

if [[ "$NO_RUN" == "1" ]]; then
  echo ">> built. skipping run (NO_RUN=1)."
  exit 0
fi

echo ">> running kiosk on the Pi (SLINT_BACKEND=linuxkms)"
# -t: allocate a TTY so the app can grab the DRM master / input and Ctrl-C works.
# WorkingDirectory must be the checkout so config.toml is found next to it.
ssh -t "$REMOTE" "cd ${PI_DIR} && sudo SLINT_BACKEND=linuxkms RUST_LOG=info ./target/release/tetra-tn-ui"
