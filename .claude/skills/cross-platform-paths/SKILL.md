---
name: cross-platform-paths
description: HostPathBuf vs GuestPathBuf, MountSpec replacing PathPair, Windows drive letter parsing, structured mount syntax, path normalization
user-invocable: false
---

# Cross-Platform Path Types and Mount Model

## Fixed decisions

- Host paths use `HostPathBuf(PathBuf)` — native OS path, never `Utf8UnixPathBuf`.
- Guest paths use `GuestPathBuf` = `Utf8UnixPathBuf` — always Unix.
- `PathPair` is replaced by `MountSpec { host: HostPathBuf, guest: GuestPathBuf }`.
- `:` splitting is not safe on Windows — use structured mount syntax.

## Type definitions

```rust
/// Wrapper for native host filesystem paths.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct HostPathBuf(pub PathBuf);

/// Guest paths are always Unix paths inside the Linux VM.
pub type GuestPathBuf = Utf8UnixPathBuf;

/// A structured mount specification replacing PathPair.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MountSpec {
    pub host: HostPathBuf,
    pub guest: GuestPathBuf,
}
```

## Current PathPair — what breaks on Windows

`microsandbox-core/lib/config/path_pair.rs` line 96-97:

```rust
// Splits on FIRST colon — breaks on "C:\Users\foo:/app"
if s.contains(':') {
    let (host, guest) = s.split_once(':').unwrap();
```

`PathPair` uses `Utf8UnixPathBuf` for both host and guest (lines 40-52):

```rust
pub enum PathPair {
    Distinct {
        host: Utf8UnixPathBuf,  // WRONG for Windows host paths
        guest: Utf8UnixPathBuf,
    },
    Same(Utf8UnixPathBuf),
}
```

## ReferenceOrPath — missing Windows path detection

`microsandbox-core/lib/config/reference_path.rs` lines 57-66:

```rust
// Only detects "." and "/" as path prefixes
if s.starts_with('.') || s.starts_with('/') {
    Ok(ReferenceOrPath::Path(PathBuf::from(s)))
```

Fix: also detect Windows absolute paths:

```rust
fn is_filesystem_path(s: &str) -> bool {
    s.starts_with('.') || s.starts_with('/')
        || (s.len() >= 3
            && s.as_bytes()[0].is_ascii_alphabetic()
            && s.as_bytes()[1] == b':'
            && (s.as_bytes()[2] == b'\\' || s.as_bytes()[2] == b'/'))
}
```

## Config fields needing type migration

**Host paths** (must become `HostPathBuf`):

| Field | File | Line |
|---|---|---|
| `Meta.readme` | `config/microsandbox/config.rs` | 89 |
| `Meta.icon` | `config/microsandbox/config.rs` | 104 |
| `Build.workdir` | `config/microsandbox/config.rs` | 166 |
| `env_file` | `microsandbox-cli/lib/args/msb.rs` | 97 |
| `PathPair.host` | `config/path_pair.rs` | 44 |
| volumes (host side) | `config/microsandbox/config.rs` | 141, 251 |

**Guest paths** (stay `GuestPathBuf` / `Utf8UnixPathBuf`):

| Field | File | Line |
|---|---|---|
| `Sandbox.workdir` | `config/microsandbox/config.rs` | 272 |
| `MicroVmConfig.exec_path` | `vm/microvm.rs` | 153 |
| `MicroVmConfig.workdir_path` | `vm/microvm.rs` | 150 |
| `MicroVmConfig.console_output` | `vm/microvm.rs` | 162 |
| `PathPair.guest` | `config/path_pair.rs` | 47 |

## Structured mount syntax

YAML config:

```yaml
volumes:
  # Legacy (Unix only, backward compat):
  - /host/path:/guest/path
  # New structured form (all platforms):
  - host: C:\Users\jonas\project
    guest: /workspace
```

CLI:

```
--volume host=C:\Users\jonas\project,guest=/workspace
```

Parsing priority:
1. If the value is a YAML map with `host`/`guest` keys, parse as structured.
2. If the value is a string, attempt legacy `:` split only on `cfg(unix)`.
3. On Windows, reject ambiguous `host:guest` strings with a targeted error.

## Path normalization

`microsandbox-utils/lib/path.rs` lines 164-205 — currently hardcodes `Utf8UnixPathBuf`:

```rust
let path = Utf8UnixPathBuf::from(path);
// ... uses Utf8UnixComponent
```

Rules:
- **Host paths**: use `std::path::PathBuf` / `std::path::Path` — respects OS separator.
- **Guest paths**: use `Utf8UnixPathBuf` — always `/` separator.
- Never normalize host paths through `Utf8UnixPathBuf`.
- Relative host paths resolved with `std::fs::canonicalize` or `std::path::Path::join`.

## Migration targets

| From | To | Files |
|---|---|---|
| `PathPair` | `MountSpec` | `config/path_pair.rs`, `vm/microvm.rs:132`, `management/rootfs.rs:119` |
| `Utf8UnixPathBuf` (host fields) | `HostPathBuf` | `config/microsandbox/config.rs:89,104,166` |
| `Vec<PathPair>` (volumes) | `Vec<MountSpec>` | `config/microsandbox/config.rs:141,251`, `cli/args/msb.rs:83-85,299-301,351-353` |
| `normalize_path` (guest) | Keep as-is for guest paths | `microsandbox-utils/lib/path.rs:157` |
| `:` split parsing | Structured or platform-aware | `config/path_pair.rs:96-97` |

## Anti-patterns

- **Don't split on `:` on Windows.** `C:\foo:/bar` has 3 colons.
- **Don't use `Utf8UnixPathBuf` for host paths.** It rejects `\` and drive letters.
- **Don't use `cfg(not(unix))` as a proxy for Windows.** Use `cfg(windows)` explicitly.
- **Don't resolve relative guest paths with host path APIs.** Guest paths are always Unix.
- **Don't pass raw `String` volume specs to the runtime.** Parse into `MountSpec` at the config boundary.
