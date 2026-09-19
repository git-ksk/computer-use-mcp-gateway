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
