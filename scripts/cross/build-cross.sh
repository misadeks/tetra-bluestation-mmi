#!/usr/bin/env bash
#
# build-cross.sh - cross-compile the release binary for the Pi from WSL/Linux,
# linking against the synced Pi sysroot. Optionally deploy + run it on the Pi.
#
# Usage (from WSL or any x86 Linux with the aarch64 cross-toolchain):
#   scripts/cross/build-cross.sh            # build only
#   scripts/cross/build-cross.sh --deploy   # build + copy binary/config to Pi
#   scripts/cross/build-cross.sh --run      # build + deploy + run (linuxkms)
#
# First-time host packages (WSL/Debian):
#   sudo apt install -y build-essential pkg-config rsync openssh-client \
#     gcc-aarch64-linux-gnu
#   rustup target add aarch64-unknown-linux-gnu
# And sync the sysroot once:  scripts/cross/sync-sysroot.sh
set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=common.sh
source "${SCRIPT_DIR}/common.sh"

DEPLOY=0
RUN=0
for arg in "$@"; do
  case "$arg" in
    --deploy) DEPLOY=1 ;;
    --run)    DEPLOY=1; RUN=1 ;;
    -h|--help) sed -n '2,18p' "$0"; exit 0 ;;
    *) echo "unknown arg: $arg" >&2; exit 2 ;;
  esac
done

require_cmd cargo
require_cmd rustup

if ! pi_sysroot_is_usable; then
  echo "No usable Pi sysroot at ${PI_SYSROOT}." >&2
  echo "Run scripts/cross/sync-sysroot.sh first." >&2
  exit 1
fi

echo "Adding target ${PI_TARGET}..."
rustup target add "${PI_TARGET}" >/dev/null

setup_cross_env
echo "Building ${BINARY_NAME} for ${PI_TARGET} (sysroot: ${PI_SYSROOT})..."
cd "${REPO_ROOT}"
cargo build --release --target "${PI_TARGET}"

# Refuse to ship a binary that needs a newer glibc than the Pi provides.
SYSROOT_GLIBC="$(max_glibc_version_from_file "$(pi_sysroot_libc)")"
BINARY_GLIBC="$(max_glibc_version_from_file "${LOCAL_BINARY}")"
if [[ -n "${SYSROOT_GLIBC}" && -n "${BINARY_GLIBC}" ]]; then
  HIGHEST="$(printf '%s\n%s\n' "${SYSROOT_GLIBC}" "${BINARY_GLIBC}" | sort -V | tail -n 1)"
  if [[ "${HIGHEST}" != "${SYSROOT_GLIBC}" ]]; then
    echo "Binary needs GLIBC_${BINARY_GLIBC} but the Pi provides GLIBC_${SYSROOT_GLIBC}." >&2
    echo "Link step didn't use the sysroot correctly; refusing to deploy." >&2
    exit 1
  fi
  echo "glibc OK: binary GLIBC_${BINARY_GLIBC} <= Pi GLIBC_${SYSROOT_GLIBC}"
fi
echo "Built ${LOCAL_BINARY}"

if [[ "${DEPLOY}" == "0" ]]; then
  exit 0
fi

require_cmd ssh
require_cmd rsync
echo "Deploying to ${REMOTE}:${REMOTE_DIR}..."
ssh "${REMOTE}" "mkdir -p ${REMOTE_DIR}"
rsync -az "${LOCAL_BINARY}" "${REMOTE}:${REMOTE_DIR}/${BINARY_NAME}"
rsync -az "${REPO_ROOT}/${CONFIG_FILE}" "${REMOTE}:${REMOTE_DIR}/${CONFIG_FILE}"
# ACELP codec libs (gitignored, arch-specific) if present locally for the Pi.
if [[ -d "${REPO_ROOT}/native" ]]; then
  rsync -az "${REPO_ROOT}/native/" "${REMOTE}:${REMOTE_DIR}/native/"
fi

# Pin the UI off the radio's cores (empty TASKSET_CPUS disables it).
TASKSET="${TASKSET_CPUS:+taskset -c ${TASKSET_CPUS} }"

if [[ "${RUN}" == "0" ]]; then
  echo "Deployed. Run it on the Pi with:"
  echo "  cd ${REMOTE_DIR} && sudo SLINT_BACKEND=linuxkms RUST_LOG=${RUST_LOG} ${TASKSET}./${BINARY_NAME}"
  exit 0
fi

echo "Running on ${REMOTE} (SLINT_BACKEND=linuxkms)..."
ssh -t "${REMOTE}" "cd ${REMOTE_DIR} && sudo SLINT_BACKEND=linuxkms RUST_LOG=${RUST_LOG} ${TASKSET}./${BINARY_NAME}"
