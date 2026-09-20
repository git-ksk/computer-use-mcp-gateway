//! Bounded Agent-native workspace mutation without shell/process authority.
//!
//! Writable roots are operator-configured separately from observation/cwd roots.
//! Existing-file replacement is compare-and-swap by expected SHA-256; creation
//! requires an explicit expected-absent contract. The target is never written
//! in place: bytes are staged to a fresh same-parent file, flushed, then
//! published atomically. Default diagnostics expose stable error codes only.

use crate::v2_m0::{DeviceResult, WorkspaceWritePrecondition};
use crate::v2_observability::SafeErrorCode;
use crate::v2_workspace_path::{WorkspacePathError, WorkspaceRoots};
use cap_std::fs::{Dir, OpenOptions};
use rand::{RngCore, rngs::OsRng};
use ring::digest::{Context, SHA256, digest};
use std::fmt;
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

pub const DEFAULT_MAX_WORKSPACE_WRITE_BYTES: usize = 32 * 1024;
pub const DEFAULT_MAX_WORKSPACE_PATH_BYTES: usize = 4 * 1024;
pub const DEFAULT_MAX_WORKSPACE_CAS_READ_BYTES: u64 = 4 * 1024 * 1024;

#[derive(Clone)]
pub struct WorkspaceMutationPolicy {
    roots: WorkspaceRoots,
    denied: Vec<PathBuf>,
    max_write_bytes: usize,
    max_cas_read_bytes: u64,
}

impl WorkspaceMutationPolicy {
    pub fn new(
        allowed_roots: Vec<PathBuf>,
        denied_subpaths: Vec<PathBuf>,
    ) -> Result<Self, WorkspaceMutationError> {
        let roots = WorkspaceRoots::new(allowed_roots).map_err(map_workspace_error)?;
        let mut denied = Vec::with_capacity(denied_subpaths.len());
        for path in denied_subpaths {
            let canonical = fs::canonicalize(path).map_err(WorkspaceMutationError::Io)?;
            if !roots.contains_canonical_path(&canonical) {
                return Err(WorkspaceMutationError::DeniedPathOutsideRoot);
            }
            denied.push(canonical);
        }
        denied.sort();
        denied.dedup();
        Ok(Self {
            roots,
            denied,
            max_write_bytes: DEFAULT_MAX_WORKSPACE_WRITE_BYTES,
            max_cas_read_bytes: DEFAULT_MAX_WORKSPACE_CAS_READ_BYTES,
        })
    }

    pub fn with_limits(
        mut self,
        max_write_bytes: usize,
        max_cas_read_bytes: u64,
    ) -> Result<Self, WorkspaceMutationError> {
        if max_write_bytes == 0 || max_cas_read_bytes == 0 {
            return Err(WorkspaceMutationError::InvalidLimit);
        }
        self.max_write_bytes = max_write_bytes;
        self.max_cas_read_bytes = max_cas_read_bytes;
        Ok(self)
    }

    fn denied(&self, target: &Path) -> Result<bool, WorkspaceMutationError> {
        if self.denied.iter().any(|denied| target.starts_with(denied)) {
            return Ok(true);
        }
        match fs::canonicalize(target) {
            Ok(canonical) => Ok(self
                .denied
                .iter()
                .any(|denied| canonical.starts_with(denied))),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(error) => Err(WorkspaceMutationError::Io(error)),
        }
    }
}

impl fmt::Debug for WorkspaceMutationPolicy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("WorkspaceMutationPolicy")
            .field("roots", &self.roots)
            .field("denied_count", &self.denied.len())
            .field("max_write_bytes", &self.max_write_bytes)
            .field("max_cas_read_bytes", &self.max_cas_read_bytes)
            .finish()
    }
}

#[derive(Debug, Clone)]
pub struct WorkspaceMutationExecutor {
    policy: WorkspaceMutationPolicy,
}

impl WorkspaceMutationExecutor {
    pub fn new(policy: WorkspaceMutationPolicy) -> Self {
        Self { policy }
    }

