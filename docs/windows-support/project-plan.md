# Native Windows Support Project Plan

## Goal

Deliver full native Windows support for Microsandbox with two outcomes:

1. Windows clients can build and use the CLI and SDKs.
2. Windows Server hosts can run `msb server`, launch Linux sandboxes, expose
   ports, mount host workspaces, collect logs, and shut down cleanly.

This project follows the architecture defined in
[WINDOWS_SUPPORT.md](WINDOWS_SUPPORT.md),
[WINDOWS_HOST_DEEP_DIVE.md](WINDOWS_HOST_DEEP_DIVE.md), and
[WINDOWS_HOST_REFERENCE_DESIGN.md](WINDOWS_HOST_REFERENCE_DESIGN.md).

## Definition of done

Windows support is complete only when all of the following are true:

- `cargo build` succeeds on `windows-latest` for the supported workspace
  targets.
- `msb init/add/remove/list` works on Windows.
- `msb server start/stop` works on Windows Server.
- Windows host can start OCI-image Linux sandboxes locally.
- Windows host can mount host directories into Linux guests.
- Windows host can expose portal traffic and user-declared TCP ports.
- Windows host can stop sandboxes reliably and reclaim VM, network, and disk
  resources.
- logs, status, and attach work on Windows.
- install, upgrade, and uninstall are documented and tested on Windows.
- CI covers Windows build, smoke, and integration paths.

## Explicit non-goals

These are not part of the Windows-native support target:

- WSL2-based hosting
- Docker Desktop as a runtime dependency
- treating arbitrary NTFS directories as authoritative Linux rootfs sources
- preserving the current Unix-only `microvm_pid` and `rootfs_paths` runtime
  model
- keeping ambiguous `host:guest` mount parsing on Windows

## Fixed architecture decisions

These are already decided and should not be reopened during implementation:

- Windows host backend: Hyper-V via HCS
- Windows networking: HCN NAT plus explicit host port mappings
- host mounts for Linux guests: `Plan9`
- interactive terminal: ConPTY
- process lifetime: runtime worker plus Job Object
- guest artifact boundary: VHD-backed Linux artifacts
- first production implementation path: Windows runtime helper built on
  `hcsshim`
- guest launch model: stable utility VM plus guest bootstrap, not direct host
  emulation of the `libkrun` launch contract

## Workstreams

The project breaks into eight workstreams:

1. Product and compatibility rules
2. Cross-platform types and config
3. Backend-neutral runtime model
4. Windows artifact pipeline
5. Windows runtime worker and guest bootstrap
6. CLI, server, and remote workflow integration
7. Packaging, install, and documentation
8. CI, validation, and rollout

The phases below are ordered. Some tasks can run in parallel inside a phase, but
phases should not be skipped.

## Phase 0: Freeze Product Rules

### Deliverables

- document final supported Windows product behaviors
- document explicit unsupported cases
- document Windows host prerequisites
- document release scope for the first Windows-native GA

### Tasks

- confirm supported host OS matrix:
  - Windows 11 for client workflows
  - Windows Server for host workflows
- confirm required Hyper-V and virtualization prerequisites
- confirm supported sandbox rootfs sources on Windows:
  - OCI images
  - explicit Linux filesystem artifacts
- confirm unsupported rootfs source:
  - raw NTFS directory as Linux rootfs
- confirm remote-mode rules for project paths and server-side workspaces
- confirm whether first GA includes MSI or starts with shim-based install only

### Exit criteria

- product behavior is stable enough that implementation does not need to reopen
  architecture decisions

## Phase 1: Cross-Platform Foundations

### Deliverables

- host-path and guest-path type split
- Windows-safe mount parsing
- Windows-safe path detection
- Windows compilation for client-safe codepaths

### Tasks

