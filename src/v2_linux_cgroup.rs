//! Optional Linux cgroup-v2 containment for bounded process/shell execution.
//!
//! The stronger backend is opt-in and requires an explicitly delegated, dedicated,
//! empty cgroup-v2 subtree. It never infers authority from the cgroup mount alone.
//! The configured subtree is serialized: one bounded process/shell operation owns
//! it at a time, and terminal proof is cgroup.kill followed by cgroup.events
//! reporting populated 0 for the whole tree.

use std::fmt;
use std::fs::File;
#[cfg(target_os = "linux")]
use std::fs::{self, OpenOptions};
use std::io;
#[cfg(target_os = "linux")]
use std::path::Path;
use std::path::PathBuf;
use std::process::Command;
use std::sync::{
    Mutex, MutexGuard,
    atomic::{AtomicBool, AtomicU64, Ordering},
};
#[cfg(target_os = "linux")]
use std::time::{Duration, Instant};

#[cfg(target_os = "linux")]
const MAX_OPERATION_CGROUPS: usize = 4096;
#[cfg(target_os = "linux")]
const TERMINATION_PROOF_TIMEOUT: Duration = Duration::from_secs(5);
#[cfg(target_os = "linux")]
const TERMINATION_POLL: Duration = Duration::from_millis(10);

#[derive(Debug)]
pub enum LinuxCgroupError {
    UnsupportedPlatform,
    RootMustBeCanonicalAbsolute,
    RootMustBeDedicatedEmpty,
    NotCgroupV2,
    NotDomainCgroup,
    DelegationUnavailable,
    DelegationTooBroad,
    BackendPoisoned,
    Io(io::Error),
}

impl LinuxCgroupError {
    pub fn safe_code(&self) -> &'static str {
        match self {
            Self::UnsupportedPlatform => "linux_cgroup_unsupported_platform",
            Self::RootMustBeCanonicalAbsolute => "linux_cgroup_root_invalid",
            Self::RootMustBeDedicatedEmpty => "linux_cgroup_root_not_empty",
            Self::NotCgroupV2 => "linux_cgroup_not_v2",
            Self::NotDomainCgroup => "linux_cgroup_not_domain",
            Self::DelegationUnavailable => "linux_cgroup_delegation_unavailable",
            Self::DelegationTooBroad => "linux_cgroup_delegation_too_broad",
            Self::BackendPoisoned => "linux_cgroup_backend_poisoned",
            Self::Io(_) => "linux_cgroup_io",
        }
    }
}

impl fmt::Display for LinuxCgroupError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.safe_code())
    }
}

impl std::error::Error for LinuxCgroupError {}

impl From<io::Error> for LinuxCgroupError {
    fn from(value: io::Error) -> Self {
        Self::Io(value)
    }
}

#[derive(Debug)]
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub struct LinuxCgroupV2Containment {
    root: PathBuf,
    serial: Mutex<()>,
    sequence: AtomicU64,
    poisoned: AtomicBool,
}

impl LinuxCgroupV2Containment {
    pub fn new(root: PathBuf) -> Result<Self, LinuxCgroupError> {
        #[cfg(not(target_os = "linux"))]
        {
            let _ = root;
            Err(LinuxCgroupError::UnsupportedPlatform)
        }

        #[cfg(target_os = "linux")]
        {
            validate_root(&root)?;
            Ok(Self {
                root,
                serial: Mutex::new(()),
                sequence: AtomicU64::new(1),
                poisoned: AtomicBool::new(false),
            })
        }
    }

    pub(crate) fn prepare_operation(&self) -> Result<LinuxCgroupOperation<'_>, LinuxCgroupError> {
        #[cfg(not(target_os = "linux"))]
        {
            Err(LinuxCgroupError::UnsupportedPlatform)
        }

        #[cfg(target_os = "linux")]
        {
            if self.poisoned.load(Ordering::SeqCst) {
                return Err(LinuxCgroupError::BackendPoisoned);
            }
            let serial = self
                .serial
                .lock()
                .map_err(|_| LinuxCgroupError::BackendPoisoned)?;
            if self.poisoned.load(Ordering::SeqCst) {
                return Err(LinuxCgroupError::BackendPoisoned);
            }
            if !cgroup_tree_empty(&self.root)? || has_child_cgroups(&self.root)? {
                self.poisoned.store(true, Ordering::SeqCst);
                return Err(LinuxCgroupError::RootMustBeDedicatedEmpty);
            }

            let sequence = self.sequence.fetch_add(1, Ordering::SeqCst);
            let operation_root = self
                .root
                .join(format!("cumg-op-{}-{sequence}", std::process::id()));
            fs::create_dir(&operation_root).map_err(LinuxCgroupError::Io)?;
            let procs = match OpenOptions::new()
                .write(true)
                .open(operation_root.join("cgroup.procs"))
            {
                Ok(file) => file,
                Err(error) => {
                    let _ = fs::remove_dir(&operation_root);
                    return Err(LinuxCgroupError::Io(error));
                }
            };
            if !operation_root.join("cgroup.kill").is_file() {
                let _ = fs::remove_dir(&operation_root);
                return Err(LinuxCgroupError::DelegationUnavailable);
            }

            Ok(LinuxCgroupOperation {
                owner: self,
                _serial: serial,
                procs,
                active: true,
            })
        }
    }

    fn poison(&self) {
        self.poisoned.store(true, Ordering::SeqCst);
    }
}

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) struct LinuxCgroupOperation<'a> {
    owner: &'a LinuxCgroupV2Containment,
    _serial: MutexGuard<'a, ()>,
    procs: File,
    active: bool,
}

