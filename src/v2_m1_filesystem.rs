//! V2 bounded Agent-native filesystem observation surface.
//!
//! Reads and directory enumeration are reopened relative to operator-approved
//! capability roots via v2_workspace_path. Ambient canonicalization is only a
//! root selector; it is not the authority proof used for the actual read.
//!
//! Ranged file reads are intentionally stateless. Each call observes the file
//! state independently and is not a snapshot continuation. Directory cursors
//! are also stateless and deterministic only while the directory contents are
//! stable between calls.

use crate::v2_m0::{DeviceResult, DirectoryEntry, DirectoryEntryKind};
use crate::v2_observability::SafeErrorCode;
use crate::v2_workspace_path::{WorkspacePathError, WorkspaceRoots};
use std::fmt;
use std::io::{Read, Seek, SeekFrom};
use std::path::PathBuf;

pub const DEFAULT_MAX_FILE_BYTES: usize = 8 * 1024;
pub const DEFAULT_MAX_DIRECTORY_ENTRIES: usize = 256;
pub const DEFAULT_MAX_DIRECTORY_SCAN_ENTRIES: usize = 4096;
pub const DEFAULT_MAX_DIRECTORY_RESULT_JSON_BYTES: usize = 24 * 1024;

#[derive(Clone)]
pub struct FilesystemPolicy {
    roots: WorkspaceRoots,
    max_file_bytes: usize,
    max_directory_entries: usize,
    max_directory_scan_entries: usize,
}

