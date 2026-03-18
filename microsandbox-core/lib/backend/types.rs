//! Backend-neutral runtime types — handles, records, and enums for cross-platform runtime identity.

use std::fmt;

use serde::{Deserialize, Serialize};

//--------------------------------------------------------------------------------------------------
// Types
//--------------------------------------------------------------------------------------------------

/// Identifies which VM backend is managing a sandbox.
///
/// Stored in the database as a string and used to dispatch runtime operations
/// to the correct platform implementation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum BackendKind {
    /// libkrun-based backend on Unix (Linux/macOS).
    #[serde(rename = "unix_krun")]
    UnixKrun,

    /// HCS (Host Compute Service) backend on Windows.
    #[serde(rename = "windows_hcs")]
    WindowsHcs,
}

/// A backend-neutral handle to a running sandbox.
///
/// Replaces raw PID-based identity. Contains enough information to locate,
/// communicate with, and stop a running sandbox regardless of platform.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeHandle {
    /// The sandbox key (e.g. "project~sandbox_name").
    pub sandbox_key: String,

    /// Which backend is managing this sandbox.
    pub backend_kind: BackendKind,

    /// A unique runtime ID for this invocation.
    pub runtime_id: String,

    /// PID of the worker process (supervisor on Unix, msbrun-hcs.exe on Windows).
    pub worker_pid: u32,

    /// Control endpoint (empty on Unix; named pipe path on Windows).
    pub control_endpoint: String,

    /// Backend-specific object ID (empty on Unix; HCS compute system ID on Windows).
    pub backend_object_id: String,
}

/// A persistable record of runtime state, stored in the database.
///
/// Contains JSON-serialized blobs for rootfs descriptor and backend-specific state,
/// allowing the database schema to remain stable across backend changes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuntimeRecord {
    /// JSON describing the materialized rootfs (layer paths, VHD paths, etc.).
    pub rootfs_descriptor_json: String,

    /// JSON blob for backend-specific state (HCS config, Plan9 share IDs, etc.).
    pub backend_state_json: String,
}

/// The status of a sandbox runtime.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeStatus {
    /// The sandbox is running.
    Running,

    /// The sandbox has stopped.
    Stopped,

    /// The status cannot be determined.
    Unknown,
}

//--------------------------------------------------------------------------------------------------
// Trait Implementations
//--------------------------------------------------------------------------------------------------

impl fmt::Display for BackendKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BackendKind::UnixKrun => write!(f, "unix_krun"),
            BackendKind::WindowsHcs => write!(f, "windows_hcs"),
        }
    }
}

impl std::str::FromStr for BackendKind {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "unix_krun" => Ok(BackendKind::UnixKrun),
            "windows_hcs" => Ok(BackendKind::WindowsHcs),
            other => Err(format!("unknown backend kind: {}", other)),
        }
    }
}

impl fmt::Display for RuntimeStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RuntimeStatus::Running => write!(f, "running"),
            RuntimeStatus::Stopped => write!(f, "stopped"),
            RuntimeStatus::Unknown => write!(f, "unknown"),
        }
    }
}

impl Default for RuntimeRecord {
    fn default() -> Self {
        Self {
            rootfs_descriptor_json: "{}".to_string(),
            backend_state_json: "{}".to_string(),
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
    fn test_backend_kind_serialization() {
        assert_eq!(
            serde_json::to_string(&BackendKind::UnixKrun).unwrap(),
            "\"unix_krun\""
        );
        assert_eq!(
            serde_json::to_string(&BackendKind::WindowsHcs).unwrap(),
            "\"windows_hcs\""
        );
    }

    #[test]
    fn test_backend_kind_deserialization() {
        let unix: BackendKind = serde_json::from_str("\"unix_krun\"").unwrap();
        assert_eq!(unix, BackendKind::UnixKrun);
        let win: BackendKind = serde_json::from_str("\"windows_hcs\"").unwrap();
        assert_eq!(win, BackendKind::WindowsHcs);
    }

    #[test]
    fn test_backend_kind_display() {
        assert_eq!(BackendKind::UnixKrun.to_string(), "unix_krun");
        assert_eq!(BackendKind::WindowsHcs.to_string(), "windows_hcs");
    }

    #[test]
    fn test_backend_kind_from_str() {
        assert_eq!(
            "unix_krun".parse::<BackendKind>().unwrap(),
            BackendKind::UnixKrun
        );
        assert_eq!(
            "windows_hcs".parse::<BackendKind>().unwrap(),
            BackendKind::WindowsHcs
        );
        assert!("invalid".parse::<BackendKind>().is_err());
    }

    #[test]
    fn test_runtime_record_default() {
        let record = RuntimeRecord::default();
        assert_eq!(record.rootfs_descriptor_json, "{}");
        assert_eq!(record.backend_state_json, "{}");
    }
}
