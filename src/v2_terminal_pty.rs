#![allow(dead_code)] // #48 experimental/internal until CUMG dogfood admission is complete.

//! Experimental/internal PTY mechanics for Handoff #48.
//!
//! This module deliberately owns no Agent/Human authority semantics. Callers must pass every
//! input/output/resize operation through the first-class Handoff coordinator before reaching this
//! object. The PTY owns only one opaque session, bounded byte buffering, writer-drain boundaries,
//! resize, process exit, and deterministic resource limits. Terminal bytes are never formatted into
//! errors, Debug output, durable state, or generic diagnostics here.

use portable_pty::{Child, CommandBuilder, MasterPty, PtySize, native_pty_system};
use rand::{RngCore, rngs::OsRng};
use std::{
    collections::{HashSet, VecDeque},
    ffi::OsString,
    fmt, fs,
    io::{Read, Write},
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

pub(crate) const MAX_PTY_INPUT_BYTES: usize = 32 * 1024;
pub(crate) const MAX_PTY_OUTPUT_BUFFER_BYTES: usize = 256 * 1024;
pub(crate) const MAX_PTY_READ_BYTES: usize = 64 * 1024;
pub(crate) const MAX_PTY_ARGS: usize = 64;
pub(crate) const MAX_PTY_ENV: usize = 32;
pub(crate) const MAX_PTY_ROWS: u16 = 200;
pub(crate) const MAX_PTY_COLS: u16 = 400;
pub(crate) const MAX_PTY_SESSION_LIFETIME: Duration = Duration::from_secs(30 * 60);
const HUMAN_CONTEXT_TAIL_BYTES: u64 = 16 * 1024;

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct TerminalPtyBinding {
    session_id: String,
    generation: u64,
}

impl TerminalPtyBinding {
    pub(crate) fn session_id(&self) -> &str {
        &self.session_id
    }

    pub(crate) fn generation(&self) -> u64 {
        self.generation
    }
}

impl fmt::Debug for TerminalPtyBinding {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TerminalPtyBinding")
            .field("session_id", &"[redacted]")
            .field("generation", &self.generation)
            .finish()
    }
}

pub(crate) struct TerminalPtySpawnConfig {
    pub program: PathBuf,
    pub args: Vec<OsString>,
    pub cwd: PathBuf,
    pub env: Vec<(OsString, OsString)>,
    pub rows: u16,
    pub cols: u16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TerminalPtyProcessState {
    Running,
    Exited { success: bool, exit_code: u32 },
    Closed,
}

pub(crate) struct TerminalPtyOutput {
    bytes: Vec<u8>,
    pub truncated_before_cursor: bool,
}

impl TerminalPtyOutput {
    pub(crate) fn as_bytes(&self) -> &[u8] {
        &self.bytes
    }
}

impl fmt::Debug for TerminalPtyOutput {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TerminalPtyOutput")
            .field("bytes", &"[redacted]")
            .field("byte_len", &self.bytes.len())
            .field("truncated_before_cursor", &self.truncated_before_cursor)
            .finish()
    }
}

#[derive(Debug)]
pub(crate) enum TerminalPtyError {
    SessionAlreadyExists,
    SessionMissing,
    SessionMismatch,
    SessionClosed,
    SessionExpired,
    SessionStillRunning,
    InvalidSpawn,
    InvalidInput,
    InvalidReadBound,
    InvalidSize,
    Io,
}

impl fmt::Display for TerminalPtyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::SessionAlreadyExists => "terminal PTY session already exists",
            Self::SessionMissing => "terminal PTY session is missing",
            Self::SessionMismatch => "terminal PTY session binding mismatch",
            Self::SessionClosed => "terminal PTY session is closed",
            Self::SessionExpired => "terminal PTY session expired",
            Self::SessionStillRunning => "terminal PTY session is still running",
            Self::InvalidSpawn => "terminal PTY spawn configuration invalid",
            Self::InvalidInput => "terminal PTY input invalid",
            Self::InvalidReadBound => "terminal PTY read bound invalid",
            Self::InvalidSize => "terminal PTY size invalid",
            Self::Io => "terminal PTY operation failed",
        })
    }
}

impl std::error::Error for TerminalPtyError {}

impl TerminalPtyError {
    pub(crate) fn closes_session(&self) -> bool {
        matches!(self, Self::SessionClosed | Self::SessionExpired | Self::Io)
    }
}

#[derive(Default)]
struct OutputRing {
    bytes: VecDeque<u8>,
    start_offset: u64,
    next_offset: u64,
}

