---
order: 830
label: Host Reference Design
---

# Windows Host Reference Design

## Use This Doc For

Use this document when you need the fixed Windows host architecture and the
decisions that should not be reopened during implementation.

- For scope, blockers, and phased direction, see [Overview](./overview.md).
- For the runtime decomposition that led to these decisions, see
  [Architecture Deep Dive](./architecture-deep-dive.md).
- For the concrete rollout sequence, see
  [Implementation Plan](./implementation-plan.md).

## Purpose

This document resolves the remaining open architectural questions from
[Overview](./overview.md) and
[Architecture Deep Dive](./architecture-deep-dive.md).

The goal is not to restate blockers. It is to define the concrete Windows host
architecture that this repo should implement for real Windows Server support.

## Final decisions

These decisions are fixed for the Windows host design.

1. The Windows host backend is Hyper-V via Host Compute Service (HCS), not
   `libkrun`, WSL2, Docker Desktop, or QEMU.
2. The Windows networking layer is Host Compute Networking (HCN) with a shared
   NAT network and explicit host port mappings for portal and user ports.
3. The Windows process-lifecycle primitive is a runtime worker process plus a
   Job Object, not Unix-style supervisor semantics.
4. Host directory mounts for Linux guests use HCS `Plan9` shares, not
   `host:guest` string parsing and not `VirtualSmb`.
5. The Windows guest-artifact boundary is VHD-backed Linux filesystem artifacts,
   not a Windows host directory tree pretending to be a Linux rootfs.
6. OCI image layers are converted into cached ext4 VHD artifacts. Per-sandbox
   mutations become a generated patch layer plus a writable scratch VHDX.
7. The sandbox payload is started by a guest bootstrap inside the VM, not by
   trying to emulate the current `libkrun` launch contract on the host.
8. The persisted runtime record becomes backend-neutral. `microvm_pid` and
   `rootfs_paths` are not the authoritative cross-platform state model.
9. The first production Windows implementation should use a dedicated
   Windows-only runtime helper built on top of Microsoft's `hcsshim`, because
   `hcsshim` already provides the HCS, HCN, `Plan9`, Job Object, ConPTY, and
   `tar2ext4` primitives this product needs.

## Why this is the right shape

The current Unix runtime works because `libkrun` accepts host directory trees,
overlay roots, and virtio-fs mappings directly from the host. Windows does not
have an equivalent host filesystem model for Linux guests.

Trying to preserve the current boundary would force Windows support to:

- reconstruct Linux metadata on NTFS,
- keep Unix rootfs patching semantics on a non-Unix host,
- emulate Unix supervision with Windows APIs,
- special-case every mount and path boundary.

That is the wrong architecture.

The stable boundary on Windows is:

- host control plane in Rust,
- Windows runtime worker as an isolated child process,
- HCS/HCN for VM and networking,
- VHD-backed Linux artifacts for rootfs and layers,
- guest bootstrap logic for overlay assembly and payload launch.

## Primary sources

The design below is based on Microsoft's published HCS and HCN interfaces and
Microsoft's own LCOW implementation in `hcsshim`.

- HCS overview:
  <https://learn.microsoft.com/en-us/virtualization/api/hcs/overview>
- HCS schema reference:
  <https://learn.microsoft.com/en-us/virtualization/api/hcs/reference/hcs-schema-reference>
- `HcsCreateComputeSystem`:
  <https://learn.microsoft.com/en-us/virtualization/api/hcs/reference/hcscreatecomputesystem>
- HCN overview:
  <https://learn.microsoft.com/en-us/windows-server/networking/technologies/hcn/hcn-top>
- Job Objects:
  <https://learn.microsoft.com/en-us/windows/win32/procthread/job-objects>
- CreatePseudoConsole:
  <https://learn.microsoft.com/en-us/windows/console/createpseudoconsole>
- Supported Linux guests on Hyper-V:
  <https://learn.microsoft.com/en-us/windows-server/virtualization/hyper-v/supported-linux-and-freebsd-virtual-machines-for-hyper-v-on-windows>
- `hcsshim` LCOW implementation:
  <https://github.com/microsoft/hcsshim/blob/main/internal/uvm/create_lcow.go>
- `hcsshim` `Plan9` mounts:
  <https://github.com/microsoft/hcsshim/blob/main/internal/uvm/plan9.go>
- `hcsshim` tar-to-ext4/VHD conversion:
  <https://github.com/microsoft/hcsshim/blob/main/ext4/tar2ext4/tar2ext4.go>
