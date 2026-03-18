<#
.SYNOPSIS
    Build the Windows boot bundle for microsandbox Linux VMs.

.DESCRIPTION
    PowerShell equivalent of scripts/build_boot_bundle.sh.
    Cross-compiles bootstrap and portal via cargo-zigbuild for x86_64-unknown-linux-musl,
    builds rootfs.vhd using an embedded Python tarfile snippet + tar2ext4.exe, and
    assembles the versioned boot bundle directory with a manifest.

.PARAMETER Version
    Semantic version for the bundle. Defaults to the version in workspace Cargo.toml.

.PARAMETER KernelPath
    Path to the Linux kernel binary. Defaults to .\kernel in the repo root.

.PARAMETER Tar2Ext4Path
    Path to tar2ext4.exe. Defaults to searching PATH, then .\tar2ext4.exe.

.PARAMETER OutputDir
    Output directory for the bundle. Defaults to target\release\boot-bundle\<Version>.

.PARAMETER SkipCrossCompile
    Skip the cargo-zigbuild cross-compilation steps (use pre-built binaries).

.EXAMPLE
    .\scripts\Build-BootBundle.ps1
    .\scripts\Build-BootBundle.ps1 -Version 0.2.6 -SkipCrossCompile
#>

[CmdletBinding()]
param(
    [string]$Version,
    [string]$KernelPath,
    [string]$Tar2Ext4Path,
    [string]$OutputDir,
    [switch]$SkipCrossCompile
)

$ErrorActionPreference = "Stop"

$REPO_ROOT = Split-Path -Parent (Split-Path -Parent $PSScriptRoot)
if (-not $REPO_ROOT) { $REPO_ROOT = Split-Path -Parent $PSScriptRoot }
# Handle case where script is invoked from repo root
if (-not (Test-Path "$REPO_ROOT\Cargo.toml")) {
    $REPO_ROOT = Split-Path -Parent $MyInvocation.MyCommand.Path
    $REPO_ROOT = Split-Path -Parent $REPO_ROOT
}

$MUSL_TARGET = "x86_64-unknown-linux-musl"

# ---------------------------------------------------------------------------
# Resolve version from workspace Cargo.toml if not specified
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

# ---------------------------------------------------------------------------
# Resolve paths
# ---------------------------------------------------------------------------
if (-not $OutputDir) {
    $OutputDir = Join-Path $REPO_ROOT "target\release\boot-bundle\$Version"
}

$BOOTSTRAP_DIR = Join-Path $REPO_ROOT "microsandbox-bootstrap"
$BOOTSTRAP_BIN = Join-Path $BOOTSTRAP_DIR "target\$MUSL_TARGET\release\bootstrap"
$PORTAL_BIN = Join-Path $REPO_ROOT "target\$MUSL_TARGET\release\portal"

if (-not $KernelPath) {
    $KernelPath = Join-Path $REPO_ROOT "kernel"
}

if (-not $Tar2Ext4Path) {
    $found = Get-Command tar2ext4.exe -ErrorAction SilentlyContinue
    if ($found) {
        $Tar2Ext4Path = $found.Source
    } elseif (Test-Path (Join-Path $REPO_ROOT "tar2ext4.exe")) {
        $Tar2Ext4Path = Join-Path $REPO_ROOT "tar2ext4.exe"
    } else {
        Write-Error "tar2ext4.exe not found. Place it in the repo root or PATH."
        exit 1
    }
}

# ---------------------------------------------------------------------------
# Validate prerequisites
# ---------------------------------------------------------------------------
function Assert-Command($cmd, $name) {
    if (-not (Get-Command $cmd -ErrorAction SilentlyContinue)) {
        Write-Error "$name ($cmd) is required but not found in PATH."
        exit 1
    }
}