impl OutputRing {
    fn push(&mut self, input: &[u8]) {
        if input.is_empty() {
            return;
        }
        self.next_offset = self
            .next_offset
            .saturating_add(u64::try_from(input.len()).unwrap_or(u64::MAX));
        if input.len() >= MAX_PTY_OUTPUT_BUFFER_BYTES {
            self.bytes.clear();
            self.bytes.extend(
                input[input.len() - MAX_PTY_OUTPUT_BUFFER_BYTES..]
                    .iter()
                    .copied(),
            );
            self.start_offset = self
                .next_offset
                .saturating_sub(u64::try_from(self.bytes.len()).unwrap_or(u64::MAX));
            return;
        }
        self.bytes.extend(input.iter().copied());
        while self.bytes.len() > MAX_PTY_OUTPUT_BUFFER_BYTES {
            self.bytes.pop_front();
            self.start_offset = self.start_offset.saturating_add(1);
        }
    }

    fn read_from(&self, cursor: u64, max_bytes: usize) -> (Vec<u8>, u64, bool) {
        let truncated = cursor < self.start_offset;
        let effective = cursor.max(self.start_offset).min(self.next_offset);
        let available = self.next_offset.saturating_sub(effective);
        let take = usize::try_from(available)
            .unwrap_or(usize::MAX)
            .min(max_bytes);
        let start_index = usize::try_from(effective.saturating_sub(self.start_offset))
            .unwrap_or(self.bytes.len())
            .min(self.bytes.len());
        let bytes = self
            .bytes
            .iter()
            .skip(start_index)
            .take(take)
            .copied()
            .collect::<Vec<_>>();
        let next = effective.saturating_add(u64::try_from(bytes.len()).unwrap_or(u64::MAX));
        (bytes, next, truncated)
    }
}

struct TerminalPtySession {
    binding: TerminalPtyBinding,
    created_at: Instant,
    root_pid: u32,
    master: Box<dyn MasterPty + Send>,
    writer: Mutex<Box<dyn Write + Send>>,
    child: Mutex<Box<dyn Child + Send + Sync>>,
    output: Arc<Mutex<OutputRing>>,
    reader_closed: Arc<AtomicBool>,
    closed: AtomicBool,
    cleanup_complete: AtomicBool,
    agent_cursor: Mutex<u64>,
    human_cursor: Mutex<u64>,
}

