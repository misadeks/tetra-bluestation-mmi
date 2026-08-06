<#
.SYNOPSIS
    RustRover-on-Windows entry point for the WSL cross-compile + Pi deploy path.
    Invokes scripts/wsl/build-deploy-run.sh inside WSL, with the repo as cwd.

.DESCRIPTION
    Mirrors the tetra-bluestation clion wrapper. WSL must be installed with a
    Debian/Ubuntu distro that has the aarch64 cross-toolchain and Rust (see
    scripts/cross/build-cross.sh header). The Pi sysroot is synced automatically
    on first run.

.EXAMPLE
    ./scripts/wsl/build-deploy-run.ps1
    ./scripts/wsl/build-deploy-run.ps1 -Sync      # re-pull the Pi sysroot first
    ./scripts/wsl/build-deploy-run.ps1 --deploy   # build + deploy, don't run

.PARAMETER Sync
    Force a fresh Pi-sysroot sync before building. Use this after apt-installing
    a new -dev package on the Pi (e.g. libfontconfig1-dev). Windows environment
    variables are NOT visible inside WSL, so this switch forwards FORCE_SYNC=1
    into the WSL shell for you.
#>
[CmdletBinding()]
param(
    [switch]$Sync,
    [Parameter(ValueFromRemainingArguments = $true)] [string[]]$Args
)

$ErrorActionPreference = "Stop"
$repo = (Resolve-Path (Join-Path $PSScriptRoot "..\..")).Path
$passthrough = if ($Args) { " " + ($Args -join " ") } else { "" }
$envPrefix = if ($Sync -or $env:FORCE_SYNC -eq "1") { "FORCE_SYNC=1 " } else { "" }

& wsl.exe --cd "$repo" bash -lc "${envPrefix}bash scripts/wsl/build-deploy-run.sh$passthrough"
exit $LASTEXITCODE