- introduce `HostPathBuf`
- keep `GuestPathBuf` as Unix-path-only guest type
- replace `PathPair` with `MountSpec { host, guest }`
- update YAML config parsing for structured mounts
- update CLI parsing for structured mount syntax
- keep Unix `host:guest` compatibility only where unambiguous
- reject ambiguous mount strings on Windows
- update `ReferenceOrPath` to recognize Windows absolute paths
- audit config fields into:
  - host paths
  - guest paths
  - backend-neutral strings or IDs
- remove inappropriate `Utf8UnixPathBuf` usage for host paths
- update path normalization helpers to stop coercing host paths into Unix form
- gate Unix-only runtime modules behind `cfg(unix)` or equivalent feature
- add explicit unsupported errors for Windows-host commands until later phases

### Primary code areas

- `microsandbox-core/lib/config`
- `microsandbox-core/lib/management/config.rs`
- `microsandbox-cli/lib/args/msb.rs`
- `microsandbox-utils/lib/path.rs`
- any public SDK surfaces that expose volume syntax

### Exit criteria

- Windows client builds succeed
- config and CLI parsing work for Windows paths
- Unix-only runtime code no longer leaks into Windows client builds

## Phase 2: Backend-Neutral Runtime Model

### Deliverables

- `ResolvedSandboxSpec`
- backend-neutral `RuntimeHandle`
- backend-neutral runtime registry schema
- `VmBackend` and `RootfsMaterializer` interfaces

### Tasks

- define `ResolvedSandboxSpec`
- define `GuestExecSpec`, `PortalSpec`, `ResourceSpec`, and related runtime
  structs
- rewrite `sandbox::prepare_run` so it produces structured runtime input rather
  than serialized child-process flags
- define `RuntimeHandle`
- define `RuntimeRecord`
- migrate DB schema away from PID-only runtime identity
- store backend kind, runtime ID, worker PID, control endpoint, backend object
  ID, rootfs descriptor JSON, and backend state JSON
- update status, stop, and metrics code to use backend-neutral runtime records
- introduce `VmBackend`
- introduce `RootfsMaterializer`
- wrap the current Unix backend as `KrunBackend`
- make current Unix rootfs behavior the Unix `RootfsMaterializer`

### Primary code areas

- `microsandbox-core/lib/management/sandbox.rs`
- `microsandbox-core/lib/management/orchestra.rs`
- `microsandbox-core/lib/management/db.rs`
- `microsandbox-core/lib/models.rs`
- `microsandbox-core/lib/migrations/sandbox/*`
- `microsandbox-core/lib/vm`
- new runtime modules under `microsandbox-core/lib/runtime`

### Exit criteria

- Unix runtime still works through the new abstractions
- DB and orchestration no longer depend on `microvm_pid` as the universal
  runtime identity

## Phase 3: Windows Artifact Pipeline

### Deliverables

- Windows boot bundle format
- Windows layer cache format
- VHD-backed OCI layer materialization
- patch-layer generation
- scratch-disk creation

### Tasks

- define boot bundle layout for the Microsandbox utility VM
- build or import the Linux utility VM bundle:
  - kernel
  - boot rootfs VHD
  - bootstrap
  - portal
  - manifest
- define Windows cache layout under `%LOCALAPPDATA%\\Microsandbox`
- implement OCI layer conversion from tar to ext4 VHD using `hcsshim` tooling
- preserve OCI whiteouts, uid, gid, mode, symlinks, and xattrs in the VHD
- define cache keys from OCI digests
- generate per-sandbox patch layers containing:
  - `/.sandbox` assets
  - DNS and hosts files
  - empty mount-point directories
  - bootstrap inputs
- create writable scratch VHDX per sandbox
- define `WindowsMaterializedRootfs`
- ensure artifact garbage collection and cache invalidation rules exist

### Primary code areas

- new Windows rootfs materializer modules
- OCI pull and extraction pipeline
- install/runtime asset management code
- boot bundle build or packaging pipeline

### Exit criteria

- Windows host can materialize all guest artifacts without using NTFS as the
  final Linux rootfs representation

## Phase 4: Windows Runtime Worker

### Deliverables

