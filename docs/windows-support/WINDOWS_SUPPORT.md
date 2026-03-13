# Windows Support Design

## Summary

For the lower-level runtime decomposition, see [WINDOWS_HOST_DEEP_DIVE.md](WINDOWS_HOST_DEEP_DIVE.md).

For the final Windows host reference architecture, see
[WINDOWS_HOST_REFERENCE_DESIGN.md](WINDOWS_HOST_REFERENCE_DESIGN.md).

Windows support in this repo needs to be split into two deliverables:

1. Windows clients: build and run the CLI and SDKs on Windows.
2. Windows hosts: run the microsandbox server and microVM runtime on Windows.

Those are not the same problem. The current codebase already supports a remote server model for SDKs, but the local runtime is tightly coupled to Unix process APIs and the `libkrun` VM backend. The fastest path to real support is:

1. make config, SDK, and CLI client code cross-platform;
2. isolate the host runtime behind backend, rootfs, and platform traits;
3. implement a real Windows host runtime for `msb server` and local sandbox execution;
4. add remote project workflow and packaging polish.

Windows client support can land earlier, but Windows hosting is a required milestone, not an optional follow-up.

## Current blockers

The repo currently assumes a Unix host in several core places:

- VM backend and linking
  - [`microsandbox-core/build.rs`](microsandbox-core/build.rs) hard-links against `libkrun` and searches Unix library locations.
  - [`microsandbox-core/lib/vm`](microsandbox-core/lib/vm) is built around `libkrun`.
- Process lifecycle and TTY handling
  - [`microsandbox-utils/lib/runtime/supervisor.rs`](microsandbox-utils/lib/runtime/supervisor.rs) uses `openpty`, `setsid`, Unix signals, and `AsyncFd`.
  - [`microsandbox-core/lib/runtime/monitor.rs`](microsandbox-core/lib/runtime/monitor.rs) uses `termios`.
  - [`microsandbox-server/lib/management.rs`](microsandbox-server/lib/management.rs) uses `libc::kill`, `setsid`, and Unix signal handlers.
- Filesystem metadata and OCI extraction
  - [`microsandbox-core/lib/management/rootfs.rs`](microsandbox-core/lib/management/rootfs.rs) uses Unix permission bits and xattrs.
  - [`microsandbox-core/lib/oci/layer/extraction.rs`](microsandbox-core/lib/oci/layer/extraction.rs) depends on Unix permissions, symlink semantics, and xattrs.
- Install locations and aliasing
  - [`microsandbox-core/lib/management/home.rs`](microsandbox-core/lib/management/home.rs) and [`microsandbox-core/lib/management/toolchain.rs`](microsandbox-core/lib/management/toolchain.rs) assume `~/.local/bin`, `~/.local/lib`, shell scripts, and `which`.
- Path modeling
  - [`microsandbox-core/lib/config/path_pair.rs`](microsandbox-core/lib/config/path_pair.rs) parses `host:guest` by splitting on `:`, which breaks on `C:\...`.
  - [`microsandbox-core/lib/config/reference_path.rs`](microsandbox-core/lib/config/reference_path.rs) treats only `.` and `/` as filesystem paths.
  - [`microsandbox-core/lib/config/microsandbox/config.rs`](microsandbox-core/lib/config/microsandbox/config.rs), [`microsandbox-core/lib/management/config.rs`](microsandbox-core/lib/management/config.rs), and [`microsandbox-cli/lib/args/msb.rs`](microsandbox-cli/lib/args/msb.rs) use `Utf8UnixPathBuf` for values that are host paths on some code paths.
  - [`microsandbox-utils/lib/path.rs`](microsandbox-utils/lib/path.rs) normalizes all paths as Unix paths.
- Runtime state and persistence
  - [`microsandbox-core/lib/models.rs`](microsandbox-core/lib/models.rs), [`microsandbox-core/lib/management/db.rs`](microsandbox-core/lib/management/db.rs), and [`microsandbox-core/lib/management/orchestra.rs`](microsandbox-core/lib/management/orchestra.rs) treat `supervisor_pid`, `microvm_pid`, and `rootfs_paths` as the authoritative runtime model.
- Home, cache, and runtime layout
  - [`microsandbox-utils/lib/defaults.rs`](microsandbox-utils/lib/defaults.rs) and [`microsandbox-utils/lib/path.rs`](microsandbox-utils/lib/path.rs) assume `~/.microsandbox`, `~/.local`, and a Unix-shaped runtime layout for caches, projects, logs, and state files.