impl LinuxCgroupOperation<'_> {
    pub(crate) fn configure_command(&self, command: &mut Command) -> Result<(), LinuxCgroupError> {
        #[cfg(not(target_os = "linux"))]
        {
            let _ = command;
            Err(LinuxCgroupError::UnsupportedPlatform)
        }

        #[cfg(target_os = "linux")]
        {
            use std::os::fd::AsRawFd;
            use std::os::unix::process::CommandExt;

            let procs = self.procs.try_clone().map_err(LinuxCgroupError::Io)?;
            unsafe {
                command.pre_exec(move || {
                    let bytes = b"0\n";
                    let written = libc::write(
                        procs.as_raw_fd(),
                        bytes.as_ptr().cast::<libc::c_void>(),
                        bytes.len(),
                    );
                    if written == isize::try_from(bytes.len()).unwrap_or(-1) {
                        Ok(())
                    } else if written < 0 {
                        Err(io::Error::last_os_error())
                    } else {
                        Err(io::Error::new(
                            io::ErrorKind::WriteZero,
                            "partial cgroup.procs write",
                        ))
                    }
                });
            }
            Ok(())
        }
    }

    pub(crate) fn terminate_and_prove_empty(&mut self) -> Result<(), LinuxCgroupError> {
        #[cfg(not(target_os = "linux"))]
        {
            Err(LinuxCgroupError::UnsupportedPlatform)
        }

        #[cfg(target_os = "linux")]
        {
            if !self.active {
                return Ok(());
            }
            if let Err(error) = write_cgroup_kill(&self.owner.root)
                .and_then(|()| wait_cgroup_tree_empty(&self.owner.root))
            {
                self.owner.poison();
                return Err(error);
            }
            if let Err(error) = remove_descendant_cgroups(&self.owner.root) {
                self.owner.poison();
                return Err(error);
            }
            self.active = false;
            Ok(())
        }
    }

    pub(crate) fn spawn_failed_cleanup(&mut self) -> Result<(), LinuxCgroupError> {
        self.terminate_and_prove_empty()
    }
}

impl Drop for LinuxCgroupOperation<'_> {
    fn drop(&mut self) {
        if !self.active {
            return;
        }
        if self.terminate_and_prove_empty().is_err() {
            self.owner.poison();
        }
    }
}

#[cfg(target_os = "linux")]
fn validate_root(root: &Path) -> Result<(), LinuxCgroupError> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    if unsafe { libc::geteuid() } == 0 {
        return Err(LinuxCgroupError::DelegationTooBroad);
    }
    if !root.is_absolute() {
        return Err(LinuxCgroupError::RootMustBeCanonicalAbsolute);
    }
    let canonical = fs::canonicalize(root).map_err(LinuxCgroupError::Io)?;
    if canonical != root {
        return Err(LinuxCgroupError::RootMustBeCanonicalAbsolute);
    }
    let metadata = fs::symlink_metadata(root).map_err(LinuxCgroupError::Io)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(LinuxCgroupError::RootMustBeCanonicalAbsolute);
    }

    let c_path = CString::new(root.as_os_str().as_bytes())
        .map_err(|_| LinuxCgroupError::RootMustBeCanonicalAbsolute)?;
    let mut stat: libc::statfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statfs(c_path.as_ptr(), &mut stat) } != 0 {
        return Err(LinuxCgroupError::Io(io::Error::last_os_error()));
    }
    const CGROUP2_SUPER_MAGIC: libc::c_long = 0x6367_7270;
    if stat.f_type != CGROUP2_SUPER_MAGIC {
        return Err(LinuxCgroupError::NotCgroupV2);
    }

    for required in [
        "cgroup.controllers",
        "cgroup.events",
        "cgroup.procs",
        "cgroup.kill",
        "cgroup.type",
    ] {
        if !root.join(required).is_file() {
            return Err(LinuxCgroupError::DelegationUnavailable);
        }
    }
    let cgroup_type = fs::read_to_string(root.join("cgroup.type")).map_err(LinuxCgroupError::Io)?;
    if cgroup_type.trim() != "domain" {
        return Err(LinuxCgroupError::NotDomainCgroup);
    }
    if !cgroup_tree_empty(root)? || has_child_cgroups(root)? {
        return Err(LinuxCgroupError::RootMustBeDedicatedEmpty);
    }

    OpenOptions::new()
        .write(true)
        .open(root.join("cgroup.procs"))
        .map_err(|_| LinuxCgroupError::DelegationUnavailable)?;
    OpenOptions::new()
        .write(true)
        .open(root.join("cgroup.kill"))
        .map_err(|_| LinuxCgroupError::DelegationUnavailable)?;

    let parent = root
        .parent()
        .ok_or(LinuxCgroupError::RootMustBeCanonicalAbsolute)?;
    if OpenOptions::new()
        .write(true)
        .open(parent.join("cgroup.procs"))
        .is_ok()
    {
        return Err(LinuxCgroupError::DelegationTooBroad);
    }

    let probe = root.join(format!(
        "cumg-probe-{}-{}",
        std::process::id(),
        rand::random::<u64>()
    ));
    fs::create_dir(&probe).map_err(|_| LinuxCgroupError::DelegationUnavailable)?;
    let probe_result = (|| {
        let probe_type =
            fs::read_to_string(probe.join("cgroup.type")).map_err(LinuxCgroupError::Io)?;
        if probe_type.trim() != "domain" {
            return Err(LinuxCgroupError::NotDomainCgroup);
        }
        OpenOptions::new()
            .write(true)
            .open(probe.join("cgroup.procs"))
            .map_err(|_| LinuxCgroupError::DelegationUnavailable)?;
        OpenOptions::new()
            .write(true)
            .open(probe.join("cgroup.kill"))
            .map_err(|_| LinuxCgroupError::DelegationUnavailable)?;
        Ok(())
    })();
    let remove_result = fs::remove_dir(&probe);
    if let Err(error) = probe_result {
        let _ = remove_result;
        return Err(error);
    }
    remove_result.map_err(LinuxCgroupError::Io)?;
    Ok(())
}

