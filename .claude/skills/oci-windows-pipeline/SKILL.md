---
name: oci-windows-pipeline
description: OCI layer to ext4 VHD conversion, whiteout handling, metadata preservation, Windows cache layout, patch layer generation, scratch VHDX
user-invocable: false
---

# OCI Layer to VHD Pipeline (Windows)

## Pipeline overview

```
OCI registry → pull layer tar.gz → decompress → convert to ext4 VHD → cache by digest
```

Each OCI layer becomes a cached, immutable ext4 VHD artifact on the Windows host.

## Current extraction — what it does on Unix

`microsandbox-core/lib/oci/layer/extraction.rs` lines 99-211 — `extract_tar_with_ownership_override`:

Two-pass extraction:
1. Regular files, directories, symlinks — extracted with uid/gid/mode preserved via xattrs.
2. Hard links — created after all regular files exist.

Metadata preservation (lines 50, 54-76):

```rust
let stat_data = format!("{}:{}:0{:o}", uid, gid, mode);
// On Linux:
libc::setxattr(path, "user.containers.override_stat", stat_data, stat_data.len(), 0);
// On macOS: similar with position parameter
```

Permission adjustment (lines 172-182):

```rust
// Files: ensure at least u+rw (0o600)
let desired = current_permission_bits | 0o600;
// Directories: ensure at least u+rwx (0o700)
let desired = current_permission_bits | 0o700;
```

Mode calculation with file type bits (lines 20-38):

```rust
fn get_full_mode(entry_type: &EntryType, permission_bits: u32) -> u32 {
    let file_type_bits = if entry_type.is_file() { libc::S_IFREG as u32 }
        else if entry_type.is_dir() { libc::S_IFDIR as u32 }
        else if entry_type.is_symlink() { libc::S_IFLNK as u32 }
        // ... S_IFBLK, S_IFCHR, S_IFIFO
    file_type_bits | permission_bits
}
```

## Whiteout handling

Constants from `microsandbox-core/lib/management/rootfs.rs`:

```rust
pub const OPAQUE_WHITEOUT_MARKER: &str = ".wh..wh..opq";  // line 23
pub const WHITEOUT_PREFIX: &str = ".wh.";                   // line 26
```

OCI whiteout semantics:
- `.wh.filename` — delete `filename` from lower layers.
- `.wh..wh..opq` — opaque directory: hide all lower layer contents.

On Windows, `tar2ext4.ConvertWhiteout` handles this during VHD creation:
- `.wh.` files become character device nodes with major 0, minor 0 (Linux whiteout).
- `.wh..wh..opq` becomes xattr `trusted.overlay.opaque=y` on the directory.

## tar2ext4 conversion

Using hcsshim's Go tool (initial approach):

```go
tar2ext4.Convert(tarReader, vhdWriter,
    tar2ext4.ConvertWhiteout,       // OCI .wh. → Linux whiteout devices
    tar2ext4.AppendVhdFooter,       // Valid VHD footer
    tar2ext4.ConvertBackslash,      // \ → / in paths
    tar2ext4.MaximumDiskSize(10<<30), // 10GB limit
)
```

Future native Rust approach using `ext4_rs`:

```rust
use ext4_rs::{Ext4, BlockDevice};

fn tar_to_ext4_vhd(tar_path: &Path, vhd_path: &Path) -> Result<()> {
    let mut vhd = VhdFile::create(vhd_path, max_size)?;
    let mut ext4 = Ext4::create(&mut vhd, block_size)?;

    let tar = File::open(tar_path)?;
    let decoder = flate2::read::GzDecoder::new(tar);
    let mut archive = tar::Archive::new(decoder);

    for entry in archive.entries()? {
        let entry = entry?;
        let path = entry.path()?;
        let header = entry.header();

        // Preserve uid, gid, mode, symlinks
        ext4.create_file(&path, header.uid()?, header.gid()?, header.mode()?)?;
        // Handle whiteouts
        if is_whiteout(&path) { /* create character device 0,0 */ }
        if is_opaque_whiteout(&path) { /* set xattr trusted.overlay.opaque=y */ }
    }

    vhd.append_footer()?;
    Ok(())
}
```

