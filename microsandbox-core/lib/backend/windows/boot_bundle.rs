//! Boot bundle management for Windows-hosted Linux VMs.
//!
//! The boot bundle contains all files needed to boot a Linux utility VM:
//! kernel, rootfs VHD, bootstrap binary, portal binary, and manifest.
//! The layout is fixed and versioned.
//!
//! When the bundle is not present locally, `ensure_bundle()` will attempt
//! to download it from GitHub releases automatically.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::fs;
use tokio::io::AsyncWriteExt;

use crate::{MicrosandboxError, MicrosandboxResult};

#[cfg(feature = "cli")]
use indicatif::{ProgressBar, ProgressStyle};
#[cfg(feature = "cli")]
use microsandbox_utils::term::MULTI_PROGRESS;

//--------------------------------------------------------------------------------------------------
// Constants
//--------------------------------------------------------------------------------------------------

/// Subdirectory under the runtime home for boot bundles.
const UVM_SUBDIR: &str = "windows/uvm";

/// Expected files in a boot bundle directory.
const KERNEL_FILENAME: &str = "kernel";
const ROOTFS_VHD_FILENAME: &str = "rootfs.vhd";
const BOOTSTRAP_FILENAME: &str = "bootstrap";
const PORTAL_FILENAME: &str = "portal";
const MANIFEST_FILENAME: &str = "manifest.json";
const TAR2EXT4_FILENAME: &str = "tar2ext4.exe";

/// GitHub release URL template for boot bundle zips.
const BUNDLE_URL_TEMPLATE: &str = "https://github.com/microsandbox/microsandbox/releases/download/microsandbox-v{version}/microsandbox-boot-bundle-{version}-windows-x86_64.zip";

/// Suffix for SHA256 checksum sidecar files.
const SHA256_SUFFIX: &str = ".sha256";

//--------------------------------------------------------------------------------------------------
// Types
//--------------------------------------------------------------------------------------------------

/// Boot bundle manifest stored as `manifest.json` in the bundle directory.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BootBundleManifest {
    /// Semantic version of the boot bundle (e.g., "0.5.0").
    pub version: String,

    /// Minimum host version required to use this bundle.
    pub min_host_version: String,

    /// SHA-256 hash of the rootfs VHD.
    pub sha256: String,

    /// ISO 8601 timestamp of when the bundle was created.
    pub created_at: String,
}

/// Represents a validated boot bundle on disk.
#[derive(Debug, Clone)]
pub struct BootBundle {
    /// Root directory of the boot bundle.
    pub dir: PathBuf,

    /// The parsed manifest.
    pub manifest: BootBundleManifest,
}

//--------------------------------------------------------------------------------------------------
// Methods
//--------------------------------------------------------------------------------------------------

impl BootBundle {
    /// Path to the Linux kernel.
    pub fn kernel_path(&self) -> PathBuf {
        self.dir.join(KERNEL_FILENAME)
    }

    /// Path to the rootfs VHD (minimal Linux userspace).
    pub fn rootfs_vhd_path(&self) -> PathBuf {
        self.dir.join(ROOTFS_VHD_FILENAME)
    }

    /// Path to the bootstrap binary.
    pub fn bootstrap_path(&self) -> PathBuf {
        self.dir.join(BOOTSTRAP_FILENAME)
    }

    /// Path to the portal binary.
    pub fn portal_path(&self) -> PathBuf {
        self.dir.join(PORTAL_FILENAME)
    }

    /// Path to the tar2ext4 conversion tool.
    pub fn tar2ext4_path(&self) -> PathBuf {
        self.dir.join(TAR2EXT4_FILENAME)
    }

    /// Path to the manifest file.
    pub fn manifest_path(&self) -> PathBuf {
        self.dir.join(MANIFEST_FILENAME)
    }
}

//--------------------------------------------------------------------------------------------------
// Functions
//--------------------------------------------------------------------------------------------------

/// Returns the boot bundle directory for a specific version.
///
/// Layout: `<runtime_dir>/windows/uvm/<version>/`
pub fn bundle_dir(runtime_dir: &Path, version: &str) -> PathBuf {
    runtime_dir.join(UVM_SUBDIR).join(version)
}

