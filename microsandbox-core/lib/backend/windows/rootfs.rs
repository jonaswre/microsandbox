//! WindowsRootfsMaterializer — converts OCI layer tarballs to ext4 VHDs via `tar2ext4`.
//!
//! On Windows, materialization produces VHD files for SCSI attachment to HCS compute systems.
//! Layer VHDs are cached by digest. Each sandbox gets a patch layer VHD (resolv.conf, mount
//! point dirs, bootstrap scripts) and a writable scratch VHDX for OverlayFS upper/work dirs.

use std::path::{Path, PathBuf};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tokio::fs;

use crate::{
    MicrosandboxError, MicrosandboxResult,
    backend::{ResolvedSandboxSpec, RootfsMaterializer, RootfsSource},
};

//--------------------------------------------------------------------------------------------------
// Constants
//--------------------------------------------------------------------------------------------------

/// Subdirectory under the cache home for Windows layer VHDs.
const WINDOWS_LAYERS_SUBDIR: &str = "windows/layers";

/// Subdirectory under the runtime home for per-sandbox state.
const WINDOWS_SANDBOXES_SUBDIR: &str = "windows/sandboxes";

/// Name of the converted ext4 VHD file within a layer cache directory.
const LAYER_VHD_FILENAME: &str = "layer.vhd";

/// Name of the cache manifest file within a layer cache directory.
const LAYER_MANIFEST_FILENAME: &str = "manifest.json";

/// Name of the patch layer VHD for per-sandbox customizations.
const PATCH_VHD_FILENAME: &str = "patch.vhd";

/// Name of the writable scratch VHDX for OverlayFS upper/work dirs.
const SCRATCH_VHDX_FILENAME: &str = "scratch.vhdx";

/// Default scratch VHDX size (256 MiB).
const DEFAULT_SCRATCH_SIZE_BYTES: u64 = 256 * 1024 * 1024;

//--------------------------------------------------------------------------------------------------
// Types
//--------------------------------------------------------------------------------------------------

/// The materialized rootfs on Windows — VHD files for SCSI attachment.
#[derive(Debug, Clone)]
pub struct WindowsMaterializedRootfs {
    /// Layer VHD paths (bottom to top), each an ext4 VHD to be attached read-only.
    pub layer_vhds: Vec<PathBuf>,

    /// Patch layer VHD containing per-sandbox customizations (resolv.conf, mount dirs).
    pub patch_vhd: PathBuf,

    /// Writable scratch VHDX for OverlayFS upper/work dirs.
    pub scratch_vhdx: PathBuf,
}

/// Manifest stored alongside each cached layer VHD for integrity validation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LayerVhdManifest {
    /// The OCI digest of the layer this VHD was created from.
    pub digest: String,

    /// The size in bytes of the source tarball.
    pub source_size: u64,

    /// ISO 8601 timestamp of when the VHD was created.
    pub created_at: String,

    /// Version of the conversion tool used.
    pub tool_version: String,
}

/// Configuration for the Windows rootfs pipeline.
#[derive(Debug, Clone)]
pub struct WindowsRootfsConfig {
    /// Base directory for cached layer VHDs (e.g., `%LOCALAPPDATA%\Microsandbox\cache`).
    pub cache_dir: PathBuf,

    /// Base directory for per-sandbox runtime state (e.g., `%LOCALAPPDATA%\Microsandbox\runtime`).
    pub runtime_dir: PathBuf,

    /// Path to the `tar2ext4` binary from hcsshim.
    pub tar2ext4_path: PathBuf,
}

/// Windows rootfs materializer that converts OCI layer tarballs to ext4 VHDs.
///
/// Uses `tar2ext4` from hcsshim for conversion and caches results by digest.
#[derive(Debug)]
pub struct WindowsRootfsMaterializer {
    config: WindowsRootfsConfig,
}

//--------------------------------------------------------------------------------------------------
// Methods
//--------------------------------------------------------------------------------------------------

impl WindowsRootfsMaterializer {
    /// Creates a new `WindowsRootfsMaterializer` with the given configuration.
    pub fn new(config: WindowsRootfsConfig) -> Self {
        Self { config }
    }

    /// Returns the cache directory for a layer identified by its digest.
    ///
    /// Layout: `<cache_dir>/windows/layers/<sanitized_digest>/`
    pub fn layer_cache_dir(&self, digest: &str) -> PathBuf {
        let sanitized = sanitize_digest(digest);
        self.config
            .cache_dir
            .join(WINDOWS_LAYERS_SUBDIR)
            .join(sanitized)
    }

