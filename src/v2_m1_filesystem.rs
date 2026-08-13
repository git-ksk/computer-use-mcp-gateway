//! V2-M1 bounded Agent-native filesystem observation surface.
//!
//! This is intentionally separate from `ExecuteProcess`. It provides narrow,
//! read-only operations whose paths are canonicalized against operator-approved
//! roots. It is not a sandbox for arbitrary processes; `ExecuteProcess` remains
//! a Dangerous capability because program arguments can address the wider host.

use crate::v2_m0::{DeviceResult, DirectoryEntry, DirectoryEntryKind};
use crate::v2_observability::SafeErrorCode;
use std::fmt;
use std::fs::{self, File};
use std::io::Read;
use std::path::{Path, PathBuf};

// The current gRPC migration carrier serializes signed application messages as
// JSON. `Vec<u8>` becomes a JSON integer array, so keep the filesystem payload
// comfortably below the 64 KiB signed-message bound even for worst-case byte
// values and envelope/signature overhead.
pub const DEFAULT_MAX_FILE_BYTES: usize = 8 * 1024;
pub const DEFAULT_MAX_DIRECTORY_ENTRIES: usize = 256;

#[derive(Debug, Clone)]
pub struct FilesystemPolicy {
    allowed_roots: Vec<PathBuf>,
    max_file_bytes: usize,
    max_directory_entries: usize,
}

impl FilesystemPolicy {
    pub fn new(allowed_roots: Vec<PathBuf>) -> Result<Self, FilesystemError> {
        if allowed_roots.is_empty() {
            return Err(FilesystemError::NoAllowedRoots);
        }
        let mut canonical = Vec::with_capacity(allowed_roots.len());
        for root in allowed_roots {
            let root = fs::canonicalize(root).map_err(FilesystemError::Io)?;
            if !root.is_dir() {
                return Err(FilesystemError::RootNotDirectory);
            }
            canonical.push(root);
        }
        Ok(Self {
            allowed_roots: canonical,
            max_file_bytes: DEFAULT_MAX_FILE_BYTES,
            max_directory_entries: DEFAULT_MAX_DIRECTORY_ENTRIES,
        })
    }

    pub fn with_limits(
        mut self,
        max_file_bytes: usize,
        max_directory_entries: usize,
    ) -> Result<Self, FilesystemError> {
        if max_file_bytes == 0 || max_directory_entries == 0 {
            return Err(FilesystemError::InvalidLimit);
        }
        self.max_file_bytes = max_file_bytes;
        self.max_directory_entries = max_directory_entries;
        Ok(self)
    }
}

#[derive(Debug, Clone)]
pub struct FilesystemExecutor {
    policy: FilesystemPolicy,
}

impl FilesystemExecutor {
    pub fn new(policy: FilesystemPolicy) -> Self {
        Self { policy }
    }

    pub fn read_file(&self, path: &str) -> Result<DeviceResult, FilesystemError> {
        let path = self.resolve_existing(path)?;
        let metadata = fs::metadata(&path).map_err(FilesystemError::Io)?;
        if !metadata.is_file() {
            return Err(FilesystemError::NotFile);
        }
        let mut file = File::open(&path).map_err(FilesystemError::Io)?;
        let mut bytes = Vec::with_capacity(self.policy.max_file_bytes.min(4096));
        file.by_ref()
            .take(u64::try_from(self.policy.max_file_bytes).unwrap_or(u64::MAX) + 1)
            .read_to_end(&mut bytes)
            .map_err(FilesystemError::Io)?;
        let truncated = bytes.len() > self.policy.max_file_bytes;
        if truncated {
            bytes.truncate(self.policy.max_file_bytes);
        }
        Ok(DeviceResult::FileContents { bytes, truncated })
    }

