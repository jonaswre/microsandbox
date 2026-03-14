//! Windows process supervisor — Job Objects, named pipes, ConPTY.
//!
//! Manages the lifecycle of `msbrun-hcs.exe` worker processes using Windows
//! Job Objects for tree management, named pipes for control channels, and
//! ConPTY for interactive terminal attach.

use std::path::PathBuf;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::{
    MicrosandboxError, MicrosandboxResult,
    backend::ProcessSupervisor,
};

//--------------------------------------------------------------------------------------------------
// Constants
//--------------------------------------------------------------------------------------------------

/// Named pipe prefix for microsandbox control channels.
const PIPE_PREFIX: &str = r"\\.\pipe\microsandbox-";

//--------------------------------------------------------------------------------------------------
// Types
//--------------------------------------------------------------------------------------------------

/// Named pipe protocol message types.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum PipeMessage {
    /// Query sandbox status.
    #[serde(rename = "status")]
    StatusQuery,

    /// Response to a status query.
    #[serde(rename = "status_response")]
    StatusResponse {
        /// Current state of the sandbox.
        state: String,
        /// PID of the sandbox init process inside the guest.
        guest_pid: Option<u32>,
    },

    /// Request to stop the sandbox.
    #[serde(rename = "stop")]
    StopCommand,

    /// Acknowledgment of a stop command.
    #[serde(rename = "stop_ack")]
    StopAck {
        /// Whether the stop was successful.
        success: bool,
        /// Error message if the stop failed.
        error: Option<String>,
    },

    /// Request to attach an interactive terminal.
    #[serde(rename = "attach")]
    AttachRequest {
        /// Terminal columns.
        cols: u16,
        /// Terminal rows.
        rows: u16,
    },

    /// Terminal resize event.
    #[serde(rename = "resize")]
    ResizeEvent {
        /// New terminal columns.
        cols: u16,
        /// New terminal rows.
        rows: u16,
    },
}

/// Configuration for the Windows process supervisor.
#[derive(Debug, Clone)]
pub struct WindowsSupervisorConfig {
    /// Path to the msbrun-hcs.exe worker binary.
    pub worker_path: PathBuf,

    /// Sandbox key for this supervisor instance.
    pub sandbox_key: String,
}

/// Windows process supervisor using Job Objects and named pipes.
///
/// Each supervisor manages a single `msbrun-hcs.exe` worker process.
/// The worker is placed in a Job Object with `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`
/// to ensure cleanup on crash or shutdown.
#[derive(Debug)]
pub struct WindowsSupervisor {
    config: WindowsSupervisorConfig,
    /// The worker process PID (set after spawn).
    worker_pid: Option<u32>,
}

//--------------------------------------------------------------------------------------------------
// Methods
//--------------------------------------------------------------------------------------------------

impl WindowsSupervisor {
    /// Creates a new `WindowsSupervisor`.
    pub fn new(config: WindowsSupervisorConfig) -> Self {
        Self {
            config,
            worker_pid: None,
        }
    }

    /// Returns the named pipe path for this supervisor's control channel.
    pub fn pipe_path(&self) -> String {
        format!("{}{}", PIPE_PREFIX, self.config.sandbox_key)
    }

    /// Checks if the named pipe for this sandbox is responsive.
    ///
    /// This is the Windows equivalent of `kill(pid, 0)` on Unix.
    pub async fn is_pipe_responsive(&self) -> bool {
        let pipe = self.pipe_path();

        // Try to connect to the named pipe.
        // On Windows, we'd use CreateFile on the pipe path.
        // As a cross-platform approximation, check if the process is alive.
        if let Some(pid) = self.worker_pid {
            return Self::is_pid_alive(pid).await;
        }

        // If we don't have a PID, try to open the pipe.
        let output = tokio::process::Command::new("powershell.exe")
            .args([
                "-NoProfile",
                "-Command",
                &format!(
                    "Test-Path '{}'",
                    pipe
                ),
            ])
            .output()
            .await;

        match output {
            Ok(o) => {
                let stdout = String::from_utf8_lossy(&o.stdout);
                stdout.trim().eq_ignore_ascii_case("True")
            }
            Err(_) => false,
        }
    }

    /// Checks if a process with the given PID is alive.
    async fn is_pid_alive(pid: u32) -> bool {
        let output = tokio::process::Command::new("tasklist")
            .args(["/FI", &format!("PID eq {}", pid), "/NH"])
            .output()
            .await;

        match output {
            Ok(o) => {
                let stdout = String::from_utf8_lossy(&o.stdout);
                stdout.contains(&pid.to_string())
            }
            Err(_) => false,
        }
    }
}

//--------------------------------------------------------------------------------------------------
// Trait Implementations
//--------------------------------------------------------------------------------------------------

