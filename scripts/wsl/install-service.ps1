<#
.SYNOPSIS
    RustRover-on-Windows entry point to install the systemd kiosk service on the
    Pi. Invokes scripts/wsl/install-service.sh inside WSL: cross-builds if
    needed, deploys the binary + config, installs+enables+starts the service.

.PARAMETER Sync
    Force a fresh Pi-sysroot sync first (after apt-installing new -dev packages).
    Windows env vars don't reach WSL, so this forwards FORCE_SYNC=1 for you.

.EXAMPLE
    ./scripts/wsl/install-service.ps1
    ./scripts/wsl/install-service.ps1 -Sync
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

& wsl.exe --cd "$repo" bash -lc "${envPrefix}bash scripts/wsl/install-service.sh$passthrough"
exit $LASTEXITCODE
