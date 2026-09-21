//! Agent-local lifecycle for explicitly managed developer processes.
//!
//! This primitive is deliberately narrower than a background shell. It accepts
//! only structured process requests, reuses the ordinary process policy and
//! supervision domain, bounds lifetime/output/concurrency, and never treats a
//! kill request as proof of termination.

use crate::v2_m0::{ProcessOutputStream, ProcessRequest};
use crate::v2_m1_process::{
    ProcessError, ProcessExecutor, ProcessUnprovenStage, SupervisedProcess,
};
use crate::v2_observability::SafeErrorCode;
use rand::{RngCore, rngs::OsRng};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, VecDeque};
use std::fmt;
use std::io::Read;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

pub const DEFAULT_MAX_CONCURRENT_MANAGED_JOBS: usize = 4;
pub const DEFAULT_MANAGED_JOB_LEASE_MS: u64 = 120_000;
pub const MAX_MANAGED_JOB_LEASE_MS: u64 = 300_000;
pub const MAX_MANAGED_JOB_LIFETIME_MS: u64 = 6 * 60 * 60 * 1000;
pub const DEFAULT_MANAGED_JOB_OUTPUT_BYTES_PER_STREAM: usize = 4 * 1024 * 1024;
pub const MAX_MANAGED_JOB_OUTPUT_READ_BYTES: usize = 64 * 1024;
const DEFAULT_MAX_RETAINED_JOBS: usize = 16;
const DEFAULT_TERMINAL_RETENTION_MS: u64 = 5 * 60 * 1000;
const DEFAULT_POLL_MS: u64 = 10;
const DEFAULT_STOP_PROOF_GRACE_MS: u64 = 5_000;

#[derive(Debug, Clone)]
pub struct ManagedJobLimits {
    pub max_concurrent_jobs: usize,
    pub default_lease: Duration,
    pub max_lease: Duration,
    pub max_hard_lifetime: Duration,
    pub output_bytes_per_stream: usize,
    pub max_output_read_bytes: usize,
    pub max_retained_jobs: usize,
    pub terminal_retention: Duration,
    pub poll_interval: Duration,
    pub stop_proof_grace: Duration,
}

impl Default for ManagedJobLimits {
    fn default() -> Self {
        Self {
            max_concurrent_jobs: DEFAULT_MAX_CONCURRENT_MANAGED_JOBS,
            default_lease: Duration::from_millis(DEFAULT_MANAGED_JOB_LEASE_MS),
            max_lease: Duration::from_millis(MAX_MANAGED_JOB_LEASE_MS),
            max_hard_lifetime: Duration::from_millis(MAX_MANAGED_JOB_LIFETIME_MS),
            output_bytes_per_stream: DEFAULT_MANAGED_JOB_OUTPUT_BYTES_PER_STREAM,
            max_output_read_bytes: MAX_MANAGED_JOB_OUTPUT_READ_BYTES,
            max_retained_jobs: DEFAULT_MAX_RETAINED_JOBS,
            terminal_retention: Duration::from_millis(DEFAULT_TERMINAL_RETENTION_MS),
            poll_interval: Duration::from_millis(DEFAULT_POLL_MS),
            stop_proof_grace: Duration::from_millis(DEFAULT_STOP_PROOF_GRACE_MS),
        }
    }
}