/// Loads and validates a boot bundle from disk.
///
/// Verifies that all required files exist and the manifest is valid.
pub async fn load_bundle(runtime_dir: &Path, version: &str) -> MicrosandboxResult<BootBundle> {
    let dir = bundle_dir(runtime_dir, version);

    if !dir.exists() {
        return Err(MicrosandboxError::PathNotFound(format!(
            "Boot bundle not found at {}. Run boot bundle install first.",
            dir.display()
        )));
    }

    // Read and parse the manifest.
    let manifest_path = dir.join(MANIFEST_FILENAME);
    let manifest_data = fs::read_to_string(&manifest_path).await.map_err(|e| {
        MicrosandboxError::PathNotFound(format!(
            "Boot bundle manifest not found at {}: {}",
            manifest_path.display(),
            e
        ))
    })?;

    let manifest: BootBundleManifest = serde_json::from_str(&manifest_data)?;

    // Verify required files exist.
    let required_files = [
        (KERNEL_FILENAME, "Linux kernel"),
        (ROOTFS_VHD_FILENAME, "rootfs VHD"),
        (BOOTSTRAP_FILENAME, "bootstrap binary"),
        (PORTAL_FILENAME, "portal binary"),
    ];

    for (filename, description) in &required_files {
        let path = dir.join(filename);
        if !path.exists() {
            return Err(MicrosandboxError::PathNotFound(format!(
                "Boot bundle missing {}: {}",
                description,
                path.display()
            )));
        }
    }

    Ok(BootBundle { dir, manifest })
}

/// Loads an existing boot bundle, or downloads it from GitHub releases if missing.
///
/// Attempts to load the boot bundle from the runtime directory.
/// If not found, downloads the versioned zip from GitHub, verifies its SHA256,
/// extracts it, and validates the result.
pub async fn ensure_bundle(runtime_dir: &Path, version: &str) -> MicrosandboxResult<BootBundle> {
    match load_bundle(runtime_dir, version).await {
        Ok(bundle) => {
            tracing::info!(
                version = %bundle.manifest.version,
                dir = %bundle.dir.display(),
                "Boot bundle loaded"
            );
            Ok(bundle)
        }
        Err(_) => {
            tracing::info!(version, "Boot bundle not found, downloading...");
            download_bundle(runtime_dir, version).await
        }
    }
}

