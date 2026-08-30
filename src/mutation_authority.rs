//! Cross-process single-writer authority for a shared physical Computer Use backend.
//!
//! V1 and V2 may remain alive at the same time for read-only health, cutover, and
//! rollback. Effectful Cua calls must hold this local authority permit so only one
//! control-plane family can mutate the shared backend at a time.

use fs2::FileExt;
use serde::{Deserialize, Serialize};
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{Read as _, Write as _};
#[cfg(unix)]
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::str::FromStr;

const LOCK_FILE: &str = "mutation-authority.lock";
const OWNER_FILE: &str = "mutation-authority.json";
const SCHEMA_VERSION: u16 = 1;
const MAX_STATE_BYTES: u64 = 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MutationAuthorityRole {
    V1,
    V2,
}

impl MutationAuthorityRole {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::V1 => "v1",
            Self::V2 => "v2",
        }
    }
}

impl fmt::Display for MutationAuthorityRole {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for MutationAuthorityRole {
    type Err = MutationAuthorityError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "v1" => Ok(Self::V1),
            "v2" => Ok(Self::V2),
            _ => Err(MutationAuthorityError::InvalidRole),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MutationAuthorityStatus {
    pub schema_version: u16,
    pub owner: MutationAuthorityRole,
    pub epoch: u64,
}

#[derive(Debug, Clone)]
pub struct MutationAuthorityGate {
    directory: PathBuf,
    role: MutationAuthorityRole,
}

impl MutationAuthorityGate {
    pub fn new(directory: impl Into<PathBuf>, role: MutationAuthorityRole) -> Self {
        Self {
            directory: directory.into(),
            role,
        }
    }

    pub fn role(&self) -> MutationAuthorityRole {
        self.role
    }

    /// Acquire the cross-process mutation permit without waiting. The returned
    /// permit owns the OS lock and must remain alive across the complete backend
    /// mutation, including response handling/recovery classification.
    pub fn try_acquire(&self) -> Result<MutationAuthorityPermit, MutationAuthorityError> {
        validate_private_directory(&self.directory)?;
        let lock = open_existing_private_file(&self.directory.join(LOCK_FILE), true)?;
        match lock.try_lock_exclusive() {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                return Err(MutationAuthorityError::Busy);
            }
            Err(_) => return Err(MutationAuthorityError::Io),
        }
        let status = match read_status_locked(&self.directory) {
            Ok(status) => status,
            Err(error) => {
                let _ = FileExt::unlock(&lock);
                return Err(error);
            }
        };
        if status.owner != self.role {
            let _ = FileExt::unlock(&lock);
            return Err(MutationAuthorityError::WrongOwner {
                expected: self.role,
                actual: status.owner,
            });
        }
        Ok(MutationAuthorityPermit {
            lock,
            owner: status.owner,
            epoch: status.epoch,
        })
    }
}

#[derive(Debug)]
pub struct MutationAuthorityPermit {
    lock: File,
    owner: MutationAuthorityRole,
    epoch: u64,
}

impl MutationAuthorityPermit {
    pub fn owner(&self) -> MutationAuthorityRole {
        self.owner
    }

    pub fn epoch(&self) -> u64 {
        self.epoch
    }
}

impl Drop for MutationAuthorityPermit {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self.lock);
    }
}

pub fn initialize_mutation_authority(
    directory: &Path,
    owner: MutationAuthorityRole,
) -> Result<MutationAuthorityStatus, MutationAuthorityError> {
    ensure_private_directory(directory)?;
    let lock_path = directory.join(LOCK_FILE);
    let owner_path = directory.join(OWNER_FILE);
    if owner_path.exists() {
        return Err(MutationAuthorityError::AlreadyInitialized);
    }
    let lock = open_or_create_private_lock(&lock_path)?;
    match lock.try_lock_exclusive() {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
            return Err(MutationAuthorityError::Busy);
        }
        Err(_) => return Err(MutationAuthorityError::Io),
    }
    if owner_path.exists() {
        let _ = FileExt::unlock(&lock);
        return Err(MutationAuthorityError::AlreadyInitialized);
    }
    let status = MutationAuthorityStatus {
        schema_version: SCHEMA_VERSION,
        owner,
        epoch: 1,
    };
    let result = write_status_atomic(directory, status);
    let _ = FileExt::unlock(&lock);
    result?;
    Ok(status)
}

