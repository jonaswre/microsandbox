//! Platform-aware path resolution for microsandbox storage and binaries.
//!
//! Provides the `PlatformPaths` trait and platform-specific implementations
//! (`UnixPlatformPaths`, `WindowsPlatformPaths`) to abstract over OS differences
//! in default data storage, cache, and binary install locations.

use std::path::PathBuf;

//--------------------------------------------------------------------------------------------------
// Types
//--------------------------------------------------------------------------------------------------

/// Trait for resolving platform-specific filesystem paths.
///
/// Each platform has different conventions for where to store application data,
/// caches, and binaries. This trait abstracts those differences.
pub trait PlatformPaths {
    /// Returns the root data directory for microsandbox.
    ///
    /// - Unix: `~/.microsandbox`
    /// - Windows: `%LOCALAPPDATA%\Microsandbox\data`
    fn data_home(&self) -> PathBuf;

    /// Returns the cache directory for microsandbox.
    ///
    /// - Unix: `~/.microsandbox` (same as data_home)
    /// - Windows: `%LOCALAPPDATA%\Microsandbox\cache`
    fn cache_home(&self) -> PathBuf;

    /// Returns the runtime directory for ephemeral state.
    ///
    /// - Unix: `~/.microsandbox` (same as data_home)
    /// - Windows: `%LOCALAPPDATA%\Microsandbox\runtime`
    fn runtime_home(&self) -> PathBuf;

    /// Returns the directory for user-installed binaries.
    ///
    /// - Unix: `~/.local/bin`
    /// - Windows: `%LOCALAPPDATA%\Microsandbox\bin`
    fn bin_dir(&self) -> PathBuf;

    /// Returns the platform's null device path.
    ///
    /// - Unix: `/dev/null`
    /// - Windows: `NUL`
    fn null_device(&self) -> &'static str;
}

/// Unix platform paths implementation.
pub struct UnixPlatformPaths;

/// Windows platform paths implementation.
pub struct WindowsPlatformPaths;

//--------------------------------------------------------------------------------------------------
// Trait Implementations
//--------------------------------------------------------------------------------------------------

#[cfg(unix)]
impl PlatformPaths for UnixPlatformPaths {
    fn data_home(&self) -> PathBuf {
        dirs::home_dir()
            .expect("could not determine home directory")
            .join(".microsandbox")
    }

    fn cache_home(&self) -> PathBuf {
        self.data_home()
    }

    fn runtime_home(&self) -> PathBuf {
        self.data_home()
    }

    fn bin_dir(&self) -> PathBuf {
        dirs::home_dir()
            .expect("could not determine home directory")
            .join(".local")
            .join("bin")
    }

    fn null_device(&self) -> &'static str {
        "/dev/null"
    }
}

#[cfg(windows)]
impl PlatformPaths for WindowsPlatformPaths {
    fn data_home(&self) -> PathBuf {
        dirs::data_local_dir()
            .expect("could not determine LOCALAPPDATA directory")
            .join("Microsandbox")
            .join("data")
    }

    fn cache_home(&self) -> PathBuf {
        dirs::data_local_dir()
            .expect("could not determine LOCALAPPDATA directory")
            .join("Microsandbox")
            .join("cache")
    }

    fn runtime_home(&self) -> PathBuf {
        dirs::data_local_dir()
            .expect("could not determine LOCALAPPDATA directory")
            .join("Microsandbox")
            .join("runtime")
    }

    fn bin_dir(&self) -> PathBuf {
        dirs::data_local_dir()
            .expect("could not determine LOCALAPPDATA directory")
            .join("Microsandbox")
            .join("bin")
    }

    fn null_device(&self) -> &'static str {
        "NUL"
    }
}

//--------------------------------------------------------------------------------------------------
// Functions
//--------------------------------------------------------------------------------------------------

/// Returns the platform-appropriate path provider.
///
/// # Examples
///
/// ```
/// use microsandbox_utils::platform::platform_paths;
///
/// let paths = platform_paths();
/// let data = paths.data_home();
/// let null = paths.null_device();
/// ```
pub fn platform_paths() -> &'static dyn PlatformPaths {
    #[cfg(unix)]
    {
        &UnixPlatformPaths
    }
    #[cfg(windows)]
    {
        &WindowsPlatformPaths
    }
}

//--------------------------------------------------------------------------------------------------
// Shell shims (Windows)
//--------------------------------------------------------------------------------------------------

/// Generates the content for `msb.cmd` (Windows cmd.exe shim).
///
/// This shim forwards all arguments to the actual `msb.exe` binary in the same directory.
pub fn generate_cmd_shim() -> &'static str {
    r#"@echo off
rem MSB-SHIM: cmd.exe shim for microsandbox CLI
rem Forwards all arguments to msb.exe in the same directory
"%~dp0msb.exe" %*
"#
}

/// Generates the content for `msb.ps1` (PowerShell shim).
///
/// This shim forwards all arguments to the actual `msb.exe` binary in the same directory.
pub fn generate_ps1_shim() -> &'static str {
    r#"# MSB-SHIM: PowerShell shim for microsandbox CLI
# Forwards all arguments to msb.exe in the same directory
$msbPath = Join-Path $PSScriptRoot "msb.exe"
& $msbPath @args
exit $LASTEXITCODE
"#
}

//--------------------------------------------------------------------------------------------------
// Tests
//--------------------------------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_platform_paths_data_home() {
        let paths = platform_paths();
        let data = paths.data_home();
        assert!(!data.as_os_str().is_empty());
    }

    #[test]
    fn test_platform_paths_cache_home() {
        let paths = platform_paths();
        let cache = paths.cache_home();
        assert!(!cache.as_os_str().is_empty());
    }

    #[test]
    fn test_platform_paths_bin_dir() {
        let paths = platform_paths();
        let bin = paths.bin_dir();
        assert!(!bin.as_os_str().is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn test_unix_null_device() {
        let paths = platform_paths();
        assert_eq!(paths.null_device(), "/dev/null");
    }

    #[cfg(windows)]
    #[test]
    fn test_windows_null_device() {
        let paths = platform_paths();
        assert_eq!(paths.null_device(), "NUL");
    }

    #[cfg(unix)]
    #[test]
    fn test_unix_data_home_ends_with_microsandbox() {
        let paths = platform_paths();
        let data = paths.data_home();
        assert!(data.ends_with(".microsandbox"));
    }

    #[cfg(unix)]
    #[test]
    fn test_unix_bin_dir_ends_with_bin() {
        let paths = platform_paths();
        let bin = paths.bin_dir();
        assert!(bin.ends_with("bin"));
    }

    #[test]
    fn test_cmd_shim_content() {
        let shim = generate_cmd_shim();
        assert!(shim.contains("msb.exe"));
        assert!(shim.contains("@echo off"));
        assert!(shim.contains("%*"));
    }

    #[test]
    fn test_ps1_shim_content() {
        let shim = generate_ps1_shim();
        assert!(shim.contains("msb.exe"));
        assert!(shim.contains("$PSScriptRoot"));
        assert!(shim.contains("@args"));
    }
}