impl ManagedJobLimits {
    pub fn validate(&self) -> Result<(), ManagedJobError> {
        if self.max_concurrent_jobs == 0
            || self.default_lease.is_zero()
            || self.max_lease.is_zero()
            || self.default_lease > self.max_lease
            || self.max_hard_lifetime.is_zero()
            || self.max_lease > self.max_hard_lifetime
            || self.output_bytes_per_stream == 0
            || self.max_output_read_bytes == 0
            || self.max_output_read_bytes > self.output_bytes_per_stream
            || self.max_retained_jobs == 0
            || self.terminal_retention.is_zero()
            || self.poll_interval.is_zero()
            || self.stop_proof_grace.is_zero()
        {
            return Err(ManagedJobError::InvalidLimits);
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ManagedJobState {
    Starting,
    Running,
    StopRequested,
    Completed,
    Stopped,
    Expired,
    IndeterminateTermination,
}

impl ManagedJobState {
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Completed | Self::Stopped | Self::Expired | Self::IndeterminateTermination
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManagedJobStatus {
    pub state: ManagedJobState,
    pub exit_code: Option<i32>,
    pub lease_remaining_ms: u64,
    pub hard_lifetime_remaining_ms: u64,
    pub stdout_total_bytes: u64,
    pub stderr_total_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManagedJobOutputRange {
    pub bytes: Vec<u8>,
    pub requested_offset: u64,
    pub earliest_available_offset: u64,
    pub next_offset: u64,
    pub total_bytes: u64,
    pub eof: bool,
    pub gap_before_range: bool,
    pub history_truncated: bool,
}

#[derive(Debug)]
pub enum ManagedJobError {
    InvalidLimits,
    InvalidLifetime,
    InvalidLease,
    InvalidRead,
    CapacityExceeded,
    UnknownJob,
    TerminalJob,
    LockPoisoned,
    ThreadSpawn(std::io::Error),
    Process(ProcessError),
}

impl SafeErrorCode for ManagedJobError {
    fn safe_error_code(&self) -> &'static str {
        match self {
            Self::InvalidLimits => "managed_job_invalid_limits",
            Self::InvalidLifetime => "managed_job_invalid_lifetime",
            Self::InvalidLease => "managed_job_invalid_lease",
            Self::InvalidRead => "managed_job_invalid_read",
            Self::CapacityExceeded => "managed_job_capacity_exceeded",
            Self::UnknownJob => "managed_job_unknown",
            Self::TerminalJob => "managed_job_terminal",
            Self::LockPoisoned => "managed_job_lock_poisoned",
            Self::ThreadSpawn(_) => "managed_job_thread_spawn_failed",
            Self::Process(error) => error.safe_error_code(),
        }
    }
}

impl fmt::Display for ManagedJobError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.safe_error_code())
    }
}

impl std::error::Error for ManagedJobError {}

impl From<ProcessError> for ManagedJobError {
    fn from(value: ProcessError) -> Self {
        Self::Process(value)
    }
}

impl ManagedJobError {
    pub fn outcome_unproven_stage(&self) -> Option<ProcessUnprovenStage> {
        match self {
            Self::Process(error) => error.outcome_unproven_stage(),
            _ => None,
        }
    }
}

#[derive(Debug)]
struct RollingBuffer {
    bytes: VecDeque<u8>,
    capacity: usize,
    earliest_offset: u64,
    total_bytes: u64,
    closed: bool,
}

impl RollingBuffer {
    fn new(capacity: usize) -> Self {
        Self {
            bytes: VecDeque::with_capacity(capacity.min(4096)),
            capacity,
            earliest_offset: 0,
            total_bytes: 0,
            closed: false,
        }
    }

    fn append(&mut self, input: &[u8]) {
        self.total_bytes = self
            .total_bytes
            .saturating_add(u64::try_from(input.len()).unwrap_or(u64::MAX));
        for byte in input {
            self.bytes.push_back(*byte);
        }
        while self.bytes.len() > self.capacity {
            let _ = self.bytes.pop_front();
            self.earliest_offset = self.earliest_offset.saturating_add(1);
        }
    }

    fn close(&mut self) {
        self.closed = true;
    }

