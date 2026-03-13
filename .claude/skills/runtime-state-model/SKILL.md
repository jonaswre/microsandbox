---
name: runtime-state-model
description: Backend-neutral RuntimeHandle, RuntimeRecord, ResolvedSandboxSpec, DB schema migration from PID-only, BackendKind enum
user-invocable: false
---

# Runtime State Model

## Current schema

`microsandbox-core/lib/models.rs` lines 10-41 — Sandbox struct:

```rust
pub struct Sandbox {
    pub id: i64,
    pub name: String,
    pub config_file: String,
    pub config_last_modified: DateTime<Utc>,
    pub status: String,
    pub supervisor_pid: u32,     // Unix-only concept
    pub microvm_pid: u32,        // Unix-only concept
    pub rootfs_paths: String,    // Serialized "overlayfs:path1:path2" or "native:path"
    pub created_at: DateTime<Utc>,
    pub modified_at: DateTime<Utc>,
}
```

`microsandbox-core/lib/management/db.rs` lines 153-155 — binds PIDs:

```rust
.bind(sandbox.supervisor_pid)
.bind(sandbox.microvm_pid)
.bind(&sandbox.rootfs_paths)
```

`microsandbox-core/lib/management/orchestra.rs` lines 65-89 — SandboxStatus:

```rust
pub struct SandboxStatus {
    pub name: String,
    pub running: bool,
    pub supervisor_pid: Option<u32>,
    pub microvm_pid: Option<u32>,
    pub cpu_usage: Option<f32>,
    pub memory_usage: Option<u64>,
    pub disk_usage: Option<u64>,
    pub rootfs_paths: Option<String>,
}
```

## rootfs_paths serialization format

`microsandbox-core/lib/runtime/monitor.rs` lines 140-150:

```rust
let rootfs_paths = match &self.rootfs {
    Rootfs::Native(path) => format!("native:{}", path.to_string_lossy()),
    Rootfs::Overlayfs(paths) => format!(
        "overlayfs:{}",
        paths.iter().map(|p| p.to_string_lossy()).collect::<Vec<_>>().join(":")
    ),
};
```

Parsing in `orchestra.rs` lines 704-722: splits on `:` — fragile and Unix-only.

## BackendKind enum

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BackendKind {
    /// libkrun-based backend (Linux/macOS)
    Krun,
    /// Hyper-V HCS-based backend (Windows)
    WindowsHcs,
}
```

## ResolvedSandboxSpec

The backend-neutral sandbox intent produced by config resolution:

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

## RuntimeHandle

Replaces `supervisor_pid + microvm_pid` as the persisted runtime identity:

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

Field semantics:

| Field | Krun (Unix) | WindowsHcs |
|---|---|---|
| `runtime_id` | Microsandbox-generated UUID | Microsandbox-generated UUID |
| `worker_pid` | supervisor PID | msbrun-hcs.exe PID |
| `control_endpoint` | empty (signals) | `\\.\pipe\microsandbox-runtime\<id>` |
| `backend_object_id` | microvm PID as string | HCS compute system ID |

## RuntimeRecord

Full persisted record with backend metadata for crash recovery:

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

`backend_state_json` for Windows:

```json
{
    "hcn_endpoint_id": "...",
    "hcn_load_balancer_ids": ["..."],
    "scratch_disk_path": "...",
    "control_share_path": "...",
    "boot_bundle_version": "...",
    "layer_digests": ["sha256:..."]
}
```

## DB migration

New columns alongside old ones for backward compatibility:

```sql
ALTER TABLE sandboxes ADD COLUMN backend_kind TEXT DEFAULT 'krun';
ALTER TABLE sandboxes ADD COLUMN runtime_id TEXT DEFAULT '';
ALTER TABLE sandboxes ADD COLUMN worker_pid INTEGER DEFAULT 0;
ALTER TABLE sandboxes ADD COLUMN control_endpoint TEXT DEFAULT '';
ALTER TABLE sandboxes ADD COLUMN backend_object_id TEXT DEFAULT '';
ALTER TABLE sandboxes ADD COLUMN rootfs_descriptor_json TEXT DEFAULT '';
ALTER TABLE sandboxes ADD COLUMN backend_state_json TEXT DEFAULT '';
ALTER TABLE sandboxes ADD COLUMN portal_port INTEGER DEFAULT 0;
```

Migration path:
1. Add new columns with defaults so existing Unix records continue working.
2. New status/stop/disk-usage code reads from new columns.
3. `save_or_update_sandbox` writes both old and new columns during transition.
4. Old columns (`supervisor_pid`, `microvm_pid`, `rootfs_paths`) are deprecated but not dropped.

## Control endpoint model

- **Unix (Krun):** empty string — supervisor uses Unix signals (`SIGTERM`).
- **Windows (HCS):** named pipe path `\\.\pipe\microsandbox-runtime\<runtime-id>` — supports `status`, `stop`, `attach`, `collect_logs` commands.

## Process health checks

Current (Unix-only) in `orchestra.rs` lines 248-250:

```rust
signal::kill(Pid::from_raw(sandbox.supervisor_pid as i32), Signal::SIGTERM)
```

And `management.rs` lines 93-97:

```rust
let process_running = unsafe { libc::kill(pid, 0) == 0 };
```

Backend-neutral replacement:

```rust
impl RuntimeHandle {
    fn is_alive(&self) -> bool {
        match self.backend_kind {
            BackendKind::Krun => {
                // Unix: kill(pid, 0)
                nix::sys::signal::kill(Pid::from_raw(self.worker_pid as i32), None).is_ok()
            }
            BackendKind::WindowsHcs => {
                // Windows: OpenProcess + check
                // or: ping the named pipe control endpoint
                todo!()
            }
        }
    }
}
```

## Anti-patterns

- **Don't use `microvm_pid` as universal process identity.** On Windows the backend object ID is the HCS compute system ID, not a PID.
- **Don't parse `rootfs_paths` strings for health or disk usage.** Use `rootfs_descriptor_json` with structured data.
- **Don't assume signal-based liveness checks.** Windows uses process handles and named pipe connectivity.
- **Don't store Unix-only state in shared DB columns.** Use `backend_state_json` for backend-specific data.
- **Don't drop old columns in a breaking migration.** Add new columns alongside, deprecate old ones gradually.
