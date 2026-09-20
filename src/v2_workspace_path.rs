//! Capability-rooted workspace path resolution shared by filesystem observation
//! and future bounded workspace mutation.
//!
//! Ambient path canonicalization is used only to select an operator-approved
//! root and preserve the existing absolute/relative path API. Authority is not
//! derived from that pathname proof: the returned path is reopened relative to
//! an already-open capability root through cap-std. If a component is replaced
//! after selection, cap-std's component-wise resolution still prevents the
//! reopen from escaping the approved root.
//!
//! Configured roots are operator-controlled startup policy. This module does
//! not claim protection against adversarial root replacement while policy is
//! being constructed; packaged root ACL/readiness preflight belongs to #314.

use cap_std::ambient_authority;
use cap_std::fs::{Dir, File};
use std::ffi::{OsStr, OsString};
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

#[derive(Clone)]
struct WorkspaceRoot {
    canonical_path: PathBuf,
    dir: Arc<Dir>,
}

#[derive(Clone)]
pub struct WorkspaceRoots {
    roots: Vec<WorkspaceRoot>,
}

impl WorkspaceRoots {
    pub fn new(roots: Vec<PathBuf>) -> Result<Self, WorkspacePathError> {
        if roots.is_empty() {
            return Err(WorkspacePathError::NoRoots);
        }

        let mut opened = Vec::with_capacity(roots.len());
        for root in roots {
            let canonical_path = fs::canonicalize(&root).map_err(WorkspacePathError::Io)?;
            if !canonical_path.is_dir() {
                return Err(WorkspacePathError::RootNotDirectory);
            }
            if opened
                .iter()
                .any(|existing: &WorkspaceRoot| existing.canonical_path == canonical_path)
            {
                continue;
            }
            let dir = Dir::open_ambient_dir(&canonical_path, ambient_authority())
                .map_err(WorkspacePathError::Io)?;
            opened.push(WorkspaceRoot {
                canonical_path,
                dir: Arc::new(dir),
            });
        }

        if opened.is_empty() {
            return Err(WorkspacePathError::NoRoots);
        }

        Ok(Self { roots: opened })
    }

    pub(crate) fn contains_canonical_path(&self, path: &Path) -> bool {
        self.roots
            .iter()
            .any(|root| path.starts_with(&root.canonical_path))
    }

    pub fn resolve_parent_for_mutation(
        &self,
        path: &str,
    ) -> Result<ResolvedWorkspaceParent, WorkspacePathError> {
        if path.trim().is_empty() {
            return Err(WorkspacePathError::InvalidPath);
        }
        let requested = Path::new(path);
        let file_name = requested
            .file_name()
            .filter(|name| !name.is_empty())
            .ok_or(WorkspacePathError::InvalidPath)?
            .to_os_string();
        if matches!(file_name.to_str(), Some(".") | Some("..")) {
            return Err(WorkspacePathError::InvalidPath);
        }
        let parent = requested.parent().ok_or(WorkspacePathError::InvalidPath)?;
        let parent = if parent.as_os_str().is_empty() {
            Path::new(".")
        } else {
            parent
        };
        let canonical_parent = fs::canonicalize(parent).map_err(WorkspacePathError::Io)?;
        let root = self
            .roots
            .iter()
            .filter(|root| canonical_parent.starts_with(&root.canonical_path))
            .max_by_key(|root| root.canonical_path.components().count())
            .ok_or(WorkspacePathError::PathDenied)?;
        let relative_parent = canonical_parent
            .strip_prefix(&root.canonical_path)
            .map_err(|_| WorkspacePathError::PathDenied)?;
        let relative_parent = if relative_parent.as_os_str().is_empty() {
            PathBuf::from(".")
        } else {
            relative_parent.to_owned()
        };
        let parent_dir = root
            .dir
            .open_dir(&relative_parent)
            .map_err(map_capability_open_error)?;
        let parent_identity = directory_identity_from_cap_dir(&parent_dir)?;
        let ambient_identity = directory_identity_from_path(&canonical_parent)?;
        if parent_identity != ambient_identity {
            return Err(WorkspacePathError::PathDenied);
        }
        Ok(ResolvedWorkspaceParent {
            dir: parent_dir,
            canonical_parent,
            file_name,
            parent_identity,
        })
    }

