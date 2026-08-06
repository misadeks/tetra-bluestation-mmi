#!/usr/bin/env bash
#
# sync-sysroot.sh - rsync the Pi's /usr/include, /usr/lib and /lib into a local
# sysroot so the cross-linker can resolve the linuxkms + audio C libraries.
#
# Run this once (and again whenever you apt-install new -dev packages on the Pi).
# The Pi must already have the build prerequisites installed - see README.md
# (build-essential pkg-config libasound2-dev libdrm-dev libgbm-dev libinput-dev
# libudev-dev libxkbcommon-dev libfontconfig1-dev) so their headers + .pc files
# come across. Re-run this after installing any new -dev package on the Pi.
set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=common.sh
source "${SCRIPT_DIR}/common.sh"

require_cmd rsync
require_cmd ssh

# The Pi's filesystem has dangling symlinks we don't care about (e.g. Qt's
# qtchooser default.conf). With --copy-unsafe-links those make rsync exit 23/24
# ("partial transfer due to vanished/unreadable files") and print "IO error
# encountered -- skipping file deletion". That's harmless for our sysroot - the
# .so symlinks we actually link against still come across - so accept 23/24.
rsync_sysroot() {
  local rc=0
  rsync -az --delete --safe-links --copy-unsafe-links "$@" || rc=$?
  if [[ ${rc} -ne 0 && ${rc} -ne 23 && ${rc} -ne 24 ]]; then
    echo "rsync failed (exit ${rc})" >&2
    return "${rc}"
  fi
  return 0
}

echo "Syncing Pi sysroot from ${REMOTE} to ${PI_SYSROOT}..."
mkdir -p "${PI_SYSROOT}/usr" "${PI_SYSROOT}/lib"

rsync_sysroot "${REMOTE}:/usr/include/" "${PI_SYSROOT}/usr/include/"
rsync_sysroot "${REMOTE}:/usr/lib/" "${PI_SYSROOT}/usr/lib/"
rsync_sysroot "${REMOTE}:/lib/" "${PI_SYSROOT}/lib/"

normalize_pi_sysroot_links

if ! pi_sysroot_is_usable; then
  echo "Sysroot synced, but no libc.so.6 found under ${PI_SYSROOT}." >&2
  echo "Check PI_TARGET / the Pi architecture and rerun." >&2
  exit 1
fi

echo "Sysroot synced. glibc: GLIBC_$(max_glibc_version_from_file "$(pi_sysroot_libc)")"
echo "Next: scripts/cross/build-cross.sh"