    pub fn write_file(
        &self,
        path: &str,
        bytes: &[u8],
        precondition: &WorkspaceWritePrecondition,
    ) -> Result<DeviceResult, WorkspaceMutationError> {
        if bytes.len() > self.policy.max_write_bytes {
            return Err(WorkspaceMutationError::PayloadTooLarge);
        }
        if path.is_empty() || path.len() > DEFAULT_MAX_WORKSPACE_PATH_BYTES {
            return Err(WorkspaceMutationError::PathTooLarge);
        }
        validate_precondition(precondition)?;

        let resolved = self
            .policy
            .roots
            .resolve_parent_for_mutation(path)
            .map_err(map_workspace_error)?;
        let target = resolved.canonical_target();
        if self.policy.denied(&target)? {
            return Err(WorkspaceMutationError::PathDenied);
        }

        let parent = resolved.dir();
        let name = resolved.file_name();
        let initial = inspect_target(parent, name, self.policy.max_cas_read_bytes)?;
        let created = match (precondition, &initial) {
            (WorkspaceWritePrecondition::ExpectedAbsent, TargetState::Absent) => true,
            (WorkspaceWritePrecondition::ExpectedAbsent, TargetState::Existing(_)) => {
                return Err(WorkspaceMutationError::PreconditionFailed);
            }
            (
                WorkspaceWritePrecondition::ExpectedSha256 { sha256 },
                TargetState::Existing(existing),
            ) if &existing.sha256 == sha256 => false,
            (WorkspaceWritePrecondition::ExpectedSha256 { .. }, _) => {
                return Err(WorkspaceMutationError::PreconditionFailed);
            }
        };

        let (temp_name, mut temp) = create_temp(parent)?;
        let publish_result = (|| {
            if let TargetState::Existing(existing) = &initial {
                temp.set_permissions(existing.permissions.clone())
                    .map_err(WorkspaceMutationError::Io)?;
            }
            temp.write_all(bytes).map_err(WorkspaceMutationError::Io)?;
            temp.sync_all().map_err(WorkspaceMutationError::Io)?;

            // Re-prove that the requested canonical parent still names the exact
            // open directory capability selected before staging. This closes the
            // canonicalize-to-open replacement gap and keeps deny-path policy bound
            // to the directory that will actually receive publication.
            resolved.reprove_parent().map_err(map_workspace_error)?;
            if self.policy.denied(&target)? {
                return Err(WorkspaceMutationError::PathDenied);
            }

            match (precondition, &initial) {
                (WorkspaceWritePrecondition::ExpectedAbsent, TargetState::Absent) => {
                    match parent.hard_link(&temp_name, parent, name) {
                        Ok(()) => {}
                        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                            return Err(WorkspaceMutationError::PreconditionFailed);
                        }
                        Err(error) => {
                            return Err(WorkspaceMutationError::OutcomeUnproven(error));
                        }
                    }
                    parent
                        .remove_file(&temp_name)
                        .map_err(WorkspaceMutationError::OutcomeUnproven)?;
                }
                (
                    WorkspaceWritePrecondition::ExpectedSha256 { sha256 },
                    TargetState::Existing(existing),
                ) => {
                    let current = inspect_target(parent, name, self.policy.max_cas_read_bytes)?;
                    let TargetState::Existing(current) = current else {
                        return Err(WorkspaceMutationError::PreconditionFailed);
                    };
                    if current.identity != existing.identity || current.sha256 != *sha256 {
                        return Err(WorkspaceMutationError::PreconditionFailed);
                    }
                    parent
                        .rename(&temp_name, parent, name)
                        .map_err(WorkspaceMutationError::OutcomeUnproven)?;
                }
                _ => return Err(WorkspaceMutationError::PreconditionFailed),
            }

            sync_parent(parent).map_err(WorkspaceMutationError::OutcomeUnproven)?;
            Ok(())
        })();

