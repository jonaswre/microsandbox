#!/bin/bash
# Build a complete boot bundle for Windows-hosted Linux VMs.
#
# Cross-compiles the bootstrap init binary and portal for linux-musl,
# builds the rootfs VHD, and packages everything as a versioned bundle.
#
# Usage:
#   ./scripts/build_boot_bundle.sh [--version VERSION]
#
# Prerequisites:
#   - cross (cargo cross-compilation tool): cargo install cross
#   - Docker (used by cross for musl targets)
#   - tar2ext4 binary in PATH or specified via TAR2EXT4 env var
#   - Linux kernel binary at KERNEL_PATH env var or ./kernel
#
# Output:
#   target/release/boot-bundle/<version>/
#     kernel, rootfs.vhd, bootstrap, portal, tar2ext4.exe, manifest.json

set -euo pipefail

VERSION="${1:-0.2.6}"
MUSL_TARGET="x86_64-unknown-linux-musl"
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

BUNDLE_DIR="$REPO_ROOT/target/release/boot-bundle/$VERSION"
BOOTSTRAP_DIR="$REPO_ROOT/microsandbox-bootstrap"
BOOTSTRAP_BIN="$BOOTSTRAP_DIR/target/$MUSL_TARGET/release/bootstrap"
PORTAL_BIN="$REPO_ROOT/target/$MUSL_TARGET/release/portal"

# Resolve tool paths from env or defaults.
TAR2EXT4="${TAR2EXT4:-tar2ext4}"
KERNEL_PATH="${KERNEL_PATH:-$REPO_ROOT/kernel}"

echo "=== Building microsandbox boot bundle v$VERSION ==="

# Step 1: Cross-compile bootstrap binary.
echo "--- Cross-compiling bootstrap (musl) ---"
(cd "$BOOTSTRAP_DIR" && cross build --release --target "$MUSL_TARGET")
if [ ! -f "$BOOTSTRAP_BIN" ]; then
    echo "Error: bootstrap binary not found at $BOOTSTRAP_BIN"
    exit 1
fi
echo "Bootstrap: $(du -h "$BOOTSTRAP_BIN" | cut -f1)"

# Step 2: Cross-compile portal binary.
echo "--- Cross-compiling portal (musl) ---"
(cd "$REPO_ROOT" && cross build --release --target "$MUSL_TARGET" -p microsandbox-portal --bin portal)
if [ ! -f "$PORTAL_BIN" ]; then
    echo "Error: portal binary not found at $PORTAL_BIN"
    exit 1
fi
echo "Portal: $(du -h "$PORTAL_BIN" | cut -f1)"

# Step 3: Build rootfs VHD.
echo "--- Building rootfs VHD ---"
mkdir -p "$BUNDLE_DIR"
"$SCRIPT_DIR/build_rootfs_vhd.sh" "$BOOTSTRAP_BIN" "$PORTAL_BIN" "$TAR2EXT4" "$BUNDLE_DIR/rootfs.vhd"

# Step 4: Copy binaries into bundle.
echo "--- Assembling bundle ---"
cp "$BOOTSTRAP_BIN" "$BUNDLE_DIR/bootstrap"
cp "$PORTAL_BIN" "$BUNDLE_DIR/portal"

if [ -f "$KERNEL_PATH" ]; then
    cp "$KERNEL_PATH" "$BUNDLE_DIR/kernel"
else
    echo "Warning: kernel not found at $KERNEL_PATH — add it manually"
fi

if command -v "$TAR2EXT4" &>/dev/null; then
    cp "$(command -v "$TAR2EXT4")" "$BUNDLE_DIR/tar2ext4.exe" 2>/dev/null || true
fi

# Step 5: Generate manifest.
echo "--- Generating manifest ---"
SHA256=$(sha256sum "$BUNDLE_DIR/rootfs.vhd" | cut -d' ' -f1)
CREATED_AT=$(date -u +"%Y-%m-%dT%H:%M:%SZ")

cat > "$BUNDLE_DIR/manifest.json" << EOF
{
  "version": "$VERSION",
  "min_host_version": "0.2.0",
  "sha256": "$SHA256",
  "created_at": "$CREATED_AT"
}
EOF

echo "=== Boot bundle v$VERSION built at $BUNDLE_DIR ==="
ls -lh "$BUNDLE_DIR/"