impl TerminalPtySession {
    fn spawn(
        binding: TerminalPtyBinding,
        config: TerminalPtySpawnConfig,
    ) -> Result<Self, TerminalPtyError> {
        validate_spawn(&config)?;
        let pty = native_pty_system();
        let pair = pty
            .openpty(PtySize {
                rows: config.rows,
                cols: config.cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|_| TerminalPtyError::Io)?;

        // Prepare the master I/O handles before spawning the child so setup failure cannot leave
        // a live PTY child behind.
        let mut reader = pair
            .master
            .try_clone_reader()
            .map_err(|_| TerminalPtyError::Io)?;
        let writer = pair
            .master
            .take_writer()
            .map_err(|_| TerminalPtyError::Io)?;

        let mut command = CommandBuilder::new(&config.program);
        command.args(config.args);
        command.cwd(&config.cwd);
        command.env_clear();
        for (key, value) in config.env {
            command.env(key, value);
        }
        let mut child = pair
            .slave
            .spawn_command(command)
            .map_err(|_| TerminalPtyError::Io)?;
        let Some(root_pid) = child.process_id() else {
            let _ = child.kill();
            let _ = child.wait();
            return Err(TerminalPtyError::Io);
        };
        drop(pair.slave);
        let output = Arc::new(Mutex::new(OutputRing::default()));
        let reader_closed = Arc::new(AtomicBool::new(false));
        let output_for_reader = Arc::clone(&output);
        let closed_for_reader = Arc::clone(&reader_closed);
        if thread::Builder::new()
            .name("cumg-v2-terminal-pty-reader".into())
            .spawn(move || {
                let mut chunk = [0_u8; 8 * 1024];
                loop {
                    match reader.read(&mut chunk) {
                        Ok(0) => break,
                        Ok(read) => {
                            if let Ok(mut ring) = output_for_reader.lock() {
                                ring.push(&chunk[..read]);
                            } else {
                                break;
                            }
                        }
                        Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                        Err(_) => break,
                    }
                }
                closed_for_reader.store(true, Ordering::Release);
            })
            .is_err()
        {
            let _ = child.kill();
            let _ = child.wait();
            return Err(TerminalPtyError::Io);
        }

        Ok(Self {
            binding,
            created_at: Instant::now(),
            root_pid,
            master: pair.master,
            writer: Mutex::new(writer),
            child: Mutex::new(child),
            output,
            reader_closed,
            closed: AtomicBool::new(false),
            cleanup_complete: AtomicBool::new(false),
            agent_cursor: Mutex::new(0),
            human_cursor: Mutex::new(0),
        })
    }

    fn ensure_identity(&self, binding: &TerminalPtyBinding) -> Result<(), TerminalPtyError> {
        if &self.binding != binding {
            return Err(TerminalPtyError::SessionMismatch);
        }
        Ok(())
    }

    fn terminate_and_reap(&self) -> Result<(), TerminalPtyError> {
        if self.cleanup_complete.load(Ordering::Acquire) {
            return Ok(());
        }
        self.closed.store(true, Ordering::Release);
        let mut child = self.child.lock().map_err(|_| TerminalPtyError::Io)?;
        let root_running = child
            .try_wait()
            .map_err(|_| TerminalPtyError::Io)?
            .is_none();

        #[cfg(unix)]
        let session_members = match fence_unix_terminal_session(self.root_pid, root_running) {
            Ok(members) => members,
            Err(error) => {
                if root_running {
                    let _ = child.kill();
                    let _ = child.wait();
                }
                return Err(error);
            }
        };

        if root_running {
            if child.kill().is_err()
                && child
                    .try_wait()
                    .map_err(|_| TerminalPtyError::Io)?
                    .is_none()
            {
                return Err(TerminalPtyError::Io);
            }
            child.wait().map_err(|_| TerminalPtyError::Io)?;
        }
        drop(child);

        #[cfg(unix)]
        terminate_fenced_unix_session_members(self.root_pid, &session_members)?;
        self.cleanup_complete.store(true, Ordering::Release);
        Ok(())
    }

    fn ensure_open(&self, binding: &TerminalPtyBinding) -> Result<(), TerminalPtyError> {
        self.ensure_identity(binding)?;
        if self.created_at.elapsed() > MAX_PTY_SESSION_LIFETIME {
            self.terminate_and_reap()?;
            return Err(TerminalPtyError::SessionExpired);
        }
        if self.closed.load(Ordering::Acquire) {
            return Err(TerminalPtyError::SessionClosed);
        }
        Ok(())
    }

    fn write(&self, binding: &TerminalPtyBinding, bytes: &[u8]) -> Result<(), TerminalPtyError> {
        self.ensure_open(binding)?;
        if bytes.is_empty() || bytes.len() > MAX_PTY_INPUT_BYTES {
            return Err(TerminalPtyError::InvalidInput);
        }
        let mut writer = self.writer.lock().map_err(|_| TerminalPtyError::Io)?;
        writer.write_all(bytes).map_err(|_| TerminalPtyError::Io)?;
        writer.flush().map_err(|_| TerminalPtyError::Io)
    }

    fn drain_writes(&self, binding: &TerminalPtyBinding) -> Result<(), TerminalPtyError> {
        self.ensure_open(binding)?;
        let mut writer = self.writer.lock().map_err(|_| TerminalPtyError::Io)?;
        writer.flush().map_err(|_| TerminalPtyError::Io)
    }

    fn resize(
        &self,
        binding: &TerminalPtyBinding,
        rows: u16,
        cols: u16,
    ) -> Result<(), TerminalPtyError> {
        self.ensure_open(binding)?;
        validate_size(rows, cols)?;
        self.master
            .resize(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|_| TerminalPtyError::Io)
    }

    fn begin_human_output(&self, binding: &TerminalPtyBinding) -> Result<(), TerminalPtyError> {
        self.ensure_open(binding)?;
        let ring = self.output.lock().map_err(|_| TerminalPtyError::Io)?;
        let cursor = ring
            .next_offset
            .saturating_sub(HUMAN_CONTEXT_TAIL_BYTES)
            .max(ring.start_offset);
        *self.human_cursor.lock().map_err(|_| TerminalPtyError::Io)? = cursor;
        Ok(())
    }

    fn finish_human_output(&self, binding: &TerminalPtyBinding) -> Result<(), TerminalPtyError> {
        self.ensure_identity(binding)?;
        let next = self
            .output
            .lock()
            .map_err(|_| TerminalPtyError::Io)?
            .next_offset;
        *self.agent_cursor.lock().map_err(|_| TerminalPtyError::Io)? = next;
        Ok(())
    }

    fn read_agent(
        &self,
        binding: &TerminalPtyBinding,
        max_bytes: usize,
    ) -> Result<TerminalPtyOutput, TerminalPtyError> {
        self.ensure_open(binding)?;
        read_cursor(&self.output, &self.agent_cursor, max_bytes)
    }

    fn read_human(
        &self,
        binding: &TerminalPtyBinding,
        max_bytes: usize,
    ) -> Result<TerminalPtyOutput, TerminalPtyError> {
        self.ensure_open(binding)?;
        read_cursor(&self.output, &self.human_cursor, max_bytes)
    }

    fn process_state(
        &self,
        binding: &TerminalPtyBinding,
    ) -> Result<TerminalPtyProcessState, TerminalPtyError> {
        self.ensure_identity(binding)?;
        if self.created_at.elapsed() > MAX_PTY_SESSION_LIFETIME {
            self.terminate_and_reap()?;
            return Ok(TerminalPtyProcessState::Closed);
        }
        if self.closed.load(Ordering::Acquire) {
            return Ok(TerminalPtyProcessState::Closed);
        }
        let mut child = self.child.lock().map_err(|_| TerminalPtyError::Io)?;
        match child.try_wait().map_err(|_| TerminalPtyError::Io)? {
            Some(status) => {
                let success = status.success();
                let exit_code = status.exit_code();
                self.closed.store(true, Ordering::Release);
                drop(child);
                self.terminate_and_reap()?;
                Ok(TerminalPtyProcessState::Exited { success, exit_code })
            }
            None if self.reader_closed.load(Ordering::Acquire) => {
                // Reader closure without a child status is not proof of success. Fence and reap.
                drop(child);
                self.terminate_and_reap()?;
                Ok(TerminalPtyProcessState::Closed)
            }
            None => Ok(TerminalPtyProcessState::Running),
        }
    }

    fn close(&self, binding: &TerminalPtyBinding) -> Result<(), TerminalPtyError> {
        self.ensure_identity(binding)?;
        self.terminate_and_reap()
    }
}

impl Drop for TerminalPtySession {
    fn drop(&mut self) {
        let _ = self.terminate_and_reap();
    }
}

pub(crate) struct TerminalPtyManager {
    session: Option<TerminalPtySession>,
    next_generation: u64,
}

impl Default for TerminalPtyManager {
    fn default() -> Self {
        Self {
            session: None,
            next_generation: 1,
        }
    }
}

impl TerminalPtyManager {
    pub(crate) fn spawn(
        &mut self,
        config: TerminalPtySpawnConfig,
    ) -> Result<TerminalPtyBinding, TerminalPtyError> {
        if self.session.is_some() {
            return Err(TerminalPtyError::SessionAlreadyExists);
        }
        let generation = self.next_generation;
        self.next_generation = self
            .next_generation
            .checked_add(1)
            .ok_or(TerminalPtyError::Io)?;
        let binding = TerminalPtyBinding {
            session_id: random_session_id(),
            generation,
        };
        self.session = Some(TerminalPtySession::spawn(binding.clone(), config)?);
        Ok(binding)
    }

