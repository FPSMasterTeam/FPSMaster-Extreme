#!/usr/bin/env pwsh
<#
.SYNOPSIS
    Build a distributable ReCraft package: release exe + DLSS dll + assets + local test server.

.DESCRIPTION
    Compiles recraft_app in release mode and assembles a self-contained `dist/` folder:

        dist/
          recraft_app.exe          # the client
          nvngx_dlss.dll           # NVIDIA DLSS Super Resolution (only with -Dlss)
          local_assets/minecraft-1.8.9/...   # vanilla 1.8.9 textures/sounds
          local_server/paper-1.8-protocol47/...   # Paper 1.8 test server (logs stripped)

    The exe resolves `local_assets/minecraft-1.8.9` relative to its own directory, so the
    package runs in-place: launch dist/recraft_app.exe and it finds the bundled assets.

.PARAMETER Dlss
    Build with the `dlss` feature and copy nvngx_dlss.dll next to the exe. Default: on.
    BUILDING it requires DLSS_SDK / VULKAN_SDK / LIBCLANG_PATH to be set (see docs/DLSS.md).
    The resulting exe still RUNS on non-RTX / non-Vulkan machines: it tries the Vulkan+DLSS
    path first and falls back to the normal wgpu backend with DLSS off. Pass -Dlss:$false
    only to skip the feature/dll entirely (smaller package, no build toolchain needed).

.PARAMETER Zip
    Also produce MiniCraft.zip at the repo root from the dist/ contents. Default: off.

.PARAMETER OutDir
    Output directory (relative to repo root). Default: dist.

.EXAMPLE
    ./scripts/package.ps1                 # release + DLSS dll + assets + server -> dist/
    ./scripts/package.ps1 -Zip            # ...and zip it to MiniCraft.zip
    ./scripts/package.ps1 -Dlss:$false    # plain build, no DLSS
#>
[CmdletBinding()]
param(
    [bool]$Dlss = $true,
    [switch]$Zip,
    [string]$OutDir = "dist"
)

$ErrorActionPreference = "Stop"

# Repo root = parent of this script's dir.
$RepoRoot = Split-Path -Parent $PSScriptRoot
$Dist = Join-Path $RepoRoot $OutDir

Write-Host "==> ReCraft package (DLSS=$Dlss)" -ForegroundColor Cyan

# --- Preflight: if building DLSS, the toolchain env vars must be present, else the
#     dlss_wgpu build.rs (bindgen) fails. Catch it here with a clear message. ---
if ($Dlss) {
    if (-not $env:DLSS_SDK) { throw "DLSS_SDK is not set. Set it or run with -Dlss:`$false. See docs/DLSS.md." }
    $DllSrc = Join-Path $env:DLSS_SDK "lib\Windows_x86_64\rel\nvngx_dlss.dll"
    if (-not (Test-Path $DllSrc)) { throw "nvngx_dlss.dll not found at $DllSrc" }
}

# --- 1. Build release ---
$features = if ($Dlss) { @("--features", "dlss") } else { @() }
Write-Host "==> cargo build --release -p recraft_app $features" -ForegroundColor Cyan
& cargo build --release -p recraft_app @features
if ($LASTEXITCODE -ne 0) { throw "cargo build failed (exit $LASTEXITCODE)" }

$ExeSrc = Join-Path $RepoRoot "target\release\recraft_app.exe"
if (-not (Test-Path $ExeSrc)) { throw "Build succeeded but exe missing: $ExeSrc" }

# --- 2. Fresh dist dir ---
if (Test-Path $Dist) { Remove-Item -Recurse -Force $Dist }
New-Item -ItemType Directory -Path $Dist | Out-Null

# --- 3. exe ---
Copy-Item $ExeSrc $Dist
Write-Host "  + recraft_app.exe"

# --- 4. DLSS dll (super resolution only; dlssd/dlssg are RR/frame-gen, unused) ---
if ($Dlss) {
    Copy-Item $DllSrc $Dist
    Write-Host "  + nvngx_dlss.dll"
}

# --- 5. assets ---
$AssetsSrc = Join-Path $RepoRoot "local_assets\minecraft-1.8.9"
if (Test-Path $AssetsSrc) {
    $AssetsDst = Join-Path $Dist "local_assets\minecraft-1.8.9"
    New-Item -ItemType Directory -Path (Split-Path $AssetsDst) | Out-Null
    Copy-Item -Recurse $AssetsSrc $AssetsDst
    Write-Host "  + local_assets/minecraft-1.8.9"
} else {
    Write-Warning "local_assets/minecraft-1.8.9 missing — run scripts/setup_minecraft_1_8_9_assets.py first. Package will use the fallback atlas."
}

# --- 6. local test server (strip regenerable logs) ---
$ServerSrc = Join-Path $RepoRoot "local_server\paper-1.8-protocol47"
if (Test-Path $ServerSrc) {
    $ServerDst = Join-Path $Dist "local_server\paper-1.8-protocol47"
    New-Item -ItemType Directory -Path (Split-Path $ServerDst) | Out-Null
    Copy-Item -Recurse $ServerSrc $ServerDst
    $ServerLogs = Join-Path $ServerDst "logs"
    if (Test-Path $ServerLogs) { Remove-Item -Recurse -Force $ServerLogs }
    Write-Host "  + local_server/paper-1.8-protocol47 (logs stripped)"
} else {
    Write-Warning "local_server/paper-1.8-protocol47 missing — skipping server. Run scripts/setup_paper_1_8_test_server.py to create it."
}

# --- 7. optional zip ---
if ($Zip) {
    $ZipPath = Join-Path $RepoRoot "MiniCraft.zip"
    if (Test-Path $ZipPath) { Remove-Item -Force $ZipPath }
    Compress-Archive -Path (Join-Path $Dist "*") -DestinationPath $ZipPath
    Write-Host "  + MiniCraft.zip" -ForegroundColor Green
}

Write-Host "==> Done. Package at: $Dist" -ForegroundColor Green
