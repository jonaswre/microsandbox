---
name: windows-hcs-hcn
description: HCS compute system creation, HCN NAT networking, LinuxKernelDirect boot, VHD attachment, Plan9 shares, hcsshim patterns
user-invocable: false
---

# Windows HCS/HCN Backend

## Fixed decisions

1. **HCS** (Host Compute Service), not WSL2, QEMU, Docker Desktop, or libkrun.
2. **HCN NAT** networking with per-sandbox endpoints and load balancers.
3. **Plan9** shares for host directory mounts, not VirtualSmb.
4. **LinuxKernelDirect** boot with kernel + initrd, not EFI or full VM image.
5. **Rust access** via `windows-sys` crate, not abandoned `hcs-rs` (last updated 2020).
6. **Runtime worker** (`msbrun-hcs.exe`) owns HCS lifecycle — control plane never calls HCS directly.

## Current libkrun wrapper — what it replaces

`microsandbox-core/lib/vm/microvm.rs` — MicroVm struct (lines 52-60):

```rust
pub struct MicroVm {
    ctx_id: u32,
    config: MicroVmConfig,
}
```

`microsandbox-core/lib/vm/ffi.rs` — 15+ FFI functions:

```rust
#[link(name = "krun")]
unsafe extern "C" {
    pub(crate) fn krun_create_ctx() -> i32;
    pub(crate) fn krun_start_enter(ctx_id: u32) -> i32;
    pub(crate) fn krun_set_root(ctx_id: u32, root_path: *const c_char) -> i32;
    pub(crate) fn krun_set_overlayfs_root(ctx_id: u32, root_layers: *const *const c_char) -> i32;
    pub(crate) fn krun_add_virtiofs(ctx_id: u32, c_tag: *const c_char, c_path: *const c_char) -> i32;
    pub(crate) fn krun_set_port_map(ctx_id: u32, c_port_map: *const *const c_char) -> i32;
    // ...
}
```

On Windows, all of this is replaced by HCS JSON schema + compute system lifecycle.

## HCS compute system creation

JSON document passed to `HcsCreateComputeSystem`:

```json
{
    "Owner": "microsandbox",
    "SchemaVersion": { "Major": 2, "Minor": 1 },
    "VirtualMachine": {
        "StopOnReset": true,
        "Chipset": {
            "LinuxKernelDirect": {
                "KernelFilePath": "C:\\...\\runtime\\windows\\uvm\\0.5.0\\kernel",
                "InitRdPath": "C:\\...\\runtime\\windows\\uvm\\0.5.0\\rootfs.vhd",
                "KernelCmdLine": "console=ttyS0 panic=-1"
            }
        },
        "ComputeTopology": {
            "Memory": { "SizeInMB": 1024 },
            "Processor": { "Count": 2 }
        },
        "Devices": {
            "Scsi": {
                "primary": {
                    "Attachments": {
                        "0": {
                            "Path": "C:\\...\\layers\\sha256_abc\\layer.vhd",
                            "Type": "VirtualDisk",
                            "ReadOnly": true
                        },
                        "1": {
                            "Path": "C:\\...\\sandboxes\\mybox\\scratch.vhdx",
                            "Type": "VirtualDisk",
                            "ReadOnly": false
                        }
                    }
                }
            },
            "Plan9": {
                "Shares": [
                    {
                        "Name": "control",
                        "AccessName": "control",
                        "Path": "C:\\...\\sandboxes\\mybox\\control",
                        "Port": 1,
                        "ReadOnly": true,
                        "Flags": 13
                    },
                    {
                        "Name": "workspace",
                        "AccessName": "workspace",
                        "Path": "C:\\Users\\jonas\\project",
                        "Port": 2,
                        "ReadOnly": false,
                        "Flags": 12
                    }
                ]
            }
        }
    }
}
```

Plan9 share flags:
- `0x1` — ReadOnly
- `0x4` — LinuxMetadata (preserve uid/gid/mode)
- `0x8` — CaseSensitive

## HCS lifecycle

```
Create → Start → Wait → Terminate → Close
```

```rust
// Pseudocode using windows-sys
unsafe {
    let doc = CString::new(hcs_json).unwrap();
    let mut handle: HCS_SYSTEM = std::ptr::null_mut();
    let mut result: *mut u16 = std::ptr::null_mut();

    // Create
    HcsCreateComputeSystem(doc.as_ptr(), &mut handle, &mut result);

    // Start
    HcsStartComputeSystem(handle, std::ptr::null(), &mut result);

    // Wait for exit
    HcsWaitForComputeSystemExit(handle, INFINITE, &mut result);

    // Terminate (if needed)
    HcsTerminateComputeSystem(handle, std::ptr::null(), &mut result);

    // Close handle
    HcsCloseComputeSystem(handle);
}
```

