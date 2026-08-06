#!/usr/bin/env bash
#
# install-service.sh (WSL entry) - mirror the Windows checkout into WSL,
# cross-build if needed, then deploy + install the systemd kiosk service on the
# Pi so it autostarts on boot.
#
# From WSL:
#   cd /mnt/c/Users/mihaj/RustroverProjects/tetra-tn-ui
#   bash scripts/wsl/install-service.sh
#
# Set FORCE_SYNC=1 to re-pull the Pi sysroot first (after new -dev packages).
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

# shellcheck source=../cross/common.sh
source scripts/cross/common.sh
if [[ "${FORCE_SYNC:-0}" == "1" ]] || ! pi_sysroot_is_usable; then
  echo "Syncing the Pi sysroot into the WSL copy..."
  bash scripts/cross/sync-sysroot.sh
fi

bash scripts/cross/install-service.sh "$@"