        if let Err(error) = publish_result {
            if let Err(cleanup_error) = remove_temp_if_present(parent, &temp_name) {
                return Err(WorkspaceMutationError::OutcomeUnproven(cleanup_error));
            }
            return Err(error);
        }

        let content_sha256 = sha256_hex(bytes);
        Ok(DeviceResult::WorkspaceFileWritten {
            bytes_written: bytes.len() as u64,
            content_sha256,
            created,
        })
    }
}

#[derive(Debug)]
enum TargetState {
    Absent,
    Existing(ExistingTarget),
}

#[derive(Debug)]
struct ExistingTarget {
    identity: FileIdentity,
    sha256: String,
    permissions: cap_std::fs::Permissions,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum FileIdentity {
    #[cfg(unix)]
    Unix { dev: u64, ino: u64 },
    #[cfg(windows)]
    Windows { volume_serial: u32, file_index: u64 },
}

fn inspect_target(
    parent: &Dir,
    name: &std::ffi::OsStr,
    max_cas_read_bytes: u64,
) -> Result<TargetState, WorkspaceMutationError> {
    let link_metadata = match parent.symlink_metadata(name) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(TargetState::Absent);
        }
        Err(error) => return Err(WorkspaceMutationError::Io(error)),
    };
    if link_metadata.is_symlink() || !link_metadata.is_file() {
        return Err(WorkspaceMutationError::PathDenied);
    }
    if link_metadata.len() > max_cas_read_bytes {
        return Err(WorkspaceMutationError::ExistingFileTooLarge);
    }

    let mut file = parent.open(name).map_err(WorkspaceMutationError::Io)?;
    let opened_metadata = file.metadata().map_err(WorkspaceMutationError::Io)?;
    let std_file = file
        .try_clone()
        .map_err(WorkspaceMutationError::Io)?
        .into_std();
    let std_metadata = std_file.metadata().map_err(WorkspaceMutationError::Io)?;
    if !opened_metadata.is_file() || !std_metadata.is_file() {
        return Err(WorkspaceMutationError::PathDenied);
    }
    if opened_metadata.len() > max_cas_read_bytes {
        return Err(WorkspaceMutationError::ExistingFileTooLarge);
    }
    if hard_link_count(&std_file, &std_metadata)? > 1 {
        return Err(WorkspaceMutationError::HardLinkDenied);
    }
    let identity = file_identity(&std_file, &std_metadata)?;
    let mut context = Context::new(&SHA256);
    let mut remaining = max_cas_read_bytes.saturating_add(1);
    let mut buffer = [0_u8; 8192];
    loop {
        if remaining == 0 {
            return Err(WorkspaceMutationError::ExistingFileTooLarge);
        }
        let take = usize::try_from(remaining.min(buffer.len() as u64))
            .map_err(|_| WorkspaceMutationError::ExistingFileTooLarge)?;
        let read = file
            .read(&mut buffer[..take])
            .map_err(WorkspaceMutationError::Io)?;
        if read == 0 {
            break;
        }
        context.update(&buffer[..read]);
        remaining = remaining.saturating_sub(read as u64);
    }

    // Re-prove that the pathname still resolves to the exact regular inode/handle
    // we hashed. A symlink/reparse or replacement race fails closed.
    let post = parent
        .symlink_metadata(name)
        .map_err(WorkspaceMutationError::Io)?;
    if post.is_symlink() || !post.is_file() {
        return Err(WorkspaceMutationError::PreconditionFailed);
    }
    let current = parent.open(name).map_err(WorkspaceMutationError::Io)?;
    let current_std_file = current.into_std();
    let current_std = current_std_file
        .metadata()
        .map_err(WorkspaceMutationError::Io)?;
    if file_identity(&current_std_file, &current_std)? != identity
        || hard_link_count(&current_std_file, &current_std)? > 1
    {
        return Err(WorkspaceMutationError::PreconditionFailed);
    }

    Ok(TargetState::Existing(ExistingTarget {
        identity,
        sha256: hex(context.finish().as_ref()),
        permissions: opened_metadata.permissions(),
    }))
}