- `hcsshim` HCN load balancer helper:
  <https://github.com/microsoft/hcsshim/blob/main/hcn/hcnloadbalancer.go>

Inference from those sources:

- HCS and HCN are the correct native control surfaces.
- Microsoft's own Linux-on-Windows runtime path already uses
  `LinuxKernelDirect`, `Plan9`, and VHD-backed Linux layers.
- Reusing that shape is lower risk than inventing a new Windows host model.

## Rejected options

These are explicitly not the design.

### 1. Port the current `libkrun` runtime to Windows

Rejected because it leaves the repo coupled to:

- host-directory rootfs layout,
- Unix PTY and signal semantics,
- Unix metadata patching,
- current `prepare_run -> CLI flags -> child reconstructs meaning` flow.

### 2. Depend on WSL2

Rejected because the requirement is Windows Server support, not "Windows with a
developer workstation stack installed".

WSL2 is the wrong product dependency for:

- headless server operation,
- predictable service deployment,
- controlled networking and lifecycle ownership,
- shipping a native Windows server runtime.

### 3. Make raw NTFS directories first-class Linux rootfs sources

Rejected because a Linux rootfs needs semantics that NTFS does not represent
reliably for this use case:

- uid and gid,
- execute bits,
- device nodes,
- FIFO and special files,
- Linux whiteouts,
- Linux symlink behavior,
- Linux xattrs.

Windows-host project directories are valid mounted workspaces. They are not
valid authoritative Linux rootfs images.

### 4. Tunnel the server through a new host-only transport

Rejected because the current server contract is good:

- the server allocates a host port,
- the portal still listens on guest port `4444`,
- the server still talks to `127.0.0.1:<assigned_port>`.

The Windows backend must preserve that contract.

## Host architecture

### Layer 1: Rust control plane stays in charge

The Rust server and orchestration layer stay as the top-level system of record
for:

- config loading,
- sandbox resolution,
- API request handling,
- workspace management,
- state persistence,
- portal forwarding.

The control plane must not know HCS JSON details, VHD attachment details, or
HCN object layout.

### Layer 2: One Windows runtime worker per sandbox

The concrete Windows runtime shape is:

1. `msbserver` resolves a `ResolvedSandboxSpec`.
2. It materializes or looks up guest artifacts in the Windows cache.
3. It spawns one Windows runtime worker for that sandbox.
4. The worker owns the HCS compute system, HCN endpoint and load balancers,
   console pipe, and cleanup.
5. The worker exits only after the sandbox is fully stopped and resources are
   reclaimed.

The runtime worker is the Windows equivalent of the current Unix supervisor.

Recommended binary:

- `msbrun-hcs.exe`

Recommended implementation:

- Go, on top of `hcsshim`

Rationale:

- `hcsshim` already wraps HCS and HCN.
- `hcsshim` already implements the LCOW boot path this repo needs.
- `hcsshim` already ships `tar2ext4`, which solves the hardest metadata
  conversion problem on Windows.
- the helper process boundary isolates the Windows-specific surface from the
  Rust control plane.

If the repo later replaces the helper with native Rust bindings, that is an
implementation change, not an architecture change.

### Layer 3: Guest bootstrap inside the VM

Windows should not try to encode the full sandbox launch contract into HCS
process APIs.

The VM should boot a stable Microsandbox utility VM image containing:

- Linux kernel,
- init or bootstrap binary,
- portal binary,
- overlay assembly logic,
- mount orchestration logic,
- shutdown and log-forwarding logic.

The guest bootstrap is responsible for:

1. reading the per-sandbox control spec,
2. mounting the read-only layer VHDs and writable scratch disk,
3. assembling the merged rootfs with overlayfs,
4. mounting host `Plan9` shares,
5. applying generated `/etc/hosts`, DNS, and script assets,
6. starting the portal,
7. starting the sandbox payload,
8. forwarding payload output to the VM console or log sink.

This keeps rootfs semantics in Linux, where they belong.

## Boot model

### Stable utility VM boot artifact

The Windows backend boots a stable, product-owned Linux utility VM image. The
sandbox image does not become the VM boot image directly.

Required boot bundle contents:

- `kernel`
- `rootfs.vhd` or `rootfs.vhdx`
- bootstrap or init program
- portal
- release metadata

The boot bundle is versioned with the Microsandbox release and cached under the
Microsandbox home directory.

Example:

```text
%LOCALAPPDATA%\Microsandbox\runtime\windows\uvm\<runtime-version>\
  kernel
  rootfs.vhd
  bootstrap
  portal
  manifest.json
```