pub fn inspect_mutation_authority(
    directory: &Path,
) -> Result<MutationAuthorityStatus, MutationAuthorityError> {
    validate_private_directory(directory)?;
    let lock = open_existing_private_file(&directory.join(LOCK_FILE), true)?;
    match FileExt::try_lock_shared(&lock) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
            return Err(MutationAuthorityError::Busy);
        }
        Err(_) => return Err(MutationAuthorityError::Io),
    }
    let result = read_status_locked(directory);
    let _ = FileExt::unlock(&lock);
    result
}

/// Atomically hand ownership to another control-plane family. The caller must
/// name the current owner (CAS-style). The same exclusive lock is used by live
/// mutation permits, so a switch cannot complete while an effectful call is in
/// flight.
pub fn switch_mutation_authority(
    directory: &Path,
    expected_owner: MutationAuthorityRole,
    new_owner: MutationAuthorityRole,
) -> Result<MutationAuthorityStatus, MutationAuthorityError> {
    switch_mutation_authority_guarded(directory, expected_owner, new_owner, || true)
}

/// CAS-switch ownership while holding the same exclusive lock used by live
/// mutation permits. `guard` runs after the current owner has been verified but
/// before the durable owner changes, so external authority (for example Human
/// Handoff state) can be checked without a Begin/switch TOCTOU window.
pub fn switch_mutation_authority_guarded<F>(
    directory: &Path,
    expected_owner: MutationAuthorityRole,
    new_owner: MutationAuthorityRole,
    guard: F,
) -> Result<MutationAuthorityStatus, MutationAuthorityError>
where
    F: FnOnce() -> bool,
{
    if expected_owner == new_owner {
        return Err(MutationAuthorityError::InvalidTransition);
    }
    validate_private_directory(directory)?;
    let lock = open_existing_private_file(&directory.join(LOCK_FILE), true)?;
    match lock.try_lock_exclusive() {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
            return Err(MutationAuthorityError::Busy);
        }
        Err(_) => return Err(MutationAuthorityError::Io),
    }
    let current = match read_status_locked(directory) {
        Ok(current) => current,
        Err(error) => {
            let _ = FileExt::unlock(&lock);
            return Err(error);
        }
    };
    if current.owner != expected_owner {
        let _ = FileExt::unlock(&lock);
        return Err(MutationAuthorityError::WrongOwner {
            expected: expected_owner,
            actual: current.owner,
        });
    }
    if !guard() {
        let _ = FileExt::unlock(&lock);
        return Err(MutationAuthorityError::TransitionGuardFailed);
    }
    let next = MutationAuthorityStatus {
        schema_version: SCHEMA_VERSION,
        owner: new_owner,
        epoch: current
            .epoch
            .checked_add(1)
            .ok_or(MutationAuthorityError::InvalidState)?,
    };
    let result = write_status_atomic(directory, next);
    let _ = FileExt::unlock(&lock);
    result?;
    Ok(next)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MutationAuthorityError {
    NotInitialized,
    AlreadyInitialized,
    Busy,
    WrongOwner {
        expected: MutationAuthorityRole,
        actual: MutationAuthorityRole,
    },
    UnsafePath,
    UnsafePermissions,
    InvalidState,
    InvalidRole,
    InvalidTransition,
    TransitionGuardFailed,
    Io,
}

impl MutationAuthorityError {
    pub const fn safe_code(self) -> &'static str {
        match self {
            Self::NotInitialized => "mutation_authority_not_initialized",
            Self::AlreadyInitialized => "mutation_authority_already_initialized",
            Self::Busy => "mutation_authority_busy",
            Self::WrongOwner { .. } => "mutation_authority_wrong_owner",
            Self::UnsafePath => "mutation_authority_unsafe_path",
            Self::UnsafePermissions => "mutation_authority_unsafe_permissions",
            Self::InvalidState => "mutation_authority_invalid_state",
            Self::InvalidRole => "mutation_authority_invalid_role",
            Self::InvalidTransition => "mutation_authority_invalid_transition",
            Self::TransitionGuardFailed => "mutation_authority_transition_guard_failed",
            Self::Io => "mutation_authority_io_failure",
        }
    }
}

impl fmt::Display for MutationAuthorityError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::WrongOwner { expected, actual } => write!(
                f,
                "{} expected={} actual={}",
                self.safe_code(),
                expected,
                actual
            ),
            _ => f.write_str(self.safe_code()),
        }
    }
}

impl std::error::Error for MutationAuthorityError {}

