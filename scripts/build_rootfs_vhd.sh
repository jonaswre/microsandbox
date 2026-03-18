#!/bin/bash
# Build the rootfs VHD for the Windows boot bundle.
#
# Creates a minimal ext4 VHD containing the bootstrap init binary, the portal
# binary, and standard mount-point directories. This VHD is loaded by HCS as
# the initial root filesystem for the Linux VM.
#
# Usage:
#   ./scripts/build_rootfs_vhd.sh <bootstrap_binary> <portal_binary> <tar2ext4> <output_vhd>
#
# Requirements:
#   - tar2ext4 binary (from hcsshim, runs on Linux or Windows)
#   - Bootstrap and portal binaries cross-compiled for x86_64-unknown-linux-musl
#
# This script should be run in a Linux environment (native, WSL, or Docker).

set -euo pipefail

if [ $# -ne 4 ]; then
    echo "Usage: $0 <bootstrap_binary> <portal_binary> <tar2ext4> <output_vhd>"
    exit 1
fi

BOOTSTRAP="$1"
PORTAL="$2"
TAR2EXT4="$3"
OUTPUT="$4"

# Validate inputs.
for f in "$BOOTSTRAP" "$PORTAL" "$TAR2EXT4"; do
    if [ ! -f "$f" ]; then
        echo "Error: file not found: $f"
        exit 1
    fi
done

# Create staging directory.
STAGING=$(mktemp -d)
trap 'rm -rf "$STAGING" "$TARBALL"' EXIT

# Create directory structure expected by the bootstrap init.
mkdir -p "$STAGING"/{proc,sys,dev,tmp}
mkdir -p "$STAGING"/mnt/{layers,patch,scratch,merged}
mkdir -p "$STAGING"/usr/local/bin
mkdir -p "$STAGING"/etc
mkdir -p "$STAGING"/old_root

# Copy binaries.
cp "$BOOTSTRAP" "$STAGING/bootstrap"
chmod 755 "$STAGING/bootstrap"

cp "$PORTAL" "$STAGING/portal"
chmod 755 "$STAGING/portal"

# Create tarball from staging directory.
TARBALL=$(mktemp --suffix=.tar)
tar -C "$STAGING" -cf "$TARBALL" .

# Convert to ext4 VHD via tar2ext4.
"$TAR2EXT4" -i "$TARBALL" -o "$OUTPUT" -vhd

echo "rootfs.vhd created at $OUTPUT ($(du -h "$OUTPUT" | cut -f1))"
