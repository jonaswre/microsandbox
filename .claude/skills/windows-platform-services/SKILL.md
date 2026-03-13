---
name: windows-platform-services
description: Windows platform services — LOCALAPPDATA layout, cmd/PowerShell shims, NUL device, home resolution, CI matrix, boot bundle versioning, install/upgrade
user-invocable: false
---

# Windows Platform Services

## Directory layout

Windows uses `%LOCALAPPDATA%\Microsandbox\` instead of `~/.microsandbox`:

```text
%LOCALAPPDATA%\Microsandbox\
  bin\                          # msb.exe, msbserver.exe, msbrun-hcs.exe
  cache\
    windows\
      layers\<digest>\          # Cached OCI layer VHDs
        layer.vhd
        manifest.json
  runtime\
    windows\
      uvm\<version>\            # Boot bundle
        kernel
        rootfs.vhd
        bootstrap
        portal
        manifest.json
      sandboxes\<sandbox-id>\   # Per-sandbox runtime state
        scratch.vhdx
        control\
        logs\
  projects\                     # Project data
  server.pid                    # Server PID (or equivalent)
  server.key                    # Server key
  oci.db                        # OCI database
```

## Home resolution — PlatformPaths trait

Current hardcoded paths in `microsandbox-utils/lib/defaults.rs` lines 34-35:

```rust
pub static DEFAULT_MICROSANDBOX_HOME: LazyLock<PathBuf> =
    LazyLock::new(|| dirs::home_dir().unwrap().join(MICROSANDBOX_HOME_DIR));
```

Replace with a `PlatformPaths` trait:

```rust
trait PlatformPaths {
    fn data_home() -> PathBuf;       // ~/.microsandbox or %LOCALAPPDATA%\Microsandbox
    fn cache_home() -> PathBuf;      // same, or separate cache dir
    fn runtime_home() -> PathBuf;    // runtime artifacts
    fn bin_dir() -> PathBuf;         // ~/.local/bin or %LOCALAPPDATA%\Microsandbox\bin
    fn null_device() -> &'static str; // /dev/null or NUL
}
```

Unix impl: uses `dirs::home_dir().join(".microsandbox")` and `~/.local/bin`.
Windows impl: uses `dirs::data_local_dir().join("Microsandbox")` for everything.

## .cmd and PowerShell alias shims

Current `generate_alias_script` in `management/home.rs` lines 426-464 creates Unix `#!/bin/sh` scripts.

Windows needs `.cmd` and `.ps1` shims:

```cmd
@echo off
REM MSB-ALIAS: myapp
SET "SCRIPT_DIR=%~dp0"
IF EXIST "%SCRIPT_DIR%msb.exe" (
    SET "MSB_PATH=%SCRIPT_DIR%msb.exe"
) ELSE (
    SET "MSB_PATH=msb"
)
"%MSB_PATH%" run "myapp" -f "%LOCALAPPDATA%\Microsandbox\installs" %*
```

PowerShell `.ps1` variant:

```powershell
# MSB-ALIAS: myapp
$ScriptDir = Split-Path -Parent $MyInvocation.MyCommand.Path
$MsbPath = if (Test-Path "$ScriptDir\msb.exe") { "$ScriptDir\msb.exe" } else { "msb" }
& $MsbPath run "myapp" -f "$env:LOCALAPPDATA\Microsandbox\installs" @args
```

## NUL device and process control

| Operation | Unix | Windows |
|---|---|---|
| Null device | `/dev/null` | `NUL` |
| Process liveness | `libc::kill(pid, 0)` | `OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, ...)` |
| Process terminate | `libc::kill(pid, SIGTERM)` | `TerminateProcess(handle, exit_code)` |
| Process detach | `libc::setsid()` in `pre_exec` | `CREATE_NEW_PROCESS_GROUP` flag in `CreateProcessW` |
| TTY detection | `libc::isatty(STDIN_FILENO)` | `GetConsoleMode` on stdin handle |
| Log follow | `tail -f` (shelled out) | Native file watching or polling |
| Command exists | `which command` | `where.exe command` or `Get-Command` |

