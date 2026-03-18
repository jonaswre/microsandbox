//! Sandbox specification for the guest bootstrap.
//!
//! Matches `SandboxSpecForGuest` in `microsandbox-core/lib/backend/windows/rootfs.rs`.

use serde::{Deserialize, Serialize};

/// Sandbox specification passed to the guest via the patch VHD.
#[derive(Debug, Serialize, Deserialize)]
pub struct SandboxSpec {
    pub sandbox_id: String,
    pub mounts: Vec<GuestMountEntry>,
    pub workdir: Option<String>,
    pub exec_path: String,
    pub exec_args: Vec<String>,
    pub env: Vec<String>,
}

/// A mount entry as seen by the guest bootstrap.
#[derive(Debug, Serialize, Deserialize)]
pub struct GuestMountEntry {
    pub guest_path: String,
    pub readonly: bool,
    /// Plan9 share tag for mounting via 9p. None if the mount point is
    /// provided by other means (e.g., bind mount from SCSI VHD).
    #[serde(default)]
    pub tag: Option<String>,
}
