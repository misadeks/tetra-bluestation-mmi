<#
.SYNOPSIS
  Build the TETRA ACELP decoder + encoder shared libraries (Windows).

.DESCRIPTION
  Compiles the ETSI reference speech codec (in native/etsi/, which you must
  supply yourself - see native/README.md) together with our stable-ABI wrappers
  into tetra_acelp.dll (decoder) and tetra_acelp_enc.dll (encoder).

  Requires clang on PATH. clang defaults to the host arch triple, which matches
  a same-arch Rust build. Override with -Target to force one.

.PARAMETER Target
  Optional clang --target triple, e.g. x86_64-pc-windows-msvc or
  aarch64-pc-windows-msvc.
#>
[CmdletBinding()]
param(
    [string]$Target
)

$ErrorActionPreference = 'Stop'
$here = Split-Path -Parent $MyInvocation.MyCommand.Path
$etsi = Join-Path $here 'etsi'

if (-not (Get-Command clang -ErrorAction SilentlyContinue)) {
    throw "clang not found on PATH. Install LLVM/clang and retry."
}
if (-not (Test-Path (Join-Path $etsi 'source.h'))) {
    throw "native/etsi/ is missing the ETSI sources (source.h not found). See native/README.md."
}

$shared = @('sub_sc_d.c', 'sub_dsp.c', 'fbas_tet.c', 'fexp_tet.c', 'fmat_tet.c', 'tetra_op.c')
$decSrc = @('sdec_tet.c') + $shared | ForEach-Object { Join-Path $etsi $_ }
$encSrc = @('scod_tet.c') + $shared | ForEach-Object { Join-Path $etsi $_ }
$decSrc += (Join-Path $here 'acelp_decode.c')
$encSrc += (Join-Path $here 'acelp_encode.c')

$targetArgs = @()
if ($Target) { $targetArgs = @("--target=$Target") }

$common = @('-shared', '-O2', "-I$etsi", "-I$here") + $targetArgs

Write-Host "Building decoder -> tetra_acelp.dll"
& clang @common @decSrc -o (Join-Path $here 'tetra_acelp.dll')
if ($LASTEXITCODE -ne 0) { throw "decoder build failed" }

Write-Host "Building encoder -> tetra_acelp_enc.dll"
& clang @common @encSrc -o (Join-Path $here 'tetra_acelp_enc.dll')
if ($LASTEXITCODE -ne 0) { throw "encoder build failed" }

Write-Host "Done. Libraries in $here"