- CI and release pipeline
  - [`.github/workflows/tests_and_checks.yml`](.github/workflows/tests_and_checks.yml) only validates Linux and macOS.
  - [`.github/workflows/release.yml`](.github/workflows/release.yml), [`scripts/package_microsandbox.sh`](scripts/package_microsandbox.sh), and [`scripts/install_microsandbox.sh`](scripts/install_microsandbox.sh) only build and install Unix artifacts.

## Product definition

The repo should define Windows support explicitly as the matrix below.

| Capability | Windows client | Windows host |
| --- | --- | --- |
| SDKs talk to server | Required in phase 1 | N/A |
| `msb init/add/remove/list` | Required in phase 1 | N/A |
| `msb run/up/down/status/log` | Required via remote mode in phase 4 | Required in phase 3 |
| `msb server start/stop` | N/A | Required in phase 3 |
| `msi` alias install | Supported with Windows shims in phase 4 | Required in phase 4 |
| Local microVM execution | N/A | Required in phase 3 |

The important constraint is that Linux guests remain Linux guests. Windows support is about the host and client OS, not changing guest path semantics.

Windows host rootfs contract:

- supported inputs:
  - OCI image references
  - prebuilt Linux filesystem artifacts such as `rootfs.vhd`, `rootfs.vhdx`, or a Linux tar archive with preserved metadata
- unsupported inputs:
  - arbitrary NTFS directories used as if they were Linux rootfs directories
- required behavior:
  - if a Windows host config resolves `image:` or rootfs input to a plain host directory, fail during config resolution with a targeted error telling the user to boot from an OCI image or Linux artifact and mount the workspace separately

## Architecture

### 1. Separate host paths from guest paths

The repo currently uses Unix path types in places that represent host filesystem paths. That needs to change first.

Introduce:

- `HostPathBuf`: wrapper around native `PathBuf`
- `GuestPathBuf`: keep using `Utf8UnixPathBuf`
- `MountSpec { host: HostPathBuf, guest: GuestPathBuf }`

Rules:

- Anything on the host machine uses `HostPathBuf`.
- Anything inside the guest rootfs or guest process environment uses `GuestPathBuf`.
- Relative project paths are host paths until they are transformed into guest-visible paths.

Immediate type migrations:

- `PathPair` becomes `MountSpec`.
- `ReferenceOrPath::Path` stays `PathBuf`, but parsing must recognize Windows absolute paths and relative paths.
- `env_file`, `meta.readme`, and `meta.icon` become host paths.
- `workdir`, `exec_path`, `imports`, and `exports` remain guest paths unless code inspection proves otherwise.

### 2. Replace stringly-typed volume syntax

`host:guest` is not a stable cross-platform format because `:` is already part of Windows drive letters.

Use structured mounts everywhere new code is added:

- YAML:
  - `volumes:`
  - `  - host: C:\\Users\\jonas\\project`
  - `    guest: /workspace`
- CLI:
  - `--volume host=C:\Users\jonas\project,guest=/workspace`
- SDKs:
  - add typed mount options; keep string overloads only for backward compatibility on Unix

Compatibility plan:

- Keep parsing `host:guest` on Unix.
- Accept a new structured representation on all platforms.
- Reject ambiguous string mounts on Windows with a targeted error message.

The same audit should be done for any config field that currently encodes paths in plain strings.

### 3. Split client-safe code from host-runtime code

The current crate boundaries mix cross-platform config logic with Unix-only runtime logic.

Refactor target:

- Cross-platform modules:
  - config parsing and serialization
  - API payloads
  - remote client
  - non-runtime CLI commands
- Host-runtime modules:
  - OCI layer extraction
  - rootfs patching
  - supervisor and monitor
  - server lifecycle management
  - microVM start/stop

Pragmatic implementation:

- Keep existing crates, but move Unix-only modules behind `cfg(unix)` or a `host-runtime` feature.
- Add explicit `UnsupportedPlatform` errors for Windows only during phase 1 while the Windows host runtime is still being built.

This lets Windows builds compile the client and config surface first, and then lets Windows-specific host modules replace the Unix implementations without carrying Unix dependencies into Windows targets.

### 4. Introduce a runtime backend trait

The VM runtime must stop depending directly on `libkrun`.

Introduce a trait such as:

```rust
trait VmBackend {
    fn validate_host(&self) -> Result<()>;
    fn supports_mounts(&self) -> bool;
    fn supports_port_mapping(&self) -> bool;
    fn start(&self, spec: &VmSpec) -> Result<ExitStatus>;
}
```

Phase 1 implementation:

- `KrunBackend`
  - current Linux/macOS implementation
- `WindowsBackend`
  - compiles on Windows behind a feature flag or development gate
  - starts as a stub in phase 1 but becomes a required production backend in phase 3

Required backend-selection spike:

- choose one Windows-capable backend after a spike confirms feature parity for:
  - Linux guest boot
  - host directory sharing
  - guest networking and port forwarding
  - acceptable startup latency
  - safe shutdown and process accounting

That spike is not a go/no-go on Windows hosting. It is a mandatory design checkpoint that picks the implementation path for a required feature.

### 5. Abstract rootfs materialization and guest metadata

Swapping the VM backend alone is not enough. The current OCI and rootfs pipeline assumes a Unix host filesystem that can preserve Linux metadata through xattrs and permissions.

Add a `RootfsMaterializer` abstraction that returns a backend-ready rootfs handle:

```rust
trait RootfsMaterializer {
    type Handle;

    fn materialize_image(&self, image: &ImageRef) -> Result<Self::Handle>;
    fn materialize_native_rootfs(&self, path: &HostPathBuf) -> Result<Self::Handle>;
    fn apply_mounts(&self, handle: &mut Self::Handle, mounts: &[MountSpec]) -> Result<()>;
}
```

Unix implementation:

- extracted directories on the host filesystem
- xattrs for original uid/gid/mode
- current rootfs patching logic

Windows implementation:

- backend-native disk image, VHDX, or other materialized guest filesystem format
- explicit metadata store instead of Unix xattrs
- no dependency on NTFS behaving like a Linux filesystem

This keeps OCI extraction, whiteout handling, permissions, and mount staging out of the VM backend itself.

### 6. Make Windows rootfs inputs an explicit product contract

Windows host support should not silently treat raw host directories as native Linux rootfs inputs.

Rules:

- `ReferenceOrPath::Path` remains a valid cross-platform config type.
- on Unix hosts, local directory rootfs inputs can keep the current meaning;
- on Windows hosts, local path rootfs inputs are accepted only for explicit Linux artifacts such as `rootfs.vhd`, `rootfs.vhdx`, or Linux tar archives with preserved metadata;
- raw Windows directories remain valid as workspace mounts, not as authoritative rootfs sources.

Implementation requirements:

- rootfs input validation happens during config resolution, before runtime launch;
- error messages explain the supported Windows host model;
- docs and examples stop implying that `image: ./rootfs-dir` is portable to Windows hosting.

### 7. Persist backend-neutral runtime state before the Windows backend lands

The runtime registry cannot stay Unix-shaped until after phase 3.

Phase 2 should introduce a backend-neutral runtime record that can represent both Unix and Windows runtimes, including:

- backend kind
- runtime ID
- worker or supervisor PID
- control endpoint
- backend object ID
- rootfs descriptor JSON
- backend state JSON

Migration rules:

- Unix runtime can continue populating the legacy PID and `rootfs_paths` fields temporarily;
- all new status, stop, log, and disk-usage code should pivot to the new runtime record;
- `microvm_pid` and `rootfs_paths` stop being the primary cross-platform health and storage model before the Windows backend ships.

### 8. Add a remote-first Windows CLI mode

The SDKs already talk to a server via `MSB_SERVER_URL`. The CLI should do the same.

Add a remote client layer used by `msb` when either:

- `--server-url` is provided, or
- `MSB_SERVER_URL` is set

Remote mode needs more than the current `sandbox.start/stop` API because the CLI also manages projects, logs, and long-lived sandboxes.

Add server APIs for:

- project workspace sync
- project config upload or apply
- sandbox status
- sandbox logs
- sandbox start/stop for project-defined sandboxes

### 9. Add workspace sync for project workflows

Remote execution is not enough for `Sandboxfile` workflows because the current model assumes the host filesystem that defines the project is also the filesystem that the runtime can mount.

That is false for a Windows client talking to a Linux server.

Add a workspace sync layer:

- CLI computes a project manifest from the local workspace.
- CLI uploads changed files to a server-managed workspace directory.
- Server rewrites relative host paths in the config to that workspace root.
- Absolute host paths are either:
  - disallowed in remote mode, or
  - explicitly marked as server-side paths