impl FilesystemPolicy {
    pub fn new(allowed_roots: Vec<PathBuf>) -> Result<Self, FilesystemError> {
        let roots = WorkspaceRoots::new(allowed_roots).map_err(map_workspace_error)?;
        Ok(Self {
            roots,
            max_file_bytes: DEFAULT_MAX_FILE_BYTES,
            max_directory_entries: DEFAULT_MAX_DIRECTORY_ENTRIES,
            max_directory_scan_entries: DEFAULT_MAX_DIRECTORY_SCAN_ENTRIES,
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

    pub fn with_directory_scan_limit(
        mut self,
        max_directory_scan_entries: usize,
    ) -> Result<Self, FilesystemError> {
        if max_directory_scan_entries == 0
            || max_directory_scan_entries < self.max_directory_entries
        {
            return Err(FilesystemError::InvalidLimit);
        }
        self.max_directory_scan_entries = max_directory_scan_entries;
        Ok(self)
    }
}

impl fmt::Debug for FilesystemPolicy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FilesystemPolicy")
            .field("roots", &self.roots)
            .field("max_file_bytes", &self.max_file_bytes)
            .field("max_directory_entries", &self.max_directory_entries)
            .field(
                "max_directory_scan_entries",
                &self.max_directory_scan_entries,
            )
            .finish()
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
        self.read_file_range(path, 0, None)
    }

    pub fn read_file_range(
        &self,
        path: &str,
        offset: u64,
        max_bytes: Option<u64>,
    ) -> Result<DeviceResult, FilesystemError> {
        let requested = max_bytes.unwrap_or(self.policy.max_file_bytes as u64);
        if requested == 0 || requested > self.policy.max_file_bytes as u64 {
            return Err(FilesystemError::RangeTooLarge);
        }
        offset
            .checked_add(requested)
            .ok_or(FilesystemError::RangeOverflow)?;

        let resolved = self
            .policy
            .roots
            .resolve_existing(path)
            .map_err(map_workspace_error)?;
        let mut file = resolved.open_file().map_err(map_workspace_error)?;
        let metadata = file.metadata().map_err(FilesystemError::Io)?;
        if !metadata.is_file() {
            return Err(FilesystemError::NotFile);
        }

        file.seek(SeekFrom::Start(offset))
            .map_err(FilesystemError::Io)?;
        let requested_usize =
            usize::try_from(requested).map_err(|_| FilesystemError::RangeTooLarge)?;
        let mut bytes = Vec::with_capacity(requested_usize.min(4096));
        file.by_ref()
            .take(requested.saturating_add(1))
            .read_to_end(&mut bytes)
            .map_err(FilesystemError::Io)?;

        let truncated = bytes.len() > requested_usize;
        if truncated {
            bytes.truncate(requested_usize);
        }
        let next_offset = if truncated {
            Some(
                offset
                    .checked_add(bytes.len() as u64)
                    .ok_or(FilesystemError::RangeOverflow)?,
            )
        } else {
            None
        };

        Ok(DeviceResult::FileContents {
            bytes,
            truncated,
            offset,
            next_offset,
        })
    }

    pub fn list_directory(&self, path: &str) -> Result<DeviceResult, FilesystemError> {
        self.list_directory_page(path, None)
    }

    pub fn list_directory_page(
        &self,
        path: &str,
        after: Option<&str>,
    ) -> Result<DeviceResult, FilesystemError> {
        let resolved = self
            .policy
            .roots
            .resolve_existing(path)
            .map_err(map_workspace_error)?;
        let dir = resolved.open_dir().map_err(map_workspace_error)?;
        let mut entries = Vec::new();

        for (index, entry) in dir.entries().map_err(FilesystemError::Io)?.enumerate() {
            if index >= self.policy.max_directory_scan_entries {
                return Err(FilesystemError::DirectoryScanLimitExceeded);
            }
            let entry = entry.map_err(FilesystemError::Io)?;
            let file_type = entry.file_type().map_err(FilesystemError::Io)?;
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
        let start = after
            .map(|cursor| entries.partition_point(|entry| entry.name.as_str() <= cursor))
            .unwrap_or(0);
        let remaining = entries.len().saturating_sub(start);
        let mut page = Vec::new();
        let mut estimated_json_bytes = 0usize;
        for entry in entries.into_iter().skip(start) {
            if page.len() >= self.policy.max_directory_entries {
                break;
            }
            // JSON escaping can expand an input byte by at most six bytes.
            // Keep a conservative fixed allowance for keys, kind and separators.
            let entry_bound = entry.name.len().saturating_mul(6).saturating_add(64);
            if entry_bound > DEFAULT_MAX_DIRECTORY_RESULT_JSON_BYTES {
                return Err(FilesystemError::DirectoryEntryTooLarge);
            }
            if estimated_json_bytes.saturating_add(entry_bound)
                > DEFAULT_MAX_DIRECTORY_RESULT_JSON_BYTES
            {
                break;
            }
            estimated_json_bytes = estimated_json_bytes.saturating_add(entry_bound);
            page.push(entry);
        }
        if remaining > 0 && page.is_empty() {
            return Err(FilesystemError::DirectoryEntryTooLarge);
        }
        let truncated = remaining > page.len();
        let next_cursor = if truncated {
            page.last().map(|entry| entry.name.clone())
        } else {
            None
        };

        Ok(DeviceResult::DirectoryEntries {
            entries: page,
            truncated,
            after: after.map(ToOwned::to_owned),
            next_cursor,
        })
    }
}

fn map_workspace_error(error: WorkspacePathError) -> FilesystemError {
    match error {
        WorkspacePathError::NoRoots => FilesystemError::NoAllowedRoots,
        WorkspacePathError::RootNotDirectory => FilesystemError::RootNotDirectory,
        WorkspacePathError::InvalidPath => FilesystemError::InvalidPath,
        WorkspacePathError::PathDenied => FilesystemError::PathDenied,
        WorkspacePathError::Io(error) => FilesystemError::Io(error),
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
    RangeTooLarge,
    RangeOverflow,
    DirectoryScanLimitExceeded,
    DirectoryEntryTooLarge,
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
            Self::RangeTooLarge => "filesystem_range_too_large",
            Self::RangeOverflow => "filesystem_range_overflow",
            Self::DirectoryScanLimitExceeded => "filesystem_directory_scan_limit_exceeded",
            Self::DirectoryEntryTooLarge => "filesystem_directory_entry_too_large",
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
    use std::fs;

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
    fn ranged_read_is_bounded_and_continuable() {
        let root = temp_root("range");
        fs::write(root.join("a.txt"), b"0123456789").unwrap();
        let executor = FilesystemExecutor::new(
            FilesystemPolicy::new(vec![root.clone()])
                .unwrap()
                .with_limits(4, 1)
                .unwrap(),
        );

        assert_eq!(
            executor
                .read_file_range(root.join("a.txt").to_str().unwrap(), 2, Some(4))
                .unwrap(),
            DeviceResult::FileContents {
                bytes: b"2345".to_vec(),
                truncated: true,
                offset: 2,
                next_offset: Some(6),
            }
        );
        assert_eq!(
            executor
                .read_file_range(root.join("a.txt").to_str().unwrap(), 10, Some(4))
                .unwrap(),
            DeviceResult::FileContents {
                bytes: Vec::new(),
                truncated: false,
                offset: 10,
                next_offset: None,
            }
        );

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn range_limit_and_overflow_fail_closed() {
        let root = temp_root("range-errors");
        fs::write(root.join("a.txt"), b"x").unwrap();
        let executor = FilesystemExecutor::new(
            FilesystemPolicy::new(vec![root.clone()])
                .unwrap()
                .with_limits(4, 1)
                .unwrap(),
        );

        assert!(matches!(
            executor.read_file_range(root.join("a.txt").to_str().unwrap(), 0, Some(5)),
            Err(FilesystemError::RangeTooLarge)
        ));
        assert!(matches!(
            executor.read_file_range(root.join("a.txt").to_str().unwrap(), u64::MAX, Some(1)),
            Err(FilesystemError::RangeOverflow)
        ));

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn directory_pages_are_sorted_and_deterministic() {
        let root = temp_root("directory-pages");
        for name in ["z.txt", "a.txt", "m.txt"] {
            fs::write(root.join(name), name.as_bytes()).unwrap();
        }
        let executor = FilesystemExecutor::new(
            FilesystemPolicy::new(vec![root.clone()])
                .unwrap()
                .with_limits(4, 2)
                .unwrap(),
        );

        let first = executor
            .list_directory_page(root.to_str().unwrap(), None)
            .unwrap();
        assert_eq!(
            first,
            DeviceResult::DirectoryEntries {
                entries: vec![
                    DirectoryEntry {
                        name: "a.txt".into(),
                        kind: DirectoryEntryKind::File,
                    },
                    DirectoryEntry {
                        name: "m.txt".into(),
                        kind: DirectoryEntryKind::File,
                    },
                ],
                truncated: true,
                after: None,
                next_cursor: Some("m.txt".into()),
            }
        );
        let second = executor
            .list_directory_page(root.to_str().unwrap(), Some("m.txt"))
            .unwrap();
        assert_eq!(
            second,
            DeviceResult::DirectoryEntries {
                entries: vec![DirectoryEntry {
                    name: "z.txt".into(),
                    kind: DirectoryEntryKind::File,
                }],
                truncated: false,
                after: Some("m.txt".into()),
                next_cursor: None,
            }
        );

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn directory_scan_budget_fails_closed() {
        let root = temp_root("scan-budget");
        for name in ["a", "b", "c"] {
            fs::write(root.join(name), b"x").unwrap();
        }
        let executor = FilesystemExecutor::new(
            FilesystemPolicy::new(vec![root.clone()])
                .unwrap()
                .with_limits(4, 1)
                .unwrap()
                .with_directory_scan_limit(2)
                .unwrap(),
        );

        assert!(matches!(
            executor.list_directory(root.to_str().unwrap()),
            Err(FilesystemError::DirectoryScanLimitExceeded)
        ));

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