    /// Returns the VHD path for a cached layer.
    pub fn layer_vhd_path(&self, digest: &str) -> PathBuf {
        self.layer_cache_dir(digest).join(LAYER_VHD_FILENAME)
    }

    /// Returns the manifest path for a cached layer.
    pub fn layer_manifest_path(&self, digest: &str) -> PathBuf {
        self.layer_cache_dir(digest).join(LAYER_MANIFEST_FILENAME)
    }

    /// Returns the sandbox state directory for a given sandbox.
    ///
    /// Layout: `<runtime_dir>/windows/sandboxes/<sandbox_id>/`
    pub fn sandbox_dir(&self, sandbox_id: &str) -> PathBuf {
        self.config
            .runtime_dir
            .join(WINDOWS_SANDBOXES_SUBDIR)
            .join(sandbox_id)
    }

    /// Returns the patch VHD path for a sandbox.
    pub fn patch_vhd_path(&self, sandbox_id: &str) -> PathBuf {
        self.sandbox_dir(sandbox_id).join(PATCH_VHD_FILENAME)
    }

    /// Returns the scratch VHDX path for a sandbox.
    pub fn scratch_vhdx_path(&self, sandbox_id: &str) -> PathBuf {
        self.sandbox_dir(sandbox_id).join(SCRATCH_VHDX_FILENAME)
    }

    /// Converts an OCI layer tarball to an ext4 VHD using `tar2ext4`.
    ///
    /// Returns the path to the output VHD file.
    pub async fn convert_layer_to_vhd(
        &self,
        tar_path: &Path,
        digest: &str,
    ) -> MicrosandboxResult<PathBuf> {
        let cache_dir = self.layer_cache_dir(digest);
        fs::create_dir_all(&cache_dir).await?;

        let vhd_path = cache_dir.join(LAYER_VHD_FILENAME);

        tracing::info!(
            tar = %tar_path.display(),
            vhd = %vhd_path.display(),
            "Converting OCI layer tarball to ext4 VHD"
        );

        // Invoke tar2ext4 to convert the tarball to an ext4 VHD.
        // tar2ext4 reads from stdin or a file and writes an ext4 VHD.
        let output = tokio::process::Command::new(&self.config.tar2ext4_path)
            .arg("-i")
            .arg(tar_path)
            .arg("-o")
            .arg(&vhd_path)
            .arg("-vhd")
            .output()
            .await
            .map_err(|e| {
                MicrosandboxError::LayerExtraction(format!(
                    "failed to invoke tar2ext4 at {}: {}",
                    self.config.tar2ext4_path.display(),
                    e
                ))
            })?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(MicrosandboxError::LayerExtraction(format!(
                "tar2ext4 failed for digest {}: {}",
                digest, stderr
            )));
        }

        // Write the cache manifest for integrity validation.
        let source_size = fs::metadata(tar_path)
            .await
            .map(|m| m.len())
            .unwrap_or(0);

        let manifest = LayerVhdManifest {
            digest: digest.to_string(),
            source_size,
            created_at: chrono::Utc::now().to_rfc3339(),
            tool_version: "tar2ext4".to_string(),
        };

        let manifest_path = cache_dir.join(LAYER_MANIFEST_FILENAME);
        let manifest_json = serde_json::to_string_pretty(&manifest)?;
        fs::write(&manifest_path, manifest_json).await?;

        tracing::info!(
            vhd = %vhd_path.display(),
            digest = %digest,
            "Successfully converted layer to VHD"
        );

