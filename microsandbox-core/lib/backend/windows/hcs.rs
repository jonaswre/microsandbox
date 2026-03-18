//! WindowsHcsBackend — HCS (Host Compute Service) VM backend for Windows.
//!
//! Creates HCS compute systems with `LinuxKernelDirect` boot mode, attaches VHD
//! layers via SCSI, configures Plan9 shares for host directory mounts, and applies
//! resource limits.

use std::path::PathBuf;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::{
    MicrosandboxError, MicrosandboxResult,
    backend::{
        BackendKind, ResolvedSandboxSpec, RootfsMaterializer, RuntimeHandle, RuntimeStatus,
        VmBackend,
        windows::rootfs::{WindowsMaterializedRootfs, WindowsRootfsMaterializer},
    },
};

//--------------------------------------------------------------------------------------------------
// Constants
//--------------------------------------------------------------------------------------------------

/// Plan9 share flag: ReadOnly.
const PLAN9_FLAG_READONLY: u32 = 0x1;

/// Plan9 share flag: LinuxMetadata (preserves uid/gid/mode).
const PLAN9_FLAG_LINUX_METADATA: u32 = 0x4;

/// Plan9 share flag: CaseSensitive.
const PLAN9_FLAG_CASE_SENSITIVE: u32 = 0x8;

/// Guest path for the control share.
const CONTROL_SHARE_GUEST_PATH: &str = "/run/microsandbox/control";

/// Name of the worker binary on Windows.
const WORKER_BINARY: &str = "msbrun-hcs.exe";

//--------------------------------------------------------------------------------------------------
// Types
//--------------------------------------------------------------------------------------------------

/// HCS compute system configuration, serialized as JSON for the worker process.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HcsComputeConfig {
    /// The compute system name (sandbox key).
    pub name: String,

    /// Path to the Linux kernel from the boot bundle.
    pub kernel_path: String,

    /// Path to the rootfs VHD from the boot bundle.
    pub initrd_path: String,

    /// Kernel command line (e.g., `init=/bootstrap console=hvc0 layers=2`).
    pub kernel_cmdline: String,

    /// Memory allocation in MiB.
    pub memory_mib: u32,

    /// Number of virtual CPUs.
    pub vcpu_count: u8,

    /// SCSI attachments.
    pub scsi_attachments: Vec<ScsiAttachment>,

    /// Plan9 shares.
    pub plan9_shares: Vec<Plan9Share>,

    /// Network endpoint ID (set by networking module).
    pub network_endpoint_id: Option<String>,

    /// Named pipe path for serial console redirection (COM1 → ttyS0).
    /// When set, HCS creates a named pipe server and the worker relays I/O.
    pub console_pipe_path: Option<String>,

    /// Host port to proxy to the guest portal (port 4444).
    /// When set, the worker starts a TCP relay on `127.0.0.1:<port>`.
    pub portal_host_port: Option<u16>,
}

/// A VHD to attach via SCSI to the compute system.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScsiAttachment {
    /// Path to the VHD or VHDX file.
    pub path: String,

    /// Whether the attachment is read-only.
    pub readonly: bool,

    /// SCSI controller number (typically 0).
    pub controller: u32,

    /// SCSI LUN number (logical unit number).
    pub lun: u32,
}

/// A Plan9 share configuration for host directory mounts.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Plan9Share {
    /// Name of the share (used as tag in the guest).
    pub name: String,

    /// Host directory path.
    pub host_path: String,

    /// Guest mount path.
    pub guest_path: String,

    /// Share flags (combination of READONLY, LINUX_METADATA, CASE_SENSITIVE).
    pub flags: u32,
}

/// Configuration for the Windows HCS backend.
#[derive(Debug, Clone)]
pub struct WindowsHcsConfig {
    /// Path to the boot bundle directory.
    pub boot_bundle_dir: PathBuf,

    /// Path to the msbrun-hcs.exe worker binary.
    pub worker_path: PathBuf,

    /// The rootfs materializer for VHD conversion.
    pub cache_dir: PathBuf,

    /// Runtime directory for per-sandbox state.
    pub runtime_dir: PathBuf,
}

/// Windows HCS VM backend implementation.
///
/// Spawns `msbrun-hcs.exe` worker processes that create HCS compute systems.
/// Communication between the Rust control plane and Go worker happens via named pipes.
#[derive(Debug)]
pub struct WindowsHcsBackend {
    config: WindowsHcsConfig,
}