#[cfg(target_os = "linux")]
fn write_cgroup_kill(root: &Path) -> Result<(), LinuxCgroupError> {
    use std::io::Write as _;

    let mut kill = OpenOptions::new()
        .write(true)
        .open(root.join("cgroup.kill"))
        .map_err(LinuxCgroupError::Io)?;
    kill.write_all(b"1\n").map_err(LinuxCgroupError::Io)
}

#[cfg(target_os = "linux")]
fn cgroup_tree_empty(root: &Path) -> Result<bool, LinuxCgroupError> {
    let events = fs::read_to_string(root.join("cgroup.events")).map_err(LinuxCgroupError::Io)?;
    for line in events.lines() {
        let mut fields = line.split_ascii_whitespace();
        if fields.next() == Some("populated") {
            return Ok(fields.next() == Some("0"));
        }
    }
    Err(LinuxCgroupError::DelegationUnavailable)
}

#[cfg(target_os = "linux")]
fn wait_cgroup_tree_empty(root: &Path) -> Result<(), LinuxCgroupError> {
    let deadline = Instant::now() + TERMINATION_PROOF_TIMEOUT;
    loop {
        if cgroup_tree_empty(root)? {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(LinuxCgroupError::DelegationUnavailable);
        }
        std::thread::sleep(TERMINATION_POLL);
    }
}

#[cfg(target_os = "linux")]
fn has_child_cgroups(root: &Path) -> Result<bool, LinuxCgroupError> {
    for entry in fs::read_dir(root).map_err(LinuxCgroupError::Io)? {
        let entry = entry.map_err(LinuxCgroupError::Io)?;
        if entry.file_type().map_err(LinuxCgroupError::Io)?.is_dir() {
            return Ok(true);
        }
    }
    Ok(false)
}

#[cfg(target_os = "linux")]
fn remove_descendant_cgroups(root: &Path) -> Result<(), LinuxCgroupError> {
    fn collect(
        path: &Path,
        depth: usize,
        seen: &mut usize,
        directories: &mut Vec<PathBuf>,
    ) -> Result<(), LinuxCgroupError> {
        if depth > MAX_OPERATION_CGROUPS {
            return Err(LinuxCgroupError::DelegationUnavailable);
        }
        for entry in fs::read_dir(path).map_err(LinuxCgroupError::Io)? {
            let entry = entry.map_err(LinuxCgroupError::Io)?;
            if !entry.file_type().map_err(LinuxCgroupError::Io)?.is_dir() {
                continue;
            }
            *seen += 1;
            if *seen > MAX_OPERATION_CGROUPS {
                return Err(LinuxCgroupError::DelegationUnavailable);
            }
            collect(&entry.path(), depth + 1, seen, directories)?;
            directories.push(entry.path());
        }
        Ok(())
    }

    let mut seen = 0;
    let mut directories = Vec::new();
    collect(root, 0, &mut seen, &mut directories)?;
    for directory in directories {
        fs::remove_dir(directory).map_err(LinuxCgroupError::Io)?;
    }
    Ok(())
}