## HCN NAT network

Shared NAT network created once for all sandboxes:

```json
{
    "SchemaVersion": { "Major": 2, "Minor": 0 },
    "Name": "MicrosandboxNAT",
    "Type": "NAT",
    "Ipams": [{
        "Type": "Static",
        "Subnets": [{
            "IpAddressPrefix": "172.28.0.0/16",
            "Routes": [{ "NextHop": "172.28.0.1", "DestinationPrefix": "0.0.0.0/0" }]
        }]
    }]
}
```

Type values: `0` = NAT, `1` = Transparent, `2` = L2Bridge.

### Per-sandbox endpoint

```json
{
    "SchemaVersion": { "Major": 2, "Minor": 0 },
    "HostComputeNetwork": "<network-id>",
    "IpConfigurations": [{
        "IpAddress": "172.28.0.10",
        "PrefixLength": 16
    }]
}
```

### Per-port load balancer

```json
{
    "SchemaVersion": { "Major": 2, "Minor": 0 },
    "HostComputeEndpoints": ["<endpoint-id>"],
    "SourceVIP": "0.0.0.0",
    "FrontendVIPs": ["127.0.0.1"],
    "PortMappings": [{
        "Protocol": 6,
        "InternalPort": 4444,
        "ExternalPort": 9001
    }],
    "Flags": 0
}
```

Protocol: `6` = TCP, `17` = UDP.

## hcsshim Go patterns (reference)

These are the Go patterns from Microsoft's `hcsshim` that inform the design:

```go
// CreateLCOW — create a Linux Container on Windows
func CreateLCOW(ctx context.Context, opts *OptionsLCOW) (*UtilityVM, error)

// OptionsLCOW struct
type OptionsLCOW struct {
    KernelFile       string
    InitRDFile       string
    KernelBootOptions string
    VPMemDeviceCount int32
    ProcessorCount   int32
    MemorySizeInMB   int64
}

// AddPlan9 — add a Plan9 share
func (uvm *UtilityVM) AddPlan9(
    ctx context.Context,
    hostPath string,
    uvmPath string,
    readOnly bool,
    restrict []string,
    allowedFiles []string,
) error

// AddSCSI — attach a VHD
func (uvm *UtilityVM) AddSCSI(
    ctx context.Context,
    hostPath string,
    uvmPath string,
    readOnly bool,
    vmAccess VMAccessType,
) (*SCSIMount, error)

// tar2ext4.Convert — convert tar to ext4 VHD
func Convert(r io.Reader, w io.ReadWriteSeeker, opts ...Option) error
// Options: ConvertWhiteout, AppendVhdFooter, ConvertBackslash, MaximumDiskSize
```

## Rust access via windows-sys

Required features:

```toml
[target.'cfg(windows)'.dependencies]
windows-sys = { version = "0.59", features = [
    "Win32_System_HostComputeSystem",
    "Win32_System_HostComputeNetwork",
] }
```

Key function signatures:

```rust
// HCS
fn HcsCreateComputeSystem(id: PCWSTR, configuration: PCWSTR, ...) -> HRESULT;
fn HcsStartComputeSystem(computeSystem: HCS_SYSTEM, ...) -> HRESULT;
fn HcsTerminateComputeSystem(computeSystem: HCS_SYSTEM, ...) -> HRESULT;
fn HcsCloseComputeSystem(computeSystem: HCS_SYSTEM);

// HCN
fn HcnCreateNetwork(id: *const GUID, settings: PCWSTR, ...) -> HRESULT;
fn HcnCreateEndpoint(network: HCN_NETWORK, id: *const GUID, settings: PCWSTR, ...) -> HRESULT;
fn HcnCreateLoadBalancer(id: *const GUID, settings: PCWSTR, ...) -> HRESULT;
```

## Anti-patterns

- **Don't port libkrun to Windows.** HCS is the correct Windows VM API.
- **Don't use VirtualSmb for Linux guests.** Microsoft's own LCOW uses Plan9.
- **Don't call HCS from the control plane.** The runtime worker (`msbrun-hcs.exe`) owns HCS resources.
- **Don't build on abandoned `hcs-rs` crate** (rafawo/hcs-rs, last update 2020). Use `windows-sys` directly.
- **Don't construct HCS JSON by string concatenation.** Use `serde_json` to build the document.
- **Don't share HCN endpoints between sandboxes.** Each sandbox gets its own endpoint for isolation and cleanup.