fn create_temp(parent: &Dir) -> Result<(PathBuf, cap_std::fs::File), WorkspaceMutationError> {
    for _ in 0..16 {
        let mut random = [0_u8; 16];
        OsRng.fill_bytes(&mut random);
        let name = PathBuf::from(format!(".cumg-write-{}.tmp", hex(&random)));
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        match parent.open_with(&name, &options) {
            Ok(file) => return Ok((name, file)),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(WorkspaceMutationError::Io(error)),
        }
    }
    Err(WorkspaceMutationError::TempNameExhausted)
}

fn remove_temp_if_present(parent: &Dir, name: &Path) -> Result<(), std::io::Error> {
    match parent.remove_file(name) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

fn sync_parent(parent: &Dir) -> Result<(), std::io::Error> {
    parent.try_clone()?.into_std_file().sync_all()
}

fn validate_precondition(
    precondition: &WorkspaceWritePrecondition,
) -> Result<(), WorkspaceMutationError> {
    if let WorkspaceWritePrecondition::ExpectedSha256 { sha256 } = precondition {
        if sha256.len() != 64
            || !sha256
                .as_bytes()
                .iter()
                .all(|byte| byte.is_ascii_digit() || matches!(*byte, b'a'..=b'f'))
        {
            return Err(WorkspaceMutationError::InvalidPrecondition);
        }
    }
    Ok(())
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    hex(digest(&SHA256, bytes).as_ref())
}

fn hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(HEX[(byte >> 4) as usize] as char);
        output.push(HEX[(byte & 0x0f) as usize] as char);
    }
    output
}

#[cfg(unix)]
fn file_identity(
    _file: &std::fs::File,
    metadata: &std::fs::Metadata,
) -> Result<FileIdentity, WorkspaceMutationError> {
    use std::os::unix::fs::MetadataExt as _;
    Ok(FileIdentity::Unix {
        dev: metadata.dev(),
        ino: metadata.ino(),
    })
}

#[cfg(unix)]
fn hard_link_count(
    _file: &std::fs::File,
    metadata: &std::fs::Metadata,
) -> Result<u64, WorkspaceMutationError> {
    use std::os::unix::fs::MetadataExt as _;
    Ok(metadata.nlink())
}

#[cfg(windows)]
fn windows_file_information(
    file: &std::fs::File,
) -> Result<
    windows_sys::Win32::Storage::FileSystem::BY_HANDLE_FILE_INFORMATION,
    WorkspaceMutationError,
> {
    use std::os::windows::io::AsRawHandle as _;
    use windows_sys::Win32::Storage::FileSystem::{
        BY_HANDLE_FILE_INFORMATION, GetFileInformationByHandle,
    };

    let mut info: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
    let ok = unsafe { GetFileInformationByHandle(file.as_raw_handle().cast(), &mut info) };
    if ok == 0 {
        return Err(WorkspaceMutationError::Io(std::io::Error::last_os_error()));
    }
    Ok(info)
}

#[cfg(windows)]
fn file_identity(
    file: &std::fs::File,
    _metadata: &std::fs::Metadata,
) -> Result<FileIdentity, WorkspaceMutationError> {
    let info = windows_file_information(file)?;
    let file_index = (u64::from(info.nFileIndexHigh) << 32) | u64::from(info.nFileIndexLow);
    Ok(FileIdentity::Windows {
        volume_serial: info.dwVolumeSerialNumber,
        file_index,
    })
}

#[cfg(windows)]
fn hard_link_count(
    file: &std::fs::File,
    _metadata: &std::fs::Metadata,
) -> Result<u64, WorkspaceMutationError> {
    let links = windows_file_information(file)?.nNumberOfLinks;
    if links == 0 {
        return Err(WorkspaceMutationError::HardLinkDenied);
    }
    Ok(u64::from(links))
}

