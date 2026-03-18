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
}
