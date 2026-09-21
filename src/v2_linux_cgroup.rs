//! Optional Linux cgroup-v2 containment for bounded process/shell execution.
//!
//! The stronger backend is opt-in. A service manager or operator must place
//! the Agent itself in an explicitly delegated cgroup-v2 root before startup.
//! Each bounded process/shell operation is moved into a fresh child cgroup
//! before exec and then receives a private user+cgroup+mount namespace view
//! whose cgroup2 mount is rooted at that operation cgroup and read-only.
//! This prevents same-UID command code from migrating back to the Agent root.
//! Terminal proof is cgroup.kill plus cgroup.events populated=0 for the exact
//! operation subtree.

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
const CGROUP_MOUNT: &str = "/sys/fs/cgroup";
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
    RootMustBeDedicated,
    NotCgroupV2,
    NotDomainCgroup,
    DelegationUnavailable,
    DelegationTooBroad,
    AgentOutsideDelegatedRoot,
    MountLayoutUnsupported,
    NamespaceIsolationUnavailable,
    BackendPoisoned,
    Io(io::Error),
}

impl LinuxCgroupError {
    pub fn safe_code(&self) -> &'static str {
        match self {
            Self::UnsupportedPlatform => "linux_cgroup_unsupported_platform",
            Self::RootMustBeCanonicalAbsolute => "linux_cgroup_root_invalid",
            Self::RootMustBeDedicated => "linux_cgroup_root_not_dedicated",
            Self::NotCgroupV2 => "linux_cgroup_not_v2",
            Self::NotDomainCgroup => "linux_cgroup_not_domain",
            Self::DelegationUnavailable => "linux_cgroup_delegation_unavailable",
            Self::DelegationTooBroad => "linux_cgroup_delegation_too_broad",
            Self::AgentOutsideDelegatedRoot => "linux_cgroup_agent_outside_delegation",
            Self::MountLayoutUnsupported => "linux_cgroup_mount_layout_unsupported",
            Self::NamespaceIsolationUnavailable => "linux_cgroup_namespace_isolation_unavailable",
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
            let containment = Self {
                root,
                serial: Mutex::new(()),
                sequence: AtomicU64::new(1),
                poisoned: AtomicBool::new(false),
            };
            containment.probe_namespace_isolation()?;
            Ok(containment)
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
            ensure_current_process_in_root(&self.root)?;
            if has_child_cgroups(&self.root)? {
                self.poisoned.store(true, Ordering::SeqCst);
                return Err(LinuxCgroupError::RootMustBeDedicated);
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
            if OpenOptions::new()
                .write(true)
                .open(operation_root.join("cgroup.kill"))
                .is_err()
            {
                let _ = fs::remove_dir(&operation_root);
                return Err(LinuxCgroupError::DelegationUnavailable);
            }

            Ok(LinuxCgroupOperation {
                owner: self,
                _serial: serial,
                operation_root,
                procs,
                active: true,
            })
        }
    }

    #[cfg(target_os = "linux")]
    fn probe_namespace_isolation(&self) -> Result<(), LinuxCgroupError> {
        let mut operation = self.prepare_operation()?;
        let mut command = Command::new("/bin/true");
        command.env_clear();
        operation.configure_command(&mut command)?;
        let status = match command.status() {
            Ok(status) => status,
            Err(_) => {
                let _ = operation.spawn_failed_cleanup();
                return Err(LinuxCgroupError::NamespaceIsolationUnavailable);
            }
        };
        operation
            .terminate_and_prove_empty()
            .map_err(|_| LinuxCgroupError::NamespaceIsolationUnavailable)?;
        if !status.success() {
            return Err(LinuxCgroupError::NamespaceIsolationUnavailable);
        }
        Ok(())
    }

