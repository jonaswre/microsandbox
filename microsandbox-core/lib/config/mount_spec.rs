use std::{fmt, path::PathBuf, str::FromStr};

use serde::{Deserialize, Deserializer, Serialize, Serializer, de, ser::SerializeStruct};
use typed_path::Utf8UnixPathBuf;

use crate::MicrosandboxError;

//--------------------------------------------------------------------------------------------------
// Types
//--------------------------------------------------------------------------------------------------

/// A path on the host operating system.
///
/// Newtype wrapper around `std::path::PathBuf` that represents paths on the host OS.
/// Handles platform-specific path semantics: Unix forward slashes, Windows backslashes
/// and drive letters (`C:\...`), UNC paths (`\\server\share\...`).
///
/// ## Examples
///
/// ```
/// use microsandbox_core::config::HostPathBuf;
/// use std::path::PathBuf;
///
/// let host = HostPathBuf::from(PathBuf::from("/home/user/project"));
/// #[cfg(unix)]
/// assert!(host.is_absolute());
/// assert_eq!(host.to_string(), "/home/user/project");
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct HostPathBuf(PathBuf);

/// A path inside the Linux guest VM.
///
/// Always uses Unix path semantics regardless of the host operating system,
/// since the guest is always a Linux VM.
pub type GuestPathBuf = Utf8UnixPathBuf;

/// A structured mount specification for mapping host directories into guest VMs.
///
/// Replaces the legacy `PathPair` type which used colon-delimited strings that
/// break on Windows drive letters (e.g., `C:\Users\foo:/app`).
///
/// ## YAML Formats
///
/// Structured form (works on all platforms):
/// ```yaml
/// volumes:
///   - host: "/home/user/project"
///     guest: "/workspace"
///   - host: "C:\\Users\\jonas\\project"
///     guest: "/workspace"
///     readonly: true
/// ```
///
/// Legacy string form (Unix only, for backward compatibility):
/// ```yaml
/// volumes:
///   - "/host/path:/guest/path"
///   - "/data"  # same path on host and guest
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MountSpec {
    /// The path on the host filesystem.
    pub host: HostPathBuf,

    /// The path inside the guest VM (always Unix-style).
    pub guest: GuestPathBuf,

    /// Whether the mount is read-only.
    pub readonly: bool,
}

//--------------------------------------------------------------------------------------------------
// Methods
//--------------------------------------------------------------------------------------------------

impl HostPathBuf {
    /// Creates a new `HostPathBuf`, validating that the path is not empty.
    pub fn new(path: PathBuf) -> Result<Self, MicrosandboxError> {
        if path.as_os_str().is_empty() {
            return Err(MicrosandboxError::InvalidMountSpec(
                "empty host path".to_string(),
            ));
        }
        Ok(Self(path))
    }

    /// Returns a reference to the inner `Path`.
    pub fn as_path(&self) -> &std::path::Path {
        &self.0
    }

    /// Consumes self and returns the inner `PathBuf`.
    pub fn into_path_buf(self) -> PathBuf {
        self.0
    }

    /// Returns whether the path is absolute.
    pub fn is_absolute(&self) -> bool {
        self.0.is_absolute()
    }

    /// Returns the path as a string slice if it's valid UTF-8.
    pub fn to_str(&self) -> Option<&str> {
        self.0.to_str()
    }

    /// Joins this path with another path component.
    pub fn join(&self, path: impl AsRef<std::path::Path>) -> PathBuf {
        self.0.join(path)
    }
}

impl MountSpec {
    /// Creates a new `MountSpec`.
    pub fn new(host: HostPathBuf, guest: GuestPathBuf, readonly: bool) -> Self {
        Self {
            host,
            guest,
            readonly,
        }
    }

    /// Creates a `MountSpec` with the same path on host and guest.
    pub fn with_same(path: &str) -> Self {
        Self {
            host: HostPathBuf(PathBuf::from(path)),
            guest: GuestPathBuf::from(path),
            readonly: false,
        }
    }

    /// Creates a `MountSpec` with distinct host and guest paths.
    pub fn with_distinct(host: impl Into<HostPathBuf>, guest: impl Into<GuestPathBuf>) -> Self {
        Self {
            host: host.into(),
            guest: guest.into(),
            readonly: false,
        }
    }

    /// Returns the host path.
    pub fn get_host(&self) -> &HostPathBuf {
        &self.host
    }

