#!/usr/bin/env bash
# Build the TETRA ACELP decoder + encoder shared libraries (Linux/macOS/Pi).
#
# Compiles the ETSI reference speech codec (native/etsi/, which you must supply
# yourself - see native/README.md) with our stable-ABI wrappers into:
#   libtetra_acelp.so       (decoder)   [.dylib on macOS]
#   libtetra_acelp_enc.so   (encoder)
#
# Uses ${CC:-cc}. Set CC to cross-compile, e.g.:
#   CC=aarch64-linux-gnu-gcc ./native/build.sh
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
etsi="$here/etsi"
cc="${CC:-cc}"

if ! command -v "$cc" >/dev/null 2>&1; then
  echo "compiler '$cc' not found. Install clang/gcc or set CC." >&2
  exit 1
fi
if [ ! -f "$etsi/source.h" ]; then
  echo "native/etsi/ is missing the ETSI sources (source.h). See native/README.md." >&2
  exit 1
fi

# .dylib on macOS, .so elsewhere.
ext="so"
case "$(uname -s)" in
  Darwin) ext="dylib" ;;
esac

shared="$etsi/sub_sc_d.c $etsi/sub_dsp.c $etsi/fbas_tet.c $etsi/fexp_tet.c $etsi/fmat_tet.c $etsi/tetra_op.c"
common="-shared -fPIC -O2 -I$etsi -I$here"

echo "Building decoder -> libtetra_acelp.$ext"
# shellcheck disable=SC2086
"$cc" $common $etsi/sdec_tet.c $shared "$here/acelp_decode.c" -o "$here/libtetra_acelp.$ext"

echo "Building encoder -> libtetra_acelp_enc.$ext"
# shellcheck disable=SC2086
"$cc" $common $etsi/scod_tet.c $shared "$here/acelp_encode.c" -o "$here/libtetra_acelp_enc.$ext"

echo "Done. Libraries in $here"
