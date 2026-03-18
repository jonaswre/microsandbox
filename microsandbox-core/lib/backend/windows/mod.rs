//! Windows backend implementations using HCS (Host Compute Service).
//!
//! Provides:
//! - `WindowsRootfsMaterializer` — OCI layer tarball → ext4 VHD conversion with caching
//! - `WindowsHcsBackend` — HCS compute system lifecycle (start/stop/status)
//! - `HcnNetworkManager` — NAT networking, endpoints, and port forwarding
//! - `WindowsSupervisor` — Job Objects, named pipes, and ConPTY
//! - `BootBundle` — Boot bundle management and version compatibility

pub mod boot_bundle;
mod hcs;
mod networking;
pub mod rootfs;
mod supervisor;

pub use boot_bundle::{BootBundle, BootBundleManifest};
pub use hcs::*;
pub use networking::*;
pub use rootfs::{
    LayerVhdManifest, WindowsMaterializedRootfs, WindowsRootfsConfig, WindowsRootfsMaterializer,
    cache_key_from_digest,
};
pub use supervisor::*;
