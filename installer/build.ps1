# Build release binaries and produce a Windows setup.exe via NSIS.
# Requires: Rust toolchain, NSIS (makensis on PATH).

$ErrorActionPreference = "Stop"

$Root = Split-Path -Parent $PSScriptRoot
Set-Location $Root

function Get-CargoPackageVersion($Package) {
    $toml = Get-Content (Join-Path $Root "$Package\Cargo.toml") -Raw
    if ($toml -match '(?m)^version\s*=\s*"([^"]+)"') {
        return $Matches[1]
    }
    throw "Could not read version from $Package/Cargo.toml"
}

$Version = Get-CargoPackageVersion "podping-gossipwatcher"
Write-Host "Building Podping Gossip Watcher $Version..."

cargo build --release --locked -p podping-gossipwatcher -p podping-gossipwatcher-tray
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }

$WatcherExe = Join-Path $Root "target\release\podping-gossipwatcher.exe"
$TrayExe = Join-Path $Root "target\release\podping-gossipwatcher-tray.exe"
if (-not (Test-Path $WatcherExe)) { throw "Missing $WatcherExe" }
if (-not (Test-Path $TrayExe)) { throw "Missing $TrayExe" }

$Makensis = Get-Command makensis -ErrorAction SilentlyContinue
if (-not $Makensis) {
    throw @"
NSIS (makensis) was not found on PATH.

Install NSIS from https://nsis.sourceforge.io/Download and ensure makensis.exe
is on PATH, then re-run:

  .\installer\build.ps1
"@
}

$Dist = Join-Path $Root "dist"
New-Item -ItemType Directory -Force -Path $Dist | Out-Null

Write-Host "Creating installer with NSIS..."
& makensis "/DPRODUCT_VERSION=$Version" (Join-Path $Root "installer\podping-gossipwatcher.nsi")
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }

$Setup = Join-Path $Dist "PodpingGossipWatcher-$Version-setup.exe"
Write-Host ""
Write-Host "Done: $Setup"