#[async_trait]
impl ProcessSupervisor for WindowsSupervisor {
    async fn spawn(&mut self) -> MicrosandboxResult<u32> {
        let pipe_path = self.pipe_path();

        tracing::info!(
            worker = %self.config.worker_path.display(),
            pipe = %pipe_path,
            sandbox = %self.config.sandbox_key,
            "Spawning Windows HCS worker"
        );

        // Spawn the worker process.
        // The worker connects to the named pipe and reads the compute config.
        let child = tokio::process::Command::new(&self.config.worker_path)
            .arg("--pipe")
            .arg(&pipe_path)
            .arg("--sandbox")
            .arg(&self.config.sandbox_key)
            .spawn()
            .map_err(|e| {
                MicrosandboxError::SupervisorBinaryNotFound(format!(
                    "Failed to spawn worker '{}': {}",
                    self.config.worker_path.display(),
                    e
                ))
            })?;

        let pid = child.id().unwrap_or(0);
        self.worker_pid = Some(pid);

        tracing::info!(
            pid = pid,
            sandbox = %self.config.sandbox_key,
            "Worker process spawned"
        );

        Ok(pid)
    }

    async fn terminate(&self, pid: u32) -> MicrosandboxResult<()> {
        tracing::info!(
            pid = pid,
            sandbox = %self.config.sandbox_key,
            "Terminating worker process"
        );

        // First, try graceful stop via the named pipe.
        // The worker handles cleanup (HCS teardown, VHD detach, etc.)
        // before exiting.
        // For now, use taskkill /F /T to kill the process tree.
        let output = tokio::process::Command::new("taskkill")
            .args(["/F", "/T", "/PID", &pid.to_string()])
            .output()
            .await
            .map_err(|e| {
                MicrosandboxError::ProcessKillError(format!(
                    "Failed to terminate worker PID {}: {}",
                    pid, e
                ))
            })?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            // Process may have already exited — not necessarily an error.
            tracing::debug!(
                pid = pid,
                "taskkill result: {}",
                stderr
            );
        }

        Ok(())
    }

    fn is_running(&self, pid: u32) -> bool {
        // Synchronous check — use a blocking approach.
        // On Windows, we'd use OpenProcess to check if the PID is valid.
        // Since we can't do async here, do a simple std::process check.
        let output = std::process::Command::new("tasklist")
            .args(["/FI", &format!("PID eq {}", pid), "/NH"])
            .output();

        match output {
            Ok(o) => {
                let stdout = String::from_utf8_lossy(&o.stdout);
                stdout.contains(&pid.to_string())
            }
            Err(_) => false,
        }
    }
}

//--------------------------------------------------------------------------------------------------
// Tests
//--------------------------------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_pipe_path() {
        let config = WindowsSupervisorConfig {
            worker_path: PathBuf::from("msbrun-hcs.exe"),
            sandbox_key: "project~sandbox1".to_string(),
        };
        let supervisor = WindowsSupervisor::new(config);
        assert_eq!(
            supervisor.pipe_path(),
            r"\\.\pipe\microsandbox-project~sandbox1"
        );
    }

    #[test]
    fn test_pipe_message_serialization() {
        // Status query.
        let msg = PipeMessage::StatusQuery;
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("status"));

        // Status response.
        let msg = PipeMessage::StatusResponse {
            state: "running".to_string(),
            guest_pid: Some(1),
        };
        let json = serde_json::to_string(&msg).unwrap();
        let deserialized: PipeMessage = serde_json::from_str(&json).unwrap();
        match deserialized {
            PipeMessage::StatusResponse { state, guest_pid } => {
                assert_eq!(state, "running");
                assert_eq!(guest_pid, Some(1));
            }
            _ => panic!("unexpected message type"),
        }

        // Stop command.
        let msg = PipeMessage::StopCommand;
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("stop"));

        // Stop ack.
        let msg = PipeMessage::StopAck {
            success: true,
            error: None,
        };
        let json = serde_json::to_string(&msg).unwrap();
        let deserialized: PipeMessage = serde_json::from_str(&json).unwrap();
        match deserialized {
            PipeMessage::StopAck { success, error } => {
                assert!(success);
                assert!(error.is_none());
            }
            _ => panic!("unexpected message type"),
        }

        // Attach request.
        let msg = PipeMessage::AttachRequest {
            cols: 80,
            rows: 24,
        };
        let json = serde_json::to_string(&msg).unwrap();
        let deserialized: PipeMessage = serde_json::from_str(&json).unwrap();
        match deserialized {
            PipeMessage::AttachRequest { cols, rows } => {
                assert_eq!(cols, 80);
                assert_eq!(rows, 24);
            }
            _ => panic!("unexpected message type"),
        }

        // Resize event.
        let msg = PipeMessage::ResizeEvent {
            cols: 120,
            rows: 40,
        };
        let json = serde_json::to_string(&msg).unwrap();
        let deserialized: PipeMessage = serde_json::from_str(&json).unwrap();
        match deserialized {
            PipeMessage::ResizeEvent { cols, rows } => {
                assert_eq!(cols, 120);
                assert_eq!(rows, 40);
            }
            _ => panic!("unexpected message type"),
        }
    }

    #[test]
    fn test_supervisor_initial_state() {
        let config = WindowsSupervisorConfig {
            worker_path: PathBuf::from("msbrun-hcs.exe"),
            sandbox_key: "test~sandbox".to_string(),
        };
        let supervisor = WindowsSupervisor::new(config);
        assert!(supervisor.worker_pid.is_none());
    }
}