/// Persisted networking state for a running sandbox.
///
/// Written to `networking-state.json` in the sandbox directory during `start()`,
/// read during `stop()` for cleanup.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct NetworkingState {
    /// HCN endpoint ID.
    endpoint_id: String,
    /// IP address assigned to the endpoint.
    endpoint_ip: String,
    /// HCN load balancer IDs for port forwarding.
    load_balancer_ids: Vec<String>,
}

//--------------------------------------------------------------------------------------------------
// Methods
//--------------------------------------------------------------------------------------------------

impl WindowsHcsBackend {
    /// Creates a new `WindowsHcsBackend` with the given configuration.
    pub fn new(config: WindowsHcsConfig) -> Self {
        Self { config }
    }

    /// Builds the HCS compute system configuration from a resolved spec and materialized rootfs.
    pub fn build_compute_config(
        &self,
        spec: &ResolvedSandboxSpec,
        rootfs: &WindowsMaterializedRootfs,
    ) -> HcsComputeConfig {
        let mut scsi_attachments = Vec::new();
        let mut lun: u32 = 0;

        // Attach layer VHDs as read-only SCSI devices.
        for vhd_path in &rootfs.layer_vhds {
            scsi_attachments.push(ScsiAttachment {
                path: vhd_path.display().to_string(),
                readonly: true,
                controller: 0,
                lun,
            });
            lun += 1;
        }

        // Attach patch VHD as read-only.
        scsi_attachments.push(ScsiAttachment {
            path: rootfs.patch_vhd.display().to_string(),
            readonly: true,
            controller: 0,
            lun,
        });
        lun += 1;

        // Attach scratch VHDX as read-write.
        scsi_attachments.push(ScsiAttachment {
            path: rootfs.scratch_vhdx.display().to_string(),
            readonly: false,
            controller: 0,
            lun,
        });

        // Plan9 shares for live bidirectional host directory sharing via 9p over vsock.
        let plan9_shares: Vec<Plan9Share> = spec
            .mounts
            .iter()
            .enumerate()
            .map(|(i, m)| {
                let flags = if m.readonly {
                    PLAN9_FLAG_READONLY | PLAN9_FLAG_LINUX_METADATA | PLAN9_FLAG_CASE_SENSITIVE
                } else {
                    PLAN9_FLAG_LINUX_METADATA | PLAN9_FLAG_CASE_SENSITIVE
                };
                Plan9Share {
                    name: format!("mount_{}", i),
                    host_path: m.host.as_path().display().to_string(),
                    guest_path: m.guest.as_str().to_string(),
                    flags,
                }
            })
            .collect();

        HcsComputeConfig {
            name: spec.sandbox_key.clone(),
            kernel_path: self
                .config
                .boot_bundle_dir
                .join("kernel")
                .display()
                .to_string(),
            initrd_path: self
                .config
                .boot_bundle_dir
                .join("rootfs.vhd")
                .display()
                .to_string(),
            kernel_cmdline: format!(
                "root=/dev/ram0 init=/bootstrap console=ttyS0 panic=1 layers={} mounts={}",
                rootfs.layer_vhds.len(),
                spec.mounts.len()
            ),
            memory_mib: spec.resources.memory_mib,
            vcpu_count: spec.resources.vcpus,
            scsi_attachments,
            plan9_shares,
            network_endpoint_id: None,
            console_pipe_path: Some(format!(
                r"\\.\pipe\microsandbox-console-{}",
                spec.sandbox_key
            )),
            portal_host_port: spec
                .ports
                .iter()
                .find(|p| p.get_guest() == 4444)
                .map(|p| p.get_host()),
        }
    }

    /// Returns the named pipe path for a sandbox's control channel.
    pub fn pipe_path(sandbox_key: &str) -> String {
        format!(r"\\.\pipe\microsandbox-{}", sandbox_key)
    }