- `msbrun-hcs.exe` runtime worker
- named-pipe control channel
- Job Object-based lifetime management
- HCS VM creation and teardown
- HCN network and port mapping support

### Tasks

- create the Windows runtime worker project
- choose exact repository placement and build integration for the helper
- implement command contract between Rust control plane and worker
- implement named-pipe control endpoint:
  - `status`
  - `stop`
  - `attach`
  - `collect_logs`
- implement Job Object ownership and cleanup
- implement HCS compute system creation from the boot bundle
- attach sandbox artifacts:
  - read-only layer VHDs
  - patch layer
  - scratch VHDX
  - `Plan9` control share
  - `Plan9` user shares
- create or reuse the shared HCN NAT network
- create per-sandbox HCN endpoints
- add load balancers or endpoint policies for:
  - portal port
  - user-published ports
- capture VM console output for logging
- implement stop and failure cleanup for:
  - compute system
  - endpoints
  - load balancers
  - scratch disks

### Primary code areas

- new Windows helper project
- new Rust Windows backend adapter
- server/runtime process-control integration

### Exit criteria

- Rust control plane can start and stop a Windows runtime worker that manages a
  real HCS compute system end to end

## Phase 5: Guest Bootstrap

### Deliverables

- guest bootstrap spec
- guest overlay assembly logic
- guest mount orchestration
- payload launch and exit propagation
- portal startup contract

### Tasks

- define `sandbox-spec.json`
- define guest bootstrap input schema
- implement guest bootstrap binary or init logic
- mount lower layers read-only
- mount scratch layer read-write
- assemble merged root with overlayfs
- mount `Plan9` shares into the merged root
- apply generated DNS and hosts settings
- start the portal on guest port `4444`
- launch payload in the merged root and requested workdir
- mirror payload exit status to VM exit status
- emit logs to the VM console
- handle guest-side cleanup and orderly shutdown

### Exit criteria

- Windows-hosted Linux guest can run the same high-level sandbox contract as the
  current Unix path

## Phase 6: CLI, Server, and Project Workflow Integration

### Deliverables

- Windows backend wired into `msb server`
- local Windows host sandbox lifecycle support
- remote-first Windows CLI behavior
- project workspace sync for remote workflows

### Tasks

- wire the Windows backend into orchestration and server lifecycle code
- implement `msb server start/stop` on Windows
- implement local `sandbox.start`, `sandbox.stop`, `status`, and `logs`
- update portal forwarding to use backend-neutral runtime state
- add remote CLI mode when `--server-url` or `MSB_SERVER_URL` is set
- add server APIs for:
  - project workspace sync
  - project config apply or upload
  - sandbox logs
  - sandbox status
- implement project workspace upload and incremental sync
- rewrite relative project paths in remote mode against the server workspace
- require explicit syntax for server-side absolute paths
- ensure local Windows projects can mount their workspace into local Windows
  hosts using `Plan9`

### Primary code areas

- `microsandbox-server`
- orchestration and payload layers
- CLI command implementation
- SDK client helpers where needed

### Exit criteria

- Windows host supports local server workflows
- Windows client supports remote workflows without Unix path assumptions

## Phase 7: Terminal, Logs, and Operator Experience

### Deliverables

- ConPTY attach support
- Windows-safe log capture
- stable status and disk-usage reporting
- actionable Windows error messages

### Tasks

- add a Windows `TerminalBroker`
- translate attach flows to ConPTY instead of Unix PTY logic
- capture VM console output into host log files
- ensure `msb log` and equivalent APIs work on Windows
- replace PID-based liveness checks with runtime-worker and control-endpoint
  health checks
- replace `rootfs_paths` string parsing with `rootfs_descriptor_json`
- add Windows-specific diagnostics for:
  - missing Hyper-V
  - missing virtualization support
  - HCN creation failures
  - `Plan9` share failures
  - boot bundle mismatch

### Exit criteria

- attach, logs, status, and common failures are usable by operators on Windows

## Phase 8: Packaging, Install, and Release

### Deliverables

