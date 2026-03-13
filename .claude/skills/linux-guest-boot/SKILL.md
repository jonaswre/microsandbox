---
name: linux-guest-boot
description: Linux guest boot chain, OverlayFS mechanics, VHD from tar, guest bootstrap contract, boot bundle layout, layer attachment model
user-invocable: false
---

# Linux Guest Boot on Windows

## Boot chain

```
kernel → initrd/rootfs.vhd → bootstrap binary → overlay assembly → portal start → payload
```

1. HCS boots Linux kernel via `LinuxKernelDirect`.
2. initrd or boot rootfs VHD provides the stable utility VM environment.
3. Bootstrap binary reads `sandbox-spec.json` from the control Plan9 share.
4. Bootstrap mounts layer VHDs (SCSI) and assembles overlayfs.
5. Bootstrap mounts user Plan9 shares into the merged root.
6. Bootstrap writes generated `/etc/resolv.conf`, `/etc/hosts` if needed.
7. Portal starts on guest port 4444.
8. Payload (user workload) is launched in the merged root.

## Current overlay model

`microsandbox-core/lib/vm/microvm.rs` lines 80-87:

```rust
pub enum Rootfs {
    Native(PathBuf),                // Single host directory
    Overlayfs(Vec<PathBuf>),        // List of host directories as layers
}
```

On Unix, `libkrun` accepts host directory paths directly and assembles overlayfs inside the VM.

On Windows, the layers must be VHD-backed and the guest assembles the overlay.

## OverlayFS mechanics

```bash
mount -t overlay overlay \
    -o lowerdir=/layer3:/layer2:/layer1,\
       upperdir=/scratch/upper,\
       workdir=/scratch/work \
    /merged
```

Rules:
- `lowerdir` layers are stacked right-to-left (last listed = bottom).
- `upperdir` and `workdir` must be on the same filesystem.
- All layers are read-only except `upperdir`.
- The guest bootstrap creates the mount command from `sandbox-spec.json`.

## VHD creation from tar

Two approaches for converting OCI layer tars to ext4 VHDs:

### hcsshim tar2ext4 (recommended for initial implementation)

```go
import "github.com/microsoft/hcsshim/ext4/tar2ext4"

f, _ := os.Create("layer.vhd")
tar2ext4.Convert(
    tarReader,
    f,
    tar2ext4.ConvertWhiteout,     // Handle OCI .wh. files
    tar2ext4.AppendVhdFooter,     // Make it a valid VHD
    tar2ext4.ConvertBackslash,    // Normalize path separators
    tar2ext4.MaximumDiskSize(10*1024*1024*1024), // 10GB max
)
```

### ext4_rs crate (potential native Rust path)

```rust
// ext4_rs v1.3.3 — pure Rust, read+write
use ext4_rs::{Ext4, BlockDevice};

struct VhdBlockDevice { file: File, size: u64 }

impl BlockDevice for VhdBlockDevice {
    fn read_offset(&self, offset: usize) -> Vec<u8> { /* ... */ }
    fn write_offset(&self, offset: usize, data: &[u8]) { /* ... */ }
}
```

## Boot bundle layout

```text
%LOCALAPPDATA%\Microsandbox\runtime\windows\uvm\<runtime-version>\
  kernel              # Linux kernel binary
  rootfs.vhd          # Utility VM root filesystem
  bootstrap           # Guest bootstrap binary (Linux ELF)
  portal              # Portal binary (Linux ELF)
  manifest.json       # Version and compatibility metadata
```

`manifest.json`:

```json
{
    "version": "0.5.0",
    "min_host_version": "0.5.0",
    "kernel": "kernel",
    "rootfs": "rootfs.vhd",
    "bootstrap": "bootstrap",
    "portal": "portal",
    "created_at": "2026-03-13T00:00:00Z"
}
```

## GuestBootstrapSpec

JSON read from `/run/microsandbox/control/sandbox-spec.json`:

```rust
struct GuestBootstrapSpec {
    sandbox_id: String,
    lower_layers: Vec<LayerAttachment>,
    patch_layer: Option<LayerAttachment>,
    scratch: ScratchAttachment,
    mounts: Vec<GuestMount>,
    workdir: Option<String>,
    exec: GuestExecSpec,
    env: Vec<(String, String)>,
    portal: PortalSpec,
    dns: DnsSpec,
}

struct LayerAttachment {
    scsi_controller: u32,
    scsi_lun: u32,
    filesystem: String,  // "ext4"
    readonly: bool,
}

struct ScratchAttachment {
    scsi_controller: u32,
    scsi_lun: u32,
    filesystem: String,  // "ext4"
}

struct GuestMount {
    plan9_name: String,
    guest_path: String,
    readonly: bool,
}

struct GuestExecSpec {
    path: String,
    args: Vec<String>,
}

struct PortalSpec {
    port: u16,       // 4444
    binary: String,  // path to portal binary
}

struct DnsSpec {
    nameservers: Vec<String>,
}
```

## Guest bootstrap operations (8-step sequence)

1. **Mount layer devices**: SCSI-attached VHDs → `/mnt/layer0`, `/mnt/layer1`, etc.
2. **Mount scratch device**: SCSI-attached VHDX → `/mnt/scratch` (contains `upper/` and `work/`).
3. **Assemble overlayfs**: `mount -t overlay` with lower layers + scratch upper/work → `/merged`.
4. **Mount Plan9 shares**: Control share → `/run/microsandbox/control`, user shares → declared guest paths.
5. **Apply DNS/hosts**: Write `/merged/etc/resolv.conf` and `/merged/etc/hosts` from spec.
6. **Start portal**: Launch portal binary bound to guest port 4444.
7. **Launch payload**: `chroot /merged` + exec the user command with env and workdir.
8. **Forward exit**: Mirror payload exit status to the VM console / exit.

## Layer attachment model

Three categories of attached storage:

| Category | Format | Mount | Lifecycle |
|---|---|---|---|
| OCI layer VHDs | ext4 VHD, read-only | SCSI, overlayfs lowerdir | Cached globally per digest |
| Patch layer VHD | ext4 VHD, read-only | SCSI, topmost lowerdir | Per-sandbox, regenerated on config change |
| Scratch VHDX | ext4 VHDX, read-write | SCSI, upperdir + workdir | Per-sandbox instance, deleted on teardown |

The patch layer replaces current Unix host-side patching:

| Unix patching function | Windows equivalent |
|---|---|
| `patch_with_sandbox_scripts` (rootfs.rs:57-90) | Scripts baked into patch VHD `/.sandbox/scripts/` |
| `patch_with_virtiofs_mounts` (rootfs.rs:117-173) | Mount points in patch VHD + declarations in `sandbox-spec.json` |
| `patch_with_default_dns_settings` (rootfs.rs:274-322) | DNS config in `sandbox-spec.json`, guest writes resolv.conf |
| `patch_with_stat_override` (rootfs.rs:336-363) | Root dir metadata in patch VHD (no xattr needed) |

## Anti-patterns

- **Don't edit fstab from Windows.** Guest bootstrap handles mounts from `sandbox-spec.json`.
- **Don't mount NTFS as rootfs.** Linux rootfs must be ext4 VHD-backed.
- **Don't try to set xattrs on NTFS.** Metadata lives inside the ext4 VHDs.
- **Don't make the OCI image the VM boot image.** The utility VM is a separate, stable boot artifact.
- **Don't pass large specs via kernel command line.** Use the Plan9 control share with `sandbox-spec.json`.
- **Don't skip the scratch layer.** Even read-only workloads need upperdir/workdir for overlayfs.