    /// Grants the Hyper-V VM worker account full control on a path.
    ///
    /// HCS runs VMs under `NT VIRTUAL MACHINE\Virtual Machines` which needs
    /// explicit permissions on VHD files and boot bundle assets. Uses backslash-
    /// normalized paths and grants `(OI)(CI)F` (full control, inherited) with a
    /// single retry on failure.
    async fn grant_vm_access(path: &std::path::Path) {
        if !path.exists() {
            return;
        }
        // Normalize to backslashes for icacls compatibility.
        let path_str = path.display().to_string().replace('/', "\\");

        for attempt in 1..=2 {
            let result = tokio::process::Command::new("icacls")
                .args([
                    &path_str,
                    "/grant",
                    "NT VIRTUAL MACHINE\\Virtual Machines:(OI)(CI)F",
                    "/T",
                    "/Q",
                ])
                .output()
                .await;

            match result {
                Ok(o) if o.status.success() => {
                    tracing::debug!(path = %path_str, "Granted VM full access");
                    return;
                }
                Ok(o) => {
                    let stderr = String::from_utf8_lossy(&o.stderr);
                    tracing::warn!(
                        path = %path_str,
                        attempt,
                        "icacls failed: {}",
                        stderr.trim()
                    );
                }
                Err(e) => {
                    tracing::warn!(path = %path_str, attempt, "icacls error: {}", e);
                }
            }

            if attempt < 2 {
                tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            }
        }
    }

    /// Checks if the current process is running with administrator privileges.
    ///
    /// HCS and HCN APIs require elevation. Calling this early produces an
    /// actionable error instead of cryptic HRESULT failures deeper in the stack.
    pub async fn check_admin_elevated() -> MicrosandboxResult<()> {
        let output = tokio::process::Command::new("powershell.exe")
            .args([
                "-NoProfile",
                "-Command",
                "([Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()).IsInRole([Security.Principal.WindowsBuiltinRole]::Administrator)",
            ])
            .output()
            .await
            .map_err(|e| {
                MicrosandboxError::NotImplemented(format!(
                    "Failed to check administrator status: {}. Ensure PowerShell is available.",
                    e
                ))
            })?;

        let stdout = String::from_utf8_lossy(&output.stdout);
        if stdout.trim().eq_ignore_ascii_case("True") {
            return Ok(());
        }

        Err(MicrosandboxError::NotImplemented(
            "Administrator privileges required. HCS and HCN APIs need elevation. \
             Please run this command from an elevated (Run as Administrator) terminal."
                .to_string(),
        ))
    }

    /// Checks if Hyper-V is available on the system.
    ///
    /// Uses WMI `HypervisorPresent` (no admin required) with a fallback to
    /// checking whether the `vmcompute` service is running.
    pub async fn check_hyperv_available() -> MicrosandboxResult<()> {
        // Primary check: WMI HypervisorPresent (works without elevation).
        let output = tokio::process::Command::new("powershell.exe")
            .args([
                "-NoProfile",
                "-Command",
                "(Get-CimInstance Win32_ComputerSystem).HypervisorPresent",
            ])
            .output()
            .await
            .map_err(|e| {
                MicrosandboxError::NotImplemented(format!(
                    "Failed to check Hyper-V status: {}. Ensure PowerShell is available.",
                    e
                ))
            })?;

        let stdout = String::from_utf8_lossy(&output.stdout);
        if stdout.trim().eq_ignore_ascii_case("True") {
            return Ok(());
        }

        // Fallback: check if the vmcompute service is running.
        let sc_output = tokio::process::Command::new("sc.exe")
            .args(["query", "vmcompute"])
            .output()
            .await;

        if let Ok(o) = sc_output {
            let sc_stdout = String::from_utf8_lossy(&o.stdout);
            if sc_stdout.contains("RUNNING") {
                return Ok(());
            }
        }

        Err(MicrosandboxError::NotImplemented(
            "Hyper-V is not enabled. Please enable Hyper-V to use Windows sandbox hosting. \
             Run 'Enable-WindowsOptionalFeature -Online -FeatureName Microsoft-Hyper-V -All' \
             in an elevated PowerShell."
                .to_string(),
        ))
    }
}

//--------------------------------------------------------------------------------------------------
// Trait Implementations
//--------------------------------------------------------------------------------------------------

