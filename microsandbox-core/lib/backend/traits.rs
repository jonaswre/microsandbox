//! Backend traits — abstractions for VM lifecycle, rootfs materialization, and process supervision.

use async_trait::async_trait;

use crate::MicrosandboxResult;

use super::{ResolvedSandboxSpec, RuntimeHandle, RuntimeStatus};

//--------------------------------------------------------------------------------------------------
// Traits
//--------------------------------------------------------------------------------------------------

/// Abstracts over the VM lifecycle.
///
/// Each platform provides its own implementation:
/// - Unix: `KrunBackend` (via libkrun FFI)
/// - Windows: `WindowsHcsBackend` (via HCS/hcsshim)
#[async_trait]
pub trait VmBackend: Send + Sync {
    /// Starts a sandbox from a resolved specification.
    ///
    /// Returns a `RuntimeHandle` identifying the running sandbox.
    async fn start(&self, spec: &ResolvedSandboxSpec) -> MicrosandboxResult<RuntimeHandle>;

    /// Stops a running sandbox.
    async fn stop(&self, handle: &RuntimeHandle) -> MicrosandboxResult<()>;

    /// Queries the current status of a sandbox.
    async fn status(&self, handle: &RuntimeHandle) -> MicrosandboxResult<RuntimeStatus>;
}

/// Abstracts over rootfs preparation.
///
/// Converts an OCI image or path source into a backend-specific rootfs representation:
/// - Unix: extracted directories for overlayfs
/// - Windows: ext4 VHD files for SCSI attachment
#[async_trait]
pub trait RootfsMaterializer: Send + Sync {
    /// The platform-specific rootfs representation produced by materialization.
    type MaterializedRootfs: Send + Sync;

    /// Materializes a rootfs from the resolved spec's rootfs source.
    ///
    /// Downloads/extracts OCI layers if needed, applies patches, and returns
    /// the platform-specific rootfs representation.
    async fn materialize(
        &self,
        spec: &ResolvedSandboxSpec,
    ) -> MicrosandboxResult<Self::MaterializedRootfs>;
}

/// Abstracts over process supervision.
///
/// Manages the lifecycle of the runtime worker process:
/// - Unix: PTY + signals via `Supervisor`
/// - Windows: Job Objects + named pipes
#[async_trait]
pub trait ProcessSupervisor: Send + Sync {
    /// Spawns the runtime worker process.
    ///
    /// Returns the PID of the spawned process.
    async fn spawn(&mut self) -> MicrosandboxResult<u32>;

    /// Terminates the runtime worker process.
    async fn terminate(&self, pid: u32) -> MicrosandboxResult<()>;

    /// Checks if the runtime worker process is still running.
    fn is_running(&self, pid: u32) -> bool;
}