    pub(crate) fn write(
        &self,
        binding: &TerminalPtyBinding,
        bytes: &[u8],
    ) -> Result<(), TerminalPtyError> {
        self.session()?.write(binding, bytes)
    }

    pub(crate) fn drain_writes(
        &self,
        binding: &TerminalPtyBinding,
    ) -> Result<(), TerminalPtyError> {
        self.session()?.drain_writes(binding)
    }

    pub(crate) fn resize(
        &self,
        binding: &TerminalPtyBinding,
        rows: u16,
        cols: u16,
    ) -> Result<(), TerminalPtyError> {
        self.session()?.resize(binding, rows, cols)
    }

    pub(crate) fn begin_human_output(
        &self,
        binding: &TerminalPtyBinding,
    ) -> Result<(), TerminalPtyError> {
        self.session()?.begin_human_output(binding)
    }

    pub(crate) fn finish_human_output(
        &self,
        binding: &TerminalPtyBinding,
    ) -> Result<(), TerminalPtyError> {
        self.session()?.finish_human_output(binding)
    }

    pub(crate) fn read_agent(
        &self,
        binding: &TerminalPtyBinding,
        max_bytes: usize,
    ) -> Result<TerminalPtyOutput, TerminalPtyError> {
        self.session()?.read_agent(binding, max_bytes)
    }

    pub(crate) fn read_human(
        &self,
        binding: &TerminalPtyBinding,
        max_bytes: usize,
    ) -> Result<TerminalPtyOutput, TerminalPtyError> {
        self.session()?.read_human(binding, max_bytes)
    }

    pub(crate) fn process_state(
        &self,
        binding: &TerminalPtyBinding,
    ) -> Result<TerminalPtyProcessState, TerminalPtyError> {
        self.session()?.process_state(binding)
    }

    pub(crate) fn terminate(&self, binding: &TerminalPtyBinding) -> Result<(), TerminalPtyError> {
        self.session()?.close(binding)
    }

    pub(crate) fn release(&mut self, binding: &TerminalPtyBinding) -> Result<(), TerminalPtyError> {
        let session = self.session()?;
        session.ensure_identity(binding)?;
        if !session.closed.load(Ordering::Acquire) {
            return Err(TerminalPtyError::SessionStillRunning);
        }
        self.session = None;
        Ok(())
    }

    pub(crate) fn close(&mut self, binding: &TerminalPtyBinding) -> Result<(), TerminalPtyError> {
        self.terminate(binding)?;
        self.release(binding)
    }