/// Downloads a boot bundle zip from GitHub releases, verifies its integrity,
/// extracts it, and returns the validated bundle.
async fn download_bundle(runtime_dir: &Path, version: &str) -> MicrosandboxResult<BootBundle> {
    let dir = bundle_dir(runtime_dir, version);
    fs::create_dir_all(&dir).await?;

    let zip_url = BUNDLE_URL_TEMPLATE.replace("{version}", version);
    let sha_url = format!("{}{}", &zip_url, SHA256_SUFFIX);
    let zip_path = dir.join(format!(
        "microsandbox-boot-bundle-{}-windows-x86_64.zip",
        version
    ));

    // Step 1: Download the .sha256 sidecar.
    tracing::info!(%sha_url, "Fetching checksum");
    let sha_response = reqwest::get(&sha_url).await.map_err(|e| {
        MicrosandboxError::ImageLayerDownloadFailed(format!(
            "Failed to download boot bundle checksum from {}: {}",
            sha_url, e
        ))
    })?;

    if !sha_response.status().is_success() {
        return Err(MicrosandboxError::ImageLayerDownloadFailed(format!(
            "Boot bundle checksum not found at {} (HTTP {}). \
             Ensure release microsandbox-v{} exists.",
            sha_url,
            sha_response.status(),
            version
        )));
    }

    let expected_hash = sha_response
        .text()
        .await
        .map_err(|e| {
            MicrosandboxError::ImageLayerDownloadFailed(format!(
                "Failed to read checksum response: {}",
                e
            ))
        })?
        .trim()
        .split_whitespace()
        .next()
        .unwrap_or("")
        .to_lowercase();

    if expected_hash.len() != 64 {
        return Err(MicrosandboxError::ImageLayerDownloadFailed(format!(
            "Invalid SHA256 checksum format from {}: '{}'",
            sha_url, expected_hash
        )));
    }

    // Step 2: Download the zip with streaming and optional progress bar.
    tracing::info!(%zip_url, "Downloading boot bundle");
    let zip_response = reqwest::get(&zip_url).await.map_err(|e| {
        MicrosandboxError::ImageLayerDownloadFailed(format!(
            "Failed to download boot bundle from {}: {}",
            zip_url, e
        ))
    })?;

    if !zip_response.status().is_success() {
        return Err(MicrosandboxError::ImageLayerDownloadFailed(format!(
            "Boot bundle not found at {} (HTTP {}). \
             Ensure release microsandbox-v{} exists.",
            zip_url,
            zip_response.status(),
            version
        )));
    }

    #[cfg(feature = "cli")]
    let progress_bar = {
        let total_size = zip_response.content_length().unwrap_or(0);
        let pb = MULTI_PROGRESS.add(ProgressBar::new(total_size));
        let style = ProgressStyle::with_template(
            "{spinner:.green} Downloading boot bundle {bar:40.green/green.dim} {bytes:.bold} / {total_bytes:.dim} ({eta})",
        )
        .unwrap()
        .progress_chars("=+-");
        pb.set_style(style);
        pb
    };

    let mut file = fs::File::create(&zip_path).await?;
    let mut hasher = Sha256::new();
    let mut stream = zip_response.bytes_stream();

    use futures::StreamExt;
    while let Some(chunk) = stream.next().await {
        let bytes = chunk.map_err(|e| {
            MicrosandboxError::ImageLayerDownloadFailed(format!(
                "Error reading boot bundle download stream: {}",
                e
            ))
        })?;
        hasher.update(&bytes);
        file.write_all(&bytes).await?;
        #[cfg(feature = "cli")]
        progress_bar.inc(bytes.len() as u64);
    }
    file.flush().await?;
    drop(file);

    #[cfg(feature = "cli")]
    progress_bar.finish_and_clear();

    // Step 3: Verify SHA256.
    let actual_hash = hex::encode(hasher.finalize());
    if actual_hash != expected_hash {
        // Clean up the corrupt download.
        let _ = fs::remove_file(&zip_path).await;
        return Err(MicrosandboxError::ImageLayerDownloadFailed(format!(
            "Boot bundle SHA256 mismatch: expected {}, got {}",
            expected_hash, actual_hash
        )));
    }
    tracing::info!("Boot bundle checksum verified");

    // Step 4: Extract via PowerShell Expand-Archive.
    tracing::info!(zip = %zip_path.display(), dest = %dir.display(), "Extracting boot bundle");
    let ps_cmd = format!(
        "Expand-Archive -Path '{}' -DestinationPath '{}' -Force",
        zip_path.display(),
        dir.display()
    );
    let output = tokio::process::Command::new("powershell")
        .args(["-NoProfile", "-NonInteractive", "-Command", &ps_cmd])
        .output()
        .await?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        // Clean up.
        let _ = fs::remove_file(&zip_path).await;
        return Err(MicrosandboxError::ImageLayerDownloadFailed(format!(
            "Failed to extract boot bundle zip: {}",
            stderr.trim()
        )));
    }

    // Clean up the zip file after successful extraction.
    let _ = fs::remove_file(&zip_path).await;

    // Step 5: Validate via load_bundle.
    load_bundle(runtime_dir, version).await.map_err(|e| {
        MicrosandboxError::ImageLayerDownloadFailed(format!(
            "Downloaded boot bundle failed validation: {}",
            e
        ))
    })
}

/// Checks if a boot bundle version is compatible with the current host version.
pub fn check_compatibility(
    bundle: &BootBundleManifest,
    host_version: &str,
) -> MicrosandboxResult<()> {
    let bundle_min: semver::Version = bundle.min_host_version.parse().map_err(|e| {
        MicrosandboxError::ConfigValidation(format!(
            "Invalid min_host_version in boot bundle: {}",
            e
        ))
    })?;

    let host: semver::Version = host_version
        .parse()
        .map_err(|e| MicrosandboxError::ConfigValidation(format!("Invalid host version: {}", e)))?;

    if host < bundle_min {
        return Err(MicrosandboxError::ConfigValidation(format!(
            "Boot bundle requires host version >= {}, but current version is {}",
            bundle.min_host_version, host_version
        )));
    }

    Ok(())
}