fn read_status_locked(directory: &Path) -> Result<MutationAuthorityStatus, MutationAuthorityError> {
    let mut file = open_existing_private_file(&directory.join(OWNER_FILE), false)?;
    let mut bytes = Vec::new();
    std::io::Read::take(&mut file, MAX_STATE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| MutationAuthorityError::Io)?;
    if bytes.is_empty() || bytes.len() as u64 > MAX_STATE_BYTES {
        return Err(MutationAuthorityError::InvalidState);
    }
    let status: MutationAuthorityStatus =
        serde_json::from_slice(&bytes).map_err(|_| MutationAuthorityError::InvalidState)?;
    if status.schema_version != SCHEMA_VERSION || status.epoch == 0 {
        return Err(MutationAuthorityError::InvalidState);
    }
    Ok(status)
}

fn write_status_atomic(
    directory: &Path,
    status: MutationAuthorityStatus,
) -> Result<(), MutationAuthorityError> {
    let owner_path = directory.join(OWNER_FILE);
    if owner_path.exists() {
        validate_private_regular_file(&owner_path)?;
    }
    let temp_path = directory.join(format!(
        ".{OWNER_FILE}.new-{}-{}",
        std::process::id(),
        rand::random::<u64>()
    ));
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    let mut temp = options
        .open(&temp_path)
        .map_err(|_| MutationAuthorityError::Io)?;
    let mut encoded =
        serde_json::to_vec(&status).map_err(|_| MutationAuthorityError::InvalidState)?;
    encoded.push(b'\n');
    if encoded.len() as u64 > MAX_STATE_BYTES {
        let _ = fs::remove_file(&temp_path);
        return Err(MutationAuthorityError::InvalidState);
    }
    if temp.write_all(&encoded).is_err() || temp.sync_all().is_err() {
        let _ = fs::remove_file(&temp_path);
        return Err(MutationAuthorityError::Io);
    }
    drop(temp);
    if fs::rename(&temp_path, &owner_path).is_err() {
        let _ = fs::remove_file(&temp_path);
        return Err(MutationAuthorityError::Io);
    }
    #[cfg(unix)]
    {
        File::open(directory)
            .and_then(|directory| directory.sync_all())
            .map_err(|_| MutationAuthorityError::Io)?;
    }
    Ok(())
}

fn ensure_private_directory(directory: &Path) -> Result<(), MutationAuthorityError> {
    if !directory.exists() {
        let mut builder = fs::DirBuilder::new();
        builder.recursive(true);
        #[cfg(unix)]
        builder.mode(0o700);
        builder
            .create(directory)
            .map_err(|_| MutationAuthorityError::Io)?;
    }
    validate_private_directory(directory)
}

fn validate_private_directory(directory: &Path) -> Result<(), MutationAuthorityError> {
    let metadata = fs::symlink_metadata(directory).map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            MutationAuthorityError::NotInitialized
        } else {
            MutationAuthorityError::Io
        }
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(MutationAuthorityError::UnsafePath);
    }
    #[cfg(unix)]
    if metadata.permissions().mode() & 0o077 != 0 {
        return Err(MutationAuthorityError::UnsafePermissions);
    }
    Ok(())
}