Recommended rule:

- in remote mode, only relative project paths are supported by default;
- absolute paths require an explicit `server:` form so the user cannot accidentally reference a Windows path that the Linux host cannot see.

This keeps behavior predictable.

### 10. Add platform services for storage, install, detach, and process control

Create platform modules for:

- data home
- cache home
- runtime home
- projects dir
- install directories
- alias creation
- null device handling
- PID liveness checks
- process termination
- TTY detection
- log-follow behavior

Unix implementation can wrap the current behavior.

Windows implementation should start with client-safe features in phase 1 and extend through phases 2 and 3 to cover host-runtime needs:

- data, cache, and runtime dirs under `%LOCALAPPDATA%\\Microsandbox`
- install dirs under `%LOCALAPPDATA%\\Microsandbox`
- alias shims as `.cmd` and PowerShell launchers
- `NUL` instead of `/dev/null`
- process lifecycle built around Windows process handles and Job Objects
- interactive sessions built around ConPTY or an equivalent Windows PTY layer
- server detach behavior based on Windows creation flags or service-style launch semantics
- native log-follow behavior instead of shelling out to `tail`

Windows host support should not emulate Unix signals internally. The process-control abstraction should expose portable operations like `terminate`, `is_running`, `attach_terminal`, and `detach`.

This abstraction should also remove hard-coded `~/.microsandbox`, `~/.local`, `/dev/null`, `tail`, and Unix TTY assumptions from shared code paths.

### 11. Add host validation, recovery, and cleanup

`VmBackend::validate_host` cannot be a stub.

Windows host preflight should validate:

- Hyper-V, HCS, and HCN availability for the selected backend
- required privileges to create compute systems, shared NAT networking, runtime files, and named pipes
- utility VM bundle presence and version compatibility
- runtime-home writability and expected path layout
- whether a shared Microsandbox NAT network already exists or can be created

Windows host recovery should also define:

- how stale runtime records are reconciled with actual worker and backend state on startup
- how leaked scratch disks, network endpoints, and load balancers are cleaned up safely after crashes
- the exact remediation messages returned when host prerequisites are missing

CI planning must reflect this requirement: `windows-latest` is fine for cross-platform client checks, but phase 2 and phase 3 host validation need a Hyper-V-capable Windows runner strategy.

### 12. Make server startup behavior explicit on Windows

In phase 1, `msb server start` on Windows should not try to partially work.

It should fail fast with a message like:

- "Windows hosting is not available in this build."
- "Use a Linux/macOS host and set `MSB_SERVER_URL`."

That phase-1 behavior should be deleted once the Windows host runtime ships in phase 3.

### 13. Ship versioned Windows runtime artifacts

The Windows host is a multi-artifact product, not just a Rust binary build.

Required runtime artifacts:

- Rust control-plane binaries
- Windows runtime worker binary such as `msbrun-hcs.exe`
- utility VM boot bundle containing kernel, boot rootfs, bootstrap, portal, and manifest
- compatibility metadata that pins the helper and utility VM bundle to the Microsandbox release

Release and packaging requirements:

- define a versioned artifact manifest in phase 2;
- build helper and utility VM assets in CI before the Windows host runtime is declared shippable;
- make the control plane verify helper and utility VM compatibility at startup;
- support client-only Windows preview archives before official Windows host releases if packaging work lands earlier than host-runtime completion.

## Delivery plan

### Phase 1: Windows client source-build baseline

Goal: build SDKs and core CLI config commands on Windows from source and validate them in CI.

Scope:

- audit host-path fields and replace Unix path types where needed
- add structured mount parsing
- gate Unix-only host-runtime modules
- add platform storage path services for data home, cache home, runtime home, and install dirs
- compile `microsandbox-cli`, `microsandbox-core` config modules, and SDKs on Windows
- make Windows-host-unsupported rootfs inputs and runtime commands fail fast with targeted errors
- return explicit unsupported errors for `msb server start`, local runtime commands, and install paths not yet ported
- add Windows CI for compile and unit tests of cross-platform modules
- document phase 1 as source-build and CI-supported only; official downloadable Windows artifacts are not required yet

Exit criteria:

- `cargo check` passes on `windows-latest` for client-safe crates
- SDKs can connect to a remote server from Windows
- `msb init/add/remove/list` work on Windows
- docs are explicit that phase 1 Windows support is source-build only