//--------------------------------------------------------------------------------------------------
// Tests
//--------------------------------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_bundle_dir() {
        let dir = bundle_dir(&PathBuf::from("/runtime"), "0.5.0");
        assert_eq!(dir, PathBuf::from("/runtime/windows/uvm/0.5.0"));
    }

    #[test]
    fn test_boot_bundle_paths() {
        let bundle = BootBundle {
            dir: PathBuf::from("/runtime/windows/uvm/0.5.0"),
            manifest: BootBundleManifest {
                version: "0.5.0".to_string(),
                min_host_version: "0.2.0".to_string(),
                sha256: "abc123".to_string(),
                created_at: "2026-03-14T00:00:00Z".to_string(),
            },
        };

        assert_eq!(
            bundle.kernel_path(),
            PathBuf::from("/runtime/windows/uvm/0.5.0/kernel")
        );
        assert_eq!(
            bundle.rootfs_vhd_path(),
            PathBuf::from("/runtime/windows/uvm/0.5.0/rootfs.vhd")
        );
        assert_eq!(
            bundle.bootstrap_path(),
            PathBuf::from("/runtime/windows/uvm/0.5.0/bootstrap")
        );
        assert_eq!(
            bundle.portal_path(),
            PathBuf::from("/runtime/windows/uvm/0.5.0/portal")
        );
        assert_eq!(
            bundle.tar2ext4_path(),
            PathBuf::from("/runtime/windows/uvm/0.5.0/tar2ext4.exe")
        );
    }

    #[test]
    fn test_manifest_serialization() {
        let manifest = BootBundleManifest {
            version: "0.5.0".to_string(),
            min_host_version: "0.2.0".to_string(),
            sha256: "deadbeef".to_string(),
            created_at: "2026-03-14T00:00:00Z".to_string(),
        };

        let json = serde_json::to_string_pretty(&manifest).unwrap();
        let deserialized: BootBundleManifest = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.version, "0.5.0");
        assert_eq!(deserialized.min_host_version, "0.2.0");
    }

    #[test]
    fn test_compatibility_check_ok() {
        let manifest = BootBundleManifest {
            version: "0.5.0".to_string(),
            min_host_version: "0.2.0".to_string(),
            sha256: "abc".to_string(),
            created_at: "2026-01-01T00:00:00Z".to_string(),
        };

        assert!(check_compatibility(&manifest, "0.2.6").is_ok());
        assert!(check_compatibility(&manifest, "0.3.0").is_ok());
        assert!(check_compatibility(&manifest, "1.0.0").is_ok());
    }

    #[test]
    fn test_compatibility_check_too_old() {
        let manifest = BootBundleManifest {
            version: "0.5.0".to_string(),
            min_host_version: "0.3.0".to_string(),
            sha256: "abc".to_string(),
            created_at: "2026-01-01T00:00:00Z".to_string(),
        };

        assert!(check_compatibility(&manifest, "0.2.6").is_err());
        assert!(check_compatibility(&manifest, "0.1.0").is_err());
    }

    #[tokio::test]
    async fn test_load_bundle_missing() {
        let temp = tempfile::tempdir().unwrap();
        let result = load_bundle(temp.path(), "0.5.0").await;
        assert!(result.is_err());
    }

    #[test]
    fn test_download_url_construction() {
        let version = "0.2.6";
        let url = BUNDLE_URL_TEMPLATE.replace("{version}", version);
        assert_eq!(
            url,
            "https://github.com/microsandbox/microsandbox/releases/download/microsandbox-v0.2.6/microsandbox-boot-bundle-0.2.6-windows-x86_64.zip"
        );
        let sha_url = format!("{}{}", &url, SHA256_SUFFIX);
        assert!(sha_url.ends_with(".zip.sha256"));
    }
}