fn open_or_create_private_lock(path: &Path) -> Result<File, MutationAuthorityError> {
    if path.exists() {
        return open_existing_private_file(path, true);
    }
    let mut options = OpenOptions::new();
    options.read(true).write(true).create_new(true);
    #[cfg(unix)]
    {
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    options.open(path).map_err(|_| MutationAuthorityError::Io)
}

fn open_existing_private_file(path: &Path, writable: bool) -> Result<File, MutationAuthorityError> {
    validate_private_regular_file(path)?;
    let mut options = OpenOptions::new();
    options.read(true).write(writable);
    #[cfg(unix)]
    options.custom_flags(libc::O_NOFOLLOW);
    options.open(path).map_err(|_| MutationAuthorityError::Io)
}

fn validate_private_regular_file(path: &Path) -> Result<(), MutationAuthorityError> {
    let metadata = fs::symlink_metadata(path).map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            MutationAuthorityError::NotInitialized
        } else {
            MutationAuthorityError::Io
        }
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(MutationAuthorityError::UnsafePath);
    }
    #[cfg(unix)]
    if metadata.permissions().mode() & 0o077 != 0 {
        return Err(MutationAuthorityError::UnsafePermissions);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn root(label: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "cumg-mutation-authority-{label}-{}-{}",
            std::process::id(),
            rand::random::<u64>()
        ))
    }

    #[test]
    fn owner_is_durable_and_only_current_owner_gets_a_permit() {
        let root = root("owner");
        let initial = initialize_mutation_authority(&root, MutationAuthorityRole::V1).unwrap();
        assert_eq!(initial.owner, MutationAuthorityRole::V1);
        assert_eq!(initial.epoch, 1);

        let v1 = MutationAuthorityGate::new(&root, MutationAuthorityRole::V1);
        let v2 = MutationAuthorityGate::new(&root, MutationAuthorityRole::V2);
        let permit = v1.try_acquire().unwrap();
        assert_eq!(permit.owner(), MutationAuthorityRole::V1);
        assert!(matches!(
            v2.try_acquire(),
            Err(MutationAuthorityError::Busy)
        ));
        drop(permit);
        assert!(matches!(
            v2.try_acquire(),
            Err(MutationAuthorityError::WrongOwner {
                expected: MutationAuthorityRole::V2,
                actual: MutationAuthorityRole::V1,
            })
        ));
        assert_eq!(
            inspect_mutation_authority(&root).unwrap(),
            MutationAuthorityStatus {
                schema_version: SCHEMA_VERSION,
                owner: MutationAuthorityRole::V1,
                epoch: 1,
            }
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn switch_is_cas_bound_and_cannot_cross_an_inflight_mutation() {
        let root = root("switch");
        initialize_mutation_authority(&root, MutationAuthorityRole::V1).unwrap();
        let v1 = MutationAuthorityGate::new(&root, MutationAuthorityRole::V1);
        let permit = v1.try_acquire().unwrap();
        assert!(matches!(
            switch_mutation_authority(&root, MutationAuthorityRole::V1, MutationAuthorityRole::V2),
            Err(MutationAuthorityError::Busy)
        ));
        drop(permit);

        let switched =
            switch_mutation_authority(&root, MutationAuthorityRole::V1, MutationAuthorityRole::V2)
                .unwrap();
        assert_eq!(switched.owner, MutationAuthorityRole::V2);
        assert_eq!(switched.epoch, 2);
        assert!(matches!(
            switch_mutation_authority(&root, MutationAuthorityRole::V1, MutationAuthorityRole::V2),
            Err(MutationAuthorityError::WrongOwner {
                expected: MutationAuthorityRole::V1,
                actual: MutationAuthorityRole::V2,
            })
        ));
        MutationAuthorityGate::new(&root, MutationAuthorityRole::V2)
            .try_acquire()
            .unwrap();
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn guarded_switch_keeps_owner_when_external_authority_is_not_idle() {
        let root = root("guarded-switch");
        initialize_mutation_authority(&root, MutationAuthorityRole::V2).unwrap();
        assert!(matches!(
            switch_mutation_authority_guarded(
                &root,
                MutationAuthorityRole::V2,
                MutationAuthorityRole::V1,
                || false,
            ),
            Err(MutationAuthorityError::TransitionGuardFailed)
        ));
        let status = inspect_mutation_authority(&root).unwrap();
        assert_eq!(status.owner, MutationAuthorityRole::V2);
        assert_eq!(status.epoch, 1);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn missing_or_malformed_authority_state_fails_closed() {
        let root = root("invalid");
        let gate = MutationAuthorityGate::new(&root, MutationAuthorityRole::V1);
        assert!(matches!(
            gate.try_acquire(),
            Err(MutationAuthorityError::NotInitialized)
        ));

        initialize_mutation_authority(&root, MutationAuthorityRole::V1).unwrap();
        fs::write(root.join(OWNER_FILE), b"{}\n").unwrap();
        #[cfg(unix)]
        fs::set_permissions(root.join(OWNER_FILE), fs::Permissions::from_mode(0o600)).unwrap();
        assert!(matches!(
            gate.try_acquire(),
            Err(MutationAuthorityError::InvalidState)
        ));
        let _ = fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn broad_permissions_and_symlinks_are_rejected() {
        let root = root("unsafe");
        initialize_mutation_authority(&root, MutationAuthorityRole::V1).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(matches!(
            MutationAuthorityGate::new(&root, MutationAuthorityRole::V1).try_acquire(),
            Err(MutationAuthorityError::UnsafePermissions)
        ));
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();

        let owner = root.join(OWNER_FILE);
        let replacement = root.join("replacement.json");
        fs::rename(&owner, &replacement).unwrap();
        std::os::unix::fs::symlink(&replacement, &owner).unwrap();
        assert!(matches!(
            MutationAuthorityGate::new(&root, MutationAuthorityRole::V1).try_acquire(),
            Err(MutationAuthorityError::UnsafePath)
        ));
        let _ = fs::remove_dir_all(root);
    }
}
