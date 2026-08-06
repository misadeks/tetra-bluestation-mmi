#!/usr/bin/env bash
#
# common.sh - shared settings + Pi-sysroot helpers for the WSL/Linux
# cross-compile path. Mirrors the approach used in the tetra-bluestation repo:
# build for aarch64 with a cross-linker and a sysroot rsynced from the Pi, so the
# C libraries the linuxkms backend links (libdrm, libinput, libxkbcommon,
# libudev, libasound) resolve against the Pi's own libs.
#
# Sourced by sync-sysroot.sh and build-cross.sh. Override any value via the
# environment or scripts/cross/pi.env (copied from pi.env.example).
set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd -- "${SCRIPT_DIR}/../.." && pwd)"

if [[ -f "${SCRIPT_DIR}/pi.env" ]]; then
  # Tolerate CRLF (e.g. pi.env edited/created on Windows) by stripping trailing
  # CRs before evaluating, so `$'\r': command not found` can't happen.
  eval "$(sed 's/\r$//' "${SCRIPT_DIR}/pi.env")"
fi

: "${PI_USER:=pi}"
: "${PI_HOST:=tetra-ms.local}"
: "${PI_TARGET:=aarch64-unknown-linux-gnu}"
: "${REMOTE_DIR:=/home/${PI_USER}/tetra-tn-ui}"
: "${BINARY_NAME:=tetra-tn-ui}"
: "${CONFIG_FILE:=config.toml}"
: "${RUST_LOG:=info}"
: "${PI_SYSROOT:=${REPO_ROOT}/.pi-sysroot/${PI_TARGET}}"

REMOTE="${PI_USER}@${PI_HOST}"
LOCAL_BINARY="${REPO_ROOT}/target/${PI_TARGET}/release/${BINARY_NAME}"

require_cmd() {
  if ! command -v "$1" >/dev/null 2>&1; then
    echo "Missing required command: $1" >&2
    exit 1
  fi
}

target_pkgconfig_arch() {
  case "${PI_TARGET}" in
    aarch64-unknown-linux-gnu) echo "aarch64-linux-gnu" ;;
    armv7-unknown-linux-gnueabihf) echo "arm-linux-gnueabihf" ;;
    *) echo "" ;;
  esac
}

target_linker() {
  case "${PI_TARGET}" in
    aarch64-unknown-linux-gnu) echo "aarch64-linux-gnu-gcc" ;;
    armv7-unknown-linux-gnueabihf) echo "arm-linux-gnueabihf-gcc" ;;
    *) echo "" ;;
  esac
}

target_dynamic_linker() {
  case "${PI_TARGET}" in
    aarch64-unknown-linux-gnu) echo "ld-linux-aarch64.so.1" ;;
    armv7-unknown-linux-gnueabihf) echo "ld-linux-armhf.so.3" ;;
    *) echo "" ;;
  esac
}

target_env_name() { echo "${PI_TARGET}" | tr '[:lower:]-' '[:upper:]_'; }

# Raspberry Pi OS is usr-merged; linker scripts under /usr/lib/<arch> still refer
# to absolute /lib/<arch>/... paths, so recreate matching compat symlinks inside
# the sysroot.
normalize_pi_sysroot_links() {
  local pkg_arch ldso
  pkg_arch="$(target_pkgconfig_arch)"
  ldso="$(target_dynamic_linker)"
  [[ -n "${pkg_arch}" && -d "${PI_SYSROOT}/usr/lib/${pkg_arch}" ]] || return 0

  mkdir -p "${PI_SYSROOT}/lib"
  [[ -e "${PI_SYSROOT}/lib/${pkg_arch}" ]] || \
    ln -s "../usr/lib/${pkg_arch}" "${PI_SYSROOT}/lib/${pkg_arch}"

  if [[ -n "${ldso}" && ! -e "${PI_SYSROOT}/lib/${ldso}" ]]; then
    if [[ -e "${PI_SYSROOT}/usr/lib/${pkg_arch}/${ldso}" ]]; then
      ln -s "../usr/lib/${pkg_arch}/${ldso}" "${PI_SYSROOT}/lib/${ldso}"
    elif [[ -e "${PI_SYSROOT}/usr/lib/${ldso}" ]]; then
      ln -s "../usr/lib/${ldso}" "${PI_SYSROOT}/lib/${ldso}"
    fi
  fi
}