#[async_trait]
impl VmBackend for WindowsHcsBackend {
    async fn start(&self, spec: &ResolvedSandboxSpec) -> MicrosandboxResult<RuntimeHandle> {
        // Check prerequisites: admin privileges then Hyper-V availability.
        Self::check_admin_elevated().await?;
        Self::check_hyperv_available().await?;

        // Generate a unique session ID for this invocation. This prevents named
        // pipe conflicts when `msb exe` is run multiple times in quick succession
        // — Windows holds closed pipes in a TIME_WAIT-like state, so reusing the
        // same pipe name causes "Access denied" errors.
        let runtime_id = uuid_v4_string();
        let session_suffix = &runtime_id[..8];

        // Materialize rootfs (convert OCI layers to VHDs).
        let rootfs_config = super::rootfs::WindowsRootfsConfig {
            cache_dir: self.config.cache_dir.clone(),
            runtime_dir: self.config.runtime_dir.clone(),
            tar2ext4_path: self.config.boot_bundle_dir.join("tar2ext4.exe"),
        };
        let materializer = WindowsRootfsMaterializer::new(rootfs_config);
        let rootfs = materializer.materialize(spec).await?;

        // Grant Hyper-V VM worker read access to VHD files.
        // HCS runs the VM under an NT VIRTUAL MACHINE account that needs explicit read access.
        // Must run after rootfs materialization so newly created files are included.
        Self::grant_vm_access(&self.config.cache_dir).await;
        Self::grant_vm_access(&self.config.runtime_dir).await;
        Self::grant_vm_access(&self.config.boot_bundle_dir).await;

        // Build compute configuration with session-scoped names to avoid conflicts.
        let mut compute_config = self.build_compute_config(spec, &rootfs);
        compute_config.name = format!("{}-{}", spec.sandbox_key, session_suffix);
        compute_config.console_pipe_path = Some(format!(
            r"\\.\pipe\microsandbox-console-{}-{}",
            spec.sandbox_key, session_suffix
        ));
        let config_json = serde_json::to_string(&compute_config)?;

        // Create a session-scoped named pipe control channel path.
        let pipe_path = format!(
            r"\\.\pipe\microsandbox-{}-{}",
            spec.sandbox_key, session_suffix
        );

        // Write the compute config to a session-scoped directory so parallel
        // invocations with the same sandbox_key don't overwrite each other.
        let sandbox_dir = self
            .config
            .runtime_dir
            .join("windows")
            .join("sandboxes")
            .join(format!("{}-{}", &spec.sandbox_key, session_suffix));
        tokio::fs::create_dir_all(&sandbox_dir).await?;
        let config_path = sandbox_dir.join("compute-config.json");
        tokio::fs::write(&config_path, &config_json).await?;

        // Spawn the worker process.
        let worker_path = &self.config.worker_path;
        let child = tokio::process::Command::new(worker_path)
            .arg("--pipe")
            .arg(&pipe_path)
            .arg("--config")
            .arg(&config_path)
            .spawn()
            .map_err(|e| {
                MicrosandboxError::SupervisorBinaryNotFound(format!(
                    "Failed to spawn {}: {}",
                    worker_path.display(),
                    e
                ))
            })?;

        let worker_pid = child.id().unwrap_or(0);

        tracing::info!(
            sandbox = %spec.sandbox_key,
            pid = worker_pid,
            pipe = %pipe_path,
            "Started Windows HCS worker"
        );

        Ok(RuntimeHandle {
            sandbox_key: spec.sandbox_key.clone(),
            backend_kind: BackendKind::WindowsHcs,
            runtime_id,
            worker_pid,
            control_endpoint: pipe_path,
            backend_object_id: compute_config.name,
        })
    }

    async fn stop(&self, handle: &RuntimeHandle) -> MicrosandboxResult<()> {
        // HCN networking cleanup is handled by the Go worker on shutdown.
        if handle.control_endpoint.is_empty() {
            tracing::warn!(
                sandbox = %handle.sandbox_key,
                "No control endpoint for sandbox, attempting process termination"
            );
            if handle.worker_pid > 0 {
                let _ = tokio::process::Command::new("taskkill")
                    .args(["/F", "/T", "/PID", &handle.worker_pid.to_string()])
                    .output()
                    .await;
            }
            return Ok(());
        }

        tracing::info!(
            sandbox = %handle.sandbox_key,
            pipe = %handle.control_endpoint,
            "Sending stop command to HCS worker"
        );

        // Terminate the worker process.
        if handle.worker_pid > 0 {
            let output = tokio::process::Command::new("taskkill")
                .args(["/F", "/T", "/PID", &handle.worker_pid.to_string()])
                .output()
                .await;

            match output {
                Ok(o) if o.status.success() => {
                    tracing::info!(
                        sandbox = %handle.sandbox_key,
                        pid = handle.worker_pid,
                        "Terminated HCS worker process"
                    );
                }
                Ok(o) => {
                    let stderr = String::from_utf8_lossy(&o.stderr);
                    tracing::debug!(
                        sandbox = %handle.sandbox_key,
                        "taskkill output: {}",
                        stderr
                    );
                }
                Err(e) => {
                    tracing::warn!(
                        sandbox = %handle.sandbox_key,
                        "Failed to run taskkill: {}",
                        e
                    );
                }
            }
        }

        Ok(())
    }