    pub fn list_directory(&self, path: &str) -> Result<DeviceResult, FilesystemError> {
        let path = self.resolve_existing(path)?;
        if !path.is_dir() {
            return Err(FilesystemError::NotDirectory);
        }
        let mut entries = Vec::new();
        let mut truncated = false;
        for entry in fs::read_dir(&path).map_err(FilesystemError::Io)? {
            let entry = entry.map_err(FilesystemError::Io)?;
            if entries.len() >= self.policy.max_directory_entries {
                truncated = true;
                break;
            }
            let metadata = fs::symlink_metadata(entry.path()).map_err(FilesystemError::Io)?;
            let file_type = metadata.file_type();
            let kind = if file_type.is_symlink() {
                DirectoryEntryKind::Symlink
            } else if file_type.is_file() {
                DirectoryEntryKind::File
            } else if file_type.is_dir() {
                DirectoryEntryKind::Directory
            } else {
                DirectoryEntryKind::Other
            };
            let name = entry
                .file_name()
                .into_string()
                .map_err(|_| FilesystemError::NonUtf8Name)?;
            entries.push(DirectoryEntry { name, kind });
        }
        entries.sort_by(|left, right| left.name.cmp(&right.name));
        Ok(DeviceResult::DirectoryEntries { entries, truncated })
    }

    fn resolve_existing(&self, path: &str) -> Result<PathBuf, FilesystemError> {
        if path.trim().is_empty() {
            return Err(FilesystemError::InvalidPath);
        }
        let resolved = fs::canonicalize(Path::new(path)).map_err(FilesystemError::Io)?;
        if !self
            .policy
            .allowed_roots
            .iter()
            .any(|root| resolved.starts_with(root))
        {
            return Err(FilesystemError::PathDenied);
        }
        Ok(resolved)
    }
}

pub enum FilesystemError {
    NoAllowedRoots,
    RootNotDirectory,
    InvalidLimit,
    InvalidPath,
    PathDenied,
    NotFile,
    NotDirectory,
    NonUtf8Name,
    Io(std::io::Error),
}

impl SafeErrorCode for FilesystemError {
    fn safe_error_code(&self) -> &'static str {
        match self {
            Self::NoAllowedRoots => "filesystem_no_allowed_roots",
            Self::RootNotDirectory => "filesystem_root_not_directory",
            Self::InvalidLimit => "filesystem_invalid_limit",
            Self::InvalidPath => "filesystem_invalid_path",
            Self::PathDenied => "filesystem_path_denied",
            Self::NotFile => "filesystem_not_file",
            Self::NotDirectory => "filesystem_not_directory",
            Self::NonUtf8Name => "filesystem_non_utf8_name",
            Self::Io(_) => "filesystem_io",
        }
    }
}

impl fmt::Debug for FilesystemError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.safe_error_code())
    }
}

impl fmt::Display for FilesystemError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.safe_error_code())
    }
}

impl std::error::Error for FilesystemError {}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_root(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "cumg-v2-fs-{name}-{}-{}",
            std::process::id(),
            rand::random::<u64>()
        ));
        fs::create_dir_all(&root).unwrap();
        root
    }

    #[test]
    fn read_is_bounded_and_directory_listing_is_entry_bounded() {
        let root = temp_root("bounds");
        fs::write(root.join("a.txt"), b"0123456789").unwrap();
        fs::write(root.join("b.txt"), b"b").unwrap();
        let executor = FilesystemExecutor::new(
            FilesystemPolicy::new(vec![root.clone()])
                .unwrap()
                .with_limits(4, 1)
                .unwrap(),
        );
        assert_eq!(
            executor
                .read_file(root.join("a.txt").to_str().unwrap())
                .unwrap(),
            DeviceResult::FileContents {
                bytes: b"0123".to_vec(),
                truncated: true,
            }
        );
        match executor.list_directory(root.to_str().unwrap()).unwrap() {
            DeviceResult::DirectoryEntries { entries, truncated } => {
                assert_eq!(entries.len(), 1);
                assert!(truncated);
            }
            other => panic!("unexpected result: {other:?}"),
        }
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn symlink_escape_is_denied_for_read_and_traversal() {
        let root = temp_root("root");
        let outside = temp_root("outside");
        fs::write(outside.join("secret.txt"), b"secret").unwrap();
        std::os::unix::fs::symlink(outside.join("secret.txt"), root.join("escape-file")).unwrap();
        std::os::unix::fs::symlink(&outside, root.join("escape-dir")).unwrap();
        let executor = FilesystemExecutor::new(FilesystemPolicy::new(vec![root.clone()]).unwrap());
        assert!(matches!(
            executor.read_file(root.join("escape-file").to_str().unwrap()),
            Err(FilesystemError::PathDenied)
        ));
        assert!(matches!(
            executor.list_directory(root.join("escape-dir").to_str().unwrap()),
            Err(FilesystemError::PathDenied)
        ));
        fs::remove_dir_all(root).unwrap();
        fs::remove_dir_all(outside).unwrap();
    }
}