    fn poison(&self) {
        self.poisoned.store(true, Ordering::SeqCst);
    }
}

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) struct LinuxCgroupOperation<'a> {
    owner: &'a LinuxCgroupV2Containment,
    _serial: MutexGuard<'a, ()>,
    operation_root: PathBuf,
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
            let uid = unsafe { libc::geteuid() };
            let gid = unsafe { libc::getegid() };
            let uid_map = format!("{uid} {uid} 1\n").into_bytes();
            let gid_map = format!("{gid} {gid} 1\n").into_bytes();

            unsafe {
                command.pre_exec(move || {
                    write_all_raw_fd(procs.as_raw_fd(), b"0\n")?;
                    establish_private_cgroup_view(&uid_map, &gid_map)?;
                    Ok(())
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
            if let Err(error) = write_cgroup_kill(&self.operation_root)
                .and_then(|()| wait_cgroup_tree_empty(&self.operation_root))
                .and_then(|()| remove_descendant_cgroups(&self.operation_root))
                .and_then(|()| fs::remove_dir(&self.operation_root).map_err(LinuxCgroupError::Io))
            {
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

    validate_mount_layout()?;

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

    OpenOptions::new()
        .write(true)
        .open(root.join("cgroup.procs"))
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

    ensure_current_process_in_root(root)?;
    if has_child_cgroups(root)? {
        return Err(LinuxCgroupError::RootMustBeDedicated);
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
fn validate_mount_layout() -> Result<(), LinuxCgroupError> {
    let mountinfo = fs::read_to_string("/proc/self/mountinfo").map_err(LinuxCgroupError::Io)?;
    let mut cgroup2 = Vec::new();
    for line in mountinfo.lines() {
        let Some((before, after)) = line.split_once(" - ") else {
            continue;
        };
        let mut after_fields = after.split_ascii_whitespace();
        if after_fields.next() != Some("cgroup2") {
            continue;
        }
        let fields = before.split_ascii_whitespace().collect::<Vec<_>>();
        if fields.len() < 5 {
            return Err(LinuxCgroupError::MountLayoutUnsupported);
        }
        cgroup2.push((fields[3], fields[4]));
    }
    if cgroup2.as_slice() != [("/", CGROUP_MOUNT)] {
        return Err(LinuxCgroupError::MountLayoutUnsupported);
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn ensure_current_process_in_root(root: &Path) -> Result<(), LinuxCgroupError> {
    let cgroup = fs::read_to_string("/proc/self/cgroup").map_err(LinuxCgroupError::Io)?;
    let relative = cgroup
        .lines()
        .find_map(|line| line.strip_prefix("0::"))
        .ok_or(LinuxCgroupError::NotCgroupV2)?;
    let expected = if relative == "/" {
        PathBuf::from(CGROUP_MOUNT)
    } else {
        Path::new(CGROUP_MOUNT).join(relative.trim_start_matches('/'))
    };
    let expected = fs::canonicalize(expected).map_err(LinuxCgroupError::Io)?;
    if expected != root {
        return Err(LinuxCgroupError::AgentOutsideDelegatedRoot);
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn write_all_raw_fd(fd: libc::c_int, bytes: &[u8]) -> io::Result<()> {
    let mut offset = 0;
    while offset < bytes.len() {
        let written = unsafe {
            libc::write(
                fd,
                bytes[offset..].as_ptr().cast::<libc::c_void>(),
                bytes.len() - offset,
            )
        };
        if written < 0 {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(error);
        }
        if written == 0 {
            return Err(io::Error::new(io::ErrorKind::WriteZero, "short write"));
        }
        offset += usize::try_from(written).map_err(|_| io::Error::other("invalid write result"))?;
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn write_proc_control(path: *const libc::c_char, bytes: &[u8], optional: bool) -> io::Result<()> {
    let fd = unsafe { libc::open(path, libc::O_WRONLY | libc::O_CLOEXEC) };
    if fd < 0 {
        let error = io::Error::last_os_error();
        if optional && error.raw_os_error() == Some(libc::ENOENT) {
            return Ok(());
        }
        return Err(error);
    }
    let result = write_all_raw_fd(fd, bytes);
    unsafe {
        libc::close(fd);
    }
    result
}

#[cfg(target_os = "linux")]
fn establish_private_cgroup_view(uid_map: &[u8], gid_map: &[u8]) -> io::Result<()> {
    const SETGROUPS: &[u8] = b"/proc/self/setgroups\0";
    const UID_MAP: &[u8] = b"/proc/self/uid_map\0";
    const GID_MAP: &[u8] = b"/proc/self/gid_map\0";
    const CGROUP_TARGET: &[u8] = b"/sys/fs/cgroup\0";
    const CGROUP_SOURCE: &[u8] = b"none\0";
    const CGROUP_FSTYPE: &[u8] = b"cgroup2\0";
    const ROOT: &[u8] = b"/\0";

    let flags = libc::CLONE_NEWUSER | libc::CLONE_NEWCGROUP | libc::CLONE_NEWNS;
    if unsafe { libc::unshare(flags) } != 0 {
        return Err(io::Error::last_os_error());
    }

    write_proc_control(SETGROUPS.as_ptr().cast::<libc::c_char>(), b"deny\n", true)?;
    write_proc_control(UID_MAP.as_ptr().cast::<libc::c_char>(), uid_map, false)?;
    write_proc_control(GID_MAP.as_ptr().cast::<libc::c_char>(), gid_map, false)?;

    let private_flags = libc::MS_REC | libc::MS_PRIVATE;
    if unsafe {
        libc::mount(
            std::ptr::null(),
            ROOT.as_ptr().cast::<libc::c_char>(),
            std::ptr::null(),
            private_flags,
            std::ptr::null(),
        )
    } != 0
    {
        return Err(io::Error::last_os_error());
    }

    let cgroup_flags = libc::MS_RDONLY | libc::MS_NOSUID | libc::MS_NODEV | libc::MS_NOEXEC;
    if unsafe {
        libc::mount(
            CGROUP_SOURCE.as_ptr().cast::<libc::c_char>(),
            CGROUP_TARGET.as_ptr().cast::<libc::c_char>(),
            CGROUP_FSTYPE.as_ptr().cast::<libc::c_char>(),
            cgroup_flags,
            std::ptr::null(),
        )
    } != 0
    {
        return Err(io::Error::last_os_error());
    }

    if unsafe { libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) } != 0 {
        return Err(io::Error::last_os_error());
    }
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
