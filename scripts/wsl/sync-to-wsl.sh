#!/usr/bin/env bash
#
# sync-to-wsl.sh - copy the Windows checkout into WSL's Linux filesystem so the
# cross-build runs off ext4 (much faster than building over /mnt/c). Prints the
# WSL repo path as its last line. Override with WSL_REPO_DIR.
set -euo pipefail

SRC_REPO_ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)"
WSL_REPO_DIR="${WSL_REPO_DIR:-${HOME}/src/tetra-tn-ui}"

command -v rsync >/dev/null 2>&1 || { echo "Missing rsync in WSL" >&2; exit 1; }

echo "Syncing Windows checkout to WSL:" >&2
echo "  ${SRC_REPO_ROOT} -> ${WSL_REPO_DIR}" >&2

mkdir -p "${WSL_REPO_DIR}"
rsync -az --delete \
  --exclude target \
  --exclude .git \
  --exclude .idea \
  --exclude .vscode \
  --exclude .pi-sysroot \
  "${SRC_REPO_ROOT}/" "${WSL_REPO_DIR}/"

echo "${WSL_REPO_DIR}"
