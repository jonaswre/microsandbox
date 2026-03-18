# Windows Host Deep Dive

## Purpose

This document is the deeper architectural follow-up to
[WINDOWS_SUPPORT.md](WINDOWS_SUPPORT.md).

For the concrete Windows host reference architecture that resolves the backend,
artifact, and runtime-state decisions discussed here, see
[WINDOWS_HOST_REFERENCE_DESIGN.md](WINDOWS_HOST_REFERENCE_DESIGN.md).

The goal here is not just to list blockers. It is to answer a narrower question:

How should this system be decomposed so that Windows can run `msb server`, launch Linux sandboxes, and preserve the same high-level product model?

## The current system, end to end

Today the sandbox runtime path is:

1. `msb server start` launches `msbserver`.
2. API requests hit [`microsandbox-server/lib/handler.rs`](/home/jonaswre/Projects/jonaswre/microsandbox/microsandbox-server/lib/handler.rs#L350).
3. `sandbox_start_impl` writes or updates `Sandboxfile`, injects the portal port mapping, and calls `orchestra::up`.
4. [`orchestra::up`](/home/jonaswre/Projects/jonaswre/microsandbox/microsandbox-core/lib/management/orchestra.rs) calls [`sandbox::run`](/home/jonaswre/Projects/jonaswre/microsandbox/microsandbox-core/lib/management/sandbox.rs#L84).
5. [`sandbox::prepare_run`](/home/jonaswre/Projects/jonaswre/microsandbox/microsandbox-core/lib/management/sandbox.rs#L157) resolves config and rootfs, then launches `msbrun supervisor`.
6. `setup_image_rootfs` or `setup_native_rootfs` patches the guest filesystem layout in [`microsandbox-core/lib/management/sandbox.rs`](/home/jonaswre/Projects/jonaswre/microsandbox/microsandbox-core/lib/management/sandbox.rs#L540).
7. `msbrun supervisor` creates a [`Supervisor`](/home/jonaswre/Projects/jonaswre/microsandbox/microsandbox-utils/lib/runtime/supervisor.rs) and spawns `msbrun microvm`.
8. `msbrun microvm` builds a [`MicroVm`](/home/jonaswre/Projects/jonaswre/microsandbox/microsandbox-core/lib/vm/microvm.rs#L268).
9. [`MicroVm::start()`](/home/jonaswre/Projects/jonaswre/microsandbox/microsandbox-core/lib/vm/microvm.rs#L268) calls `krun_start_enter` through FFI.
10. The server forwards RPC traffic to the in-guest portal through host port mappings in [`microsandbox-server/lib/handler.rs`](/home/jonaswre/Projects/jonaswre/microsandbox/microsandbox-server/lib/handler.rs#L207).

That flow currently couples together three different concerns:

- control plane
- guest artifact preparation
- host execution runtime

Windows host support fails because only the first of those is close to portable.

## What is already portable enough

The control plane is mostly reusable:

- server HTTP and JSON-RPC routing
- project config loading and mutation
- sandbox naming and state polling
- port assignment logic
- SDK server protocol

Files mostly in this category:

- [`microsandbox-server/lib/handler.rs`](/home/jonaswre/Projects/jonaswre/microsandbox/microsandbox-server/lib/handler.rs)
- [`microsandbox-server/lib/route.rs`](/home/jonaswre/Projects/jonaswre/microsandbox/microsandbox-server/lib/route.rs)
- [`microsandbox-server/lib/state.rs`](/home/jonaswre/Projects/jonaswre/microsandbox/microsandbox-server/lib/state.rs)
- [`microsandbox-server/lib/port.rs`](/home/jonaswre/Projects/jonaswre/microsandbox/microsandbox-server/lib/port.rs)

This should stay as the stable top layer. Windows support should not begin by rewriting the server API.

## Where the current design is too fused

There are four hard couplings that need to be broken.

### 1. Sandbox preparation is tied directly to the host filesystem layout

[`sandbox::prepare_run`](/home/jonaswre/Projects/jonaswre/microsandbox/microsandbox-core/lib/management/sandbox.rs#L157) does all of these together:

- config resolution
- rootfs setup
- host path rewriting
- direct CLI argument construction for `msbrun`
- detach mode handling

That is acceptable for one Unix backend, but it is the wrong seam for Windows hosting. The preparation stage should output a runtime-neutral `ResolvedSandboxSpec`, not a half-serialized command line.

### 2. Rootfs preparation assumes a Unix host filesystem

[`setup_image_rootfs`](/home/jonaswre/Projects/jonaswre/microsandbox/microsandbox-core/lib/management/sandbox.rs#L540) and [`rootfs.rs`](/home/jonaswre/Projects/jonaswre/microsandbox/microsandbox-core/lib/management/rootfs.rs) assume:

- overlay layers are host directories
- guest metadata can be preserved using Unix mode bits
- host filesystems can carry Linux xattrs
- virtio-fs mount points can be expressed by editing `/etc/fstab`

That is a Unix-host artifact pipeline, not a general Linux-guest materialization pipeline.

### 3. Process supervision is defined in Unix terms

[`microsandbox-utils/lib/runtime/supervisor.rs`](/home/jonaswre/Projects/jonaswre/microsandbox/microsandbox-utils/lib/runtime/supervisor.rs) is not just using Unix APIs. It is modeling the process lifecycle in Unix concepts:

- `setsid`
- PTY master/slave pairs
- signal-driven shutdown
- raw file-descriptor async I/O

Windows support needs a process model based on:

- process handles
- Job Objects
- ConPTY or equivalent terminal brokering
- explicit kill/wait semantics

### 4. The VM abstraction is not an abstraction yet

[`MicroVm`](/home/jonaswre/Projects/jonaswre/microsandbox/microsandbox-core/lib/vm/microvm.rs) is a light wrapper around `libkrun` calls. It is not a backend-neutral VM model.

That means the repo currently has no place to put a Windows runtime except by duplicating the entire VM layer.

## The architecture that should exist

The system should be reorganized into five layers.

## Layer 1: Control Plane

This is the stable API-facing layer.

Responsibilities:

- accept API requests
- validate user input
- persist sandbox intent
- ask the runtime layer to realize that intent
- expose status, logs, metrics, and portal connectivity

Inputs:

- `SandboxStartParams`
- `SandboxStopParams`
- `Sandboxfile`

Outputs:

- `ResolvedSandboxSpec`
- `RuntimeHandle`
- `SandboxStatus`

This layer should know nothing about:

- `libkrun`
- PTYs
- xattrs
- VHDX
- Unix vs Windows detach

It should call a `SandboxRuntimeService`.

Suggested trait:

```rust
trait SandboxRuntimeService {
    fn start(&self, spec: ResolvedSandboxSpec) -> Result<RuntimeHandle>;
    fn stop(&self, handle: &RuntimeHandle) -> Result<()>;
    fn status(&self, handle: &RuntimeHandle) -> Result<SandboxRuntimeStatus>;
}
```

## Layer 2: Sandbox Resolution

This is the normalization layer between config and runtime.

Responsibilities:

- load config
- apply image defaults
- resolve relative host paths
- validate guest paths
- inject system requirements such as portal ports
- produce a fully normalized spec

Output type:

```rust
struct ResolvedSandboxSpec {
    sandbox_name: String,
    config_file: String,
    rootfs_source: RootfsSource,
    mounts: Vec<MountSpec>,
    port_map: Vec<PortPair>,
    env: Vec<EnvPair>,
    workdir: Option<GuestPathBuf>,
    exec: GuestExecSpec,
    resources: ResourceSpec,
    network: NetworkSpec,
    portal: PortalSpec,
}
```

This layer should own the host-path/guest-path distinction.

That means:

- relative project paths are resolved here
- Windows drive letters are parsed here
- no runtime backend should ever parse `host:guest` strings

This layer should be derived primarily from:

- [`microsandbox-core/lib/management/sandbox.rs`](/home/jonaswre/Projects/jonaswre/microsandbox/microsandbox-core/lib/management/sandbox.rs)
- [`microsandbox-core/lib/config/path_pair.rs`](/home/jonaswre/Projects/jonaswre/microsandbox/microsandbox-core/lib/config/path_pair.rs)
- [`microsandbox-core/lib/config/reference_path.rs`](/home/jonaswre/Projects/jonaswre/microsandbox/microsandbox-core/lib/config/reference_path.rs)

## Layer 3: Guest Artifact Pipeline

This is the layer that turns image data and config into something the VM backend can boot.

Responsibilities:

- image pull
- layer discovery and cache lookup
- whiteout processing
- guest script injection
- DNS and hosts patching
- mount-point creation
- guest metadata preservation
- backend-specific rootfs packaging

The key point is this:

The guest artifact pipeline should not assume that the final bootable rootfs is a host directory tree.

Instead it should output a `MaterializedRootfs`.

Suggested abstraction:

```rust
trait RootfsMaterializer {
    type MaterializedRootfs;

    fn materialize(
        &self,
        source: RootfsSource,
        patch_plan: GuestPatchPlan,
        mounts: &[MountSpec],
    ) -> Result<Self::MaterializedRootfs>;
}
```

Unix implementation:

- materialized rootfs is a list of extracted host directories
- patches happen in host directories
- metadata uses xattrs and Unix permissions

Windows implementation:

- materialized rootfs is probably a backend-native disk artifact
- metadata lives in an explicit manifest, not xattrs
- host directories are staging inputs, not the boot format

This is the single most important architectural shift for Windows hosting.

If this layer is not separated cleanly, the Windows backend will be forced to fake a Unix host filesystem model and that will stay fragile.

## Layer 4: Host Runtime

This is the layer that actually launches and supervises execution.

Responsibilities:

- own the VM backend
- translate `ResolvedSandboxSpec` plus `MaterializedRootfs` into backend calls
- manage lifecycle
- publish runtime IDs and PIDs
- handle stdout/stderr/terminal bridging

Suggested split:

- `VmBackend`
- `ProcessSupervisor`
- `TerminalBroker`
- `RuntimeRegistry`

Suggested traits:

```rust
trait VmBackend {
    type VmHandle;
    type RootfsHandle;

    fn create_vm(&self, spec: &ResolvedSandboxSpec, rootfs: Self::RootfsHandle) -> Result<Self::VmHandle>;
    fn start(&self, vm: Self::VmHandle) -> Result<BackendExitStatus>;
    fn terminate(&self, vm: &Self::VmHandle) -> Result<()>;
}

trait ProcessSupervisor {
    fn spawn_runtime(&self, req: RuntimeSpawnRequest) -> Result<SupervisedProcess>;
    fn terminate(&self, process: &SupervisedProcess) -> Result<()>;
    fn is_running(&self, process: &SupervisedProcess) -> Result<bool>;
}
```

Unix implementation:

- current `msbrun supervisor` pattern
- PTY-based TTY
- signal handling

Windows implementation:

- process handles + Job Objects
- ConPTY when interactive
- explicit termination, not signal emulation

This layer is where `msbrun` should eventually become backend-neutral instead of Unix-coded.

## Layer 5: Platform Services

This is the thin OS adapter layer.

Responsibilities:

- null device path
- install layout
- alias shims
- process detachment
- TTY detection
- child process environment conventions

This should be a very small layer.

If Windows support requires large platform-specific branches outside this layer and the host-runtime layer, the architecture is still too fused.

## What the Windows host must actually do

For Windows server support to be real, the Windows host path needs to support this exact product loop:

1. Start `msb server`.
2. Receive `sandbox.start`.
3. Materialize a Linux guest rootfs from an OCI image.
4. Mount host directories into the guest.
5. Expose guest ports back to the Windows host.
6. Run the in-guest portal on the mapped portal port.
7. Forward RPC from the server to that portal.
8. Stop the sandbox and reclaim all resources.

That means the Windows solution is only acceptable if it supports all of:

- Linux guest boot
- shared-directory mounting
- host<->guest TCP port mapping
- startup time in the same qualitative range as current product goals
- reliable cleanup when the parent process dies
- enough observability for logs and metrics

If any proposed Windows backend cannot do that, it is the wrong backend for this repo.

## Recommended internal object model

The current system uses CLI command lines as an implicit boundary. That should change.

The internal runtime flow should become:

1. `SandboxStartParams`
2. `ResolvedSandboxSpec`
3. `GuestPatchPlan`
4. `MaterializedRootfs`
5. `RuntimeSpawnRequest`
6. `RuntimeHandle`

Where:

- `ResolvedSandboxSpec` is backend-neutral sandbox intent
- `GuestPatchPlan` is the set of guest filesystem changes required
- `MaterializedRootfs` is backend-ready boot input
- `RuntimeSpawnRequest` is host-runtime execution input
- `RuntimeHandle` is what the control plane stores and queries

This removes the current hidden contract where `prepare_run` serializes everything into `msbrun` flags and hopes the child process reconstructs the same meaning.

## What should stay CLI-process based vs library based

Today there are multiple process boundaries:

- `msb`
- `msbserver`
- `msbrun supervisor`
- `msbrun microvm`

For Windows support, the useful question is not "how many binaries should exist?" but "where should the crash and lifetime boundaries be?"

Recommended answer:

- keep `msb` and `msbserver` as binaries
- keep a separate runtime child process boundary between the server/orchestrator and the VM runtime
- do not require that boundary to stay encoded as `msbrun supervisor` + `msbrun microvm` forever

Specifically:

- the control plane should not be in the same process as the VM backend
- the supervising process should own cleanup if the server dies
- Windows and Unix may use different child-process topologies internally as long as they expose the same runtime handle model

That gives Windows room to use a different low-level launch shape without changing the server API.

## Portal and networking implications

The in-guest portal model is good and should stay.

The current server assumes:

- a host port is assigned in [`microsandbox-server/lib/handler.rs`](/home/jonaswre/Projects/jonaswre/microsandbox/microsandbox-server/lib/handler.rs#L599)
- a `host:guest` port mapping is written to config
- the guest portal listens on guest port `4444`
- the server forwards HTTP/JSON-RPC to `127.0.0.1:<assigned_port>`

That contract is portable enough.

So the Windows backend must adapt to that contract rather than inventing a new networking model for the server layer.

This is important because it contains Windows complexity inside the backend:

- if the backend needs a helper process for port forwarding, that is backend detail
- if the backend needs a special network switch or NAT object, that is backend detail
- the server should still just talk to `127.0.0.1:<port>`

## Logging and metrics implications

The current runtime stores:

- supervisor PID
- microVM PID
- status
- rootfs paths

That schema is already slightly Unix-biased.

For Windows hosting, the runtime registry should evolve to store:

- supervisor process identity
- backend runtime identity
- sandbox status
- rootfs descriptor
- portal port
- backend kind

The important point is that "microVM PID" may stop being the right universal identifier.

Some Windows backends may expose:

- a VM ID
- a worker process tree
- a host service process

So the persisted runtime model should become backend-neutral before Windows implementation starts.

## Migration strategy

This is the order that reduces risk.

### Step 1: Normalize types and specs

Do first:

- host path vs guest path types
- structured mounts
- `ResolvedSandboxSpec`
- backend-neutral runtime handle types

Do not start with:

- ConPTY
- Job Objects
- backend FFI

Those are lower-level details and are easier once the data model is correct.

### Step 2: Extract guest artifact pipeline

Lift the rootfs logic out of `sandbox.rs` into a dedicated pipeline module.

Done correctly, Unix should still work with almost no behavior change, but the output becomes a `MaterializedRootfs` instead of raw host paths.

### Step 3: Extract host-runtime abstractions

Replace direct:

- `libkrun` calls
- `nix`
- `libc`
- PTY handling

with trait-backed services.

Only after that should the first Windows host backend be wired in.

### Step 4: Implement Windows runtime stack

Build:

- Windows process supervisor
- Windows terminal broker
- Windows backend adapter
- Windows rootfs materializer

At that point `msb server start` on Windows can move from explicit unsupported to real support.

## What not to do

These approaches will look faster but will trap the implementation.

- Do not sprinkle `#[cfg(windows)]` throughout current Unix-centric modules and call that architecture.
- Do not keep `PathPair` string parsing as the main mount model.
- Do not force Windows to preserve Linux metadata through NTFS/xattr hacks if the backend supports a better artifact format.
- Do not couple the control plane to a specific Windows backend.
- Do not make the server layer care whether the runtime uses a process, helper service, or VM ID internally.

## Concrete repo changes implied by this design

The deep-dive architecture translates into these concrete refactors.

### New modules

- `microsandbox-core/lib/runtime/spec.rs`
- `microsandbox-core/lib/runtime/backend/mod.rs`
- `microsandbox-core/lib/runtime/backend/krun.rs`
- `microsandbox-core/lib/runtime/backend/windows.rs`
- `microsandbox-core/lib/runtime/rootfs_materializer/mod.rs`
- `microsandbox-core/lib/runtime/rootfs_materializer/unix.rs`
- `microsandbox-core/lib/runtime/rootfs_materializer/windows.rs`
- `microsandbox-utils/lib/platform/mod.rs`
- `microsandbox-utils/lib/platform/unix.rs`
- `microsandbox-utils/lib/platform/windows.rs`

### Existing modules to shrink

- [`microsandbox-core/lib/management/sandbox.rs`](/home/jonaswre/Projects/jonaswre/microsandbox/microsandbox-core/lib/management/sandbox.rs)
  - should stop doing rootfs-specific patch logic directly
- [`microsandbox-core/lib/vm/microvm.rs`](/home/jonaswre/Projects/jonaswre/microsandbox/microsandbox-core/lib/vm/microvm.rs)
  - should stop being synonymous with `libkrun`
- [`microsandbox-utils/lib/runtime/supervisor.rs`](/home/jonaswre/Projects/jonaswre/microsandbox/microsandbox-utils/lib/runtime/supervisor.rs)
  - should become a trait-backed implementation, not the only implementation

## The boot artifact decision this doc motivates

The key decision surfaced by this deep dive is the boot artifact boundary.

That decision is:

Should the runtime boundary consume:

- host directory trees, or
- backend-native materialized guest images?

For Unix today, host directory trees work because `libkrun` accepts them directly.

For Windows hosting, the safer architecture is:

- the sandbox-resolution layer still thinks in guest files and mounts
- the guest artifact pipeline emits a backend-native rootfs artifact
- the backend consumes that artifact

That decision keeps Windows host support from being a permanent special case.

The reference design in [WINDOWS_HOST_REFERENCE_DESIGN.md](WINDOWS_HOST_REFERENCE_DESIGN.md)
resolves this concretely as:

- a stable utility VM boot image,
- VHD-backed sandbox layer artifacts,
- guest-side overlay assembly,
- Windows host mounts through `Plan9`,
- HCS and HCN as the Windows backend.

## Recommended implementation move

The architecture question is no longer "pick a Windows API." The next move is
to implement the abstraction and data-model changes that let the chosen Windows
design land cleanly.

It should be:

1. define `ResolvedSandboxSpec`
2. define `RuntimeHandle`
3. define `RootfsMaterializer`
4. define `VmBackend`
5. rewrite `sandbox::prepare_run` to produce a runtime spec instead of a command line

That is the point where the system stops being Unix-only by construction.