    pub fn resolve_existing(
        &self,
        path: &str,
    ) -> Result<ResolvedWorkspacePath, WorkspacePathError> {
        if path.trim().is_empty() {
            return Err(WorkspacePathError::InvalidPath);
        }

        let resolved = fs::canonicalize(Path::new(path)).map_err(WorkspacePathError::Io)?;
        let root = self
            .roots
            .iter()
            .filter(|root| resolved.starts_with(&root.canonical_path))
            .max_by_key(|root| root.canonical_path.components().count())
            .ok_or(WorkspacePathError::PathDenied)?;

        let relative = resolved
            .strip_prefix(&root.canonical_path)
            .map_err(|_| WorkspacePathError::PathDenied)?;
        let relative = if relative.as_os_str().is_empty() {
            PathBuf::from(".")
        } else {
            relative.to_owned()
        };

        Ok(ResolvedWorkspacePath {
            root: Arc::clone(&root.dir),
            relative,
        })
    }
}

impl fmt::Debug for WorkspaceRoots {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("WorkspaceRoots")
            .field("root_count", &self.roots.len())
            .finish()
    }
}

pub struct ResolvedWorkspacePath {
    root: Arc<Dir>,
    relative: PathBuf,
}

impl ResolvedWorkspacePath {
    pub fn open_file(&self) -> Result<File, WorkspacePathError> {
        self.root
            .open(&self.relative)
            .map_err(map_capability_open_error)
    }

    pub fn open_dir(&self) -> Result<Dir, WorkspacePathError> {
        self.root
            .open_dir(&self.relative)
            .map_err(map_capability_open_error)
    }
}

impl fmt::Debug for ResolvedWorkspacePath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ResolvedWorkspacePath([redacted])")
    }
}

pub struct ResolvedWorkspaceParent {
    dir: Dir,
    canonical_parent: PathBuf,
    file_name: OsString,
    parent_identity: WorkspaceDirectoryIdentity,
}

impl ResolvedWorkspaceParent {
    pub(crate) fn dir(&self) -> &Dir {
        &self.dir
    }

    pub(crate) fn file_name(&self) -> &OsStr {
        &self.file_name
    }

    pub(crate) fn canonical_target(&self) -> PathBuf {
        self.canonical_parent.join(&self.file_name)
    }