- Windows install layout
- alias shims
- runtime asset installation
- upgrade and uninstall behavior
- release packaging

### Tasks

- define install root under `%LOCALAPPDATA%\\Microsandbox`
- install CLI binaries and helper binaries
- install or fetch boot bundle assets
- create `.cmd` and PowerShell shims for aliases
- define upgrade strategy for:
  - CLI
  - helper
  - boot bundle
  - cache compatibility
- define uninstall and cleanup behavior
- decide whether MSI ships in first Windows release or later phase
- document manual and packaged installation flows

### Exit criteria

- fresh install, upgrade, and uninstall work on supported Windows targets

## Phase 9: CI, Test Matrix, and Validation

### Deliverables

- Windows compile jobs
- Windows unit tests
- Windows host smoke tests
- Windows integration coverage
- release checklist

### Tasks

- add `windows-latest` compile and test jobs
- add unit tests for:
  - Windows path parsing
  - structured mounts
  - rejection of ambiguous mount strings
  - config field path classification
- add DB migration tests for runtime-record changes
- add artifact-pipeline tests for:
  - layer conversion
  - patch-layer generation
  - scratch-disk creation
- add helper contract tests for:
  - named-pipe control API
  - HCS lifecycle
  - HCN lifecycle
  - cleanup on worker crash
- add end-to-end Windows host smoke tests:
  - start server
  - launch sandbox from OCI image
  - verify portal health
  - verify published port
  - verify workspace mount
  - verify logs
  - stop sandbox
- add performance guardrails for startup latency and cleanup time
- define manual release checklist for Windows Server validation

### Exit criteria

- Windows support is continuously validated in CI and release qualification

## Task dependency graph

Critical dependencies:

1. Phase 1 before any serious Windows runtime work
2. Phase 2 before backend wiring or DB migration consumers
3. Phase 3 before guest bootstrap and runtime worker integration
4. Phase 4 before local Windows host support
5. Phase 5 before end-to-end Windows sandbox execution
6. Phase 6 before user-facing Windows support claims
7. Phase 8 and Phase 9 before GA release

Parallelizable work after Phase 2:

- boot bundle packaging
- helper project scaffolding
- DB migration implementation
- CLI remote-mode work

## Release milestones

### Milestone A: Windows client baseline

- Windows builds succeed
- CLI config commands work
- remote mode works
- local Windows host commands return explicit unsupported errors where not yet
  implemented

### Milestone B: Backend-neutral runtime core

- Unix runtime uses `ResolvedSandboxSpec`, `VmBackend`, and `RootfsMaterializer`
- runtime DB no longer relies on Unix-only identity

### Milestone C: Windows host alpha

- Windows helper can boot utility VM
- OCI image sandboxes start on Windows host
- portal forwarding works
- logs and stop work

### Milestone D: Windows host beta

- workspace mounts work
- published ports work
- attach works
- cleanup is reliable across worker and server failure paths

### Milestone E: Windows-native GA

- install and upgrade are supported
- CI coverage is in place
- docs are complete
- Windows Server support is externally claimable

## Risks that must be managed

- Hyper-V and HCN behavior differs across Windows versions
- boot bundle and helper version skew can break runtime startup
- OCI layer conversion correctness is critical and easy to get subtly wrong
- cleanup leaks in HCS or HCN will accumulate state on developer machines and
  servers
- trying to preserve Unix behaviors too literally will create permanent
  architecture debt

Mitigations:

- keep the architecture fixed
- gate each phase with end-to-end validation
- keep runtime state backend-neutral from the start
- treat helper, boot bundle, and cache formats as versioned artifacts

## First implementation batch

The first code batch after this plan should be intentionally narrow:

1. introduce `HostPathBuf`, `GuestPathBuf`, and `MountSpec`
2. fix Windows path parsing and config classification
3. add Windows compile jobs
4. define `ResolvedSandboxSpec`
5. define `RuntimeHandle` and start the DB migration

That batch creates the seam for everything else without starting Windows helper
or guest-runtime work too early.