    fn session(&self) -> Result<&TerminalPtySession, TerminalPtyError> {
        self.session
            .as_ref()
            .ok_or(TerminalPtyError::SessionMissing)
    }
}

#[cfg(unix)]
#[derive(Clone, Copy)]
struct UnixProcessSnapshot {
    pid: u32,
    session_id: u32,
    zombie: bool,
}

#[cfg(unix)]
const MAX_UNIX_PROCESS_SNAPSHOT_BYTES: usize = 2 * 1024 * 1024;
#[cfg(unix)]
const MAX_UNIX_PROCESS_SNAPSHOT_ENTRIES: usize = 32 * 1024;
#[cfg(unix)]
const UNIX_PROCESS_SNAPSHOT_TIMEOUT: Duration = Duration::from_millis(500);

#[cfg(unix)]
fn unix_process_snapshot() -> Result<Vec<UnixProcessSnapshot>, TerminalPtyError> {
    let ps = ["/bin/ps", "/usr/bin/ps"]
        .into_iter()
        .find(|candidate| PathBuf::from(candidate).is_file())
        .ok_or(TerminalPtyError::Io)?;
    let mut child = std::process::Command::new(ps)
        .args(["-axo", "pid=,stat="])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|_| TerminalPtyError::Io)?;
    let deadline = Instant::now() + UNIX_PROCESS_SNAPSHOT_TIMEOUT;
    loop {
        match child.try_wait().map_err(|_| TerminalPtyError::Io)? {
            Some(_) => break,
            None if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(TerminalPtyError::Io);
            }
            None => thread::sleep(Duration::from_millis(10)),
        }
    }
    let output = child.wait_with_output().map_err(|_| TerminalPtyError::Io)?;
    if !output.status.success()
        || output.stdout.len() > MAX_UNIX_PROCESS_SNAPSHOT_BYTES
        || !output.stderr.is_empty()
    {
        return Err(TerminalPtyError::Io);
    }
    let text = std::str::from_utf8(&output.stdout).map_err(|_| TerminalPtyError::Io)?;
    let mut processes = Vec::new();
    for line in text.lines() {
        let mut fields = line.split_whitespace();
        let pid = fields
            .next()
            .ok_or(TerminalPtyError::Io)?
            .parse::<u32>()
            .map_err(|_| TerminalPtyError::Io)?;
        let stat = fields.next().ok_or(TerminalPtyError::Io)?;
        if fields.next().is_some() || stat.len() > 16 {
            return Err(TerminalPtyError::Io);
        }
        let raw_pid = i32::try_from(pid).map_err(|_| TerminalPtyError::Io)?;
        let raw_session_id = unsafe { libc::getsid(raw_pid) };
        if raw_session_id < 0 {
            let error = std::io::Error::last_os_error();
            if error.raw_os_error() == Some(libc::ESRCH) {
                continue;
            }
            return Err(TerminalPtyError::Io);
        }
        let session_id = u32::try_from(raw_session_id).map_err(|_| TerminalPtyError::Io)?;
        processes.push(UnixProcessSnapshot {
            pid,
            session_id,
            zombie: stat.starts_with('Z'),
        });
        if processes.len() > MAX_UNIX_PROCESS_SNAPSHOT_ENTRIES {
            return Err(TerminalPtyError::Io);
        }
    }
    Ok(processes)
}

#[cfg(unix)]
fn terminal_session_members(root_pid: u32, processes: &[UnixProcessSnapshot]) -> Vec<u32> {
    let mut members = processes
        .iter()
        .filter(|process| {
            process.pid != root_pid && process.session_id == root_pid && !process.zombie
        })
        .map(|process| process.pid)
        .collect::<Vec<_>>();
    members.sort_unstable();
    members.dedup();
    members
}

#[cfg(unix)]
fn signal_unix_process(pid: u32, signal: i32) -> Result<(), TerminalPtyError> {
    let pid = i32::try_from(pid).map_err(|_| TerminalPtyError::Io)?;
    let result = unsafe { libc::kill(pid, signal) };
    if result == 0 {
        return Ok(());
    }
    let error = std::io::Error::last_os_error();
    if error.raw_os_error() == Some(libc::ESRCH) {
        Ok(())
    } else {
        Err(TerminalPtyError::Io)
    }
}

