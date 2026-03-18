<#
.SYNOPSIS
    Package microsandbox Windows release artifacts.

.DESCRIPTION
    Builds all Windows host binaries (msb.exe, msbserver.exe, msbrun-hcs.exe),
    creates the boot bundle, and packages everything into two zip files with
    SHA256 checksums for GitHub release upload.

    Output:
      build/microsandbox-{VERSION}-windows-x86_64.zip          (host binaries)
      build/microsandbox-{VERSION}-windows-x86_64.zip.sha256
      build/microsandbox-boot-bundle-{VERSION}-windows-x86_64.zip   (boot bundle)
      build/microsandbox-boot-bundle-{VERSION}-windows-x86_64.zip.sha256

.PARAMETER Version
    Semantic version. Defaults to the version in workspace Cargo.toml.

.PARAMETER SkipBuild
    Skip building binaries (use pre-existing build artifacts).

.PARAMETER SkipBootBundle
    Skip building the boot bundle zip.

.EXAMPLE
    .\scripts\Package-WindowsRelease.ps1
    .\scripts\Package-WindowsRelease.ps1 -Version 0.2.6 -SkipBootBundle
#>

[CmdletBinding()]
param(
    [string]$Version,
    [switch]$SkipBuild,
    [switch]$SkipBootBundle
)

$ErrorActionPreference = "Stop"

$REPO_ROOT = Split-Path -Parent (Split-Path -Parent $PSScriptRoot)
if (-not $REPO_ROOT) { $REPO_ROOT = Split-Path -Parent $PSScriptRoot }
if (-not (Test-Path "$REPO_ROOT\Cargo.toml")) {
    $REPO_ROOT = Split-Path -Parent $MyInvocation.MyCommand.Path
    $REPO_ROOT = Split-Path -Parent $REPO_ROOT
}

# ---------------------------------------------------------------------------
# Resolve version
# ---------------------------------------------------------------------------
if (-not $Version) {
    $cargoToml = Get-Content "$REPO_ROOT\Cargo.toml" -Raw
    if ($cargoToml -match 'version\s*=\s*"([^"]+)"') {
        $Version = $Matches[1]
    } else {
        Write-Error "Could not extract version from Cargo.toml"
        exit 1
    }
}

$BUILD_DIR = Join-Path $REPO_ROOT "build"
New-Item -ItemType Directory -Path $BUILD_DIR -Force | Out-Null

Write-Host "=== Packaging microsandbox v$Version for Windows x86_64 ===" -ForegroundColor Cyan

# ---------------------------------------------------------------------------
# Step 1: Build host binaries
# ---------------------------------------------------------------------------
if (-not $SkipBuild) {
    Write-Host "--- Building Rust host binaries ---" -ForegroundColor Yellow
    Push-Location $REPO_ROOT
    try {
        & cargo build --release --bin msb --bin msbserver --features cli
        if ($LASTEXITCODE -ne 0) { Write-Error "Rust build failed"; exit 1 }
    } finally {
        Pop-Location
    }

    Write-Host "--- Building Go HCS worker ---" -ForegroundColor Yellow
    Push-Location (Join-Path $REPO_ROOT "microsandbox-hcs-worker")
    try {
        & go build -buildvcs=false -o (Join-Path $REPO_ROOT "target\release\msbrun-hcs.exe") .
        if ($LASTEXITCODE -ne 0) { Write-Error "Go build failed"; exit 1 }
    } finally {
        Pop-Location
    }
}

# ---------------------------------------------------------------------------
# Step 2: Package host binaries zip
# ---------------------------------------------------------------------------
Write-Host "--- Packaging host binaries ---" -ForegroundColor Yellow

$HOST_PKG_NAME = "microsandbox-$Version-windows-x86_64"
$HOST_PKG_DIR = Join-Path $BUILD_DIR $HOST_PKG_NAME
$HOST_ZIP = "$HOST_PKG_DIR.zip"

# Clean previous package.
if (Test-Path $HOST_PKG_DIR) { Remove-Item $HOST_PKG_DIR -Recurse -Force }
if (Test-Path $HOST_ZIP) { Remove-Item $HOST_ZIP -Force }
New-Item -ItemType Directory -Path $HOST_PKG_DIR -Force | Out-Null

$RELEASE_DIR = Join-Path $REPO_ROOT "target\release"

Copy-Item (Join-Path $RELEASE_DIR "msb.exe") $HOST_PKG_DIR -Force
Copy-Item (Join-Path $RELEASE_DIR "msbserver.exe") $HOST_PKG_DIR -Force
Copy-Item (Join-Path $RELEASE_DIR "msbrun-hcs.exe") $HOST_PKG_DIR -Force

Compress-Archive -Path "$HOST_PKG_DIR\*" -DestinationPath $HOST_ZIP -Force
$hash = (Get-FileHash $HOST_ZIP -Algorithm SHA256).Hash.ToLower()
"$hash  $(Split-Path $HOST_ZIP -Leaf)" | Set-Content "$HOST_ZIP.sha256" -Encoding UTF8

Write-Host "Host binaries: $HOST_ZIP"
Write-Host "  SHA256: $hash"

# ---------------------------------------------------------------------------
# Step 3: Package boot bundle zip (optional)
# ---------------------------------------------------------------------------
if (-not $SkipBootBundle) {
    Write-Host "--- Packaging boot bundle ---" -ForegroundColor Yellow

    $BUNDLE_DIR = Join-Path $REPO_ROOT "target\release\boot-bundle\$Version"
    if (-not (Test-Path $BUNDLE_DIR)) {
        Write-Host "Boot bundle not found at $BUNDLE_DIR, building..." -ForegroundColor Yellow
        & (Join-Path $REPO_ROOT "scripts\Build-BootBundle.ps1") -Version $Version
        if ($LASTEXITCODE -ne 0) { Write-Error "Boot bundle build failed"; exit 1 }
    }

    $BUNDLE_PKG_NAME = "microsandbox-boot-bundle-$Version-windows-x86_64"
    $BUNDLE_ZIP = Join-Path $BUILD_DIR "$BUNDLE_PKG_NAME.zip"

    if (Test-Path $BUNDLE_ZIP) { Remove-Item $BUNDLE_ZIP -Force }

    Compress-Archive -Path "$BUNDLE_DIR\*" -DestinationPath $BUNDLE_ZIP -Force
    $bundleHash = (Get-FileHash $BUNDLE_ZIP -Algorithm SHA256).Hash.ToLower()
    "$bundleHash  $(Split-Path $BUNDLE_ZIP -Leaf)" | Set-Content "$BUNDLE_ZIP.sha256" -Encoding UTF8

    Write-Host "Boot bundle: $BUNDLE_ZIP"
    Write-Host "  SHA256: $bundleHash"
}

# ---------------------------------------------------------------------------
# Done
# ---------------------------------------------------------------------------
Write-Host ""
Write-Host "=== Packaging complete ===" -ForegroundColor Green
Get-ChildItem $BUILD_DIR -Filter "microsandbox-*" | Format-Table Name, @{N="Size";E={"{0:N0} KB" -f ($_.Length / 1KB)}} -AutoSize