Current uses to replace:

| File | Line | Unix API |
|---|---|---|
| `management.rs` | 93-97 | `libc::kill(pid, 0)` |
| `management.rs` | 206-210 | `pre_exec setsid` |
| `management.rs` | 372-395 | SIGTERM sending |
| `defaults.rs` | 50 | `DEFAULT_SHELL = /bin/sh` |
| `defaults.rs` | 67 | `DEFAULT_WORKDIR = /` |
| `home.rs` | 286-305 | `perms.set_mode(0o755)` |
| `home.rs` | 395-403 | `which` command |

## Server detach on Windows

Current Unix approach in `microsandbox-server/lib/management.rs` lines 206-210:

```rust
unsafe {
    command.pre_exec(|| {
        libc::setsid();
        Ok(())
    });
}
command.stdout(Stdio::null()); // /dev/null
```

Windows replacement:

```rust
#[cfg(windows)]
{
    use std::os::windows::process::CommandExt;
    command.creation_flags(
        windows_sys::Win32::System::Threading::CREATE_NEW_PROCESS_GROUP
        | windows_sys::Win32::System::Threading::DETACHED_PROCESS
    );
    command.stdout(Stdio::null()); // NUL
    command.stderr(Stdio::null());
    command.stdin(Stdio::null());
}
```

## CI matrix

| Runner | Purpose | Phase |
|---|---|---|
| `ubuntu-latest` | Full host-runtime tests | All |
| `macos-latest` | Host-runtime compile + selected tests | All |
| `windows-latest` | Cross-platform unit tests, CLI parsing, SDK smoke | Phase 1+ |
| Hyper-V Windows runner | Boot validation, host smoke tests | Phase 2+ |

## Boot bundle versioning

`manifest.json` in the boot bundle:

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

Compatibility check at startup:

```rust
fn check_boot_bundle_compat(bundle: &BootBundleManifest, host_version: &str) -> Result<()> {
    if !semver_compat(host_version, &bundle.min_host_version) {
        return Err(anyhow!(
            "Boot bundle {} requires host >= {}, current: {}",
            bundle.version, bundle.min_host_version, host_version
        ));
    }
    Ok(())
}
```

## Install/upgrade/uninstall strategy

**Install:**
1. Download or build: `msb.exe`, `msbserver.exe`, `msbrun-hcs.exe`.
2. Place in `%LOCALAPPDATA%\Microsandbox\bin\`.
3. Download boot bundle to `runtime\windows\uvm\<version>\`.
4. Optionally add `bin\` to user PATH.

**Upgrade:**
1. Download new binaries + boot bundle.
2. Verify compatibility between host and boot bundle versions.
3. Replace binaries (requires server stop).
4. Old boot bundles can be cleaned up after migration.

**Uninstall:**
1. Stop server if running.
2. Remove `%LOCALAPPDATA%\Microsandbox\` entirely.
3. Remove `.cmd`/`.ps1` shims from PATH if present.
4. Clean up any alias scripts created by `msb install`.

## Anti-patterns

- **Don't hardcode `~/.microsandbox`.** Use `PlatformPaths::data_home()`.
- **Don't use `which`.** Use `where.exe` on Windows or `std::process::Command::new(name).output()`.
- **Don't use `tail -f` for log following.** Use native file watching or async polling.
- **Don't assume `/dev/null`.** Use `PlatformPaths::null_device()` or `Stdio::null()`.
- **Don't use `set_mode(0o755)` on Windows.** Windows has ACLs, not Unix permission bits. Skip or use Windows security APIs.
- **Don't assume `~/.local/bin` is in PATH.** On Windows, install to a directory and suggest PATH addition.