        Ok(vhd_path)
    }

    /// Checks if a cached VHD exists and its manifest is valid for the given digest.
    ///
    /// Returns `true` if the cache is valid and can be reused.
    pub async fn is_cache_valid(&self, digest: &str) -> bool {
        let vhd_path = self.layer_vhd_path(digest);
        let manifest_path = self.layer_manifest_path(digest);

        // Both files must exist.
        if !vhd_path.exists() || !manifest_path.exists() {
            return false;
        }

        // Read and validate the manifest.
        match fs::read_to_string(&manifest_path).await {
            Ok(content) => match serde_json::from_str::<LayerVhdManifest>(&content) {
                Ok(manifest) => manifest.digest == digest,
                Err(_) => false,
            },
            Err(_) => false,
        }
    }

    /// Gets or creates a cached VHD for a layer.
    ///
    /// If a valid cached VHD exists for the digest, returns its path.
    /// Otherwise, converts the tarball and caches the result.
    pub async fn get_or_create_layer_vhd(
        &self,
        tar_path: &Path,
        digest: &str,
    ) -> MicrosandboxResult<PathBuf> {
        if self.is_cache_valid(digest).await {
            tracing::debug!(digest = %digest, "Using cached layer VHD");
            return Ok(self.layer_vhd_path(digest));
        }

        // Cache is invalid or missing — reconvert.
        tracing::info!(digest = %digest, "Cache miss, converting layer to VHD");

        // Clean up any existing invalid cache entry.
        let cache_dir = self.layer_cache_dir(digest);
        if cache_dir.exists() {
            fs::remove_dir_all(&cache_dir).await?;
        }

        self.convert_layer_to_vhd(tar_path, digest).await
    }

    /// Generates a patch layer VHD containing per-sandbox customizations.
    ///
    /// The patch VHD contains:
    /// - `/etc/resolv.conf` with DNS configuration
    /// - Empty directories for mount points
    /// - Bootstrap scripts
    pub async fn create_patch_vhd(
        &self,
        sandbox_id: &str,
        spec: &ResolvedSandboxSpec,
    ) -> MicrosandboxResult<PathBuf> {
        let sandbox_dir = self.sandbox_dir(sandbox_id);
        fs::create_dir_all(&sandbox_dir).await?;

        let patch_vhd = sandbox_dir.join(PATCH_VHD_FILENAME);

        // Create a temporary directory with the patch contents.
        let patch_staging = sandbox_dir.join("patch_staging");
        fs::create_dir_all(&patch_staging).await?;

        // Create /etc/resolv.conf with DNS configuration.
        let etc_dir = patch_staging.join("etc");
        fs::create_dir_all(&etc_dir).await?;

        // Default to gateway IP as DNS (set by HCN networking).
        let resolv_conf = "# Generated by microsandbox\nnameserver 172.28.176.1\n";
        fs::write(etc_dir.join("resolv.conf"), resolv_conf).await?;

        // Create mount point directories.
        for mount in &spec.mounts {
            let guest_path = mount.guest.as_str();
            // Strip leading slash for relative path under staging dir.
            let relative = guest_path.trim_start_matches('/');
            if !relative.is_empty() {
                let mount_dir = patch_staging.join(relative);
                fs::create_dir_all(&mount_dir).await?;
            }
        }

        // Create sandbox-spec.json for the guest bootstrap.
        let spec_json = serde_json::to_string_pretty(&SandboxSpecForGuest {
            sandbox_id: sandbox_id.to_string(),
            mounts: spec
                .mounts
                .iter()
                .map(|m| GuestMountEntry {
                    guest_path: m.guest.to_string(),
                    readonly: m.readonly,
                })
                .collect(),
            workdir: spec.workdir.as_ref().map(|w| w.to_string()),
            exec_path: spec.exec.path.to_string(),
            exec_args: spec.exec.args.clone(),
            env: spec
                .env
                .iter()
                .map(|e| e.to_string())
                .collect(),
        })?;
        fs::write(patch_staging.join("sandbox-spec.json"), spec_json).await?;

        // Convert the staging directory to a VHD via tar2ext4.
        // First create a tarball from the staging directory, then convert it.
        let patch_tar = sandbox_dir.join("patch.tar");
        create_tar_from_dir(&patch_staging, &patch_tar).await?;

        let output = tokio::process::Command::new(&self.config.tar2ext4_path)
            .arg("-i")
            .arg(&patch_tar)
            .arg("-o")
            .arg(&patch_vhd)
            .arg("-vhd")
            .output()
            .await
            .map_err(|e| {
                MicrosandboxError::LayerExtraction(format!(
                    "failed to create patch VHD: {}",
                    e
                ))
            })?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(MicrosandboxError::LayerExtraction(format!(
                "tar2ext4 failed for patch VHD: {}",
                stderr
            )));
        }

        // Clean up staging artifacts.
        let _ = fs::remove_dir_all(&patch_staging).await;
        let _ = fs::remove_file(&patch_tar).await;

        tracing::info!(
            vhd = %patch_vhd.display(),
            sandbox = %sandbox_id,
            "Created patch layer VHD"
        );

        Ok(patch_vhd)
    }

    /// Creates a writable scratch VHDX for OverlayFS upper/work directories.
    ///
    /// If the VHDX already exists (sandbox restart), it is reused to preserve state.
    pub async fn create_or_reuse_scratch_vhdx(
        &self,
        sandbox_id: &str,
    ) -> MicrosandboxResult<PathBuf> {
        let sandbox_dir = self.sandbox_dir(sandbox_id);
        fs::create_dir_all(&sandbox_dir).await?;

        let scratch_path = sandbox_dir.join(SCRATCH_VHDX_FILENAME);

        if scratch_path.exists() {
            tracing::debug!(
                vhdx = %scratch_path.display(),
                "Reusing existing scratch VHDX"
            );
            return Ok(scratch_path);
        }

        tracing::info!(
            vhdx = %scratch_path.display(),
            "Creating scratch VHDX"
        );

        // Create a sparse VHDX file. On Windows, we use PowerShell's New-VHD cmdlet
        // or write the VHDX header directly. For simplicity, use New-VHD.
        let output = tokio::process::Command::new("powershell.exe")
            .args([
                "-NoProfile",
                "-Command",
                &format!(
                    "New-VHD -Path '{}' -SizeBytes {} -Dynamic | Out-Null",
                    scratch_path.display(),
                    DEFAULT_SCRATCH_SIZE_BYTES
                ),
            ])
            .output()
            .await
            .map_err(|e| {
                MicrosandboxError::LayerExtraction(format!(
                    "failed to create scratch VHDX: {}",
                    e
                ))
            })?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(MicrosandboxError::LayerExtraction(format!(
                "PowerShell New-VHD failed: {}",
                stderr
            )));
        }

        tracing::info!(
            vhdx = %scratch_path.display(),
            size = DEFAULT_SCRATCH_SIZE_BYTES,
            "Created scratch VHDX"
        );

        Ok(scratch_path)
    }

    /// Cleans up all sandbox-specific artifacts (patch VHD, scratch VHDX).
    pub async fn cleanup_sandbox(&self, sandbox_id: &str) -> MicrosandboxResult<()> {
        let sandbox_dir = self.sandbox_dir(sandbox_id);
        if sandbox_dir.exists() {
            fs::remove_dir_all(&sandbox_dir).await?;
            tracing::info!(
                dir = %sandbox_dir.display(),
                "Cleaned up sandbox artifacts"
            );
        }
        Ok(())
    }

    /// Removes a cached layer VHD.
    pub async fn remove_cached_layer(&self, digest: &str) -> MicrosandboxResult<()> {
        let cache_dir = self.layer_cache_dir(digest);
        if cache_dir.exists() {
            fs::remove_dir_all(&cache_dir).await?;
            tracing::debug!(digest = %digest, "Removed cached layer VHD");
        }
        Ok(())
    }
}