    pub(crate) fn reprove_parent(&self) -> Result<(), WorkspacePathError> {
        let canonical_now =
            fs::canonicalize(&self.canonical_parent).map_err(WorkspacePathError::Io)?;
        if canonical_now != self.canonical_parent {
            return Err(WorkspacePathError::PathDenied);
        }
        let ambient_identity = directory_identity_from_path(&self.canonical_parent)?;
        let handle_identity = directory_identity_from_cap_dir(&self.dir)?;
        if ambient_identity != self.parent_identity || handle_identity != self.parent_identity {
            return Err(WorkspacePathError::PathDenied);
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum WorkspaceDirectoryIdentity {
    #[cfg(unix)]
    Unix { dev: u64, ino: u64 },
    #[cfg(windows)]
    Windows { volume_serial: u32, file_index: u64 },
}

fn directory_identity_from_cap_dir(
    dir: &Dir,
) -> Result<WorkspaceDirectoryIdentity, WorkspacePathError> {
    let file = dir
        .try_clone()
        .map_err(WorkspacePathError::Io)?
        .into_std_file();
    directory_identity_from_file(&file)
}

#[cfg(unix)]
fn directory_identity_from_path(
    path: &Path,
) -> Result<WorkspaceDirectoryIdentity, WorkspacePathError> {
    let metadata = fs::metadata(path).map_err(WorkspacePathError::Io)?;
    directory_identity(&metadata)
}

#[cfg(windows)]
fn directory_identity_from_path(
    path: &Path,
) -> Result<WorkspaceDirectoryIdentity, WorkspacePathError> {
    let dir = Dir::open_ambient_dir(path, ambient_authority()).map_err(WorkspacePathError::Io)?;
    let file = dir.into_std_file();
    directory_identity_from_file(&file)
}

#[cfg(unix)]
fn directory_identity_from_file(
    file: &fs::File,
) -> Result<WorkspaceDirectoryIdentity, WorkspacePathError> {
    let metadata = file.metadata().map_err(WorkspacePathError::Io)?;
    directory_identity(&metadata)
}

#[cfg(unix)]
fn directory_identity(
    metadata: &fs::Metadata,
) -> Result<WorkspaceDirectoryIdentity, WorkspacePathError> {
    use std::os::unix::fs::MetadataExt as _;
    if !metadata.is_dir() {
        return Err(WorkspacePathError::PathDenied);
    }
    Ok(WorkspaceDirectoryIdentity::Unix {
        dev: metadata.dev(),
        ino: metadata.ino(),
    })
}

#[cfg(windows)]
fn directory_identity_from_file(
    file: &fs::File,
) -> Result<WorkspaceDirectoryIdentity, WorkspacePathError> {
    use std::os::windows::io::AsRawHandle as _;
    use windows_sys::Win32::Storage::FileSystem::{
        BY_HANDLE_FILE_INFORMATION, GetFileInformationByHandle,
    };

    let metadata = file.metadata().map_err(WorkspacePathError::Io)?;
    if !metadata.is_dir() {
        return Err(WorkspacePathError::PathDenied);
    }
    let mut info: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
    let ok = unsafe { GetFileInformationByHandle(file.as_raw_handle().cast(), &mut info) };
    if ok == 0 {
        return Err(WorkspacePathError::Io(std::io::Error::last_os_error()));
    }
    let file_index = (u64::from(info.nFileIndexHigh) << 32) | u64::from(info.nFileIndexLow);
    Ok(WorkspaceDirectoryIdentity::Windows {
        volume_serial: info.dwVolumeSerialNumber,
        file_index,
    })
}

impl fmt::Debug for ResolvedWorkspaceParent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ResolvedWorkspaceParent([redacted])")
    }
}

fn map_capability_open_error(error: std::io::Error) -> WorkspacePathError {
    if matches!(
        error.kind(),
        std::io::ErrorKind::PermissionDenied | std::io::ErrorKind::InvalidInput
    ) {
        WorkspacePathError::PathDenied
    } else {
        WorkspacePathError::Io(error)
    }
}

pub enum WorkspacePathError {
    NoRoots,
    RootNotDirectory,
    InvalidPath,
    PathDenied,
    Io(std::io::Error),
}

impl fmt::Debug for WorkspacePathError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::NoRoots => "workspace_no_roots",
            Self::RootNotDirectory => "workspace_root_not_directory",
            Self::InvalidPath => "workspace_invalid_path",
            Self::PathDenied => "workspace_path_denied",
            Self::Io(_) => "workspace_io",
        })
    }
}

impl fmt::Display for WorkspacePathError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self, f)
    }
}

