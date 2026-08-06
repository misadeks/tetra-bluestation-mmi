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
    ./scripts/wsl/build-deploy-run.ps1 --deploy   # build + deploy, don't run
#>
[CmdletBinding()]
param([Parameter(ValueFromRemainingArguments = $true)] [string[]]$Args)

$ErrorActionPreference = "Stop"
$repo = (Resolve-Path (Join-Path $PSScriptRoot "..\..")).Path
$passthrough = if ($Args) { " " + ($Args -join " ") } else { "" }

& wsl.exe --cd "$repo" bash -lc "bash scripts/wsl/build-deploy-run.sh$passthrough"
exit $LASTEXITCODE
