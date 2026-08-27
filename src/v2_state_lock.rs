//! Exclusive ownership of a V2 Hub state directory.

use fs2::FileExt as _;
#[cfg(windows)]
use std::collections::HashSet;
use std::fmt;
use std::fs::{self, File, OpenOptions};
#[cfg(unix)]
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::Path;
#[cfg(windows)]
use std::path::PathBuf;
#[cfg(windows)]
use std::sync::{Mutex, OnceLock};

const STATE_LOCK_FILE: &str = ".cumg-v2-state.lock";

#[derive(Debug)]
pub struct StateDirectoryLock {
    file: File,
    #[cfg(windows)]
    process_key: PathBuf,
}

impl StateDirectoryLock {
    pub fn acquire(directory: &Path) -> Result<Self, StateDirectoryLockError> {
        ensure_private_directory(directory)?;
        let path = directory.join(STATE_LOCK_FILE);
        match fs::symlink_metadata(&path) {
            Ok(metadata) => {
                if metadata.file_type().is_symlink() || !metadata.is_file() {
                    return Err(StateDirectoryLockError::UnsafePath);
                }
                #[cfg(unix)]
                if metadata.permissions().mode() & 0o077 != 0 {
                    return Err(StateDirectoryLockError::UnsafePermissions);
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(StateDirectoryLockError::Io(error)),
        }
        let mut options = OpenOptions::new();
        options.read(true).write(true).create(true);
        #[cfg(unix)]
        options.mode(0o600);
        let file = options.open(path).map_err(StateDirectoryLockError::Io)?;
        let metadata = file.metadata().map_err(StateDirectoryLockError::Io)?;
        if !metadata.is_file() {
            return Err(StateDirectoryLockError::UnsafePath);
        }
        #[cfg(unix)]
        if metadata.permissions().mode() & 0o077 != 0 {
            return Err(StateDirectoryLockError::UnsafePermissions);
        }

        #[cfg(windows)]
        let process_key = reserve_process_lock(directory)?;
        match file.try_lock_exclusive() {
            Ok(()) => Ok(Self {
                file,
                #[cfg(windows)]
                process_key,
            }),
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                #[cfg(windows)]
                release_process_lock(&process_key);
                Err(StateDirectoryLockError::Busy)
            }
            Err(error) => {
                #[cfg(windows)]
                release_process_lock(&process_key);
                Err(StateDirectoryLockError::Io(error))
            }
        }
    }
}

impl Drop for StateDirectoryLock {
    fn drop(&mut self) {
        let _ = fs2::FileExt::unlock(&self.file);
        #[cfg(windows)]
        release_process_lock(&self.process_key);
    }
}

#[cfg(windows)]
static PROCESS_LOCKS: OnceLock<Mutex<HashSet<PathBuf>>> = OnceLock::new();

#[cfg(windows)]
fn reserve_process_lock(directory: &Path) -> Result<PathBuf, StateDirectoryLockError> {
    let key = fs::canonicalize(directory).map_err(StateDirectoryLockError::Io)?;
    let locks = PROCESS_LOCKS.get_or_init(|| Mutex::new(HashSet::new()));
    let mut locks = locks
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if !locks.insert(key.clone()) {
        return Err(StateDirectoryLockError::Busy);
    }
    Ok(key)
}

#[cfg(windows)]
fn release_process_lock(key: &Path) {
    if let Some(locks) = PROCESS_LOCKS.get() {
        let mut locks = locks
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        locks.remove(key);
    }
}

#[derive(Debug)]
pub enum StateDirectoryLockError {
    Busy,
    UnsafePath,
    UnsafePermissions,
    Io(std::io::Error),
}

impl fmt::Display for StateDirectoryLockError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Busy => f.write_str("V2 Hub state directory is already in use"),
            Self::UnsafePath => f.write_str("V2 Hub state lock path is unsafe"),
            Self::UnsafePermissions => {
                f.write_str("V2 Hub state directory or lock permissions are too broad")
            }
            Self::Io(error) => write!(f, "V2 Hub state lock I/O failure: {error}"),
        }
    }
}
impl std::error::Error for StateDirectoryLockError {}

fn ensure_private_directory(directory: &Path) -> Result<(), StateDirectoryLockError> {
    if !directory.exists() {
        let mut builder = fs::DirBuilder::new();
        builder.recursive(true);
        #[cfg(unix)]
        builder.mode(0o700);
        builder
            .create(directory)
            .map_err(StateDirectoryLockError::Io)?;
    }
    let metadata = fs::symlink_metadata(directory).map_err(StateDirectoryLockError::Io)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(StateDirectoryLockError::UnsafePath);
    }
    #[cfg(unix)]
    if metadata.permissions().mode() & 0o077 != 0 {
        return Err(StateDirectoryLockError::UnsafePermissions);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn exclusive_lock_refuses_second_owner_and_releases() {
        let dir = std::env::temp_dir().join(format!(
            "cumg-v2-state-lock-{}-{}",
            std::process::id(),
            rand::random::<u64>()
        ));
        let first = StateDirectoryLock::acquire(&dir).unwrap();
        assert!(matches!(
            StateDirectoryLock::acquire(&dir),
            Err(StateDirectoryLockError::Busy)
        ));
        drop(first);
        StateDirectoryLock::acquire(&dir).unwrap();
        let _ = fs::remove_dir_all(dir);
    }
}