impl std::error::Error for WorkspacePathError {}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_root(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "cumg-v2-workspace-path-{name}-{}-{}",
            std::process::id(),
            rand::random::<u64>()
        ));
        fs::create_dir_all(&root).unwrap();
        root
    }

    #[cfg(unix)]
    #[test]
    fn mutation_parent_replacement_is_reproved_before_publish() {
        use std::os::unix::fs::symlink;

        let root = temp_root("mutation-parent-root");
        let outside = temp_root("mutation-parent-outside");
        let parent = root.join("parent");
        let moved_parent = root.join("parent-old");
        fs::create_dir_all(&parent).unwrap();

        let roots = WorkspaceRoots::new(vec![root.clone()]).unwrap();
        let resolved = roots
            .resolve_parent_for_mutation(parent.join("note.txt").to_str().unwrap())
            .unwrap();

        fs::rename(&parent, &moved_parent).unwrap();
        symlink(&outside, &parent).unwrap();

        assert!(matches!(
            resolved.reprove_parent(),
            Err(WorkspacePathError::PathDenied)
        ));

        fs::remove_file(&parent).unwrap();
        drop(resolved);
        drop(roots);
        fs::remove_dir_all(root).unwrap();
        fs::remove_dir_all(outside).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn replacement_after_resolution_cannot_escape_capability_root() {
        use std::os::unix::fs::symlink;

        let root = temp_root("root");
        let outside = temp_root("outside");
        let candidate = root.join("candidate.txt");
        fs::write(&candidate, b"inside").unwrap();
        fs::write(outside.join("secret.txt"), b"outside").unwrap();

        let roots = WorkspaceRoots::new(vec![root.clone()]).unwrap();
        let resolved = roots.resolve_existing(candidate.to_str().unwrap()).unwrap();

        fs::remove_file(&candidate).unwrap();
        symlink(outside.join("secret.txt"), &candidate).unwrap();

        assert!(matches!(
            resolved.open_file(),
            Err(WorkspacePathError::PathDenied) | Err(WorkspacePathError::Io(_))
        ));

        fs::remove_dir_all(root).unwrap();
        fs::remove_dir_all(outside).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn replacement_directory_after_resolution_cannot_escape_capability_root() {
        use std::os::unix::fs::symlink;

        let root = temp_root("dir-root");
        let outside = temp_root("dir-outside");
        let candidate = root.join("candidate");
        fs::create_dir_all(&candidate).unwrap();
        fs::write(outside.join("secret.txt"), b"outside").unwrap();

        let roots = WorkspaceRoots::new(vec![root.clone()]).unwrap();
        let resolved = roots.resolve_existing(candidate.to_str().unwrap()).unwrap();

        fs::remove_dir(&candidate).unwrap();
        symlink(&outside, &candidate).unwrap();

        assert!(matches!(
            resolved.open_dir(),
            Err(WorkspacePathError::PathDenied) | Err(WorkspacePathError::Io(_))
        ));

        fs::remove_dir_all(root).unwrap();
        fs::remove_dir_all(outside).unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn windows_reparse_replacement_cannot_escape_capability_root() {
        use std::process::Command;

        fn junction(link: &Path, target: &Path) {
            let status = Command::new("cmd.exe")
                .args(["/D", "/C", "mklink", "/J"])
                .arg(link)
                .arg(target)
                .status()
                .expect("create junction");
            assert!(status.success());
        }

        let root = temp_root("windows-root");
        let outside = temp_root("windows-outside");
        let file_parent = root.join("file-parent");
        let directory = root.join("directory");
        fs::create_dir_all(&file_parent).unwrap();
        fs::create_dir_all(&directory).unwrap();
        fs::write(file_parent.join("note.txt"), b"inside").unwrap();
        fs::write(outside.join("note.txt"), b"outside").unwrap();

        let roots = WorkspaceRoots::new(vec![root.clone()]).unwrap();
        let resolved_file = roots
            .resolve_existing(file_parent.join("note.txt").to_str().unwrap())
            .unwrap();
        let resolved_dir = roots.resolve_existing(directory.to_str().unwrap()).unwrap();

        fs::remove_file(file_parent.join("note.txt")).unwrap();
        fs::remove_dir(&file_parent).unwrap();
        fs::remove_dir(&directory).unwrap();
        junction(&file_parent, &outside);
        junction(&directory, &outside);

        let file_denied = matches!(
            resolved_file.open_file(),
            Err(WorkspacePathError::PathDenied) | Err(WorkspacePathError::Io(_))
        );
        let directory_denied = matches!(
            resolved_dir.open_dir(),
            Err(WorkspacePathError::PathDenied) | Err(WorkspacePathError::Io(_))
        );

        assert!(file_denied);
        assert!(directory_denied);

        // Windows keeps the approved root handle open for the lifetime of the
        // capability objects. Drop them before fixture cleanup so removing the
        // temporary root does not fail with a sharing violation.
        drop(resolved_file);
        drop(resolved_dir);
        drop(roots);
        fs::remove_dir(&file_parent).unwrap();
        fs::remove_dir(&directory).unwrap();
        fs::remove_dir_all(root).unwrap();
        fs::remove_dir_all(outside).unwrap();
    }
}