pi_sysroot_libc() {
  local pkg_arch candidate
  pkg_arch="$(target_pkgconfig_arch)"
  for candidate in \
    "${PI_SYSROOT}/lib/${pkg_arch}/libc.so.6" \
    "${PI_SYSROOT}/usr/lib/${pkg_arch}/libc.so.6" \
    "${PI_SYSROOT}/lib/libc.so.6" \
    "${PI_SYSROOT}/usr/lib/libc.so.6"; do
    [[ -f "${candidate}" ]] && { echo "${candidate}"; return 0; }
  done
  return 1
}

pi_sysroot_is_usable() {
  [[ -d "${PI_SYSROOT}" ]] || return 1
  normalize_pi_sysroot_links
  pi_sysroot_libc >/dev/null || return 1
  # Also require dev pkg-config metadata. A sysroot synced BEFORE the -dev
  # packages were installed on the Pi has libc + runtime .so files but no .pc
  # files, which otherwise fails the build confusingly deep in a build script.
  # libdrm.pc is always needed (drm-sys), so use it as the "dev is present"
  # marker; a stale runtime-only sysroot then triggers an automatic re-sync.
  local pkg_arch
  pkg_arch="$(target_pkgconfig_arch)"
  [[ -z "${pkg_arch}" || -e "${PI_SYSROOT}/usr/lib/${pkg_arch}/pkgconfig/libdrm.pc" ]]
}

max_glibc_version_from_file() {
  strings "$1" 2>/dev/null \
    | grep -oE 'GLIBC_[0-9]+\.[0-9]+' | sed 's/^GLIBC_//' | sort -Vu | tail -n 1
}

# Export the linker + pkg-config + sysroot RUSTFLAGS so `cargo build --target
# ${PI_TARGET}` links against the Pi sysroot. Requires a usable sysroot.
setup_cross_env() {
  local pkg_arch linker target_env linker_var rustflags_var
  pkg_arch="$(target_pkgconfig_arch)"
  linker="$(target_linker)"
  target_env="$(target_env_name)"
  require_cmd "${linker}"

  linker_var="CARGO_TARGET_${target_env}_LINKER"
  rustflags_var="CARGO_TARGET_${target_env}_RUSTFLAGS"
  export "${linker_var}=${!linker_var:-${linker}}"

  local sysroot_flags=(
    "-C" "link-arg=--sysroot=${PI_SYSROOT}"
    "-C" "link-arg=-Wl,-rpath-link,${PI_SYSROOT}/lib/${pkg_arch}"
    "-C" "link-arg=-Wl,-rpath-link,${PI_SYSROOT}/usr/lib/${pkg_arch}"
    "-C" "link-arg=-Wl,-rpath-link,${PI_SYSROOT}/lib"
    "-C" "link-arg=-Wl,-rpath-link,${PI_SYSROOT}/usr/lib"
  )
  local existing="${!rustflags_var:-}"
  export "${rustflags_var}=${existing:+${existing} }${sysroot_flags[*]}"

  export PKG_CONFIG_ALLOW_CROSS=1
  export PKG_CONFIG_SYSROOT_DIR="${PI_SYSROOT}"
  if [[ -n "${pkg_arch}" ]]; then
    export PKG_CONFIG_PATH="${PI_SYSROOT}/usr/lib/${pkg_arch}/pkgconfig:${PI_SYSROOT}/usr/lib/pkgconfig:${PI_SYSROOT}/usr/share/pkgconfig"
  else
    export PKG_CONFIG_PATH="${PI_SYSROOT}/usr/lib/pkgconfig:${PI_SYSROOT}/usr/share/pkgconfig"
  fi
}
