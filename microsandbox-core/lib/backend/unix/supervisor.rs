//! UnixSupervisor — wraps existing PTY + signal process supervision behind the ProcessSupervisor trait.

use async_trait::async_trait;

use crate::{MicrosandboxResult, backend::ProcessSupervisor};

//--------------------------------------------------------------------------------------------------
// Types
//--------------------------------------------------------------------------------------------------

/// Unix process supervisor using signals for lifecycle management.
///
/// Wraps the existing Unix process supervision patterns:
/// - Process liveness via `kill(pid, 0)`
/// - Termination via `SIGTERM`
#[derive(Debug, Default)]
pub struct UnixProcessSupervisor;

//--------------------------------------------------------------------------------------------------
// Methods
//--------------------------------------------------------------------------------------------------

impl UnixProcessSupervisor {
    /// Creates a new `UnixProcessSupervisor`.
    pub fn new() -> Self {
        Self
    }
}

//--------------------------------------------------------------------------------------------------
// Trait Implementations
//--------------------------------------------------------------------------------------------------

#[async_trait]
impl ProcessSupervisor for UnixProcessSupervisor {
    async fn spawn(&mut self) -> MicrosandboxResult<u32> {
        // The actual spawning is handled by KrunBackend::start(), which spawns
        // the msbrun supervisor process. This trait method is available for
        // cases where the process supervisor needs to be used independently.
        Err(crate::MicrosandboxError::NotImplemented(
            "UnixProcessSupervisor::spawn() — use KrunBackend::start() for full lifecycle"
                .to_string(),
        ))
    }

    async fn terminate(&self, pid: u32) -> MicrosandboxResult<()> {
        let pid = pid as i32;
        unsafe {
            if libc::kill(pid, libc::SIGTERM) != 0 {
                let err = std::io::Error::last_os_error();
                if err.raw_os_error() == Some(libc::ESRCH) {
                    tracing::warn!("process {} not found (already stopped)", pid);
                    return Ok(());
                }
                return Err(crate::MicrosandboxError::ProcessKillError(format!(
                    "failed to terminate process {}: {}",
                    pid, err
                )));
            }
        }
        Ok(())
    }

    fn is_running(&self, pid: u32) -> bool {
        unsafe { libc::kill(pid as i32, 0) == 0 }
    }
}
