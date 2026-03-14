//! KrunBackend — Unix VM backend using libkrun via msbrun subprocess.
//!
//! Wraps the existing sandbox lifecycle (spawning msbrun supervisor) behind the
//! `VmBackend` trait. The msbrun process handles the actual libkrun FFI calls.

use std::{path::PathBuf, process::Stdio};

use async_trait::async_trait;
use tokio::process::Command;

use crate::{
    MicrosandboxResult,
    backend::{
        BackendKind, ResolvedSandboxSpec, RootfsSource, RuntimeHandle, RuntimeStatus, VmBackend,
    },
};
use microsandbox_utils::{DEFAULT_MSBRUN_EXE_PATH, MSBRUN_EXE_ENV_VAR};

//--------------------------------------------------------------------------------------------------
// Types
//--------------------------------------------------------------------------------------------------

/// Unix VM backend that spawns msbrun supervisor processes.
///
/// This backend wraps the existing Unix sandbox lifecycle:
/// 1. Resolves rootfs and config into msbrun CLI arguments
/// 2. Spawns `msbrun supervisor` as a detached process
/// 3. The supervisor spawns `msbrun microvm` which calls libkrun FFI
#[derive(Debug, Default)]
pub struct KrunBackend {
    /// Path to the msbrun binary (resolved lazily if not set).
    msbrun_path: Option<PathBuf>,
}

//--------------------------------------------------------------------------------------------------
// Methods
//--------------------------------------------------------------------------------------------------

impl KrunBackend {
    /// Creates a new `KrunBackend`.
    pub fn new() -> Self {
        Self::default()
    }

    /// Creates a new `KrunBackend` with a specific msbrun path.
    pub fn with_msbrun_path(msbrun_path: PathBuf) -> Self {
        Self {
            msbrun_path: Some(msbrun_path),
        }
    }

    /// Resolves the msbrun binary path.
    fn resolve_msbrun_path(&self) -> MicrosandboxResult<PathBuf> {
        if let Some(ref path) = self.msbrun_path {
            return Ok(path.clone());
        }
        microsandbox_utils::path::resolve_env_path(MSBRUN_EXE_ENV_VAR, &*DEFAULT_MSBRUN_EXE_PATH)
            .map_err(Into::into)
    }

    /// Builds the msbrun supervisor command from a resolved spec.
    fn build_command(&self, spec: &ResolvedSandboxSpec) -> MicrosandboxResult<Command> {
        let msbrun_path = self.resolve_msbrun_path()?;
        let mut command = Command::new(msbrun_path);

        command
            .arg("supervisor")
            .arg("--sandbox-name")
            .arg(&spec.sandbox_key)
            .arg("--scope")
            .arg(spec.scope.to_string())
            .arg("--exec-path")
            .arg(spec.exec.path.as_str());

        // Resources
        command
            .arg("--num-vcpus")
            .arg(spec.resources.vcpus.to_string());
        command
            .arg("--memory-mib")
            .arg(spec.resources.memory_mib.to_string());

        // Workdir
        if let Some(ref workdir) = spec.workdir {
            command.arg("--workdir-path").arg(workdir.as_str());
        }

        // Environment
        for env in &spec.env {
            command.arg("--env").arg(env.to_string());
        }

        // Ports
        for port in &spec.ports {
            command.arg("--port-map").arg(port.to_string());
        }

        // Mounts
        for mount in &spec.mounts {
            command.arg("--mapped-dir").arg(mount.to_string());
        }

        // Rootfs
        match &spec.rootfs_source {
            RootfsSource::Native(path) => {
                command.arg("--native-rootfs").arg(path);
            }
            RootfsSource::Overlayfs { layers, .. } => {
                for layer in layers {
                    command.arg("--overlayfs-layer").arg(layer);
                }
            }
            RootfsSource::OciImage { reference } => {
                // Image should be materialized before reaching this point
                tracing::warn!(
                    "OCI image {} passed to KrunBackend without materialization",
                    reference
                );
            }
        }

        // Extra args
        if !spec.exec.args.is_empty() {
            command.arg("--");
            for arg in &spec.exec.args {
                command.arg(arg);
            }
        }

        // Pass RUST_LOG if set
        if let Some(rust_log) = std::env::var_os("RUST_LOG") {
            command.env("RUST_LOG", rust_log);
        }

        Ok(command)
    }
}

//--------------------------------------------------------------------------------------------------
// Trait Implementations
//--------------------------------------------------------------------------------------------------

#[async_trait]
impl VmBackend for KrunBackend {
    async fn start(&self, spec: &ResolvedSandboxSpec) -> MicrosandboxResult<RuntimeHandle> {
        let mut command = self.build_command(spec)?;

        // Detach the process
        unsafe {
            command.pre_exec(|| {
                libc::setsid();
                Ok(())
            });
        }
        command.stdout(Stdio::null());
        command.stderr(Stdio::null());
        command.stdin(Stdio::null());

        let child = command.spawn().map_err(|e| {
            crate::MicrosandboxError::SupervisorError(format!(
                "failed to spawn msbrun supervisor: {}",
                e
            ))
        })?;

        let pid = child.id().unwrap_or(0);
        tracing::info!("started msbrun supervisor with PID: {}", pid);

        Ok(RuntimeHandle {
            sandbox_key: spec.sandbox_key.clone(),
            backend_kind: BackendKind::UnixKrun,
            runtime_id: format!("krun-{}", pid),
            worker_pid: pid,
            control_endpoint: String::new(),
            backend_object_id: String::new(),
        })
    }

    async fn stop(&self, handle: &RuntimeHandle) -> MicrosandboxResult<()> {
        let pid = handle.worker_pid as i32;
        unsafe {
            if libc::kill(pid, libc::SIGTERM) != 0 {
                let err = std::io::Error::last_os_error();
                if err.raw_os_error() == Some(libc::ESRCH) {
                    tracing::warn!("process {} not found (already stopped)", pid);
                    return Ok(());
                }
                return Err(crate::MicrosandboxError::ProcessKillError(format!(
                    "failed to kill process {}: {}",
                    pid, err
                )));
            }
        }
        Ok(())
    }

    async fn status(&self, handle: &RuntimeHandle) -> MicrosandboxResult<RuntimeStatus> {
        let pid = handle.worker_pid as i32;
        let running = unsafe { libc::kill(pid, 0) == 0 };
        Ok(if running {
            RuntimeStatus::Running
        } else {
            RuntimeStatus::Stopped
        })
    }
}
