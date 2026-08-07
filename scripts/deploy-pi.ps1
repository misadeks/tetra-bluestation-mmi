<#
.SYNOPSIS
    Sync this repo to a Raspberry Pi, build it there, and run the kiosk straight
    to DRM/KMS via the Slint linuxkms backend. PowerShell twin of deploy-pi.sh
    for RustRover-on-Windows developers.

.DESCRIPTION
    Building happens ON THE PI (native aarch64) - the primary path, since
    cross-compiling aarch64 from Windows needs a cross-linker + Pi sysroot (see
    README.md). Uses ssh/scp (bundled with Windows OpenSSH). rsync is used if it
    is on PATH, otherwise it falls back to scp.

    Prereqs on the Pi (see README.md): build-essential pkg-config libasound2-dev
    libdrm-dev libgbm-dev libinput-dev libudev-dev libxkbcommon-dev
    libfontconfig1-dev fonts-dejavu-core, the Rust
    toolchain (rustup), and the DSI panel enabled via the
    vc4-kms-dsi-waveshare-panel-v2 overlay so /dev/dri/card* exists.

.EXAMPLE
    ./scripts/deploy-pi.ps1 -PiHost 192.168.1.42 -PiUser pi

.EXAMPLE
    $env:PI_HOST = "tetra-ms.local"; ./scripts/deploy-pi.ps1
#>
[CmdletBinding()]
param(
    [string]$PiHost = $(if ($env:PI_HOST) { $env:PI_HOST } else { "tetra-ms.local" }),
    [string]$PiUser = $(if ($env:PI_USER) { $env:PI_USER } else { "pi" }),
    [string]$PiDir  = $(if ($env:PI_DIR)  { $env:PI_DIR }  else { "~/tetra-tn-ui" }),
    [switch]$NoRun
)

$ErrorActionPreference = "Stop"
$repoRoot = (Resolve-Path (Join-Path $PSScriptRoot "..")).Path
$remote   = "$PiUser@$PiHost"

Write-Host ">> syncing $repoRoot -> ${remote}:$PiDir"
if (Get-Command rsync -ErrorAction SilentlyContinue) {
    # rsync (e.g. from Git-for-Windows / MSYS2) keeps the Pi in lockstep.
    rsync -az --delete `
        --exclude '/target' --exclude '/.git' --exclude '/.idea' `
        --exclude '/data' --exclude '*.log' `
        "$repoRoot/" "${remote}:$PiDir/"
    if ($LASTEXITCODE -ne 0) { throw "rsync failed ($LASTEXITCODE)" }
} else {
    Write-Host ">> rsync not found, falling back to scp (no --delete)"
    ssh $remote "mkdir -p $PiDir"
    if ($LASTEXITCODE -ne 0) { throw "ssh mkdir failed ($LASTEXITCODE)" }
    # Copy tracked-ish contents; scp -r pulls the whole tree including target/,
    # so prefer installing rsync for real use.
    scp -r "$repoRoot/*" "${remote}:$PiDir/"
    if ($LASTEXITCODE -ne 0) { throw "scp failed ($LASTEXITCODE)" }
}

Write-Host ">> building on the Pi (cargo build --release)"
ssh $remote "cd $PiDir && cargo build --release"
if ($LASTEXITCODE -ne 0) { throw "remote build failed ($LASTEXITCODE)" }

if ($NoRun) {
    Write-Host ">> built. skipping run (-NoRun)."
    exit 0
}

Write-Host ">> running kiosk on the Pi (SLINT_BACKEND=linuxkms)"
# -t allocates a TTY so the app can grab DRM master / input and Ctrl-C works.
ssh -t $remote "cd $PiDir && sudo SLINT_BACKEND=linuxkms SLINT_BACKEND_LINUXFB=1 RUST_LOG=info ./target/release/tetra-tn-ui"