### Why the VM boot image is separate from the sandbox rootfs

This matches the LCOW shape used by Microsoft itself:

- the VM has a stable Linux environment,
- workload layers are attached as additional Linux filesystem artifacts,
- the guest assembles the final runtime filesystem.

This is the only clean way to support:

- OCI image layering,
- per-sandbox patching,
- writable scratch state,
- host-mounted workspaces,
- Windows-native lifecycle management.

## Guest artifact model

### Terminology

This design separates three things that are currently overloaded as "rootfs":

1. `BootRootfs`
   - the stable utility VM boot image shipped with Microsandbox
2. `SandboxLayers`
   - the workload filesystem derived from OCI layers and per-sandbox patch data
3. `WorkspaceMounts`
   - host directories shared into the guest for source code, artifacts, or
     project files

Only `SandboxLayers` represent the sandbox's Linux root filesystem.

### Immutable OCI layers

For image-based sandboxes, each OCI layer tar becomes a cached ext4 VHD
artifact.

Required conversion behavior:

- preserve uid, gid, mode, symlink, and xattr data,
- convert OCI whiteouts correctly,
- produce deterministic layer cache keys from the OCI digest,
- avoid materializing the final Linux rootfs as a Windows directory tree.

Recommended on-disk cache:

```text
%LOCALAPPDATA%\Microsandbox\cache\windows\layers\<digest>\
  layer.vhd
  manifest.json
```

The layer conversion tool should be `hcsshim`'s `tar2ext4` path, not a new
NTFS-based extractor.

### Per-sandbox patch layer

The current Unix flow mutates host directories for:

- scripts,
- DNS defaults,
- mount-point creation,
- stat overrides.

On Windows this becomes a generated Linux patch layer, not an edited NTFS tree.

For each sandbox revision, generate a small ext4 VHD patch layer that contains:

- `/.sandbox/...` scripts and metadata,
- generated `/etc/resolv.conf` and `/etc/hosts` content when required,
- empty mount-point directories inside the guest filesystem,
- any additional bootstrap inputs that are part of the guest filesystem view.

This patch layer is treated like the topmost read-only lower layer.

### Writable scratch layer

Each running sandbox gets a writable scratch VHDX.

Properties:

- created per sandbox instance,
- attached read-write,
- used as overlayfs upper and work dirs,
- deleted on sandbox teardown unless persistence is introduced later.

Recommended location:

```text
%LOCALAPPDATA%\Microsandbox\runtime\windows\sandboxes\<sandbox-id>\
  scratch.vhdx
  control\
  logs\
```

### Native rootfs on Windows

A raw Windows host directory is not a supported authoritative Linux rootfs
source.

Windows-host support will explicitly divide rootfs sources into:

- supported:
  - OCI image references
  - prebuilt Linux filesystem artifacts such as `rootfs.vhd`, `rootfs.vhdx`,
    or a Linux tar archive with preserved metadata
- unsupported:
  - arbitrary NTFS directories used as if they were Linux rootfs directories

For project workflows on Windows, the supported model is:

- boot from an OCI image or other explicit Linux artifact,
- mount the workspace separately as a host share.

That is a deliberate product decision, not a temporary limitation.

## Mount model

### Host directory mounts use `Plan9`

For Linux guests on Windows, host directory sharing uses HCS `Plan9` shares.

Why `Plan9`:

- it is the Linux guest directory-sharing path used in `hcsshim`,
- it is explicitly represented in the HCS schema for the Linux VM path,
- it matches the repo's existing "share a host directory into the guest"
  requirement.

Why not `VirtualSmb`:

- it is the wrong default abstraction for Linux guest workload mounts,
- Microsoft's LCOW implementation uses `Plan9` for Linux mapped directories.

### Control share

Every sandbox gets one additional read-only `Plan9` control share containing
the runtime-generated control spec and auxiliary files.

Example guest path:

```text
/run/microsandbox/control
```

The control share contains:

- `sandbox-spec.json`
- optional generated script files
- optional bootstrap manifests

This avoids stuffing large serialized state into kernel arguments.

### User-declared mounts

User mounts remain distinct from the rootfs and are applied after the guest
bootstrap assembles the overlay root.

Rules:

- mount declaration format becomes structured and Windows-safe,
- Windows never accepts ambiguous `host:guest` parsing,
- relative paths are resolved on the host before they reach the runtime worker,
- guest paths remain Unix paths.

## Networking model

### Shared HCN NAT network

The server creates or reuses one Microsandbox NAT network on the Windows host.

Recommended lifetime:

- created on first sandbox start,
- reused across sandbox lifetimes,
- cleaned only on explicit runtime cleanup or uninstall.

### Per-sandbox endpoint

Each sandbox gets one HCN endpoint attached to the compute system.

That endpoint is the anchor for:

- guest IP assignment,
- per-sandbox port exposure,
- cleanup and status correlation.

### Per-port HCN load balancers

Each host port mapping becomes an HCN load balancer or equivalent endpoint
policy tied to the sandbox endpoint.

That includes:

- the portal mapping `<host_port>:4444`,
- all user-declared published ports.

The control plane contract does not change:

- the server still forwards to `127.0.0.1:<assigned_port>`,
- the backend is responsible for making that host port reach the guest.

### No new server transport

The Windows backend will not replace portal networking with HvSocket,
named-pipe-only RPC, or a special side channel.

HvSocket may still be used internally later for optional control or telemetry,
but it is not the first transport the server depends on.

## Runtime control and state

### Worker ownership

The runtime worker owns:

- HCS compute system ID,
- HCN endpoint ID,
- HCN load balancer IDs,
- console pipe or log stream,
- scratch disk path,
- control-share path,
- cleanup on normal exit.

The control plane owns:

- desired state,
- persisted runtime handle,
- restart and reconciliation policy.

### Control endpoint

Each runtime worker exposes a local control endpoint over a Windows named pipe.

Recommended format:

```text
\\.\pipe\microsandbox-runtime\<runtime-id>
```

Supported commands:

- `status`
- `stop`
- `attach`
- `collect_logs`

The control plane should not talk to HCS directly for routine stop or attach
operations once the worker is running.

### Job Object policy

Each worker process is placed in a Job Object configured to tear down child
processes if the worker exits unexpectedly.

The Job Object is the Windows lifetime boundary that replaces Unix signal-tree
assumptions.

### RuntimeHandle

The persisted runtime identity becomes:

```rust
struct RuntimeHandle {
    sandbox_key: SandboxKey,
    backend_kind: BackendKind,
    runtime_id: String,
    worker_pid: u32,
    control_endpoint: String,
    backend_object_id: String,
}
```

For the Windows backend:

- `backend_kind = "windows_hcs"`
- `runtime_id` is Microsandbox-generated and stable
- `backend_object_id` is the HCS compute system ID

### RuntimeRecord

The runtime registry also stores backend metadata as JSON so the control plane
can recover or reconcile after crashes.

```rust
struct RuntimeRecord {
    runtime_handle: RuntimeHandle,
    rootfs_descriptor_json: String,
    backend_state_json: String,
    portal_port: u16,
    created_at: DateTime<Utc>,
    modified_at: DateTime<Utc>,
}
```

`backend_state_json` for Windows should contain:

- HCN endpoint ID
- HCN load balancer IDs
- scratch disk path
- control share path
- boot bundle version
- layer artifact digests

### Database migration

The current schema:

- `supervisor_pid`
- `microvm_pid`
- `rootfs_paths`

must be replaced or augmented with:

- `backend_kind`
- `runtime_id`
- `worker_pid`
- `control_endpoint`
- `backend_object_id`
- `rootfs_descriptor_json`
- `backend_state_json`

Compatibility rule:

- Unix runtime can continue populating the old fields temporarily,
- all new status, stop, and disk-usage code must pivot to the new runtime
  record,
- `microvm_pid` stops being the primary process-health check.

## Guest bootstrap contract

### Input spec

The guest bootstrap reads a JSON spec from the control share.

Recommended structure:

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
```

### Guest responsibilities

The bootstrap performs these operations in order:

1. mount read-only lower layer devices,
2. mount writable scratch device,
3. assemble overlayfs into a merged root,
4. bind or mount the user-declared `Plan9` shares into the merged root,
5. prepare generated DNS and hosts files,
6. start the portal bound to guest port `4444`,
7. launch the payload in the merged root and configured workdir,
8. mirror payload exit status to the VM exit status.

### Logging

The payload's stdout and stderr are forwarded to the VM console. The runtime
worker captures the console pipe and writes the host log files.

This preserves the current product behavior where logs remain available from the
host even though execution happens inside the guest.

### Interactive TTY

Interactive attach on Windows uses:

- guest console pipe on the VM side,
- ConPTY on the host side,
- a `TerminalBroker` in the runtime worker to shuttle bytes between them.

That is the portable replacement for the current Unix PTY assumptions.

## Concrete type design

### `ResolvedSandboxSpec`

This becomes the last control-plane type before backend-specific materialization.

```rust
struct ResolvedSandboxSpec {
    sandbox_key: SandboxKey,
    rootfs_source: RootfsSource,
    mounts: Vec<MountSpec>,
    ports: Vec<PortPair>,
    env: Vec<EnvPair>,
    workdir: Option<GuestPathBuf>,
    exec: GuestExecSpec,
    resources: ResourceSpec,
    portal: PortalSpec,
}
```

### `MaterializedRootfs`

The Windows materializer output is explicit:

```rust
struct WindowsMaterializedRootfs {
    boot_bundle_version: String,
    lower_layer_vhds: Vec<HostPathBuf>,
    patch_layer_vhd: Option<HostPathBuf>,
    scratch_vhdx: HostPathBuf,
    control_share_dir: HostPathBuf,
}
```

### `VmBackend`

The backend consumes a resolved spec and a materialized rootfs, then returns a
runtime handle.

```rust
trait VmBackend {
    type MaterializedRootfs;