### Phase 2: Host-runtime abstraction and backend selection

Goal: make Windows hosting technically possible without forking the codebase.

Scope:

- introduce `VmBackend`
- introduce `RootfsMaterializer`
- make Windows host rootfs validation explicit and reject unsupported native directory rootfs inputs
- migrate the runtime DB and orchestration logic to backend-neutral runtime records
- separate Unix-only process primitives from portable control flow
- add platform storage and process services needed by both Unix and Windows backends
- reduce direct `nix`, `libc`, `xattr`, and PTY use to backend/platform modules
- define the Windows helper and utility VM artifact manifest
- define the Hyper-V-capable runner strategy for Windows host CI
- add preview Windows client archive builds in CI if packaging work is ready before host runtime ships
- complete the backend-selection spike and create a booting Windows host prototype

Exit criteria:

- host-runtime code has a clean abstraction boundary
- runtime state and orchestration no longer depend on `microvm_pid` and `rootfs_paths` as the primary model
- Windows builds no longer need to compile or link Unix backend code
- a versioned Windows helper and utility VM artifact contract exists
- a development build can boot a Linux guest on Windows through the chosen backend

### Phase 3: Windows host runtime

Goal: ship `msb server` and local sandbox execution on Windows.

Scope:

- finish the Windows VM backend
- finish Windows rootfs materialization and layer caching
- ship the Windows runtime worker and utility VM bundle
- implement Windows supervisor and monitor logic
- implement Windows server lifecycle management
- implement host preflight, runtime recovery, and stale-resource cleanup
- support `msb server start/stop`
- support local `msb run/up/down/status/log`
- add Windows packaging for host binaries, helper binaries, and utility VM assets
- publish a Windows host preview bundle from the release pipeline

Exit criteria:

- a Windows host can start the server locally
- the packaged Windows host bundle contains the control plane, runtime worker, and utility VM assets
- the server can launch and stop Python and Node sandboxes
- logs, status, and process cleanup work after normal exit and forced termination

### Phase 4: Remote project workflow and polish

Goal: make Windows a complete first-class client and host platform.

Scope:

- add `--server-url` / `MSB_SERVER_URL` support to CLI flows
- add workspace sync API and client implementation
- add remote `run/up/down/status/log` support
- finalize official Windows client and host packaging format
- add Windows alias shims for `msi`
- expand CI to include Windows host smoke tests on the Hyper-V-capable runner class
- document supported and unsupported Windows host capabilities

## Testing and CI

Add a CI matrix instead of a single OS assumption.

Required matrix:

- `ubuntu-latest`
  - full host-runtime tests
- `macos-latest`
  - host-runtime compile and selected tests
- `windows-latest`
  - cross-platform unit tests
  - CLI parsing tests
  - SDK smoke tests against a mock HTTP server
  - preview Windows client archive builds
- Hyper-V-capable Windows runner
  - phase 2 onward: prototype boot validation for the chosen backend
  - phase 3 onward: host smoke tests against the Windows backend

Add tests for:

- Windows path parsing in `ReferenceOrPath`
- structured mount parsing
- rejection of ambiguous mount strings on Windows
- rejection of unsupported Windows native rootfs directories
- remote workspace rewrite logic
- runtime-record migration and backward compatibility
- Windows data-home, cache-home, and runtime-home resolution
- install-dir resolution on Windows
- Windows host process-lifecycle behavior
- Windows host preflight and stale-resource cleanup behavior
- helper and utility VM compatibility checks
- Windows rootfs materialization and metadata preservation

## Recommended first patch set

If this work starts now, the first PR should be narrow:

1. Introduce `HostPathBuf` and `GuestPathBuf`.
2. Replace `PathPair` with a structured mount type that supports Windows-safe parsing.
3. Fix `ReferenceOrPath` path detection for Windows paths.
4. Gate Unix-only runtime modules so the workspace compiles on Windows.
5. Add `windows-latest` compile jobs.

That PR will not make Windows fully usable, but it creates the seam needed for every later step.

## Follow-up work

The Windows host architecture is no longer an open question. The remaining work
is implementation and audit work:

- audit every config field into host-path, guest-path, or backend-neutral data
- finish the final Windows installer format in phase 4 without changing the
  runtime architecture
- audit docs and examples so Windows host guidance consistently reflects the
  supported rootfs and workspace model
