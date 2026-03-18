---
name: cross-platform-rust
description: Cross-platform Rust patterns — cfg gating, trait-based backend abstraction, platform module layout, feature flags, conditional dependencies
user-invocable: false
---

# Cross-Platform Rust Patterns

## Platform module layout

Use `cfg_attr` path switching for platform-specific implementations:

```rust
// In mod.rs or parent module:
#[cfg_attr(unix, path = "unix.rs")]
#[cfg_attr(windows, path = "windows.rs")]
mod platform;

pub use platform::*;
```

Or with explicit modules when shared code exists:

```rust
mod common;  // shared logic

#[cfg(unix)]
mod unix;
#[cfg(windows)]
mod windows;

#[cfg(unix)]
pub use unix::*;
#[cfg(windows)]
pub use windows::*;
```

## Trait signatures

Backend abstraction traits that the codebase should introduce:

```rust
/// VM lifecycle — one impl per backend.
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

/// Rootfs materialization — Unix extracts dirs, Windows builds VHDs.
trait RootfsMaterializer {
    type MaterializedRootfs;

    fn materialize(
        &self,
        source: RootfsSource,
        patch_plan: GuestPatchPlan,
        mounts: &[MountSpec],
    ) -> Result<Self::MaterializedRootfs>;
}

/// Process supervision — Unix PTY+signals, Windows Job Object+ConPTY.
trait ProcessSupervisor {
    fn spawn_runtime(&self, req: RuntimeSpawnRequest) -> Result<SupervisedProcess>;
    fn terminate(&self, process: &SupervisedProcess) -> Result<()>;
    fn is_running(&self, process: &SupervisedProcess) -> Result<bool>;
}

/// Top-level service consumed by the control plane.
trait SandboxRuntimeService {
    fn start(&self, spec: ResolvedSandboxSpec) -> Result<RuntimeHandle>;
    fn stop(&self, handle: &RuntimeHandle) -> Result<()>;
    fn status(&self, handle: &RuntimeHandle) -> Result<SandboxRuntimeStatus>;
}
```

## Feature-gated compilation

Use a `host-runtime` feature to gate Unix-only runtime code:

```toml
# microsandbox-core/Cargo.toml
[features]
default = ["host-runtime"]
host-runtime = ["dep:nix", "dep:libc", "dep:xattr"]
```

In `build.rs`, gate libkrun linking:

```rust
// microsandbox-core/build.rs — currently hardcodes libkrun search paths
fn main() {
    #[cfg(all(feature = "host-runtime", not(target_os = "windows")))]
    {
        println!("cargo:rustc-link-lib=dylib=krun");
        // ... search paths
    }
}
```

## Target-specific dependencies in Cargo.toml

```toml
[target.'cfg(unix)'.dependencies]
nix = { version = "0.29", features = ["term", "signal", "fs", "process"] }
libc = "0.2"
xattr = "1"

[target.'cfg(windows)'.dependencies]
windows-sys = { version = "0.59", features = [
    "Win32_System_Threading",
    "Win32_System_JobObjects",
    "Win32_System_Pipes",
    "Win32_System_Console",
    "Win32_System_HostComputeSystem",
    "Win32_System_HostComputeNetwork",
] }
```

## Tokio platform differences

| Feature | Unix | Windows |
|---|---|---|
| Async FD polling | `AsyncFd` | Not available — use `tokio::net::windows::named_pipe` |
| Signal handling | `signal::unix::signal(SignalKind::terminate())` | `signal::ctrl_c()`, `signal::windows::ctrl_break()` |
| Named pipes | Unix domain sockets | `\\.\pipe\name` via `ServerOptions` |
| PTY | `openpty` + `AsyncFd` | ConPTY + pipe pairs |
| Process detach | `libc::setsid()` | `CREATE_NEW_PROCESS_GROUP` |

## Concrete files needing cfg gates

| File | Unix-only API | Gate needed |
|---|---|---|
| `microsandbox-core/lib/vm/ffi.rs` | `#[link(name = "krun")]` | `#[cfg(all(unix, feature = "host-runtime"))]` |
| `microsandbox-core/lib/vm/microvm.rs` | `ffi::krun_*` calls | `#[cfg(all(unix, feature = "host-runtime"))]` |
| `microsandbox-core/lib/management/rootfs.rs:10` | `use os::unix::fs::PermissionsExt` | `#[cfg(unix)]` |
| `microsandbox-core/lib/oci/layer/extraction.rs:4` | `use os::unix::fs::PermissionsExt` | `#[cfg(unix)]` |
| `microsandbox-core/lib/oci/layer/extraction.rs:54-76` | `libc::setxattr` | `#[cfg(unix)]` |
| `microsandbox-utils/lib/runtime/supervisor.rs:1-16` | `nix::pty`, `AsyncFd`, Unix signals | `#[cfg(unix)]` entire module |
| `microsandbox-core/lib/runtime/monitor.rs:59` | `nix::sys::termios::Termios` | `#[cfg(unix)]` |
| `microsandbox-core/lib/runtime/monitor.rs:250-260` | `tcgetattr`, `cfmakeraw`, `tcsetattr` | `#[cfg(unix)]` |
| `microsandbox-server/lib/management.rs:93-97` | `libc::kill(pid, 0)` | `#[cfg(unix)]` |
| `microsandbox-server/lib/management.rs:206-210` | `pre_exec setsid` | `#[cfg(unix)]` |
| `microsandbox-server/lib/management.rs:372-395` | SIGTERM sending | `#[cfg(unix)]` |
| `microsandbox-core/lib/management/home.rs:20` | `use os::unix::fs::PermissionsExt` | `#[cfg(unix)]` |
| `microsandbox-core/lib/management/home.rs:286-305` | `perms.set_mode(0o755)` | `#[cfg(unix)]` |

## Anti-patterns

- **Don't scatter inline `#[cfg(windows)]` through Unix modules.** Create platform modules with clean trait impls.
- **Don't use `#[cfg(not(unix))]`** — it matches WASM, Fuchsia, etc. Use `#[cfg(windows)]` explicitly.
- **Don't import `nix` or `libc` in cross-platform modules.** Keep them behind `#[cfg(unix)]` or in platform modules.
- **Don't make Windows a "polyfill" of Unix.** Windows code should use native Windows idioms (Job Objects, named pipes, ConPTY), not wrappers that emulate Unix behavior.
- **Don't gate on `target_os` when `target_family` suffices.** `cfg(unix)` covers Linux + macOS. `cfg(windows)` covers all Windows.
- **Don't add dead platform code.** If the Windows impl doesn't exist yet, use `compile_error!` or `unimplemented!` behind `cfg(windows)` so it's discoverable.