    async fn status(&self, handle: &RuntimeHandle) -> MicrosandboxResult<RuntimeStatus> {
        if handle.control_endpoint.is_empty() {
            return Ok(RuntimeStatus::Unknown);
        }

        // Check if the worker process is still running by checking the named pipe.
        // For now, fall back to checking if the PID is alive.
        if handle.worker_pid > 0 {
            let output = tokio::process::Command::new("tasklist")
                .args(["/FI", &format!("PID eq {}", handle.worker_pid), "/NH"])
                .output()
                .await;

            match output {
                Ok(o) => {
                    let stdout = String::from_utf8_lossy(&o.stdout);
                    if stdout.contains(&handle.worker_pid.to_string()) {
                        return Ok(RuntimeStatus::Running);
                    }
                    return Ok(RuntimeStatus::Stopped);
                }
                Err(_) => return Ok(RuntimeStatus::Unknown),
            }
        }

        Ok(RuntimeStatus::Unknown)
    }
}

//--------------------------------------------------------------------------------------------------
// Helper functions
//--------------------------------------------------------------------------------------------------

/// Generates a pseudo-UUID v4 string for runtime IDs.
///
/// Uses the process ID for the first segment (unique across concurrent
/// invocations) and nanosecond timestamp for the remaining segments (unique
/// across sequential invocations from the same PID).
fn uuid_v4_string() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};

    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos() as u64;
    let pid = std::process::id();

    format!(
        "{:08x}-{:04x}-4{:03x}-{:04x}-{:012x}",
        pid,
        (nanos >> 48) as u16,
        (nanos >> 36) as u16 & 0xfff,
        ((nanos >> 24) as u16 & 0x3fff) | 0x8000,
        nanos & 0xffffffffffff,
    )
}

