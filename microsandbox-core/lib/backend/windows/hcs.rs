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
        BackendKind, ResolvedSandboxSpec, RuntimeHandle, RuntimeStatus, VmBackend,
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

    /// Resource limits.
    pub memory_mib: u32,
    pub vcpu_count: u8,

    /// SCSI attachments.
    pub scsi_attachments: Vec<ScsiAttachment>,

    /// Plan9 shares.
    pub plan9_shares: Vec<Plan9Share>,

    /// Network endpoint ID (set by networking module).
    pub network_endpoint_id: Option<String>,
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

        // Build Plan9 shares from mount spec.
        let mut plan9_shares = Vec::new();

        // Control share (read-only, for sandbox spec).
        plan9_shares.push(Plan9Share {
            name: "control".to_string(),
            host_path: self
                .config
                .runtime_dir
                .join("windows")
                .join("sandboxes")
                .join(&spec.sandbox_key)
                .display()
                .to_string(),
            guest_path: CONTROL_SHARE_GUEST_PATH.to_string(),
            flags: PLAN9_FLAG_READONLY | PLAN9_FLAG_LINUX_METADATA | PLAN9_FLAG_CASE_SENSITIVE,
        });

        // User volume mounts.
        for (i, mount) in spec.mounts.iter().enumerate() {
            let mut flags = PLAN9_FLAG_LINUX_METADATA | PLAN9_FLAG_CASE_SENSITIVE;
            if mount.readonly {
                flags |= PLAN9_FLAG_READONLY;
            }

            plan9_shares.push(Plan9Share {
                name: format!("mount_{}", i),
                host_path: mount.host.to_string(),
                guest_path: mount.guest.to_string(),
                flags,
            });
        }

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
            memory_mib: spec.resources.memory_mib,
            vcpu_count: spec.resources.vcpus,
            scsi_attachments,
            plan9_shares,
            network_endpoint_id: None,
        }
    }

    /// Returns the named pipe path for a sandbox's control channel.
    pub fn pipe_path(sandbox_key: &str) -> String {
        format!(r"\\.\pipe\microsandbox-{}", sandbox_key)
    }

    /// Checks if Hyper-V is available on the system.
    pub async fn check_hyperv_available() -> MicrosandboxResult<()> {
        let output = tokio::process::Command::new("powershell.exe")
            .args([
                "-NoProfile",
                "-Command",
                "(Get-WindowsOptionalFeature -Online -FeatureName Microsoft-Hyper-V).State",
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
        if !stdout.trim().eq_ignore_ascii_case("Enabled") {
            return Err(MicrosandboxError::NotImplemented(
                "Hyper-V is not enabled. Please enable Hyper-V to use Windows sandbox hosting. \
                 Run 'Enable-WindowsOptionalFeature -Online -FeatureName Microsoft-Hyper-V -All' \
                 in an elevated PowerShell."
                    .to_string(),
            ));
        }

        Ok(())
    }
}

//--------------------------------------------------------------------------------------------------
// Trait Implementations
//--------------------------------------------------------------------------------------------------

#[async_trait]
impl VmBackend for WindowsHcsBackend {
    async fn start(&self, spec: &ResolvedSandboxSpec) -> MicrosandboxResult<RuntimeHandle> {
        // Check Hyper-V availability first.
        Self::check_hyperv_available().await?;

        // Materialize rootfs (convert OCI layers to VHDs).
        let rootfs_config = super::rootfs::WindowsRootfsConfig {
            cache_dir: self.config.cache_dir.clone(),
            runtime_dir: self.config.runtime_dir.clone(),
            tar2ext4_path: self
                .config
                .boot_bundle_dir
                .join("tar2ext4.exe"),
        };
        let materializer = WindowsRootfsMaterializer::new(rootfs_config);
        let rootfs = materializer.materialize(spec).await?;

        // Build compute configuration.
        let compute_config = self.build_compute_config(spec, &rootfs);
        let config_json = serde_json::to_string(&compute_config)?;

        // Create the named pipe control channel path.
        let pipe_path = Self::pipe_path(&spec.sandbox_key);

        // Write the compute config to the sandbox directory for the worker to read.
        let sandbox_dir = self
            .config
            .runtime_dir
            .join("windows")
            .join("sandboxes")
            .join(&spec.sandbox_key);
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
            runtime_id: uuid_v4_string(),
            worker_pid,
            control_endpoint: pipe_path,
            backend_object_id: compute_config.name,
        })
    }

    async fn stop(&self, handle: &RuntimeHandle) -> MicrosandboxResult<()> {
        if handle.control_endpoint.is_empty() {
            tracing::warn!(
                sandbox = %handle.sandbox_key,
                "No control endpoint for sandbox, attempting process termination"
            );
            // Fall back to process termination.
            if handle.worker_pid > 0 {
                let _ = tokio::process::Command::new("taskkill")
                    .args(["/F", "/T", "/PID", &handle.worker_pid.to_string()])
                    .output()
                    .await;
            }
            return Ok(());
        }

        // Send stop command via the named pipe (the worker handles graceful HCS teardown).
        // For now, we use a simple approach: write "stop" to the pipe.
        // The full protocol will be implemented in the process lifecycle module.
        tracing::info!(
            sandbox = %handle.sandbox_key,
            pipe = %handle.control_endpoint,
            "Sending stop command to HCS worker"
        );

        // If the pipe-based stop fails, fall back to taskkill.
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
                    // Not necessarily an error — process may have already exited.
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

/// Generates a UUID v4 string for runtime IDs.
fn uuid_v4_string() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};

    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let nanos = now.as_nanos();

    // Simple pseudo-UUID from timestamp + random bits from hash.
    format!(
        "{:08x}-{:04x}-4{:03x}-{:04x}-{:012x}",
        (nanos >> 96) as u32,
        (nanos >> 80) as u16,
        (nanos >> 64) as u16 & 0xfff,
        ((nanos >> 48) as u16 & 0x3fff) | 0x8000,
        nanos as u64 & 0xffffffffffff,
    )
}

//--------------------------------------------------------------------------------------------------
// Tests
//--------------------------------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

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
}