fn map_workspace_error(error: WorkspacePathError) -> WorkspaceMutationError {
    match error {
        WorkspacePathError::NoRoots => WorkspaceMutationError::NoAllowedRoots,
        WorkspacePathError::RootNotDirectory => WorkspaceMutationError::RootNotDirectory,
        WorkspacePathError::InvalidPath => WorkspaceMutationError::InvalidPath,
        WorkspacePathError::PathDenied => WorkspaceMutationError::PathDenied,
        WorkspacePathError::Io(error) => WorkspaceMutationError::Io(error),
    }
}

pub enum WorkspaceMutationError {
    NoAllowedRoots,
    RootNotDirectory,
    DeniedPathOutsideRoot,
    InvalidLimit,
    InvalidPath,
    PathTooLarge,
    PathDenied,
    InvalidPrecondition,
    InvalidPayload,
    PreconditionFailed,
    PayloadTooLarge,
    ExistingFileTooLarge,
    HardLinkDenied,
    TempNameExhausted,
    Io(std::io::Error),
    /// Publication may already have happened. This must become Indeterminate,
    /// never a terminal error/retry.
    OutcomeUnproven(std::io::Error),
}

impl WorkspaceMutationError {
    pub fn outcome_unproven(&self) -> bool {
        matches!(self, Self::OutcomeUnproven(_))
    }
}

impl fmt::Debug for WorkspaceMutationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.safe_error_code())
    }
}

impl fmt::Display for WorkspaceMutationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self, f)
    }
}

impl std::error::Error for WorkspaceMutationError {}