//--------------------------------------------------------------------------------------------------
// Tests
//--------------------------------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::spec::{ExecSpec, ResolvedSandboxSpec, ResourceLimits, RootfsSource};
    use crate::config::{NetworkScope, PortPair};

    /// Creates a minimal `ResolvedSandboxSpec` for testing.
    fn make_test_spec(sandbox_key: &str) -> ResolvedSandboxSpec {
        ResolvedSandboxSpec {
            sandbox_key: sandbox_key.to_string(),
            rootfs_source: RootfsSource::Overlayfs {
                layers: vec![],
                rw_dir: PathBuf::from(r"C:\rw"),
                patch_dir: PathBuf::from(r"C:\patch"),
            },
            mounts: vec![],
            ports: vec![],
            env: vec![],
            workdir: None,
            exec: ExecSpec {
                path: typed_path::Utf8UnixPathBuf::from("/bin/sh"),
                args: vec![],
            },
            resources: ResourceLimits::default(),
            scope: NetworkScope::default(),
            portal: None,
        }
    }

    #[test]
    fn test_pipe_path() {
        assert_eq!(
            WindowsHcsBackend::pipe_path("project~sandbox1"),
            r"\\.\pipe\microsandbox-project~sandbox1"
        );
    }

    #[test]
    fn test_plan9_share_flags() {
        // User mount: LinuxMetadata + CaseSensitive.
        let user_flags = PLAN9_FLAG_LINUX_METADATA | PLAN9_FLAG_CASE_SENSITIVE;
        assert_eq!(user_flags, 0xC);

        // Readonly user mount: ReadOnly + LinuxMetadata + CaseSensitive.
        let ro_flags = PLAN9_FLAG_READONLY | PLAN9_FLAG_LINUX_METADATA | PLAN9_FLAG_CASE_SENSITIVE;
        assert_eq!(ro_flags, 0xD);

        // Control share: ReadOnly + LinuxMetadata + CaseSensitive.
        assert_eq!(ro_flags, 0xD);
    }

    #[test]
    fn test_scsi_attachment_serialization() {
        let attachment = ScsiAttachment {
            path: r"C:\cache\layers\sha256_abc\layer.vhd".to_string(),
            readonly: true,
            controller: 0,
            lun: 0,
        };

        let json = serde_json::to_string(&attachment).unwrap();
        let deserialized: ScsiAttachment = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.path, attachment.path);
        assert!(deserialized.readonly);
        assert_eq!(deserialized.lun, 0);
    }

    #[test]
    fn test_plan9_share_serialization() {
        let share = Plan9Share {
            name: "mount_0".to_string(),
            host_path: r"C:\Users\jonas\project".to_string(),
            guest_path: "/workspace".to_string(),
            flags: PLAN9_FLAG_LINUX_METADATA | PLAN9_FLAG_CASE_SENSITIVE,
        };

        let json = serde_json::to_string(&share).unwrap();
        let deserialized: Plan9Share = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.name, "mount_0");
        assert_eq!(deserialized.flags, 0xC);
    }

    #[test]
    fn test_compute_config_serialization() {
        let config = HcsComputeConfig {
            name: "test~sandbox".to_string(),
            kernel_path: r"C:\boot\kernel".to_string(),
            initrd_path: r"C:\boot\rootfs.vhd".to_string(),
            kernel_cmdline: "init=/bootstrap console=ttyS0 panic=1 layers=1".to_string(),
            memory_mib: 512,
            vcpu_count: 2,
            scsi_attachments: vec![ScsiAttachment {
                path: r"C:\layer.vhd".to_string(),
                readonly: true,
                controller: 0,
                lun: 0,
            }],
            plan9_shares: vec![Plan9Share {
                name: "control".to_string(),
                host_path: r"C:\runtime\sandbox".to_string(),
                guest_path: "/run/microsandbox/control".to_string(),
                flags: PLAN9_FLAG_READONLY | PLAN9_FLAG_LINUX_METADATA | PLAN9_FLAG_CASE_SENSITIVE,
            }],
            network_endpoint_id: None,
            console_pipe_path: Some(r"\\.\pipe\microsandbox-console-test".to_string()),
            portal_host_port: None,
        };

        let json = serde_json::to_string_pretty(&config).unwrap();
        let deserialized: HcsComputeConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.name, "test~sandbox");
        assert_eq!(deserialized.memory_mib, 512);
        assert_eq!(deserialized.vcpu_count, 2);
        assert_eq!(deserialized.scsi_attachments.len(), 1);
        assert_eq!(deserialized.plan9_shares.len(), 1);
        assert!(deserialized.network_endpoint_id.is_none());
    }

    #[test]
    fn test_uuid_v4_format() {
        let id = uuid_v4_string();
        // Should have 5 groups separated by hyphens.
        let parts: Vec<&str> = id.split('-').collect();
        assert_eq!(parts.len(), 5);
        // Third group should start with '4' (version 4).
        assert!(parts[2].starts_with('4'));
    }

    #[test]
    fn test_kernel_cmdline_format() {
        let cmdline = format!(
            "root=/dev/ram0 init=/bootstrap console=ttyS0 panic=1 layers={} mounts={}",
            3, 2
        );
        assert!(cmdline.contains("init=/bootstrap"));
        assert!(cmdline.contains("console=ttyS0"));
        assert!(cmdline.contains("layers=3"));
        assert!(cmdline.contains("mounts=2"));
        assert!(cmdline.contains("panic=1"));
    }

    #[test]
    fn test_kernel_cmdline_zero_mounts() {
        let cmdline = format!(
            "root=/dev/ram0 init=/bootstrap console=ttyS0 panic=1 layers={} mounts={}",
            2, 0
        );
        assert!(cmdline.contains("layers=2"));
        assert!(cmdline.contains("mounts=0"));
    }

    #[test]
    fn test_scsi_lun_numbering() {
        use super::super::rootfs::WindowsMaterializedRootfs;

        let rootfs = WindowsMaterializedRootfs {
            layer_vhds: vec![
                PathBuf::from(r"C:\cache\layer0.vhd"),
                PathBuf::from(r"C:\cache\layer1.vhd"),
            ],
            patch_vhd: PathBuf::from(r"C:\runtime\patch.vhd"),
            scratch_vhdx: PathBuf::from(r"C:\runtime\scratch.vhdx"),
        };

        let config = WindowsHcsConfig {
            boot_bundle_dir: PathBuf::from(r"C:\boot"),
            worker_path: PathBuf::from(r"C:\msbrun-hcs.exe"),
            cache_dir: PathBuf::from(r"C:\cache"),
            runtime_dir: PathBuf::from(r"C:\runtime"),
        };
        let backend = WindowsHcsBackend::new(config);

        let spec = make_test_spec("test~sandbox");
        let compute = backend.build_compute_config(&spec, &rootfs);

        // Expected LUN layout: 0: layer0, 1: layer1, 2: patch, 3: scratch
        // (mounts use 9p over vsock, not SCSI)
        assert_eq!(compute.scsi_attachments.len(), 4);
        assert_eq!(compute.scsi_attachments[0].lun, 0); // layer0
        assert_eq!(compute.scsi_attachments[1].lun, 1); // layer1
        assert_eq!(compute.scsi_attachments[2].lun, 2); // patch
        assert_eq!(compute.scsi_attachments[3].lun, 3); // scratch

        // Verify readonly flags.
        assert!(compute.scsi_attachments[0].readonly);  // layer: ro
        assert!(compute.scsi_attachments[2].readonly);  // patch: ro
        assert!(!compute.scsi_attachments[3].readonly); // scratch: rw

        // Verify kernel cmdline (no mounts in this spec).
        assert!(compute.kernel_cmdline.contains("layers=2"));
        assert!(compute.kernel_cmdline.contains("mounts=0"));
    }

    #[test]
    fn test_plan9_shares_from_mounts() {
        use super::super::rootfs::WindowsMaterializedRootfs;
        use crate::config::{GuestPathBuf, HostPathBuf, MountSpec};

        let rootfs = WindowsMaterializedRootfs {
            layer_vhds: vec![PathBuf::from(r"C:\cache\layer0.vhd")],
            patch_vhd: PathBuf::from(r"C:\runtime\patch.vhd"),
            scratch_vhdx: PathBuf::from(r"C:\runtime\scratch.vhdx"),
        };

        let config = WindowsHcsConfig {
            boot_bundle_dir: PathBuf::from(r"C:\boot"),
            worker_path: PathBuf::from(r"C:\msbrun-hcs.exe"),
            cache_dir: PathBuf::from(r"C:\cache"),
            runtime_dir: PathBuf::from(r"C:\runtime"),
        };
        let backend = WindowsHcsBackend::new(config);

        let mut spec = make_test_spec("test~sandbox");
        spec.mounts = vec![
            MountSpec::new(
                HostPathBuf::from(r"C:\Users\jonas\project"),
                GuestPathBuf::from("/workspace"),
                false,
            ),
            MountSpec::new(
                HostPathBuf::from(r"C:\data\readonly"),
                GuestPathBuf::from("/data"),
                true,
            ),
        ];

        let compute = backend.build_compute_config(&spec, &rootfs);

        // SCSI should only have layers + patch + scratch (no mount VHDs).
        assert_eq!(compute.scsi_attachments.len(), 3);

        // Plan9 shares should be populated from mounts.
        assert_eq!(compute.plan9_shares.len(), 2);
        assert_eq!(compute.plan9_shares[0].name, "mount_0");
        assert_eq!(compute.plan9_shares[0].host_path, r"C:\Users\jonas\project");
        assert_eq!(compute.plan9_shares[0].guest_path, "/workspace");
        assert_eq!(
            compute.plan9_shares[0].flags,
            PLAN9_FLAG_LINUX_METADATA | PLAN9_FLAG_CASE_SENSITIVE
        );

        assert_eq!(compute.plan9_shares[1].name, "mount_1");
        assert_eq!(compute.plan9_shares[1].guest_path, "/data");
        assert_eq!(
            compute.plan9_shares[1].flags,
            PLAN9_FLAG_READONLY | PLAN9_FLAG_LINUX_METADATA | PLAN9_FLAG_CASE_SENSITIVE
        );

        // Kernel cmdline should have mounts=2.
        assert!(compute.kernel_cmdline.contains("mounts=2"));
    }

    #[test]
    fn test_console_pipe_path_format() {
        let key = "project~sandbox1";
        let pipe = format!(r"\\.\pipe\microsandbox-console-{}", key);
        assert_eq!(pipe, r"\\.\pipe\microsandbox-console-project~sandbox1");
    }

    #[test]
    fn test_networking_state_serialization() {
        let state = NetworkingState {
            endpoint_id: "ep-abc123".to_string(),
            endpoint_ip: "172.28.0.2".to_string(),
            load_balancer_ids: vec!["lb-001".to_string(), "lb-002".to_string()],
        };

        let json = serde_json::to_string(&state).unwrap();
        let deserialized: NetworkingState = serde_json::from_str(&json).unwrap();

        assert_eq!(deserialized.endpoint_id, "ep-abc123");
        assert_eq!(deserialized.endpoint_ip, "172.28.0.2");
        assert_eq!(deserialized.load_balancer_ids.len(), 2);
        assert_eq!(deserialized.load_balancer_ids[0], "lb-001");
    }

    #[test]
    fn test_compute_config_with_console_pipe() {
        let config = HcsComputeConfig {
            name: "test".to_string(),
            kernel_path: r"C:\boot\kernel".to_string(),
            initrd_path: r"C:\boot\rootfs.vhd".to_string(),
            kernel_cmdline: "init=/bootstrap console=ttyS0 panic=1 layers=1".to_string(),
            memory_mib: 256,
            vcpu_count: 1,
            scsi_attachments: vec![],
            plan9_shares: vec![],
            network_endpoint_id: None,
            console_pipe_path: Some(r"\\.\pipe\microsandbox-console-test".to_string()),
            portal_host_port: None,
        };

        let json = serde_json::to_string(&config).unwrap();
        assert!(json.contains("console_pipe_path"));
        assert!(json.contains("microsandbox-console-test"));

        let deserialized: HcsComputeConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(
            deserialized.console_pipe_path.as_deref(),
            Some(r"\\.\pipe\microsandbox-console-test")
        );
    }

    #[tokio::test]
    async fn test_check_admin_elevated_returns_result() {
        let result = WindowsHcsBackend::check_admin_elevated().await;
        // In CI or non-admin contexts this returns an error containing "Administrator".
        // In an admin terminal it returns Ok. Either is valid — just verify the function works.
        match result {
            Ok(()) => {} // running elevated
            Err(e) => {
                let msg = format!("{}", e);
                assert!(
                    msg.contains("Administrator"),
                    "Error should mention Administrator: {}",
                    msg
                );
            }
        }
    }

    #[test]
    fn test_portal_host_port_from_spec() {
        use super::super::rootfs::WindowsMaterializedRootfs;
        use crate::config::PortPair;

        let rootfs = WindowsMaterializedRootfs {
            layer_vhds: vec![PathBuf::from(r"C:\cache\layer0.vhd")],
            patch_vhd: PathBuf::from(r"C:\runtime\patch.vhd"),
            scratch_vhdx: PathBuf::from(r"C:\runtime\scratch.vhdx"),
        };

        let config = WindowsHcsConfig {
            boot_bundle_dir: PathBuf::from(r"C:\boot"),
            worker_path: PathBuf::from(r"C:\msbrun-hcs.exe"),
            cache_dir: PathBuf::from(r"C:\cache"),
            runtime_dir: PathBuf::from(r"C:\runtime"),
        };
        let backend = WindowsHcsBackend::new(config);

        // Spec with a port pair mapping host 52345 → guest 4444.
        let mut spec = make_test_spec("test~portal");
        spec.ports = vec![PortPair::with_distinct(52345, 4444)];

        let compute = backend.build_compute_config(&spec, &rootfs);
        assert_eq!(compute.portal_host_port, Some(52345));

        // Spec without the portal port → None.
        let mut spec_no_portal = make_test_spec("test~noportal");
        spec_no_portal.ports = vec![PortPair::with_same(8080)];

        let compute2 = backend.build_compute_config(&spec_no_portal, &rootfs);
        assert!(compute2.portal_host_port.is_none());
    }

    #[test]
    fn test_portal_host_port_serialization() {
        let config = HcsComputeConfig {
            name: "test".to_string(),
            kernel_path: r"C:\boot\kernel".to_string(),
            initrd_path: r"C:\boot\rootfs.vhd".to_string(),
            kernel_cmdline: "init=/bootstrap console=ttyS0 panic=1 layers=1".to_string(),
            memory_mib: 256,
            vcpu_count: 1,
            scsi_attachments: vec![],
            plan9_shares: vec![],
            network_endpoint_id: None,
            console_pipe_path: None,
            portal_host_port: Some(52345),
        };

        let json = serde_json::to_string(&config).unwrap();
        assert!(json.contains("52345"));

        let deserialized: HcsComputeConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.portal_host_port, Some(52345));
    }
}