    fn start(
        &self,
        spec: ResolvedSandboxSpec,
        rootfs: Self::MaterializedRootfs,
    ) -> Result<RuntimeHandle>;

    fn stop(&self, handle: &RuntimeHandle) -> Result<()>;
    fn status(&self, handle: &RuntimeHandle) -> Result<SandboxRuntimeStatus>;
}
```

For Windows, the Rust implementation of `VmBackend` is a thin wrapper over the
runtime helper process and named-pipe control channel.

## Mapping current Unix behavior to the Windows design

### `setup_image_rootfs`

Current behavior:

- pull image,
- find extracted host directories,
- generate patch dirs,
- pass host paths to `libkrun`.

Windows replacement:

- pull image,
- convert each layer tar to cached ext4 VHD,
- generate per-sandbox patch layer VHD,
- create scratch VHDX,
- pass VHD descriptors to the runtime worker.

### `patch_with_virtiofs_mounts`

Current behavior:

- create mount points and patch `fstab`.

Windows replacement:

- generate mount-point directories in the patch layer,
- put mount declarations into `sandbox-spec.json`,
- let the guest bootstrap mount `Plan9` shares at runtime.

No Windows code should edit guest `fstab`.

### `Supervisor`

Current behavior:

- Unix process tree,
- PTY master/slave,
- signal-based shutdown.

Windows replacement:

- runtime worker process,
- named-pipe control plane,
- Job Object cleanup,
- ConPTY for interactive attach,
- HCS compute-system lifecycle.

### `MicroVm`

Current behavior:

- `libkrun` wrapper.

Windows replacement:

- backend-neutral trait on the Rust side,
- helper-driven HCS lifecycle on the Windows side.

`MicroVm` cannot remain the universal abstraction name once one backend is not
`libkrun`.

## Explicit product behaviors

### Supported on Windows host

- `msb server start`
- `sandbox.start`
- OCI-image sandboxes
- host workspace mounts
- portal forwarding
- published TCP ports
- `sandbox.stop`
- log collection
- interactive attach through the runtime worker

### Not supported on Windows host

- raw NTFS directories as Linux rootfs sources
- ambiguous `host:guest` mount strings
- Unix-only lifecycle assumptions such as `kill -TERM <microvm_pid>`
- code paths that inspect overlay usage by parsing `rootfs_paths` strings

## Delivery order

This is the only order that keeps the implementation coherent.

1. Introduce host-path versus guest-path types and structured mounts.
2. Introduce backend-neutral runtime records and migrate the DB.
3. Extract the rootfs materializer boundary.
4. Add the Windows artifact cache model:
   - boot bundle
   - layer VHD cache
   - patch layer generation
   - scratch VHDX creation
5. Add the Windows runtime worker contract and named-pipe control channel.
6. Implement the HCS and HCN worker using `hcsshim`.
7. Add the guest bootstrap logic for overlay assembly and mount orchestration.
8. Wire `msb server` and orchestration to the Windows backend.

## What should change in the existing docs

After this document lands:

- [Overview](./overview.md) should stop listing backend and
  artifact questions as open.
- [Architecture Deep Dive](./architecture-deep-dive.md) should point here as
  the reference design that resolves the architectural fork.

## Bottom line

Windows Server support is not "make the current Unix runtime compile on
Windows".

It is:

- Rust control plane,
- Windows runtime worker,
- HCS compute systems,
- HCN networking,
- `Plan9` host shares,
- VHD-backed Linux layer artifacts,
- guest bootstrap assembling the final overlay root,
- backend-neutral runtime state.

That is the architecture that closes the open questions without leaving Windows
support as a permanent special case.