    fn read(&self, requested_offset: u64, max_bytes: usize) -> ManagedJobOutputRange {
        let start = requested_offset
            .max(self.earliest_offset)
            .min(self.total_bytes);
        let available = self.total_bytes.saturating_sub(start);
        let take = usize::try_from(available)
            .unwrap_or(usize::MAX)
            .min(max_bytes);
        let deque_index =
            usize::try_from(start.saturating_sub(self.earliest_offset)).unwrap_or(usize::MAX);
        let bytes = self
            .bytes
            .iter()
            .skip(deque_index)
            .take(take)
            .copied()
            .collect::<Vec<_>>();
        let next_offset = start.saturating_add(u64::try_from(bytes.len()).unwrap_or(u64::MAX));
        ManagedJobOutputRange {
            bytes,
            requested_offset,
            earliest_available_offset: self.earliest_offset,
            next_offset,
            total_bytes: self.total_bytes,
            eof: self.closed && next_offset >= self.total_bytes,
            gap_before_range: requested_offset < self.earliest_offset,
            history_truncated: self.earliest_offset > 0,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StopReason {
    User,
    LeaseExpired,
    HardExpired,
    Shutdown,
}

struct JobRecord {
    state: ManagedJobState,
    exit_code: Option<i32>,
    hard_deadline: Instant,
    lease_deadline: Instant,
    stop_reason: Option<StopReason>,
    terminal_at: Option<Instant>,
    stdout: RollingBuffer,
    stderr: RollingBuffer,
}

struct JobShared {
    record: Mutex<JobRecord>,
    process: Mutex<SupervisedProcess>,
}

struct ManagedJobInner {
    executor: ProcessExecutor,
    limits: ManagedJobLimits,
    start_gate: Mutex<()>,
    jobs: Mutex<HashMap<String, Arc<JobShared>>>,
}

#[derive(Clone)]
pub struct ManagedJobManager {
    inner: Arc<ManagedJobInner>,
}

impl ManagedJobManager {
    pub fn new(
        executor: ProcessExecutor,
        limits: ManagedJobLimits,
    ) -> Result<Self, ManagedJobError> {
        limits.validate()?;
        Ok(Self {
            inner: Arc::new(ManagedJobInner {
                executor,
                limits,
                start_gate: Mutex::new(()),
                jobs: Mutex::new(HashMap::new()),
            }),
        })
    }

    pub fn limits(&self) -> &ManagedJobLimits {
        &self.inner.limits
    }

    /// Start a structured managed job. The ProcessRequest timeout is the
    /// requested hard lifetime for this internal primitive.
    pub fn start(
        &self,
        request: &ProcessRequest,
    ) -> Result<(String, ManagedJobStatus), ManagedJobError> {
        let _start_guard = self
            .inner
            .start_gate
            .lock()
            .map_err(|_| ManagedJobError::LockPoisoned)?;
        let hard_lifetime = Duration::from_millis(request.timeout_ms);
        if hard_lifetime.is_zero() || hard_lifetime > self.inner.limits.max_hard_lifetime {
            return Err(ManagedJobError::InvalidLifetime);
        }
        self.prune_terminal()?;
        {
            let jobs = self
                .inner
                .jobs
                .lock()
                .map_err(|_| ManagedJobError::LockPoisoned)?;
            let active = jobs
                .values()
                .filter(|job| {
                    job.record
                        .lock()
                        .map(|record| !record.state.is_terminal())
                        .unwrap_or(true)
                })
                .count();
            if active >= self.inner.limits.max_concurrent_jobs {
                return Err(ManagedJobError::CapacityExceeded);
            }
        }

        let locator = {
            let jobs = self
                .inner
                .jobs
                .lock()
                .map_err(|_| ManagedJobError::LockPoisoned)?;
            let mut selected = None;
            for _ in 0..8 {
                let candidate = random_locator();
                if !jobs.contains_key(&candidate) {
                    selected = Some(candidate);
                    break;
                }
            }
            selected.ok_or(ManagedJobError::CapacityExceeded)?
        };

        let mut process = self.inner.executor.spawn_supervised(request)?;
        let stdout = match process.take_stdout() {
            Some(stdout) => stdout,
            None => {
                process
                    .prove_terminal(ProcessUnprovenStage::PipeSetup)
                    .map_err(ManagedJobError::Process)?;
                return Err(ManagedJobError::Process(ProcessError::PipeUnavailable));
            }
        };
        let stderr = match process.take_stderr() {
            Some(stderr) => stderr,
            None => {
                drop(stdout);
                process
                    .prove_terminal(ProcessUnprovenStage::PipeSetup)
                    .map_err(ManagedJobError::Process)?;
                return Err(ManagedJobError::Process(ProcessError::PipeUnavailable));
            }
        };
        let now = Instant::now();
        let hard_deadline = now + hard_lifetime;
        let lease_deadline = (now + self.inner.limits.default_lease).min(hard_deadline);
        let shared = Arc::new(JobShared {
            record: Mutex::new(JobRecord {
                state: ManagedJobState::Starting,
                exit_code: None,
                hard_deadline,
                lease_deadline,
                stop_reason: None,
                terminal_at: None,
                stdout: RollingBuffer::new(self.inner.limits.output_bytes_per_stream),
                stderr: RollingBuffer::new(self.inner.limits.output_bytes_per_stream),
            }),
            process: Mutex::new(process),
        });
        {
            let mut jobs = self
                .inner
                .jobs
                .lock()
                .map_err(|_| ManagedJobError::LockPoisoned)?;
            // start_gate serializes admission, so a pre-spawn unique locator
            // cannot race another start into this map.
            if jobs.contains_key(&locator) {
                drop(jobs);
                let proof = shared
                    .process
                    .lock()
                    .map_err(|_| ManagedJobError::LockPoisoned)?
                    .prove_terminal(ProcessUnprovenStage::Termination);
                return match proof {
                    Ok(_) => Err(ManagedJobError::CapacityExceeded),
                    Err(error) => Err(ManagedJobError::Process(error)),
                };
            }
            jobs.insert(locator.clone(), shared.clone());
        }

        if let Err(error) = spawn_reader("cumg-managed-job-stdout", stdout, shared.clone(), true) {
            self.fail_start_cleanup(&locator, &shared)?;
            return Err(ManagedJobError::ThreadSpawn(error));
        }
        if let Err(error) = spawn_reader("cumg-managed-job-stderr", stderr, shared.clone(), false) {
            self.fail_start_cleanup(&locator, &shared)?;
            return Err(ManagedJobError::ThreadSpawn(error));
        }

        {
            let mut record = shared
                .record
                .lock()
                .map_err(|_| ManagedJobError::LockPoisoned)?;
            record.state = ManagedJobState::Running;
        }
        let worker_shared = shared.clone();
        let limits = self.inner.limits.clone();
        if let Err(error) = thread::Builder::new()
            .name("cumg-managed-job-supervisor".into())
            .spawn(move || supervise_job(worker_shared, limits))
        {
            self.fail_start_cleanup(&locator, &shared)?;
            return Err(ManagedJobError::ThreadSpawn(error));
        }

        Ok((locator.clone(), self.status(&locator)?))
    }

    pub fn status(&self, locator: &str) -> Result<ManagedJobStatus, ManagedJobError> {
        self.prune_terminal()?;
        let job = self.lookup(locator)?;
        let record = job
            .record
            .lock()
            .map_err(|_| ManagedJobError::LockPoisoned)?;
        Ok(status_from_record(&record))
    }

    pub fn output(
        &self,
        locator: &str,
        stream: ProcessOutputStream,
        offset: u64,
        max_bytes: usize,
    ) -> Result<ManagedJobOutputRange, ManagedJobError> {
        if max_bytes == 0 || max_bytes > self.inner.limits.max_output_read_bytes {
            return Err(ManagedJobError::InvalidRead);
        }
        self.prune_terminal()?;
        let job = self.lookup(locator)?;
        let record = job
            .record
            .lock()
            .map_err(|_| ManagedJobError::LockPoisoned)?;
        Ok(match stream {
            ProcessOutputStream::Stdout => record.stdout.read(offset, max_bytes),
            ProcessOutputStream::Stderr => record.stderr.read(offset, max_bytes),
        })
    }

    pub fn renew(&self, locator: &str, lease_ms: u64) -> Result<ManagedJobStatus, ManagedJobError> {
        let requested = Duration::from_millis(lease_ms);
        if requested.is_zero() || requested > self.inner.limits.max_lease {
            return Err(ManagedJobError::InvalidLease);
        }
        self.prune_terminal()?;
        let job = self.lookup(locator)?;
        let mut record = job
            .record
            .lock()
            .map_err(|_| ManagedJobError::LockPoisoned)?;
        if record.state.is_terminal() {
            return Err(ManagedJobError::TerminalJob);
        }
        let now = Instant::now();
        if now >= record.hard_deadline {
            record.state = ManagedJobState::StopRequested;
            record.stop_reason = Some(StopReason::HardExpired);
            return Err(ManagedJobError::TerminalJob);
        }
        record.lease_deadline = (now + requested).min(record.hard_deadline);
        Ok(status_from_record(&record))
    }

    pub fn stop(&self, locator: &str) -> Result<ManagedJobStatus, ManagedJobError> {
        self.prune_terminal()?;
        let job = self.lookup(locator)?;
        {
            let mut record = job
                .record
                .lock()
                .map_err(|_| ManagedJobError::LockPoisoned)?;
            if record.state.is_terminal() {
                return Ok(status_from_record(&record));
            }
            record.state = ManagedJobState::StopRequested;
            record.stop_reason = Some(StopReason::User);
        }
        self.wait_for_terminal(&job)
    }

    /// Used at Agent session loss/shutdown. Every live job receives a stop
    /// request. A job that cannot establish terminal proof within the bounded
    /// grace becomes indeterminate rather than being reported as stopped.
    pub fn shutdown_all(&self) -> Result<(), ManagedJobError> {
        let jobs = {
            let jobs = self
                .inner
                .jobs
                .lock()
                .map_err(|_| ManagedJobError::LockPoisoned)?;
            jobs.values().cloned().collect::<Vec<_>>()
        };
        for job in &jobs {
            let mut record = job
                .record
                .lock()
                .map_err(|_| ManagedJobError::LockPoisoned)?;
            if !record.state.is_terminal() {
                record.state = ManagedJobState::StopRequested;
                record.stop_reason = Some(StopReason::Shutdown);
            }
        }
        let deadline = Instant::now() + self.inner.limits.stop_proof_grace;
        let mut any_indeterminate = false;
        loop {
            let mut pending = false;
            for job in &jobs {
                let mut record = job
                    .record
                    .lock()
                    .map_err(|_| ManagedJobError::LockPoisoned)?;
                if !record.state.is_terminal() {
                    pending = true;
                    if Instant::now() >= deadline {
                        record.state = ManagedJobState::IndeterminateTermination;
                        record.terminal_at = Some(Instant::now());
                        any_indeterminate = true;
                    }
                } else if record.state == ManagedJobState::IndeterminateTermination {
                    any_indeterminate = true;
                }
            }
            if !pending || Instant::now() >= deadline {
                break;
            }
            thread::sleep(self.inner.limits.poll_interval);
        }
        if any_indeterminate {
            return Err(ManagedJobError::Process(ProcessError::OutcomeUnproven(
                ProcessUnprovenStage::Wait,
            )));
        }
        Ok(())
    }

    pub fn has_indeterminate_termination(&self) -> Result<bool, ManagedJobError> {
        let jobs = self
            .inner
            .jobs
            .lock()
            .map_err(|_| ManagedJobError::LockPoisoned)?;
        for job in jobs.values() {
            if job
                .record
                .lock()
                .map_err(|_| ManagedJobError::LockPoisoned)?
                .state
                == ManagedJobState::IndeterminateTermination
            {
                return Ok(true);
            }
        }
        Ok(false)
    }

    pub fn active_count(&self) -> Result<usize, ManagedJobError> {
        let jobs = self
            .inner
            .jobs
            .lock()
            .map_err(|_| ManagedJobError::LockPoisoned)?;
        Ok(jobs
            .values()
            .filter(|job| {
                job.record
                    .lock()
                    .map(|record| !record.state.is_terminal())
                    .unwrap_or(true)
            })
            .count())
    }

    fn lookup(&self, locator: &str) -> Result<Arc<JobShared>, ManagedJobError> {
        let jobs = self
            .inner
            .jobs
            .lock()
            .map_err(|_| ManagedJobError::LockPoisoned)?;
        jobs.get(locator)
            .cloned()
            .ok_or(ManagedJobError::UnknownJob)
    }

    fn wait_for_terminal(&self, job: &Arc<JobShared>) -> Result<ManagedJobStatus, ManagedJobError> {
        let deadline = Instant::now() + self.inner.limits.stop_proof_grace;
        loop {
            {
                let mut record = job
                    .record
                    .lock()
                    .map_err(|_| ManagedJobError::LockPoisoned)?;
                if record.state.is_terminal() {
                    return Ok(status_from_record(&record));
                }
                if Instant::now() >= deadline {
                    record.state = ManagedJobState::IndeterminateTermination;
                    record.terminal_at = Some(Instant::now());
                    return Err(ManagedJobError::Process(ProcessError::OutcomeUnproven(
                        ProcessUnprovenStage::Wait,
                    )));
                }
            }
            thread::sleep(self.inner.limits.poll_interval);
        }
    }

    fn fail_start_cleanup(
        &self,
        locator: &str,
        job: &Arc<JobShared>,
    ) -> Result<(), ManagedJobError> {
        let proof = job
            .process
            .lock()
            .map_err(|_| ManagedJobError::LockPoisoned)?
            .prove_terminal(ProcessUnprovenStage::Termination);
        if let Ok(mut jobs) = self.inner.jobs.lock() {
            jobs.remove(locator);
        }
        proof.map(|_| ()).map_err(ManagedJobError::Process)
    }

    fn prune_terminal(&self) -> Result<(), ManagedJobError> {
        let now = Instant::now();
        let mut jobs = self
            .inner
            .jobs
            .lock()
            .map_err(|_| ManagedJobError::LockPoisoned)?;
        jobs.retain(|_, job| {
            let Ok(record) = job.record.lock() else {
                return true;
            };
            match record.terminal_at {
                Some(terminal_at) => {
                    now.saturating_duration_since(terminal_at)
                        < self.inner.limits.terminal_retention
                }
                None => true,
            }
        });
        let mut terminal = jobs
            .iter()
            .filter_map(|(locator, job)| {
                let record = job.record.lock().ok()?;
                record.terminal_at.map(|at| (locator.clone(), at))
            })
            .collect::<Vec<_>>();
        if terminal.len() > self.inner.limits.max_retained_jobs {
            terminal.sort_by_key(|(_, at)| *at);
            let remove_count = terminal.len() - self.inner.limits.max_retained_jobs;
            for (locator, _) in terminal.into_iter().take(remove_count) {
                jobs.remove(&locator);
            }
        }
        Ok(())
    }
}

fn status_from_record(record: &JobRecord) -> ManagedJobStatus {
    let now = Instant::now();
    ManagedJobStatus {
        state: record.state,
        exit_code: record.exit_code,
        lease_remaining_ms: duration_ms(record.lease_deadline.saturating_duration_since(now)),
        hard_lifetime_remaining_ms: duration_ms(
            record.hard_deadline.saturating_duration_since(now),
        ),
        stdout_total_bytes: record.stdout.total_bytes,
        stderr_total_bytes: record.stderr.total_bytes,
    }
}

fn duration_ms(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

fn random_locator() -> String {
    let mut bytes = [0_u8; 24];
    OsRng.fill_bytes(&mut bytes);
    let mut out = String::with_capacity(48);
    for byte in bytes {
        use std::fmt::Write as _;
        let _ = write!(&mut out, "{byte:02x}");
    }
    out
}

fn spawn_reader<R: Read + Send + 'static>(
    name: &str,
    mut reader: R,
    shared: Arc<JobShared>,
    stdout: bool,
) -> Result<(), std::io::Error> {
    thread::Builder::new()
        .name(name.into())
        .spawn(move || {
            let mut buffer = [0_u8; 4096];
            loop {
                match reader.read(&mut buffer) {
                    Ok(0) => {
                        if let Ok(mut record) = shared.record.lock() {
                            if stdout {
                                record.stdout.close();
                            } else {
                                record.stderr.close();
                            }
                        }
                        break;
                    }
                    Ok(read) => {
                        if let Ok(mut record) = shared.record.lock() {
                            if stdout {
                                record.stdout.append(&buffer[..read]);
                            } else {
                                record.stderr.append(&buffer[..read]);
                            }
                        } else {
                            break;
                        }
                    }
                    Err(_) => {
                        if let Ok(mut record) = shared.record.lock() {
                            if stdout {
                                record.stdout.close();
                            } else {
                                record.stderr.close();
                            }
                        }
                        break;
                    }
                }
            }
        })
        .map(|_| ())
}

fn supervise_job(shared: Arc<JobShared>, limits: ManagedJobLimits) {
    loop {
        let decision = {
            let Ok(mut record) = shared.record.lock() else {
                return;
            };
            if record.state.is_terminal() {
                return;
            }
            let now = Instant::now();
            if now >= record.hard_deadline {
                record.state = ManagedJobState::StopRequested;
                record.stop_reason = Some(StopReason::HardExpired);
                Some(StopReason::HardExpired)
            } else if now >= record.lease_deadline {
                record.state = ManagedJobState::StopRequested;
                record.stop_reason = Some(StopReason::LeaseExpired);
                Some(StopReason::LeaseExpired)
            } else if record.state == ManagedJobState::StopRequested {
                Some(record.stop_reason.unwrap_or(StopReason::User))
            } else {
                None
            }
        };

        if let Some(reason) = decision {
            let proof = shared
                .process
                .lock()
                .map_err(|_| ())
                .and_then(|mut process| {
                    process
                        .prove_terminal(ProcessUnprovenStage::Termination)
                        .map_err(|_| ())
                });
            let Ok(mut record) = shared.record.lock() else {
                return;
            };
            if record.state == ManagedJobState::IndeterminateTermination {
                return;
            }
            match proof {
                Ok(status) => {
                    record.exit_code = status.code();
                    record.state = match reason {
                        StopReason::LeaseExpired | StopReason::HardExpired => {
                            ManagedJobState::Expired
                        }
                        StopReason::User | StopReason::Shutdown => ManagedJobState::Stopped,
                    };
                }
                Err(()) => {
                    record.state = ManagedJobState::IndeterminateTermination;
                }
            }
            record.terminal_at = Some(Instant::now());
            return;
        }

        let poll = shared
            .process
            .lock()
            .map_err(|_| ManagedJobError::LockPoisoned)
            .and_then(|mut process| process.try_wait().map_err(ManagedJobError::Process));
        match poll {
            Ok(Some(status)) => {
                let proof = shared
                    .process
                    .lock()
                    .map_err(|_| ManagedJobError::LockPoisoned)
                    .and_then(|mut process| {
                        process
                            .prove_terminal(ProcessUnprovenStage::Termination)
                            .map_err(ManagedJobError::Process)
                    });
                let Ok(mut record) = shared.record.lock() else {
                    return;
                };
                if record.state == ManagedJobState::IndeterminateTermination {
                    return;
                }
                match proof {
                    Ok(_) => {
                        record.exit_code = status.code();
                        record.state = ManagedJobState::Completed;
                    }
                    Err(_) => {
                        record.state = ManagedJobState::IndeterminateTermination;
                    }
                }
                record.terminal_at = Some(Instant::now());
                return;
            }
            Ok(None) => {}
            Err(_) => {
                let proof = shared
                    .process
                    .lock()
                    .map_err(|_| ManagedJobError::LockPoisoned)
                    .and_then(|mut process| {
                        process
                            .prove_terminal(ProcessUnprovenStage::Poll)
                            .map_err(ManagedJobError::Process)
                    });
                let Ok(mut record) = shared.record.lock() else {
                    return;
                };
                if record.state == ManagedJobState::IndeterminateTermination {
                    return;
                }
                match proof {
                    Ok(status) => {
                        record.exit_code = status.code();
                        record.state = ManagedJobState::Completed;
                    }
                    Err(_) => record.state = ManagedJobState::IndeterminateTermination,
                }
                record.terminal_at = Some(Instant::now());
                return;
            }
        }
        thread::sleep(limits.poll_interval);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rolling_buffer_reports_eviction_and_gap() {
        let mut buffer = RollingBuffer::new(4);
        buffer.append(b"abcdef");
        let range = buffer.read(0, 4);
        assert_eq!(range.bytes, b"cdef");
        assert_eq!(range.earliest_available_offset, 2);
        assert_eq!(range.next_offset, 6);
        assert_eq!(range.total_bytes, 6);
        assert!(range.gap_before_range);
        assert!(range.history_truncated);
        assert!(!range.eof);
        buffer.close();
        assert!(buffer.read(6, 4).eof);
    }

    #[test]
    fn default_limits_validate() {
        ManagedJobLimits::default().validate().unwrap();
        let mut invalid = ManagedJobLimits::default();
        invalid.default_lease = invalid.max_lease + Duration::from_millis(1);
        assert!(matches!(
            invalid.validate(),
            Err(ManagedJobError::InvalidLimits)
        ));
    }

    #[cfg(unix)]
    fn temp_root(name: &str) -> std::path::PathBuf {
        let root = std::env::temp_dir().join(format!(
            "cumg-managed-job-{name}-{}-{}",
            std::process::id(),
            random_locator()
        ));
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    #[cfg(unix)]
    fn test_manager(
        root: &std::path::Path,
        max_concurrent_jobs: usize,
        default_lease_ms: u64,
    ) -> ManagedJobManager {
        let executor = ProcessExecutor::new(
            crate::v2_m1_process::ProcessPolicy::developer_defaults(vec![root.to_path_buf()])
                .unwrap(),
        );
        let limits = ManagedJobLimits {
            max_concurrent_jobs,
            default_lease: Duration::from_millis(default_lease_ms),
            max_lease: Duration::from_millis(500),
            max_hard_lifetime: Duration::from_secs(2),
            output_bytes_per_stream: 1024,
            max_output_read_bytes: 256,
            max_retained_jobs: 8,
            terminal_retention: Duration::from_secs(1),
            poll_interval: Duration::from_millis(5),
            stop_proof_grace: Duration::from_secs(1),
        };
        ManagedJobManager::new(executor, limits).unwrap()
    }

    #[cfg(unix)]
    fn request(
        program: &str,
        args: &[&str],
        root: &std::path::Path,
        timeout_ms: u64,
    ) -> ProcessRequest {
        ProcessRequest {
            program: program.to_owned(),
            args: args.iter().map(|arg| (*arg).to_owned()).collect(),
            cwd: root.to_string_lossy().into_owned(),
            env: vec![],
            timeout_ms,
        }
    }

    #[cfg(unix)]
    fn wait_terminal(manager: &ManagedJobManager, locator: &str) -> ManagedJobStatus {
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            let status = manager.status(locator).unwrap();
            if status.state.is_terminal() {
                return status;
            }
            assert!(
                Instant::now() < deadline,
                "managed job did not become terminal"
            );
            thread::sleep(Duration::from_millis(5));
        }
    }

    #[cfg(unix)]
    #[test]
    fn start_renew_status_and_stop_are_bounded() {
        let root = temp_root("lifecycle");
        let manager = test_manager(&root, 1, 200);
        let (locator, started) = manager
            .start(&request("/bin/sleep", &["1"], &root, 1500))
            .unwrap();
        assert!(matches!(
            started.state,
            ManagedJobState::Running | ManagedJobState::Starting
        ));
        assert_eq!(manager.active_count().unwrap(), 1);

        let renewed = manager.renew(&locator, 400).unwrap();
        assert!(renewed.lease_remaining_ms <= 400);
        assert!(renewed.lease_remaining_ms > 0);

        let stopped = manager.stop(&locator).unwrap();
        assert_eq!(stopped.state, ManagedJobState::Stopped);
        assert_eq!(manager.active_count().unwrap(), 0);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn output_is_bounded_and_retrievable() {
        let root = temp_root("output");
        let manager = test_manager(&root, 1, 500);
        let (locator, _) = manager
            .start(&request(
                "/usr/bin/printf",
                &["hello-managed-job"],
                &root,
                1000,
            ))
            .unwrap();
        let terminal = wait_terminal(&manager, &locator);
        assert_eq!(terminal.state, ManagedJobState::Completed);

        let deadline = Instant::now() + Duration::from_secs(1);
        let range = loop {
            let range = manager
                .output(&locator, ProcessOutputStream::Stdout, 0, 256)
                .unwrap();
            if range.eof {
                break range;
            }
            assert!(Instant::now() < deadline, "stdout reader did not reach eof");
            thread::sleep(Duration::from_millis(5));
        };
        assert_eq!(range.bytes, b"hello-managed-job");
        assert_eq!(range.total_bytes, 17);
        assert!(!range.gap_before_range);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn lease_expiry_proves_terminal_before_expired() {
        let root = temp_root("expiry");
        let manager = test_manager(&root, 1, 40);
        let (locator, _) = manager
            .start(&request("/bin/sleep", &["1"], &root, 1000))
            .unwrap();
        let terminal = wait_terminal(&manager, &locator);
        assert_eq!(terminal.state, ManagedJobState::Expired);
        assert_eq!(manager.active_count().unwrap(), 0);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn concurrency_limit_is_enforced() {
        let root = temp_root("capacity");
        let manager = test_manager(&root, 1, 500);
        let (first, _) = manager
            .start(&request("/bin/sleep", &["1"], &root, 1500))
            .unwrap();
        assert!(matches!(
            manager.start(&request("/bin/sleep", &["1"], &root, 1500)),
            Err(ManagedJobError::CapacityExceeded)
        ));
        assert_eq!(
            manager.stop(&first).unwrap().state,
            ManagedJobState::Stopped
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn managed_job_never_turns_structured_process_into_shell_authority() {
        let root = temp_root("no-shell");
        let manager = test_manager(&root, 1, 500);
        assert!(matches!(
            manager.start(&request("/bin/sh", &["-c", "exit 0"], &root, 1000)),
            Err(ManagedJobError::Process(ProcessError::ShellProgramDenied))
        ));
        std::fs::remove_dir_all(root).unwrap();
    }

    fn portable_temp_root(name: &str) -> std::path::PathBuf {
        let root = std::env::temp_dir().join(format!(
            "cumg-managed-job-portable-{name}-{}-{}",
            std::process::id(),
            random_locator()
        ));
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    fn portable_manager(root: &std::path::Path, default_lease_ms: u64) -> ManagedJobManager {
        let executor = ProcessExecutor::new(
            crate::v2_m1_process::ProcessPolicy::developer_defaults(vec![root.to_path_buf()])
                .unwrap(),
        );
        ManagedJobManager::new(
            executor,
            ManagedJobLimits {
                max_concurrent_jobs: 1,
                default_lease: Duration::from_millis(default_lease_ms),
                max_lease: Duration::from_millis(500),
                max_hard_lifetime: Duration::from_secs(2),
                output_bytes_per_stream: 4096,
                max_output_read_bytes: 1024,
                max_retained_jobs: 8,
                terminal_retention: Duration::from_secs(1),
                poll_interval: Duration::from_millis(5),
                stop_proof_grace: Duration::from_secs(1),
            },
        )
        .unwrap()
    }

    fn portable_helper_request(root: &std::path::Path, timeout_ms: u64) -> ProcessRequest {
        let current_exe = std::env::current_exe().unwrap();
        ProcessRequest {
            program: current_exe.to_string_lossy().into_owned(),
            args: vec![
                "--ignored".into(),
                "--exact".into(),
                "v2_managed_job::tests::managed_job_portable_helper_process".into(),
                "--nocapture".into(),
                "--test-threads=1".into(),
            ],
            cwd: root.to_string_lossy().into_owned(),
            env: vec![],
            timeout_ms,
        }
    }

    #[test]
    #[ignore]
    fn managed_job_portable_helper_process() {
        println!("CUMG_MANAGED_JOB_PORTABLE_OUTPUT");
        thread::sleep(Duration::from_millis(150));
    }

    #[test]
    fn portable_lifecycle_uses_supervised_process_domain() {
        let root = portable_temp_root("lifecycle");
        let manager = portable_manager(&root, 500);
        let (locator, _) = manager
            .start(&portable_helper_request(&root, 1500))
            .unwrap();
        let renewed = manager.renew(&locator, 400).unwrap();
        assert!(renewed.lease_remaining_ms > 0);
        let stopped = manager.stop(&locator).unwrap();
        assert_eq!(stopped.state, ManagedJobState::Stopped);
        assert_eq!(manager.active_count().unwrap(), 0);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn portable_lease_expiry_proves_terminal_before_expired() {
        let root = portable_temp_root("expiry");
        let manager = portable_manager(&root, 40);
        let (locator, _) = manager
            .start(&portable_helper_request(&root, 1000))
            .unwrap();
        let terminal = wait_terminal_portable(&manager, &locator);
        assert_eq!(terminal.state, ManagedJobState::Expired);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn portable_output_is_bounded_and_retrievable() {
        let root = portable_temp_root("output");
        let manager = portable_manager(&root, 500);
        let (locator, _) = manager
            .start(&portable_helper_request(&root, 1000))
            .unwrap();
        let terminal = wait_terminal_portable(&manager, &locator);
        assert_eq!(terminal.state, ManagedJobState::Completed);
        let deadline = Instant::now() + Duration::from_secs(1);
        let range = loop {
            let range = manager
                .output(&locator, ProcessOutputStream::Stdout, 0, 1024)
                .unwrap();
            if range.eof {
                break range;
            }
            assert!(
                Instant::now() < deadline,
                "portable stdout did not reach eof"
            );
            thread::sleep(Duration::from_millis(5));
        };
        let text = String::from_utf8_lossy(&range.bytes);
        assert!(text.contains("CUMG_MANAGED_JOB_PORTABLE_OUTPUT"));
        assert!(range.bytes.len() <= 1024);
        std::fs::remove_dir_all(root).unwrap();
    }

    fn wait_terminal_portable(manager: &ManagedJobManager, locator: &str) -> ManagedJobStatus {
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            let status = manager.status(locator).unwrap();
            if status.state.is_terminal() {
                return status;
            }
            assert!(
                Instant::now() < deadline,
                "portable managed job did not terminate"
            );
            thread::sleep(Duration::from_millis(5));
        }
    }
}
