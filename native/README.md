# TETRA ACELP codec (native voice libraries)

Two-way voice (M5) is driven by the **ETSI EN 300 395-2** TETRA ACELP reference
speech codec. The app loads two shared libraries at runtime (path from
`[audio].codec_dir` in `config.toml`) through a tiny, stable C ABI:

| Library (Windows / Linux / macOS)                         | Role    |
|-----------------------------------------------------------|---------|
| `tetra_acelp.dll` / `libtetra_acelp.so` / `libtetra_acelp.dylib`         | decoder |
| `tetra_acelp_enc.dll` / `libtetra_acelp_enc.so` / `libtetra_acelp_enc.dylib` | encoder |

If the libraries are missing or fail to load (e.g. an architecture mismatch),
the app logs it and simply runs **without voice** — everything else works.

## What is committed vs. what you must supply

Committed here (ours):

- `acelp_decode.c` — BFI-aware decoder wrapper exposing the stable ABI.
- `acelp_encode.c` — encoder wrapper for the uplink/TX path.
- `enc_test.c` — tiny standalone encoder smoke test.
- `build.ps1`, `build.sh` — build scripts.

**Not committed** (see the repo `.gitignore`):

- `etsi/` — the ETSI reference C sources, headers and `*.tab` tables. These are
  **ETSI-copyrighted** and must not live in this repo. Obtain EN 300 395-2 from
  <https://www.etsi.org/standards> (the C source ships as the electronic annex)
  and drop the speech-codec files into `native/etsi/` (flat, no sub-folders).
- The built `*.dll` / `*.so` / `*.dylib` / `*.lib` / `*.exp` artifacts.

### ABI (what the wrappers export)

```c
void* tetra_dec_create(void);
void  tetra_dec_destroy(void* ctx);
int   tetra_dec_decode(void* ctx, const uint8_t* bits137, int bfi, int16_t* pcm240);

void* tetra_enc_create(void);
void  tetra_enc_destroy(void* ctx);
int   tetra_enc_encode(void* ctx, const int16_t* pcm240, uint8_t* bits137);
```

One `MsSpeechFrame` carries 274 codec bits = two 137-bit sub-frames, which decode
to 480 int16 samples (60 ms @ 8 kHz). Uplink is the mirror: 480 samples encode to
274 bits shipped as `MsUplinkSpeech`.

## `etsi/` file set

Copy every `*.c`, `*.h` and `*.tab` from the ETSI speech codec. The build only
pulls in what it needs; the required subset is:

- **Decoder:** `sdec_tet.c`, `sub_sc_d.c`, `sub_dsp.c`, `fbas_tet.c`,
  `fexp_tet.c`, `fmat_tet.c`, `tetra_op.c`
- **Encoder:** `scod_tet.c` (plus the shared units above)
- **Header:** `source.h`
- **Tables:** `const.tab`, `grid.tab`, `ener_qua.tab`, `lag_wind.tab`,
  `window.tab`, `inv_sqrt.tab`, `log2.tab`, `pow2.tab`, `clsp_334.tab`

Also keep the re-entrancy shim `etsi/acelp_state_bridge.h` (ours) here — it lets
the decoder and encoder hold per-instance state instead of file-scope globals, so
duplex calls and concurrent encode/decode are safe.

## Build

Requires `clang` (or any C compiler) on `PATH` and the ETSI sources in `etsi/`.

### Automatic (on first run)

Like the Python repo's `app/acelp.py`, the app **builds the libraries on demand**:
on startup, if `tetra_acelp*` are missing from `[audio].codec_dir` but the ETSI
sources (`etsi/`) and `clang` are present, it compiles them with the exact same
command as below and then loads them. If the sources or `clang` are absent it
logs the reason and runs without voice. The scripts below do the same thing
manually.

### Manual

From the repo root:

```powershell
# Windows (PowerShell)
native\build.ps1
```

```bash
# macOS / Linux / Raspberry Pi
./native/build.sh
```

Both scripts emit the two libraries into `native/` next to the sources. With
`config.toml` set to `codec_dir = 'native'` the app loads them from there.

### Architecture must match the app

The libraries must be built for the **same architecture as the running binary**.
On an aarch64 (ARM64) Windows host, `clang` defaults to
`aarch64-pc-windows-msvc`, which matches an aarch64 Rust build — no `--target`
needed. To force a target explicitly:

```
clang --target=x86_64-pc-windows-msvc ...   # 64-bit x64 Windows
clang --target=aarch64-pc-windows-msvc ...   # ARM64 Windows
```

### Raspberry Pi (aarch64 Linux)

Two options:

1. **On-device:** install a compiler (`sudo apt install clang` or `build-essential`)
   and run `./native/build.sh` on the Pi. It produces `libtetra_acelp.so` and
   `libtetra_acelp_enc.so`. Deploy them next to the binary (or point `codec_dir`
   at their folder).
2. **Cross-compile** from a dev host with the aarch64 toolchain
   (`aarch64-linux-gnu-gcc`); pass `CC=aarch64-linux-gnu-gcc ./native/build.sh`
   and copy the artifacts to the Pi.

## Verify

```bash
python -c "print('decoder+encoder present')"   # or just run the app
```

Start the app and place a call; the startup log shows either
`codec: ACELP libraries loaded` or the reason it fell back to no-voice.