## Windows cache layout

```text
%LOCALAPPDATA%\Microsandbox\cache\windows\layers\<digest>\
  layer.vhd           # Immutable ext4 VHD
  manifest.json        # Layer metadata
```

`manifest.json`:

```json
{
    "digest": "sha256:abc123...",
    "media_type": "application/vnd.oci.image.layer.v1.tar+gzip",
    "size_bytes": 52428800,
    "vhd_size_bytes": 67108864,
    "created_at": "2026-03-13T00:00:00Z"
}
```

Cache key: OCI digest (deterministic).
Cache invalidation: never (layers are immutable by digest).

## Per-sandbox patch layer

Replaces the current Unix host-side patching functions:

| Unix function | What it patches | Patch VHD equivalent |
|---|---|---|
| `patch_with_sandbox_scripts` | `/.sandbox/scripts/*` | Bake scripts into patch ext4 VHD |
| `patch_with_virtiofs_mounts` | `/etc/fstab` + mount point dirs | Mount point directories only (no fstab) |
| `patch_with_default_dns_settings` | `/etc/resolv.conf` | DNS config in `sandbox-spec.json` instead |
| `patch_with_stat_override` | xattr on root dir | Root dir metadata inside ext4 |

Patch layer generation:

```rust
struct PatchLayerBuilder {
    scripts: HashMap<String, String>,
    mount_points: Vec<String>,     // Guest paths needing empty dirs
    dns: Option<DnsSpec>,
    hosts: Option<Vec<(String, String)>>,
}

impl PatchLayerBuilder {
    fn build_vhd(&self, output_path: &Path) -> Result<()> {
        // 1. Create small ext4 VHD
        // 2. Write /.sandbox/scripts/* with shebangs
        // 3. Create empty mount-point directories
        // 4. Optionally write /etc/resolv.conf, /etc/hosts
        // 5. Set root dir uid=0, gid=0, mode=0o755
        // 6. Append VHD footer
        Ok(())
    }
}
```

Patch VHD location:

```text
%LOCALAPPDATA%\Microsandbox\runtime\windows\sandboxes\<sandbox-id>\
  patch.vhd
```

## Scratch VHDX

Each running sandbox gets a writable scratch disk:

```text
%LOCALAPPDATA%\Microsandbox\runtime\windows\sandboxes\<sandbox-id>\
  scratch.vhdx
```

Properties:
- Format: VHDX (dynamically expanding).
- Filesystem: ext4, formatted on creation.
- Contains: `upper/` and `work/` directories for overlayfs.
- Lifecycle: created per sandbox instance, deleted on teardown.

Creation:

```rust
fn create_scratch_vhdx(path: &Path, max_size: u64) -> Result<()> {
    // 1. Create VHDX file
    // 2. Format as ext4
    // 3. Create upper/ and work/ directories
    Ok(())
}
```

## WindowsMaterializedRootfs

The output of Windows rootfs materialization:

```rust
struct WindowsMaterializedRootfs {
    boot_bundle_version: String,
    lower_layer_vhds: Vec<HostPathBuf>,     // Ordered bottom-to-top
    patch_layer_vhd: Option<HostPathBuf>,   // Topmost read-only layer
    scratch_vhdx: HostPathBuf,              // Writable layer
    control_share_dir: HostPathBuf,         // Contains sandbox-spec.json
}
```

## Anti-patterns

- **Don't extract OCI layers to NTFS directories.** NTFS cannot represent Linux metadata. Convert to ext4 VHDs.
- **Don't use NTFS xattrs (alternate data streams) for uid/gid.** Store metadata inside the ext4 filesystem.
- **Don't skip whiteout conversion.** Missing whiteouts break overlayfs layer stacking.
- **Don't cache VHDs by path or name.** Cache by OCI digest for correctness.
- **Don't create the scratch layer inside a layer VHD.** Scratch is a separate VHDX per sandbox instance.
- **Don't patch fstab on Windows.** Use `sandbox-spec.json` and guest bootstrap for mounts.
- **Don't generate patch layers in the global cache.** Patch layers are per-sandbox, stored under `runtime/windows/sandboxes/`.