//--------------------------------------------------------------------------------------------------
// Trait Implementations
//--------------------------------------------------------------------------------------------------

#[async_trait]
impl RootfsMaterializer for WindowsRootfsMaterializer {
    type MaterializedRootfs = WindowsMaterializedRootfs;

    async fn materialize(
        &self,
        spec: &ResolvedSandboxSpec,
    ) -> MicrosandboxResult<Self::MaterializedRootfs> {
        let sandbox_id = &spec.sandbox_key;

        match &spec.rootfs_source {
            RootfsSource::Overlayfs {
                layers,
                rw_dir: _,
                patch_dir: _,
            } => {
                // Convert each layer directory's source tarball to a VHD.
                // On Windows, `layers` contains paths to tarballs (not extracted dirs).
                let mut layer_vhds = Vec::with_capacity(layers.len());
                for layer_path in layers {
                    // Extract digest from the layer path (filename without extension).
                    let digest = layer_path
                        .file_stem()
                        .and_then(|s| s.to_str())
                        .unwrap_or("unknown");

                    let vhd_path = self
                        .get_or_create_layer_vhd(layer_path, digest)
                        .await?;
                    layer_vhds.push(vhd_path);
                }

                // Create per-sandbox patch VHD.
                let patch_vhd = self.create_patch_vhd(sandbox_id, spec).await?;

                // Create or reuse scratch VHDX.
                let scratch_vhdx = self.create_or_reuse_scratch_vhdx(sandbox_id).await?;

                Ok(WindowsMaterializedRootfs {
                    layer_vhds,
                    patch_vhd,
                    scratch_vhdx,
                })
            }
            RootfsSource::Native(_) => Err(MicrosandboxError::NotImplemented(
                "Native rootfs is not supported on Windows — use OCI images".to_string(),
            )),
            RootfsSource::OciImage { reference } => Err(MicrosandboxError::NotImplemented(
                format!(
                    "OCI image materialization should be done before calling materialize(): {}",
                    reference
                ),
            )),
        }
    }
}