    /// Returns the guest path.
    pub fn get_guest(&self) -> &GuestPathBuf {
        &self.guest
    }
}

//--------------------------------------------------------------------------------------------------
// Trait Implementations
//--------------------------------------------------------------------------------------------------

impl fmt::Display for HostPathBuf {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0.display())
    }
}

impl From<PathBuf> for HostPathBuf {
    fn from(path: PathBuf) -> Self {
        Self(path)
    }
}

impl From<&str> for HostPathBuf {
    fn from(s: &str) -> Self {
        Self(PathBuf::from(s))
    }
}

impl From<String> for HostPathBuf {
    fn from(s: String) -> Self {
        Self(PathBuf::from(s))
    }
}

impl AsRef<std::path::Path> for HostPathBuf {
    fn as_ref(&self) -> &std::path::Path {
        &self.0
    }
}

impl Serialize for HostPathBuf {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.0.to_string_lossy())
    }
}

impl<'de> Deserialize<'de> for HostPathBuf {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        if s.is_empty() {
            return Err(de::Error::custom("empty host path"));
        }
        Ok(Self(PathBuf::from(s)))
    }
}

impl fmt::Display for MountSpec {
    /// Formats the mount spec as "host:guest" for display and CLI arguments.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.host, self.guest)
    }
}

impl FromStr for MountSpec {
    type Err = MicrosandboxError;

    /// Parses a colon-delimited mount spec string.
    ///
    /// On Unix, supports:
    /// - `"host:guest"` - distinct host and guest paths
    /// - `"path"` - same path on host and guest
    ///
    /// On Windows, colon-delimited strings are rejected because they're ambiguous
    /// with drive letters. Use structured YAML syntax instead.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if s.is_empty() {
            return Err(MicrosandboxError::InvalidMountSpec(
                "empty mount spec".to_string(),
            ));
        }

        #[cfg(windows)]
        {
            // On Windows, drive letter paths (e.g., C:\foo) contain a colon that is NOT
            // a host:guest delimiter. Detect the drive letter prefix and look for the
            // delimiter colon after it.
            let delimiter_pos = if s.len() >= 3
                && s.as_bytes()[0].is_ascii_alphabetic()
                && s.as_bytes()[1] == b':'
                && (s.as_bytes()[2] == b'\\' || s.as_bytes()[2] == b'/')
            {
                // Drive letter detected (e.g., "C:\..."), skip drive colon.
                s[2..].find(':').map(|p| p + 2)
            } else {
                s.find(':')
            };

            if let Some(pos) = delimiter_pos {
                let host = &s[..pos];
                let guest = &s[pos + 1..];
                if host.is_empty() || guest.is_empty() {
                    return Err(MicrosandboxError::InvalidMountSpec(format!(
                        "invalid mount spec: \"{}\"",
                        s
                    )));
                }
                return Ok(MountSpec {
                    host: HostPathBuf(PathBuf::from(host)),
                    guest: GuestPathBuf::from(guest),
                    readonly: false,
                });
            }

            return Ok(MountSpec {
                host: HostPathBuf(PathBuf::from(s)),
                guest: GuestPathBuf::from(s),
                readonly: false,
            });
        }

        #[cfg(not(windows))]
        {
            if let Some((host, guest)) = s.split_once(':') {
                if host.is_empty() || guest.is_empty() {
                    return Err(MicrosandboxError::InvalidMountSpec(format!(
                        "invalid mount spec: \"{}\"",
                        s
                    )));
                }

                if host == guest {
                    return Ok(MountSpec {
                        host: HostPathBuf(PathBuf::from(host)),
                        guest: GuestPathBuf::from(host),
                        readonly: false,
                    });
                }

                return Ok(MountSpec {
                    host: HostPathBuf(PathBuf::from(host)),
                    guest: GuestPathBuf::from(guest),
                    readonly: false,
                });
            }

            Ok(MountSpec {
                host: HostPathBuf(PathBuf::from(s)),
                guest: GuestPathBuf::from(s),
                readonly: false,
            })
        }
    }
}

impl Serialize for MountSpec {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let field_count = if self.readonly { 3 } else { 2 };
        let mut state = serializer.serialize_struct("MountSpec", field_count)?;
        state.serialize_field("host", &self.host)?;
        state.serialize_field("guest", self.guest.as_str())?;
        if self.readonly {
            state.serialize_field("readonly", &self.readonly)?;
        }
        state.end()
    }
}

