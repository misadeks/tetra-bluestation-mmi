#!/usr/bin/env bash
#
# sync-sysroot.sh - rsync the Pi's /usr/include, /usr/lib and /lib into a local
# sysroot so the cross-linker can resolve the linuxkms + audio C libraries.
#
# Run this once (and again whenever you apt-install new -dev packages on the Pi).
# The Pi must already have the build prerequisites installed - see README.md
# (build-essential pkg-config libasound2-dev libdrm-dev libgbm-dev libinput-dev
# libudev-dev libxkbcommon-dev) so their headers + .pc files come across.
set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=common.sh
source "${SCRIPT_DIR}/common.sh"

require_cmd rsync
require_cmd ssh

echo "Syncing Pi sysroot from ${REMOTE} to ${PI_SYSROOT}..."
mkdir -p "${PI_SYSROOT}/usr" "${PI_SYSROOT}/lib"

rsync -az --delete --safe-links --copy-unsafe-links \
  "${REMOTE}:/usr/include/" "${PI_SYSROOT}/usr/include/"
rsync -az --delete --safe-links --copy-unsafe-links \
  "${REMOTE}:/usr/lib/" "${PI_SYSROOT}/usr/lib/"
rsync -az --delete --safe-links --copy-unsafe-links \
  "${REMOTE}:/lib/" "${PI_SYSROOT}/lib/"

normalize_pi_sysroot_links

if ! pi_sysroot_is_usable; then
  echo "Sysroot synced, but no libc.so.6 found under ${PI_SYSROOT}." >&2
  echo "Check PI_TARGET / the Pi architecture and rerun." >&2
  exit 1
fi

echo "Sysroot synced. glibc: GLIBC_$(max_glibc_version_from_file "$(pi_sysroot_libc)")"
echo "Next: scripts/cross/build-cross.sh"