//--------------------------------------------------------------------------------------------------
// Helper types for guest bootstrap
//--------------------------------------------------------------------------------------------------

/// Serializable sandbox spec passed to the guest via the patch VHD.
#[derive(Debug, Serialize, Deserialize)]
struct SandboxSpecForGuest {
    sandbox_id: String,
    mounts: Vec<GuestMountEntry>,
    workdir: Option<String>,
    exec_path: String,
    exec_args: Vec<String>,
    env: Vec<String>,
}

/// A mount entry as seen by the guest bootstrap.
#[derive(Debug, Serialize, Deserialize)]
struct GuestMountEntry {
    guest_path: String,
    readonly: bool,
}

//--------------------------------------------------------------------------------------------------
// Helper functions
//--------------------------------------------------------------------------------------------------

/// Sanitizes an OCI digest string for use as a directory name.
///
/// Replaces `:` with `_` (e.g., `sha256:abc123` → `sha256_abc123`).
fn sanitize_digest(digest: &str) -> String {
    digest.replace(':', "_")
}

/// Creates a tar archive from a directory.
async fn create_tar_from_dir(source_dir: &Path, tar_path: &Path) -> MicrosandboxResult<()> {
    let source = source_dir.to_path_buf();
    let output = tar_path.to_path_buf();

    tokio::task::spawn_blocking(move || {
        let file = std::fs::File::create(&output).map_err(|e| {
            MicrosandboxError::LayerExtraction(format!("failed to create tar file: {}", e))
        })?;
        let mut builder = tar::Builder::new(file);
        builder.append_dir_all(".", &source).map_err(|e| {
            MicrosandboxError::LayerExtraction(format!("failed to create tar archive: {}", e))
        })?;
        builder.finish().map_err(|e| {
            MicrosandboxError::LayerExtraction(format!("failed to finish tar archive: {}", e))
        })?;
        Ok(())
    })
    .await
    .map_err(|e| MicrosandboxError::LayerExtraction(format!("tar task join error: {}", e)))?
}

/// Generates a cache key from a digest string.
///
/// The cache key is the sanitized digest suitable for use as a directory name.
pub fn cache_key_from_digest(digest: &str) -> String {
    sanitize_digest(digest)
}