impl SafeErrorCode for WorkspaceMutationError {
    fn safe_error_code(&self) -> &'static str {
        match self {
            Self::NoAllowedRoots => "workspace_mutation_no_roots",
            Self::RootNotDirectory => "workspace_mutation_root_not_directory",
            Self::DeniedPathOutsideRoot => "workspace_mutation_deny_outside_root",
            Self::InvalidLimit => "workspace_mutation_invalid_limit",
            Self::InvalidPath => "workspace_mutation_invalid_path",
            Self::PathTooLarge => "workspace_mutation_path_too_large",
            Self::PathDenied => "workspace_mutation_path_denied",
            Self::InvalidPrecondition => "workspace_mutation_invalid_precondition",
            Self::InvalidPayload => "workspace_mutation_invalid_payload",
            Self::PreconditionFailed => "workspace_mutation_precondition_failed",
            Self::PayloadTooLarge => "workspace_mutation_payload_too_large",
            Self::ExistingFileTooLarge => "workspace_mutation_existing_file_too_large",
            Self::HardLinkDenied => "workspace_mutation_hard_link_denied",
            Self::TempNameExhausted => "workspace_mutation_temp_name_exhausted",
            Self::Io(_) => "workspace_mutation_io",
            Self::OutcomeUnproven(_) => "workspace_mutation_outcome_unproven",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_root(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "cumg-v2-workspace-mutation-{name}-{}-{}",
            std::process::id(),
            rand::random::<u64>()
        ));
        fs::create_dir_all(&root).unwrap();
        root
    }

    #[test]
    fn create_requires_expected_absent_and_publishes_complete_file() {
        let root = temp_root("create");
        let executor = WorkspaceMutationExecutor::new(
            WorkspaceMutationPolicy::new(vec![root.clone()], vec![]).unwrap(),
        );
        let target = root.join("note.txt");
        let result = executor
            .write_file(
                target.to_str().unwrap(),
                b"hello",
                &WorkspaceWritePrecondition::ExpectedAbsent,
            )
            .unwrap();
        assert_eq!(fs::read(&target).unwrap(), b"hello");
        assert!(matches!(
            result,
            DeviceResult::WorkspaceFileWritten {
                bytes_written: 5,
                created: true,
                ..
            }
        ));
        assert!(fs::read_dir(&root).unwrap().all(|entry| {
            !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(".cumg-write-")
        }));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn stale_hash_is_rejected_without_mutating_file() {
        let root = temp_root("stale");
        let target = root.join("note.txt");
        fs::write(&target, b"old").unwrap();
        let executor = WorkspaceMutationExecutor::new(
            WorkspaceMutationPolicy::new(vec![root.clone()], vec![]).unwrap(),
        );
        let error = executor
            .write_file(
                target.to_str().unwrap(),
                b"new",
                &WorkspaceWritePrecondition::ExpectedSha256 {
                    sha256: sha256_hex(b"different"),
                },
            )
            .unwrap_err();
        assert!(matches!(error, WorkspaceMutationError::PreconditionFailed));
        assert_eq!(fs::read(&target).unwrap(), b"old");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn matching_hash_atomically_replaces_file() {
        let root = temp_root("replace");
        let target = root.join("note.txt");
        fs::write(&target, b"old").unwrap();
        let executor = WorkspaceMutationExecutor::new(
            WorkspaceMutationPolicy::new(vec![root.clone()], vec![]).unwrap(),
        );
        let result = executor
            .write_file(
                target.to_str().unwrap(),
                b"new",
                &WorkspaceWritePrecondition::ExpectedSha256 {
                    sha256: sha256_hex(b"old"),
                },
            )
            .unwrap();
        assert_eq!(fs::read(&target).unwrap(), b"new");
        assert!(matches!(
            result,
            DeviceResult::WorkspaceFileWritten {
                bytes_written: 3,
                created: false,
                ..
            }
        ));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn deny_subpath_wins_over_writable_root() {
        let root = temp_root("deny");
        let denied = root.join("private");
        fs::create_dir_all(&denied).unwrap();
        let executor = WorkspaceMutationExecutor::new(
            WorkspaceMutationPolicy::new(vec![root.clone()], vec![denied.clone()]).unwrap(),
        );
        let error = executor
            .write_file(
                denied.join("secret.txt").to_str().unwrap(),
                b"nope",
                &WorkspaceWritePrecondition::ExpectedAbsent,
            )
            .unwrap_err();
        assert!(matches!(error, WorkspaceMutationError::PathDenied));
        assert!(!denied.join("secret.txt").exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn oversized_path_is_rejected_before_filesystem_access() {
        let root = temp_root("oversized-path");
        let executor = WorkspaceMutationExecutor::new(
            WorkspaceMutationPolicy::new(vec![root.clone()], vec![]).unwrap(),
        );
        let path = "x".repeat(DEFAULT_MAX_WORKSPACE_PATH_BYTES + 1);
        let error = executor
            .write_file(&path, b"data", &WorkspaceWritePrecondition::ExpectedAbsent)
            .unwrap_err();
        assert!(matches!(error, WorkspaceMutationError::PathTooLarge));
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn denied_existing_file_is_case_alias_safe() {
        let root = temp_root("deny-case-alias");
        let denied = root.join("Secret.txt");
        fs::write(&denied, b"secret").unwrap();
        let executor = WorkspaceMutationExecutor::new(
            WorkspaceMutationPolicy::new(vec![root.clone()], vec![denied]).unwrap(),
        );
        let alias = root.join("secret.TXT");
        let error = executor
            .write_file(
                alias.to_str().unwrap(),
                b"new",
                &WorkspaceWritePrecondition::ExpectedSha256 {
                    sha256: sha256_hex(b"secret"),
                },
            )
            .unwrap_err();
        assert!(matches!(error, WorkspaceMutationError::PathDenied));
        assert_eq!(fs::read(root.join("Secret.txt")).unwrap(), b"secret");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn oversized_payload_is_rejected_before_write() {
        let root = temp_root("oversized");
        let executor = WorkspaceMutationExecutor::new(
            WorkspaceMutationPolicy::new(vec![root.clone()], vec![])
                .unwrap()
                .with_limits(4, 1024)
                .unwrap(),
        );
        let target = root.join("note.txt");
        let error = executor
            .write_file(
                target.to_str().unwrap(),
                b"12345",
                &WorkspaceWritePrecondition::ExpectedAbsent,
            )
            .unwrap_err();
        assert!(matches!(error, WorkspaceMutationError::PayloadTooLarge));
        assert!(!target.exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn symlink_target_is_denied_without_touching_target() {
        use std::os::unix::fs::symlink;

        let root = temp_root("symlink-root");
        let outside = temp_root("symlink-outside");
        let outside_file = outside.join("outside.txt");
        fs::write(&outside_file, b"outside").unwrap();
        let target = root.join("note.txt");
        symlink(&outside_file, &target).unwrap();
        let executor = WorkspaceMutationExecutor::new(
            WorkspaceMutationPolicy::new(vec![root.clone()], vec![]).unwrap(),
        );
        let error = executor
            .write_file(
                target.to_str().unwrap(),
                b"new",
                &WorkspaceWritePrecondition::ExpectedSha256 {
                    sha256: sha256_hex(b"outside"),
                },
            )
            .unwrap_err();
        assert!(matches!(error, WorkspaceMutationError::PathDenied));
        assert_eq!(fs::read(&outside_file).unwrap(), b"outside");
        fs::remove_dir_all(root).unwrap();
        fs::remove_dir_all(outside).unwrap();
    }

    #[test]
    fn path_outside_writable_root_is_denied() {
        let root = temp_root("escape-root");
        let outside = temp_root("escape-outside");
        let executor = WorkspaceMutationExecutor::new(
            WorkspaceMutationPolicy::new(vec![root.clone()], vec![]).unwrap(),
        );
        let target = outside.join("note.txt");
        let error = executor
            .write_file(
                target.to_str().unwrap(),
                b"nope",
                &WorkspaceWritePrecondition::ExpectedAbsent,
            )
            .unwrap_err();
        assert!(matches!(error, WorkspaceMutationError::PathDenied));
        assert!(!target.exists());
        fs::remove_dir_all(root).unwrap();
        fs::remove_dir_all(outside).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_parent_escape_is_denied() {
        use std::os::unix::fs::symlink;

        let root = temp_root("parent-symlink-root");
        let outside = temp_root("parent-symlink-outside");
        symlink(&outside, root.join("linked")).unwrap();
        let executor = WorkspaceMutationExecutor::new(
            WorkspaceMutationPolicy::new(vec![root.clone()], vec![]).unwrap(),
        );
        let error = executor
            .write_file(
                root.join("linked/note.txt").to_str().unwrap(),
                b"nope",
                &WorkspaceWritePrecondition::ExpectedAbsent,
            )
            .unwrap_err();
        assert!(matches!(error, WorkspaceMutationError::PathDenied));
        assert!(!outside.join("note.txt").exists());
        fs::remove_dir_all(root).unwrap();
        fs::remove_dir_all(outside).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn hard_link_target_is_denied_without_writing_through_inode() {
        let root = temp_root("hardlink-root");
        let outside = temp_root("hardlink-outside");
        let outside_file = outside.join("outside.txt");
        fs::write(&outside_file, b"outside").unwrap();
        let target = root.join("note.txt");
        fs::hard_link(&outside_file, &target).unwrap();
        let executor = WorkspaceMutationExecutor::new(
            WorkspaceMutationPolicy::new(vec![root.clone()], vec![]).unwrap(),
        );
        let error = executor
            .write_file(
                target.to_str().unwrap(),
                b"new",
                &WorkspaceWritePrecondition::ExpectedSha256 {
                    sha256: sha256_hex(b"outside"),
                },
            )
            .unwrap_err();
        assert!(matches!(error, WorkspaceMutationError::HardLinkDenied));
        assert_eq!(fs::read(&outside_file).unwrap(), b"outside");
        fs::remove_dir_all(root).unwrap();
        fs::remove_dir_all(outside).unwrap();
    }
}