if (-not $SkipCrossCompile) {
    Assert-Command "cargo" "Rust toolchain"
    Assert-Command "cargo-zigbuild" "cargo-zigbuild (cargo install cargo-zigbuild)"
    # Verify musl target is installed
    $targets = & rustup target list --installed 2>&1
    if ($targets -notcontains $MUSL_TARGET) {
        Write-Host "Adding musl target..." -ForegroundColor Yellow
        & rustup target add $MUSL_TARGET
        if ($LASTEXITCODE -ne 0) { Write-Error "Failed to add $MUSL_TARGET target"; exit 1 }
    }
}

Assert-Command "python3" "Python 3"

if (-not (Test-Path $Tar2Ext4Path)) {
    Write-Error "tar2ext4.exe not found at $Tar2Ext4Path"
    exit 1
}

Write-Host "=== Building microsandbox boot bundle v$Version ===" -ForegroundColor Cyan

# ---------------------------------------------------------------------------
# Step 1: Cross-compile bootstrap binary
# ---------------------------------------------------------------------------
if (-not $SkipCrossCompile) {
    Write-Host "--- Cross-compiling bootstrap (musl) ---" -ForegroundColor Yellow
    Push-Location $BOOTSTRAP_DIR
    try {
        & cargo zigbuild --release --target $MUSL_TARGET
        if ($LASTEXITCODE -ne 0) { Write-Error "bootstrap cross-compilation failed"; exit 1 }
    } finally {
        Pop-Location
    }

    if (-not (Test-Path $BOOTSTRAP_BIN)) {
        Write-Error "bootstrap binary not found at $BOOTSTRAP_BIN"
        exit 1
    }
    $size = (Get-Item $BOOTSTRAP_BIN).Length / 1KB
    Write-Host "Bootstrap: $([math]::Round($size))KB"
} else {
    Write-Host "--- Skipping bootstrap cross-compilation ---" -ForegroundColor Yellow
    if (-not (Test-Path $BOOTSTRAP_BIN)) {
        Write-Error "bootstrap binary not found at $BOOTSTRAP_BIN (use -SkipCrossCompile only with pre-built binaries)"
        exit 1
    }
}

# ---------------------------------------------------------------------------
# Step 2: Cross-compile portal binary
# ---------------------------------------------------------------------------
if (-not $SkipCrossCompile) {
    Write-Host "--- Cross-compiling portal (musl) ---" -ForegroundColor Yellow
    Push-Location $REPO_ROOT
    try {
        & cargo zigbuild --release --target $MUSL_TARGET -p microsandbox-portal --bin portal
        if ($LASTEXITCODE -ne 0) { Write-Error "portal cross-compilation failed"; exit 1 }
    } finally {
        Pop-Location
    }

    if (-not (Test-Path $PORTAL_BIN)) {
        Write-Error "portal binary not found at $PORTAL_BIN"
        exit 1
    }
    $size = (Get-Item $PORTAL_BIN).Length / 1KB
    Write-Host "Portal: $([math]::Round($size))KB"
} else {
    Write-Host "--- Skipping portal cross-compilation ---" -ForegroundColor Yellow
    if (-not (Test-Path $PORTAL_BIN)) {
        Write-Error "portal binary not found at $PORTAL_BIN (use -SkipCrossCompile only with pre-built binaries)"
        exit 1
    }
}

# ---------------------------------------------------------------------------
# Step 3: Build rootfs.vhd via Python tarfile + tar2ext4
# ---------------------------------------------------------------------------
Write-Host "--- Building rootfs.vhd ---" -ForegroundColor Yellow

New-Item -ItemType Directory -Path $OutputDir -Force | Out-Null

$ROOTFS_VHD = Join-Path $OutputDir "rootfs.vhd"

# Create a temporary Python script that builds a tar with correct Unix permissions.
# Windows tar doesn't preserve Unix executable bits, so we use Python tarfile
# to set mode=0o755 and uid/gid=0 on all entries.
$pyScript = @"
import tarfile, tempfile, shutil, os, sys

bootstrap_path = sys.argv[1]
portal_path = sys.argv[2]
tar_output = sys.argv[3]

