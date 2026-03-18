//! Resolved sandbox specification — a backend-neutral description of a sandbox to run.

use std::path::PathBuf;

use typed_path::Utf8UnixPathBuf;

use crate::config::{EnvPair, GuestPathBuf, MountSpec, NetworkScope, PortPair};

//--------------------------------------------------------------------------------------------------
// Types
//--------------------------------------------------------------------------------------------------

/// A fully-resolved sandbox specification, produced from config resolution.
///
/// This struct captures everything needed to start a sandbox in a backend-neutral format.
/// It replaces the previous approach of constructing CLI flags directly in `prepare_run()`.
///
/// Backend implementations (`KrunBackend`, `WindowsHcsBackend`) consume this to create
/// platform-specific VM configurations.
#[derive(Debug, Clone)]
pub struct ResolvedSandboxSpec {
    /// The sandbox key (unique identifier, e.g. "project~sandbox_name").
    pub sandbox_key: String,

    /// The source of the root filesystem.
    pub rootfs_source: RootfsSource,

    /// Host-to-guest directory mounts.
    pub mounts: Vec<MountSpec>,

    /// Port mappings between host and guest.
    pub ports: Vec<PortPair>,

    /// Environment variables to set inside the guest.
    pub env: Vec<EnvPair>,

    /// Working directory inside the guest VM.
    pub workdir: Option<GuestPathBuf>,

    /// The executable to run inside the guest.
    pub exec: ExecSpec,

    /// Resource limits.
    pub resources: ResourceLimits,

    /// Network scope.
    pub scope: NetworkScope,

    /// Portal configuration.
    pub portal: Option<PortalConfig>,
}

/// The source of a sandbox's root filesystem.
#[derive(Debug, Clone)]
pub enum RootfsSource {
    /// A native rootfs path on the host.
    Native(PathBuf),

    /// Overlayfs layers (ordered bottom to top, plus RW and patch paths).
    Overlayfs {
        /// Layer directories (bottom to top).
        layers: Vec<PathBuf>,
        /// Read-write directory for the upper layer.
        rw_dir: PathBuf,
        /// Patch directory for sandbox customizations.
        patch_dir: PathBuf,
    },

    /// An OCI image reference (not yet materialized).
    OciImage {
        /// The image reference string.
        reference: String,
    },
}

/// What to execute inside the sandbox.
#[derive(Debug, Clone)]
pub struct ExecSpec {
    /// The path to the executable (inside the guest).
    pub path: Utf8UnixPathBuf,

    /// Arguments to pass to the executable.
    pub args: Vec<String>,
}

/// Resource limits for the sandbox.
#[derive(Debug, Clone)]
pub struct ResourceLimits {
    /// Number of virtual CPUs.
    pub vcpus: u8,

    /// Memory in MiB.
    pub memory_mib: u32,

    /// POSIX rlimits (as strings, e.g. "RLIMIT_NOFILE=1024:1024").
    pub rlimits: Vec<String>,
}

/// Portal configuration for the sandbox.
#[derive(Debug, Clone)]
pub struct PortalConfig {
    /// The guest port the portal listens on.
    pub guest_port: u16,
}

//--------------------------------------------------------------------------------------------------
// Methods
//--------------------------------------------------------------------------------------------------

impl Default for ResourceLimits {
    fn default() -> Self {
        Self {
            vcpus: 1,
            memory_mib: 1024,
            rlimits: Vec::new(),
        }
    }
}