#[cfg(unix)]
fn fence_unix_terminal_session(
    root_pid: u32,
    root_running: bool,
) -> Result<Vec<u32>, TerminalPtyError> {
    if root_running {
        signal_unix_process(root_pid, libc::SIGSTOP)?;
    }
    let mut fenced = HashSet::new();
    for _ in 0..3 {
        let snapshot = unix_process_snapshot()?;
        let discovered = terminal_session_members(root_pid, &snapshot);
        let mut changed = false;
        for pid in discovered {
            if fenced.insert(pid) {
                signal_unix_process(pid, libc::SIGSTOP)?;
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    let mut fenced = fenced.into_iter().collect::<Vec<_>>();
    fenced.sort_unstable();
    Ok(fenced)
}

#[cfg(unix)]
fn terminate_fenced_unix_session_members(
    root_pid: u32,
    members: &[u32],
) -> Result<(), TerminalPtyError> {
    let mut signal_failed = false;
    for &pid in members.iter().rev() {
        if signal_unix_process(pid, libc::SIGKILL).is_err() {
            signal_failed = true;
        }
    }
    let deadline = Instant::now() + Duration::from_millis(500);
    loop {
        let snapshot = unix_process_snapshot()?;
        let live = members.iter().any(|pid| {
            snapshot.iter().any(|process| {
                process.pid == *pid && process.session_id == root_pid && !process.zombie
            })
        });
        if !live {
            return if signal_failed {
                Err(TerminalPtyError::Io)
            } else {
                Ok(())
            };
        }
        if Instant::now() >= deadline {
            return Err(TerminalPtyError::Io);
        }
        thread::sleep(Duration::from_millis(20));
    }
}

fn read_cursor(
    output: &Mutex<OutputRing>,
    cursor: &Mutex<u64>,
    max_bytes: usize,
) -> Result<TerminalPtyOutput, TerminalPtyError> {
    if max_bytes == 0 || max_bytes > MAX_PTY_READ_BYTES {
        return Err(TerminalPtyError::InvalidReadBound);
    }
    let ring = output.lock().map_err(|_| TerminalPtyError::Io)?;
    let mut cursor = cursor.lock().map_err(|_| TerminalPtyError::Io)?;
    let (bytes, next, truncated_before_cursor) = ring.read_from(*cursor, max_bytes);
    *cursor = next;
    Ok(TerminalPtyOutput {
        bytes,
        truncated_before_cursor,
    })
}

fn validate_spawn(config: &TerminalPtySpawnConfig) -> Result<(), TerminalPtyError> {
    validate_size(config.rows, config.cols)?;
    if !config.program.is_absolute()
        || !config.cwd.is_absolute()
        || config.args.len() > MAX_PTY_ARGS
        || config.env.len() > MAX_PTY_ENV
    {
        return Err(TerminalPtyError::InvalidSpawn);
    }
    let program =
        fs::symlink_metadata(&config.program).map_err(|_| TerminalPtyError::InvalidSpawn)?;
    if !program.file_type().is_file() || program.file_type().is_symlink() {
        return Err(TerminalPtyError::InvalidSpawn);
    }
    let cwd = fs::metadata(&config.cwd).map_err(|_| TerminalPtyError::InvalidSpawn)?;
    if !cwd.is_dir() {
        return Err(TerminalPtyError::InvalidSpawn);
    }
    if config
        .args
        .iter()
        .any(|value| invalid_os_value(value, 4096))
        || config
            .env
            .iter()
            .any(|(key, value)| invalid_env_key(key) || invalid_os_value(value, 4096))
    {
        return Err(TerminalPtyError::InvalidSpawn);
    }
    Ok(())
}

fn validate_size(rows: u16, cols: u16) -> Result<(), TerminalPtyError> {
    if !(2..=MAX_PTY_ROWS).contains(&rows) || !(2..=MAX_PTY_COLS).contains(&cols) {
        return Err(TerminalPtyError::InvalidSize);
    }
    Ok(())
}

fn invalid_env_key(value: &OsString) -> bool {
    let Some(value) = value.to_str() else {
        return true;
    };
    value.is_empty()
        || value.len() > 128
        || value.contains('=')
        || value.contains('\0')
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
}

fn invalid_os_value(value: &OsString, max_bytes: usize) -> bool {
    let Some(value) = value.to_str() else {
        return true;
    };
    value.len() > max_bytes || value.contains('\0')
}

fn random_session_id() -> String {
    let mut bytes = [0_u8; 16];
    OsRng.fill_bytes(&mut bytes);
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        use std::fmt::Write as _;
        let _ = write!(&mut output, "{byte:02x}");
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{ffi::OsString, path::Path, thread::sleep};

    fn config(program: &Path, cwd: &Path) -> TerminalPtySpawnConfig {
        TerminalPtySpawnConfig {
            program: program.to_path_buf(),
            args: Vec::new(),
            cwd: cwd.to_path_buf(),
            env: vec![(OsString::from("TERM"), OsString::from("xterm-256color"))],
            rows: 24,
            cols: 80,
        }
    }

    #[test]
    fn privacy_debug_redacts_session_identity_and_terminal_bytes() {
        let binding = TerminalPtyBinding {
            session_id: "private-session-id".into(),
            generation: 4,
        };
        let output = TerminalPtyOutput {
            bytes: b"password-token-secret".to_vec(),
            truncated_before_cursor: false,
        };
        assert!(!format!("{binding:?}").contains("private-session-id"));
        assert!(!format!("{output:?}").contains("password-token-secret"));
    }

    #[test]
    fn size_input_and_read_bounds_fail_closed() {
        assert!(matches!(
            validate_size(1, 80),
            Err(TerminalPtyError::InvalidSize)
        ));
        assert!(matches!(
            validate_size(24, 401),
            Err(TerminalPtyError::InvalidSize)
        ));
        let ring = Mutex::new(OutputRing::default());
        let cursor = Mutex::new(0);
        assert!(matches!(
            read_cursor(&ring, &cursor, 0),
            Err(TerminalPtyError::InvalidReadBound)
        ));
        assert!(matches!(
            read_cursor(&ring, &cursor, MAX_PTY_READ_BYTES + 1),
            Err(TerminalPtyError::InvalidReadBound)
        ));
    }

    #[test]
    fn output_ring_is_bounded_and_reports_cursor_truncation() {
        let mut ring = OutputRing::default();
        ring.push(&vec![b'x'; MAX_PTY_OUTPUT_BUFFER_BYTES + 17]);
        assert_eq!(ring.bytes.len(), MAX_PTY_OUTPUT_BUFFER_BYTES);
        let (read, _, truncated) = ring.read_from(0, 32);
        assert_eq!(read.len(), 32);
        assert!(truncated);
    }

    #[cfg(unix)]
    #[test]
    fn expired_pty_is_reaped_and_session_slot_can_be_reused() {
        let program = Path::new("/bin/cat");
        if !program.exists() {
            return;
        }
        let mut manager = TerminalPtyManager::default();
        let binding = manager.spawn(config(program, Path::new("/tmp"))).unwrap();
        manager.session.as_mut().unwrap().created_at =
            Instant::now() - MAX_PTY_SESSION_LIFETIME - Duration::from_secs(1);

        assert_eq!(
            manager.process_state(&binding).unwrap(),
            TerminalPtyProcessState::Closed
        );
        {
            let mut child = manager.session.as_ref().unwrap().child.lock().unwrap();
            assert!(
                child.try_wait().unwrap().is_some(),
                "expired PTY child must be reaped"
            );
        }
        manager.close(&binding).unwrap();
        let replacement = manager.spawn(config(program, Path::new("/tmp"))).unwrap();
        assert_eq!(replacement.generation(), 2);
        manager.close(&replacement).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn close_contains_nohup_background_descendant() {
        use std::fs;

        let Ok(shell) = fs::canonicalize("/bin/sh") else {
            return;
        };
        let sleep_executable = ["/bin/sleep", "/usr/bin/sleep"]
            .into_iter()
            .find(|candidate| Path::new(candidate).is_file());
        let nohup = ["/usr/bin/nohup", "/bin/nohup"]
            .into_iter()
            .find(|candidate| Path::new(candidate).is_file());
        let (Some(sleep_executable), Some(nohup)) = (sleep_executable, nohup) else {
            return;
        };
        if !shell.is_file() {
            return;
        }

        let mut random = [0_u8; 8];
        OsRng.fill_bytes(&mut random);
        let temp = std::env::temp_dir().join(format!(
            "cumg-terminal-pty-descendant-{}-{}",
            std::process::id(),
            u64::from_le_bytes(random),
        ));
        fs::create_dir(&temp).unwrap();
        let pid_file = temp.join("background.pid");
        let command =
            format!("{nohup} {sleep_executable} 3 >/dev/null 2>&1 & echo $! > \"$PID_FILE\"; wait");
        let mut spawn = config(&shell, &temp);
        spawn.args = vec![OsString::from("-c"), OsString::from(command)];
        spawn.env.push((
            OsString::from("PID_FILE"),
            pid_file.clone().into_os_string(),
        ));

        let mut manager = TerminalPtyManager::default();
        let binding = manager.spawn(spawn).unwrap();
        let deadline = Instant::now() + Duration::from_secs(1);
        while !pid_file.is_file() && Instant::now() < deadline {
            sleep(Duration::from_millis(20));
        }
        let background_pid = fs::read_to_string(&pid_file)
            .unwrap()
            .trim()
            .parse::<u32>()
            .unwrap();
        let root_pid = manager.session.as_ref().unwrap().root_pid;
        assert_ne!(background_pid, root_pid);
        let before_close = unix_process_snapshot().unwrap();
        assert!(before_close.iter().any(|process| {
            process.pid == background_pid && process.session_id == root_pid && !process.zombie
        }));

        manager.close(&binding).unwrap();
        let snapshot = unix_process_snapshot().unwrap();
        assert!(
            !snapshot.iter().any(|process| {
                process.pid == background_pid && process.session_id == root_pid && !process.zombie
            }),
            "PTY close must contain background descendants rather than leaving them live"
        );

        // The test never sends a fallback signal to an unrelated process. The short-lived child
        // naturally exits within three seconds even if this assertion regresses.
        fs::remove_dir_all(temp).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn natural_root_exit_contains_remaining_session_members() {
        use std::fs;

        let Ok(shell) = fs::canonicalize("/bin/sh") else {
            return;
        };
        let sleep_executable = ["/bin/sleep", "/usr/bin/sleep"]
            .into_iter()
            .find(|candidate| Path::new(candidate).is_file());
        let nohup = ["/usr/bin/nohup", "/bin/nohup"]
            .into_iter()
            .find(|candidate| Path::new(candidate).is_file());
        let (Some(sleep_executable), Some(nohup)) = (sleep_executable, nohup) else {
            return;
        };
        if !shell.is_file() {
            return;
        }

        let mut random = [0_u8; 8];
        OsRng.fill_bytes(&mut random);
        let temp = std::env::temp_dir().join(format!(
            "cumg-terminal-pty-natural-exit-{}-{}",
            std::process::id(),
            u64::from_le_bytes(random),
        ));
        fs::create_dir(&temp).unwrap();
        let pid_file = temp.join("background.pid");
        let command = format!(
            "{nohup} {sleep_executable} 3 >/dev/null 2>&1 & echo $! > \"$PID_FILE\"; exit 0"
        );
        let mut spawn = config(&shell, &temp);
        spawn.args = vec![OsString::from("-c"), OsString::from(command)];
        spawn.env.push((
            OsString::from("PID_FILE"),
            pid_file.clone().into_os_string(),
        ));

        let mut manager = TerminalPtyManager::default();
        let binding = manager.spawn(spawn).unwrap();
        let root_pid = manager.session.as_ref().unwrap().root_pid;
        let pid_deadline = Instant::now() + Duration::from_secs(1);
        while !pid_file.is_file() && Instant::now() < pid_deadline {
            sleep(Duration::from_millis(20));
        }
        let background_pid = fs::read_to_string(&pid_file)
            .unwrap()
            .trim()
            .parse::<u32>()
            .unwrap();
        let exit_deadline = Instant::now() + Duration::from_secs(1);
        let state = loop {
            let state = manager.process_state(&binding).unwrap();
            if !matches!(state, TerminalPtyProcessState::Running) {
                break state;
            }
            assert!(
                Instant::now() < exit_deadline,
                "PTY root did not exit within test bound"
            );
            sleep(Duration::from_millis(20));
        };
        assert!(matches!(state, TerminalPtyProcessState::Exited { .. }));
        let snapshot = unix_process_snapshot().unwrap();
        assert!(
            !snapshot.iter().any(|process| {
                process.pid == background_pid && process.session_id == root_pid && !process.zombie
            }),
            "natural PTY root exit must contain remaining session members"
        );
        manager.release(&binding).unwrap();
        fs::remove_dir_all(temp).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn real_pty_is_single_session_bounded_and_human_period_output_is_not_replayed_to_agent() {
        let program = Path::new("/bin/cat");
        if !program.exists() {
            return;
        }
        let mut manager = TerminalPtyManager::default();
        let binding = manager.spawn(config(program, Path::new("/tmp"))).unwrap();
        assert_eq!(binding.generation(), 1);
        assert_eq!(binding.session_id().len(), 32);
        assert!(matches!(
            manager.spawn(config(program, Path::new("/tmp"))),
            Err(TerminalPtyError::SessionAlreadyExists)
        ));

        manager.write(&binding, b"agent-before\n").unwrap();
        manager.drain_writes(&binding).unwrap();
        sleep(Duration::from_millis(60));
        let before = manager.read_agent(&binding, MAX_PTY_READ_BYTES).unwrap();
        assert!(
            before
                .as_bytes()
                .windows(b"agent-before".len())
                .any(|w| w == b"agent-before")
        );

        manager.begin_human_output(&binding).unwrap();
        manager.write(&binding, b"human-secret-period\n").unwrap();
        manager.drain_writes(&binding).unwrap();
        sleep(Duration::from_millis(60));
        let human = manager.read_human(&binding, MAX_PTY_READ_BYTES).unwrap();
        assert!(
            human
                .as_bytes()
                .windows(b"human-secret-period".len())
                .any(|w| w == b"human-secret-period")
        );

        manager.finish_human_output(&binding).unwrap();
        let agent_after = manager.read_agent(&binding, MAX_PTY_READ_BYTES).unwrap();
        assert!(
            !agent_after
                .as_bytes()
                .windows(b"human-secret-period".len())
                .any(|w| w == b"human-secret-period")
        );

        manager.resize(&binding, 30, 100).unwrap();
        assert_eq!(
            manager.process_state(&binding).unwrap(),
            TerminalPtyProcessState::Running
        );
        manager.close(&binding).unwrap();
    }
}