staging = tempfile.mkdtemp()
try:
    # Create directory structure expected by bootstrap init
    for d in ['proc', 'sys', 'dev', 'tmp', 'mnt/layers', 'mnt/patch',
              'mnt/scratch', 'mnt/merged', 'usr/local/bin', 'etc', 'old_root']:
        os.makedirs(os.path.join(staging, d), exist_ok=True)

    # Copy binaries
    shutil.copy2(bootstrap_path, os.path.join(staging, 'bootstrap'))
    shutil.copy2(portal_path, os.path.join(staging, 'portal'))

    # Build tar with correct permissions
    with tarfile.open(tar_output, 'w') as tar:
        for root, dirs, files in os.walk(staging):
            for name in dirs + files:
                full = os.path.join(root, name)
                arcname = os.path.relpath(full, staging)
                info = tar.gettarinfo(full, arcname=arcname)
                info.uid = 0
                info.gid = 0
                info.uname = 'root'
                info.gname = 'root'
                info.mode = 0o755
                if info.isreg():
                    with open(full, 'rb') as f:
                        tar.addfile(info, f)
                else:
                    tar.addfile(info)
finally:
    shutil.rmtree(staging)
"@

$pyScriptPath = Join-Path $env:TEMP "msb_build_rootfs.py"
$tarPath = Join-Path $env:TEMP "msb_rootfs.tar"
try {
    Set-Content -Path $pyScriptPath -Value $pyScript -Encoding UTF8

    & python3 $pyScriptPath $BOOTSTRAP_BIN $PORTAL_BIN $tarPath
    if ($LASTEXITCODE -ne 0) { Write-Error "Python rootfs tar creation failed"; exit 1 }

    # Convert tar to ext4 VHD
    & $Tar2Ext4Path -i $tarPath -o $ROOTFS_VHD -vhd
    if ($LASTEXITCODE -ne 0) { Write-Error "tar2ext4 conversion failed"; exit 1 }

    $size = (Get-Item $ROOTFS_VHD).Length / 1MB
    Write-Host "rootfs.vhd: $([math]::Round($size, 1))MB"
} finally {
    Remove-Item -Path $pyScriptPath -ErrorAction SilentlyContinue
    Remove-Item -Path $tarPath -ErrorAction SilentlyContinue
}

# ---------------------------------------------------------------------------
# Step 4: Copy binaries into bundle
# ---------------------------------------------------------------------------
Write-Host "--- Assembling bundle ---" -ForegroundColor Yellow

Copy-Item $BOOTSTRAP_BIN (Join-Path $OutputDir "bootstrap") -Force
Copy-Item $PORTAL_BIN (Join-Path $OutputDir "portal") -Force

if (Test-Path $KernelPath) {
    Copy-Item $KernelPath (Join-Path $OutputDir "kernel") -Force
} else {
    Write-Warning "Kernel not found at $KernelPath - add it manually"
}

Copy-Item $Tar2Ext4Path (Join-Path $OutputDir "tar2ext4.exe") -Force

# ---------------------------------------------------------------------------
# Step 5: Generate manifest with SHA256
# ---------------------------------------------------------------------------
Write-Host "--- Generating manifest ---" -ForegroundColor Yellow

$sha256 = (Get-FileHash $ROOTFS_VHD -Algorithm SHA256).Hash.ToLower()
$createdAt = (Get-Date).ToUniversalTime().ToString("yyyy-MM-ddTHH:mm:ssZ")

$manifest = @{
    version          = $Version
    min_host_version = "0.2.0"
    sha256           = $sha256
    created_at       = $createdAt
} | ConvertTo-Json -Depth 2

Set-Content -Path (Join-Path $OutputDir "manifest.json") -Value $manifest -Encoding UTF8

# ---------------------------------------------------------------------------
# Done
# ---------------------------------------------------------------------------
Write-Host ""
Write-Host "=== Boot bundle v$Version built at $OutputDir ===" -ForegroundColor Green
Get-ChildItem $OutputDir | Format-Table Name, @{N="Size";E={"{0:N0} KB" -f ($_.Length / 1KB)}} -AutoSize
