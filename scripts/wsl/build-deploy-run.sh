#!/usr/bin/env bash
#
# build-deploy-run.sh - one-shot WSL entry point: mirror the Windows checkout
# into WSL, cross-compile for the Pi against the synced sysroot, then deploy and
# run it on the Pi with the linuxkms backend.
#
# From WSL:
#   cd /mnt/c/Users/mihaj/RustroverProjects/tetra-tn-ui
#   bash scripts/wsl/build-deploy-run.sh
#
# The Pi sysroot must exist first (once):  scripts/cross/sync-sysroot.sh
# Pass extra args straight through to build-cross.sh (default here is --run).
set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"

for cmd in rsync cargo rustup ssh; do
  command -v "${cmd}" >/dev/null 2>&1 || { echo "Missing in WSL: ${cmd}" >&2; exit 1; }
done

WSL_REPO_DIR="$("${SCRIPT_DIR}/sync-to-wsl.sh" | tail -n 1)"
cd "${WSL_REPO_DIR}"

if [[ ! -f scripts/cross/pi.env && -f scripts/cross/pi.env.example ]]; then
  cp scripts/cross/pi.env.example scripts/cross/pi.env
  echo "Created ${WSL_REPO_DIR}/scripts/cross/pi.env from example - edit if your Pi differs."
fi

# Sync the sysroot into the WSL copy if it isn't there yet.
if [[ ! -d .pi-sysroot ]]; then
  echo "No sysroot in the WSL copy; syncing it from the Pi..."
  bash scripts/cross/sync-sysroot.sh
fi

ARGS=("$@")
[[ ${#ARGS[@]} -eq 0 ]] && ARGS=("--run")
bash scripts/cross/build-cross.sh "${ARGS[@]}"
