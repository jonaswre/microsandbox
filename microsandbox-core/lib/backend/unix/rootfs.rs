//! UnixRootfsMaterializer — wraps existing rootfs setup behind the RootfsMaterializer trait.
//!
//! On Unix, materialization produces extracted layer directories for overlayfs.

use std::path::PathBuf;

use async_trait::async_trait;

use crate::{
    MicrosandboxResult,
    backend::{ResolvedSandboxSpec, RootfsMaterializer},
};

//--------------------------------------------------------------------------------------------------
// Types
//--------------------------------------------------------------------------------------------------

/// The materialized rootfs on Unix — extracted layer directories.
#[derive(Debug, Clone)]
pub struct UnixMaterializedRootfs {
    /// Layer directories (bottom to top) for overlayfs.
    pub layers: Vec<PathBuf>,

    /// Patch directory for sandbox customizations.
    pub patch_dir: PathBuf,

    /// Read-write directory for the overlayfs upper layer.
    pub rw_dir: PathBuf,
}

/// Unix rootfs materializer that wraps the existing `setup_image_rootfs()`
/// and `setup_native_rootfs()` functions.
///
/// This materializer produces extracted directories suitable for overlayfs assembly.
#[derive(Debug, Default)]
pub struct UnixRootfsMaterializer;

//--------------------------------------------------------------------------------------------------
// Methods
//--------------------------------------------------------------------------------------------------

impl UnixRootfsMaterializer {
    /// Creates a new `UnixRootfsMaterializer`.
    pub fn new() -> Self {
        Self
    }
}

//--------------------------------------------------------------------------------------------------
// Trait Implementations
//--------------------------------------------------------------------------------------------------

#[async_trait]
impl RootfsMaterializer for UnixRootfsMaterializer {
    type MaterializedRootfs = UnixMaterializedRootfs;

    async fn materialize(
        &self,
        spec: &ResolvedSandboxSpec,
    ) -> MicrosandboxResult<Self::MaterializedRootfs> {
        // The rootfs should already be resolved by the time it reaches the materializer.
        // This extracts the paths from the spec.
        match &spec.rootfs_source {
            crate::backend::RootfsSource::Overlayfs {
                layers,
                rw_dir,
                patch_dir,
            } => Ok(UnixMaterializedRootfs {
                layers: layers.clone(),
                patch_dir: patch_dir.clone(),
                rw_dir: rw_dir.clone(),
            }),
            crate::backend::RootfsSource::Native(path) => Ok(UnixMaterializedRootfs {
                layers: vec![path.clone()],
                patch_dir: PathBuf::new(),
                rw_dir: PathBuf::new(),
            }),
            crate::backend::RootfsSource::OciImage { reference } => {
                Err(crate::MicrosandboxError::NotImplemented(format!(
                    "OCI image materialization should be done before calling materialize(): {}",
                    reference
                )))
            }
        }
    }
}