impl<'de> Deserialize<'de> for MountSpec {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        /// Helper enum to support both structured and legacy string forms.
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum MountSpecRepr {
            Structured {
                host: HostPathBuf,
                guest: String,
                #[serde(default)]
                readonly: bool,
            },
            LegacyString(String),
        }

        match MountSpecRepr::deserialize(deserializer)? {
            MountSpecRepr::Structured {
                host,
                guest,
                readonly,
            } => {
                if guest.is_empty() {
                    return Err(de::Error::custom("empty guest path"));
                }
                Ok(MountSpec {
                    host,
                    guest: GuestPathBuf::from(guest),
                    readonly,
                })
            }
            MountSpecRepr::LegacyString(s) => MountSpec::from_str(&s).map_err(de::Error::custom),
        }
    }
}

//--------------------------------------------------------------------------------------------------
// Tests
//--------------------------------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // -- HostPathBuf tests --

    #[test]
    fn test_host_path_buf_from_pathbuf() {
        let host = HostPathBuf::from(PathBuf::from("/home/user/project"));
        assert_eq!(host.to_string(), "/home/user/project");
        #[cfg(unix)]
        assert!(host.is_absolute());
    }

    #[test]
    fn test_host_path_buf_from_str() {
        let host = HostPathBuf::from("./relative/path");
        assert!(!host.is_absolute());
    }

    #[test]
    fn test_host_path_buf_new_rejects_empty() {
        assert!(HostPathBuf::new(PathBuf::from("")).is_err());
    }

    #[test]
    fn test_host_path_buf_new_accepts_nonempty() {
        assert!(HostPathBuf::new(PathBuf::from("/data")).is_ok());
    }

    #[test]
    fn test_host_path_buf_serde_roundtrip() {
        let host = HostPathBuf::from("/home/user/project");
        let json = serde_json::to_string(&host).unwrap();
        assert_eq!(json, "\"/home/user/project\"");
        let deserialized: HostPathBuf = serde_json::from_str(&json).unwrap();
        assert_eq!(host, deserialized);
    }

    #[test]
    fn test_host_path_buf_deserialize_rejects_empty() {
        let result: Result<HostPathBuf, _> = serde_json::from_str("\"\"");
        assert!(result.is_err());
    }

    #[test]
    fn test_host_path_buf_display() {
        let host = HostPathBuf::from("/home/user");
        assert_eq!(format!("{}", host), "/home/user");
    }

    // -- GuestPathBuf tests --

    #[test]
    fn test_guest_path_buf_unix_semantics() {
        let guest: GuestPathBuf = GuestPathBuf::from("/workspace");
        assert_eq!(guest.as_str(), "/workspace");
    }

    // -- MountSpec tests --

    #[cfg(unix)]
    #[test]
    fn test_mount_spec_from_str_distinct() {
        let mount: MountSpec = "/host/data:/container/data".parse().unwrap();
        assert_eq!(mount.host.to_string(), "/host/data");
        assert_eq!(mount.guest.as_str(), "/container/data");
        assert!(!mount.readonly);
    }

    #[test]
    fn test_mount_spec_from_str_same() {
        let mount: MountSpec = "/data".parse().unwrap();
        assert_eq!(mount.host.to_string(), "/data");
        assert_eq!(mount.guest.as_str(), "/data");
    }

    #[cfg(unix)]
    #[test]
    fn test_mount_spec_from_str_same_explicit() {
        let mount: MountSpec = "/data:/data".parse().unwrap();
        assert_eq!(mount.host.to_string(), "/data");
        assert_eq!(mount.guest.as_str(), "/data");
    }

    #[cfg(windows)]
    #[test]
    fn test_mount_spec_from_str_windows_drive_letter() {
        let mount: MountSpec = r"C:\temp\share:/mnt/share".parse().unwrap();
        assert_eq!(mount.host.to_string(), r"C:\temp\share");
        assert_eq!(mount.guest.as_str(), "/mnt/share");
        assert!(!mount.readonly);
    }

    #[cfg(windows)]
    #[test]
    fn test_mount_spec_from_str_windows_drive_only() {
        // No delimiter colon — just a Windows path
        let mount: MountSpec = r"C:\data".parse().unwrap();
        assert_eq!(mount.host.to_string(), r"C:\data");
    }

    #[cfg(windows)]
    #[test]
    fn test_mount_spec_from_str_windows_forward_slash() {
        let mount: MountSpec = "C:/temp/share:/mnt/share".parse().unwrap();
        assert_eq!(mount.host.to_string(), "C:/temp/share");
        assert_eq!(mount.guest.as_str(), "/mnt/share");
    }

    #[test]
    fn test_mount_spec_from_str_empty_rejected() {
        assert!("".parse::<MountSpec>().is_err());
    }

    #[test]
    fn test_mount_spec_from_str_invalid_colon() {
        assert!(":".parse::<MountSpec>().is_err());
        assert!(":/data".parse::<MountSpec>().is_err());
        assert!("/data:".parse::<MountSpec>().is_err());
    }

    #[test]
    fn test_mount_spec_display() {
        let mount = MountSpec::with_distinct(
            HostPathBuf::from("/host/data"),
            GuestPathBuf::from("/container/data"),
        );
        assert_eq!(mount.to_string(), "/host/data:/container/data");
    }

    #[test]
    fn test_mount_spec_deserialize_structured() {
        let yaml = r#"
            host: "/home/user/project"
            guest: "/workspace"
        "#;
        let mount: MountSpec = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(mount.host.to_string(), "/home/user/project");
        assert_eq!(mount.guest.as_str(), "/workspace");
        assert!(!mount.readonly);
    }

    #[test]
    fn test_mount_spec_deserialize_structured_readonly() {
        let yaml = r#"
            host: "/data"
            guest: "/mnt/data"
            readonly: true
        "#;
        let mount: MountSpec = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(mount.host.to_string(), "/data");
        assert_eq!(mount.guest.as_str(), "/mnt/data");
        assert!(mount.readonly);
    }

    #[cfg(unix)]
    #[test]
    fn test_mount_spec_deserialize_legacy_string() {
        let yaml = r#""/host/path:/guest/path""#;
        let mount: MountSpec = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(mount.host.to_string(), "/host/path");
        assert_eq!(mount.guest.as_str(), "/guest/path");
    }

    #[test]
    fn test_mount_spec_deserialize_legacy_same() {
        let yaml = r#""/data""#;
        let mount: MountSpec = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(mount.host.to_string(), "/data");
        assert_eq!(mount.guest.as_str(), "/data");
    }

    #[test]
    fn test_mount_spec_serialize_structured() {
        let mount = MountSpec::with_distinct(
            HostPathBuf::from("/home/user"),
            GuestPathBuf::from("/workspace"),
        );
        let yaml = serde_yaml::to_string(&mount).unwrap();
        assert!(yaml.contains("host:"));
        assert!(yaml.contains("guest:"));
    }

    #[test]
    fn test_mount_spec_serialize_readonly() {
        let mount = MountSpec::new(
            HostPathBuf::from("/data"),
            GuestPathBuf::from("/mnt/data"),
            true,
        );
        let yaml = serde_yaml::to_string(&mount).unwrap();
        assert!(yaml.contains("readonly:"));
    }

    #[test]
    fn test_mount_spec_serialize_omits_readonly_false() {
        let mount =
            MountSpec::with_distinct(HostPathBuf::from("/data"), GuestPathBuf::from("/mnt/data"));
        let yaml = serde_yaml::to_string(&mount).unwrap();
        assert!(!yaml.contains("readonly"));
    }

    #[test]
    fn test_mount_spec_with_same() {
        let mount = MountSpec::with_same("/data");
        assert_eq!(mount.host.to_string(), "/data");
        assert_eq!(mount.guest.as_str(), "/data");
        assert!(!mount.readonly);
    }

    #[test]
    fn test_mount_spec_getters() {
        let mount =
            MountSpec::with_distinct(HostPathBuf::from("/host"), GuestPathBuf::from("/guest"));
        assert_eq!(mount.get_host().to_string(), "/host");
        assert_eq!(mount.get_guest().as_str(), "/guest");
    }

    #[cfg(unix)]
    #[test]
    fn test_mount_spec_in_list_mixed_formats() {
        let yaml = r#"
            - host: "/structured/path"
              guest: "/workspace"
            - "/legacy/path:/app"
        "#;
        let mounts: Vec<MountSpec> = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(mounts.len(), 2);
        assert_eq!(mounts[0].host.to_string(), "/structured/path");
        assert_eq!(mounts[0].guest.as_str(), "/workspace");
        assert_eq!(mounts[1].host.to_string(), "/legacy/path");
        assert_eq!(mounts[1].guest.as_str(), "/app");
    }
}