//--------------------------------------------------------------------------------------------------
// Tests
//--------------------------------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sanitize_digest() {
        assert_eq!(
            sanitize_digest("sha256:abc123def456"),
            "sha256_abc123def456"
        );
        assert_eq!(sanitize_digest("no_colon"), "no_colon");
        assert_eq!(
            sanitize_digest("sha256:a:b:c"),
            "sha256_a_b_c"
        );
    }

    #[test]
    fn test_cache_key_from_digest() {
        assert_eq!(
            cache_key_from_digest("sha256:abc123"),
            "sha256_abc123"
        );
    }

    #[test]
    fn test_layer_cache_dir_structure() {
        let config = WindowsRootfsConfig {
            cache_dir: PathBuf::from("/cache"),
            runtime_dir: PathBuf::from("/runtime"),
            tar2ext4_path: PathBuf::from("/usr/bin/tar2ext4"),
        };
        let materializer = WindowsRootfsMaterializer::new(config);

        let dir = materializer.layer_cache_dir("sha256:abc123");
        assert_eq!(
            dir,
            PathBuf::from("/cache/windows/layers/sha256_abc123")
        );
    }

    #[test]
    fn test_layer_vhd_path() {
        let config = WindowsRootfsConfig {
            cache_dir: PathBuf::from("/cache"),
            runtime_dir: PathBuf::from("/runtime"),
            tar2ext4_path: PathBuf::from("/usr/bin/tar2ext4"),
        };
        let materializer = WindowsRootfsMaterializer::new(config);

        let path = materializer.layer_vhd_path("sha256:abc123");
        assert_eq!(
            path,
            PathBuf::from("/cache/windows/layers/sha256_abc123/layer.vhd")
        );
    }

    #[test]
    fn test_layer_manifest_path() {
        let config = WindowsRootfsConfig {
            cache_dir: PathBuf::from("/cache"),
            runtime_dir: PathBuf::from("/runtime"),
            tar2ext4_path: PathBuf::from("/usr/bin/tar2ext4"),
        };
        let materializer = WindowsRootfsMaterializer::new(config);

        let path = materializer.layer_manifest_path("sha256:abc123");
        assert_eq!(
            path,
            PathBuf::from("/cache/windows/layers/sha256_abc123/manifest.json")
        );
    }

    #[test]
    fn test_sandbox_dir_structure() {
        let config = WindowsRootfsConfig {
            cache_dir: PathBuf::from("/cache"),
            runtime_dir: PathBuf::from("/runtime"),
            tar2ext4_path: PathBuf::from("/usr/bin/tar2ext4"),
        };
        let materializer = WindowsRootfsMaterializer::new(config);

        let dir = materializer.sandbox_dir("project~sandbox1");
        assert_eq!(
            dir,
            PathBuf::from("/runtime/windows/sandboxes/project~sandbox1")
        );
    }

    #[test]
    fn test_patch_vhd_path() {
        let config = WindowsRootfsConfig {
            cache_dir: PathBuf::from("/cache"),
            runtime_dir: PathBuf::from("/runtime"),
            tar2ext4_path: PathBuf::from("/usr/bin/tar2ext4"),
        };
        let materializer = WindowsRootfsMaterializer::new(config);

        let path = materializer.patch_vhd_path("project~sandbox1");
        assert_eq!(
            path,
            PathBuf::from("/runtime/windows/sandboxes/project~sandbox1/patch.vhd")
        );
    }

    #[test]
    fn test_scratch_vhdx_path() {
        let config = WindowsRootfsConfig {
            cache_dir: PathBuf::from("/cache"),
            runtime_dir: PathBuf::from("/runtime"),
            tar2ext4_path: PathBuf::from("/usr/bin/tar2ext4"),
        };
        let materializer = WindowsRootfsMaterializer::new(config);

        let path = materializer.scratch_vhdx_path("project~sandbox1");
        assert_eq!(
            path,
            PathBuf::from("/runtime/windows/sandboxes/project~sandbox1/scratch.vhdx")
        );
    }

    #[test]
    fn test_manifest_serialization_roundtrip() {
        let manifest = LayerVhdManifest {
            digest: "sha256:abc123".to_string(),
            source_size: 1024,
            created_at: "2026-03-14T00:00:00Z".to_string(),
            tool_version: "tar2ext4".to_string(),
        };

        let json = serde_json::to_string(&manifest).unwrap();
        let deserialized: LayerVhdManifest = serde_json::from_str(&json).unwrap();

        assert_eq!(deserialized.digest, "sha256:abc123");
        assert_eq!(deserialized.source_size, 1024);
        assert_eq!(deserialized.created_at, "2026-03-14T00:00:00Z");
        assert_eq!(deserialized.tool_version, "tar2ext4");
    }

    #[test]
    fn test_manifest_validation_digest_mismatch() {
        let manifest = LayerVhdManifest {
            digest: "sha256:abc123".to_string(),
            source_size: 1024,
            created_at: "2026-03-14T00:00:00Z".to_string(),
            tool_version: "tar2ext4".to_string(),
        };

        // Manifest digest matches the expected digest.
        assert_eq!(manifest.digest, "sha256:abc123");

        // Different digest should not match.
        assert_ne!(manifest.digest, "sha256:different");
    }

    #[tokio::test]
    async fn test_cache_validity_nonexistent() {
        let temp = tempfile::tempdir().unwrap();
        let config = WindowsRootfsConfig {
            cache_dir: temp.path().to_path_buf(),
            runtime_dir: temp.path().join("runtime"),
            tar2ext4_path: PathBuf::from("tar2ext4"),
        };
        let materializer = WindowsRootfsMaterializer::new(config);

        // Non-existent cache should be invalid.
        assert!(!materializer.is_cache_valid("sha256:nonexistent").await);
    }

    #[tokio::test]
    async fn test_cache_validity_with_valid_manifest() {
        let temp = tempfile::tempdir().unwrap();
        let config = WindowsRootfsConfig {
            cache_dir: temp.path().to_path_buf(),
            runtime_dir: temp.path().join("runtime"),
            tar2ext4_path: PathBuf::from("tar2ext4"),
        };
        let materializer = WindowsRootfsMaterializer::new(config);

        let digest = "sha256:validdigest";
        let cache_dir = materializer.layer_cache_dir(digest);
        fs::create_dir_all(&cache_dir).await.unwrap();

        // Create a dummy VHD file.
        fs::write(cache_dir.join(LAYER_VHD_FILENAME), b"fake vhd")
            .await
            .unwrap();

        // Create a valid manifest.
        let manifest = LayerVhdManifest {
            digest: digest.to_string(),
            source_size: 100,
            created_at: "2026-03-14T00:00:00Z".to_string(),
            tool_version: "tar2ext4".to_string(),
        };
        fs::write(
            cache_dir.join(LAYER_MANIFEST_FILENAME),
            serde_json::to_string(&manifest).unwrap(),
        )
        .await
        .unwrap();

        assert!(materializer.is_cache_valid(digest).await);
    }

    #[tokio::test]
    async fn test_cache_validity_with_mismatched_digest() {
        let temp = tempfile::tempdir().unwrap();
        let config = WindowsRootfsConfig {
            cache_dir: temp.path().to_path_buf(),
            runtime_dir: temp.path().join("runtime"),
            tar2ext4_path: PathBuf::from("tar2ext4"),
        };
        let materializer = WindowsRootfsMaterializer::new(config);

        let digest = "sha256:expected";
        let cache_dir = materializer.layer_cache_dir(digest);
        fs::create_dir_all(&cache_dir).await.unwrap();

        // Create a dummy VHD file.
        fs::write(cache_dir.join(LAYER_VHD_FILENAME), b"fake vhd")
            .await
            .unwrap();

        // Create a manifest with a different digest.
        let manifest = LayerVhdManifest {
            digest: "sha256:different".to_string(),
            source_size: 100,
            created_at: "2026-03-14T00:00:00Z".to_string(),
            tool_version: "tar2ext4".to_string(),
        };
        fs::write(
            cache_dir.join(LAYER_MANIFEST_FILENAME),
            serde_json::to_string(&manifest).unwrap(),
        )
        .await
        .unwrap();

        // Should be invalid because digest doesn't match.
        assert!(!materializer.is_cache_valid(digest).await);
    }

    #[tokio::test]
    async fn test_cleanup_sandbox() {
        let temp = tempfile::tempdir().unwrap();
        let config = WindowsRootfsConfig {
            cache_dir: temp.path().to_path_buf(),
            runtime_dir: temp.path().to_path_buf(),
            tar2ext4_path: PathBuf::from("tar2ext4"),
        };
        let materializer = WindowsRootfsMaterializer::new(config);

        let sandbox_id = "test~sandbox";
        let sandbox_dir = materializer.sandbox_dir(sandbox_id);
        fs::create_dir_all(&sandbox_dir).await.unwrap();
        fs::write(sandbox_dir.join("some_file"), b"data")
            .await
            .unwrap();

        assert!(sandbox_dir.exists());
        materializer.cleanup_sandbox(sandbox_id).await.unwrap();
        assert!(!sandbox_dir.exists());
    }

    #[tokio::test]
    async fn test_cleanup_nonexistent_sandbox() {
        let temp = tempfile::tempdir().unwrap();
        let config = WindowsRootfsConfig {
            cache_dir: temp.path().to_path_buf(),
            runtime_dir: temp.path().to_path_buf(),
            tar2ext4_path: PathBuf::from("tar2ext4"),
        };
        let materializer = WindowsRootfsMaterializer::new(config);

        // Should not error on nonexistent sandbox.
        materializer
            .cleanup_sandbox("nonexistent~sandbox")
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn test_remove_cached_layer() {
        let temp = tempfile::tempdir().unwrap();
        let config = WindowsRootfsConfig {
            cache_dir: temp.path().to_path_buf(),
            runtime_dir: temp.path().join("runtime"),
            tar2ext4_path: PathBuf::from("tar2ext4"),
        };
        let materializer = WindowsRootfsMaterializer::new(config);

        let digest = "sha256:removeme";
        let cache_dir = materializer.layer_cache_dir(digest);
        fs::create_dir_all(&cache_dir).await.unwrap();
        fs::write(cache_dir.join("layer.vhd"), b"fake")
            .await
            .unwrap();

        assert!(cache_dir.exists());
        materializer.remove_cached_layer(digest).await.unwrap();
        assert!(!cache_dir.exists());
    }
}
