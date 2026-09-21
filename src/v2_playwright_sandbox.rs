//! Agent-native sandboxed Playwright test execution.
//!
//! The untrusted test/config code runs only inside an operator-configured
//! container runtime. CUMG never treats ProcessGroup/JobObject supervision as
//! the sandbox: those primitives supervise only the trusted container-runtime
//! process on the host.

use crate::v2_m0::{ProcessOutputStream, ProcessRequest};
use crate::v2_m1_process::{
    ProcessCancellation, ProcessError, ProcessExecutor, ProcessPolicy, ProcessUnprovenStage,
};
use crate::v2_m1_workspace_mutation::sha256_hex;
use crate::v2_managed_job::{
    ManagedJobError, ManagedJobLimits, ManagedJobManager, ManagedJobOutputRange, ManagedJobStatus,
};
use crate::v2_observability::SafeErrorCode;
use rand::{RngCore, rngs::OsRng};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};
use std::thread;
use std::time::{Duration, Instant};

pub const MAX_PLAYWRIGHT_TEST_PATHS: usize = 32;
pub const MAX_PLAYWRIGHT_TEST_PATH_BYTES: usize = 256;
pub const MAX_PLAYWRIGHT_PROJECT_BYTES: usize = 128;
pub const MAX_PLAYWRIGHT_GREP_BYTES: usize = 512;
pub const MAX_PLAYWRIGHT_WORKERS: u16 = 16;
pub const MAX_PLAYWRIGHT_TEST_LIFETIME_MS: u64 = 30 * 60 * 1000;
pub const MAX_PLAYWRIGHT_OUTPUT_READ_BYTES: usize = 64 * 1024;
const PLAYWRIGHT_ARTIFACT_ROOT_NAME: &str = "playwright-artifacts";
const PLAYWRIGHT_CONTAINER_WORKSPACE: &str = "/workspace";
const PLAYWRIGHT_CONTAINER_ARTIFACTS: &str = "/artifacts";
const PLAYWRIGHT_CONTAINER_HOME: &str = "/home/pwuser";
const PLAYWRIGHT_BINARY: &str = "/workspace/node_modules/.bin/playwright";
const PLAYWRIGHT_MAX_CONCURRENT: usize = 2;
const PLAYWRIGHT_MAX_RETAINED_BYTES: usize = 4 * 1024 * 1024;
const PLAYWRIGHT_PIDS_LIMIT: &str = "512";
const PLAYWRIGHT_MEMORY_LIMIT: &str = "2g";
const PLAYWRIGHT_CPU_LIMIT: &str = "2";
const PLAYWRIGHT_SHM_SIZE: &str = "1g";
const PLAYWRIGHT_PROVIDER_CLEANUP_BUFFER_MS: u64 = 30_000;
const PLAYWRIGHT_PROVIDER_COMMAND_TIMEOUT_MS: u64 = 5_000;
const PLAYWRIGHT_PROVIDER_PROOF_GRACE: Duration = Duration::from_secs(5);
const PLAYWRIGHT_MONITOR_POLL: Duration = Duration::from_millis(100);
const PLAYWRIGHT_PROVIDER_LABEL: &str = "io.cumg.playwright=v1";
const PLAYWRIGHT_PROVIDER_OWNER_LABEL_KEY: &str = "io.cumg.owner";
const PLAYWRIGHT_MAX_RECOVERY_CONTAINERS: usize = 32;

#[derive(Debug, Clone)]
pub struct PlaywrightSandboxConfig {
    runtime: PathBuf,
    image: String,
    allowed_workspace_roots: Vec<PathBuf>,
}

impl PlaywrightSandboxConfig {
    pub fn new(
        runtime: PathBuf,
        image: String,
        allowed_workspace_roots: Vec<PathBuf>,
    ) -> Result<Self, PlaywrightSandboxError> {
        if !runtime.is_absolute() || allowed_workspace_roots.is_empty() {
            return Err(PlaywrightSandboxError::InvalidConfig);
        }
        validate_image_reference(&image)?;
        let runtime_input_meta = fs::symlink_metadata(&runtime)
            .map_err(|_| PlaywrightSandboxError::RuntimeUnavailable)?;
        if runtime_input_meta.file_type().is_symlink() {
            return Err(PlaywrightSandboxError::RuntimeUnavailable);
        }
        let runtime =
            fs::canonicalize(runtime).map_err(|_| PlaywrightSandboxError::RuntimeUnavailable)?;
        let runtime_meta =
            fs::metadata(&runtime).map_err(|_| PlaywrightSandboxError::RuntimeUnavailable)?;
        if !runtime_meta.is_file() {
            return Err(PlaywrightSandboxError::RuntimeUnavailable);
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            if runtime_meta.permissions().mode() & 0o111 == 0 {
                return Err(PlaywrightSandboxError::RuntimeUnavailable);
            }
        }

        let mut roots = Vec::with_capacity(allowed_workspace_roots.len());
        for root in allowed_workspace_roots {
            if !root.is_absolute() {
                return Err(PlaywrightSandboxError::InvalidConfig);
            }
            let canonical =
                fs::canonicalize(root).map_err(|_| PlaywrightSandboxError::InvalidWorkspace)?;
            if !canonical.is_dir() || canonical.to_string_lossy().contains(',') {
                return Err(PlaywrightSandboxError::InvalidWorkspace);
            }
            if !roots.contains(&canonical) {
                roots.push(canonical);
            }
        }
        if roots.is_empty() {
            return Err(PlaywrightSandboxError::InvalidConfig);
        }

        Ok(Self {
            runtime,
            image,
            allowed_workspace_roots: roots,
        })
    }

    pub fn runtime(&self) -> &Path {
        &self.runtime
    }

    pub fn image(&self) -> &str {
        &self.image
    }

    pub fn allowed_workspace_roots(&self) -> &[PathBuf] {
        &self.allowed_workspace_roots
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlaywrightTestRequest {
    pub workspace: String,
    pub test_paths: Vec<String>,
    #[serde(default)]
    pub project: Option<String>,
    #[serde(default)]
    pub grep: Option<String>,
    #[serde(default)]
    pub workers: Option<u16>,
    pub hard_lifetime_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlaywrightTestStatus {
    pub job: ManagedJobStatus,
    pub artifact_count: u64,
    pub artifact_total_bytes: u64,
}

#[derive(Debug)]
struct ArtifactRecord {
    directory: PathBuf,
    container_name: String,
    hard_deadline: Instant,
    provider_terminal_proven: bool,
    expired: bool,
}

#[derive(Clone)]
pub struct PlaywrightSandboxRunner {
    config: Arc<PlaywrightSandboxConfig>,
    jobs: ManagedJobManager,
    probe_executor: ProcessExecutor,
    artifact_root: Arc<PathBuf>,
    artifacts: Arc<Mutex<HashMap<String, ArtifactRecord>>>,
    provider_indeterminate: Arc<AtomicBool>,
    provider_owner_label: Arc<String>,
    runtime_cwd: Arc<PathBuf>,
}

impl PlaywrightSandboxRunner {
    pub fn new(
        config: PlaywrightSandboxConfig,
        state_dir: &Path,
        device_id: &str,
    ) -> Result<Self, PlaywrightSandboxError> {
        if device_id.trim().is_empty() {
            return Err(PlaywrightSandboxError::InvalidConfig);
        }
        let runtime_cwd =
            fs::canonicalize(state_dir).map_err(|_| PlaywrightSandboxError::InvalidConfig)?;
        if !runtime_cwd.is_dir() {
            return Err(PlaywrightSandboxError::InvalidConfig);
        }
        if runtime_cwd.to_string_lossy().contains(',') {
            return Err(PlaywrightSandboxError::InvalidConfig);
        }
        let artifact_root = prepare_artifact_root(&runtime_cwd)?;
        let owner_material = format!("{}\0{}", device_id, runtime_cwd.display());
        let owner_hash = sha256_hex(owner_material.as_bytes());
        let provider_owner_label = owner_hash
            .get(..32)
            .ok_or(PlaywrightSandboxError::InvalidConfig)?
            .to_owned();

        // Critical boundary: the trusted container runtime receives no inherited
        // host environment from CUMG. The untrusted test container gets only
        // explicit fixed env flags in argv below.
        let policy = ProcessPolicy::new(
            vec![runtime_cwd.clone()],
            HashSet::new(),
            HashSet::new(),
            HashSet::new(),
        )
        .map_err(PlaywrightSandboxError::Process)?;
        let limits = ManagedJobLimits {
            max_concurrent_jobs: PLAYWRIGHT_MAX_CONCURRENT,
            default_lease: Duration::from_millis(
                MAX_PLAYWRIGHT_TEST_LIFETIME_MS + PLAYWRIGHT_PROVIDER_CLEANUP_BUFFER_MS,
            ),
            max_lease: Duration::from_millis(
                MAX_PLAYWRIGHT_TEST_LIFETIME_MS + PLAYWRIGHT_PROVIDER_CLEANUP_BUFFER_MS,
            ),
            max_hard_lifetime: Duration::from_millis(
                MAX_PLAYWRIGHT_TEST_LIFETIME_MS + PLAYWRIGHT_PROVIDER_CLEANUP_BUFFER_MS,
            ),
            output_bytes_per_stream: PLAYWRIGHT_MAX_RETAINED_BYTES,
            max_output_read_bytes: MAX_PLAYWRIGHT_OUTPUT_READ_BYTES,
            max_retained_jobs: 16,
            terminal_retention: Duration::from_secs(5 * 60),
            poll_interval: Duration::from_millis(10),
            stop_proof_grace: Duration::from_secs(5),
        };
        let probe_executor = ProcessExecutor::new(policy);
        let jobs = ManagedJobManager::new(probe_executor.clone(), limits)
            .map_err(PlaywrightSandboxError::ManagedJob)?;

        Ok(Self {
            config: Arc::new(config),
            jobs,
            probe_executor,
            artifact_root: Arc::new(artifact_root),
            artifacts: Arc::new(Mutex::new(HashMap::new())),
            provider_indeterminate: Arc::new(AtomicBool::new(false)),
            provider_owner_label: Arc::new(provider_owner_label),
            runtime_cwd: Arc::new(runtime_cwd),
        })
    }

    pub fn probe_provider(&self) -> Result<(), PlaywrightSandboxError> {
        let output = self.run_provider_command(vec![
            "image".into(),
            "inspect".into(),
            "--format".into(),
            "{{.Id}}".into(),
            self.config.image.clone(),
        ])?;
        if output.exit_code != Some(0) || output.timed_out || output.cancelled {
            return Err(PlaywrightSandboxError::RuntimeUnavailable);
        }
        Ok(())
    }

    pub fn recover_provider_orphans(&self) -> Result<(), PlaywrightSandboxError> {
        let containers = self.list_owned_provider_containers()?;
        for container_id in containers {
            let output = self
                .run_provider_command(vec![
                    "container".into(),
                    "rm".into(),
                    "-f".into(),
                    container_id,
                ])
                .map_err(|_| PlaywrightSandboxError::ProviderOutcomeUnproven)?;
            if output.exit_code != Some(0) || output.timed_out || output.cancelled {
                return Err(PlaywrightSandboxError::ProviderOutcomeUnproven);
            }
        }
        if !self.list_owned_provider_containers()?.is_empty() {
            return Err(PlaywrightSandboxError::ProviderOutcomeUnproven);
        }
        self.cleanup_artifacts()
    }

    fn list_owned_provider_containers(&self) -> Result<Vec<String>, PlaywrightSandboxError> {
        let output = self
            .run_provider_command(vec![
                "container".into(),
                "ps".into(),
                "-a".into(),
                "--filter".into(),
                format!("label={PLAYWRIGHT_PROVIDER_LABEL}"),
                "--filter".into(),
                format!(
                    "label={PLAYWRIGHT_PROVIDER_OWNER_LABEL_KEY}={}",
                    self.provider_owner_label
                ),
                "--format".into(),
                "{{.ID}}".into(),
            ])
            .map_err(|_| PlaywrightSandboxError::ProviderOutcomeUnproven)?;
        if output.exit_code != Some(0) || output.timed_out || output.cancelled {
            return Err(PlaywrightSandboxError::ProviderOutcomeUnproven);
        }
        let mut containers = Vec::new();
        for line in output.stdout.lines() {
            let id = line.trim();
            if id.is_empty() {
                continue;
            }
            if id.len() > 128
                || !id
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
                || containers.len() >= PLAYWRIGHT_MAX_RECOVERY_CONTAINERS
            {
                return Err(PlaywrightSandboxError::ProviderOutcomeUnproven);
            }
            containers.push(id.to_owned());
        }
        Ok(containers)
    }

    pub fn start(
        &self,
        request: &PlaywrightTestRequest,
    ) -> Result<(String, PlaywrightTestStatus), PlaywrightSandboxError> {
        self.prune_orphaned_artifacts()?;
        let workspace = self.validate_request(request)?;
        let artifact_dir = self.create_artifact_dir()?;
        let container_name = random_id("cumg-pw-");
        let process =
            self.build_process_request(request, &workspace, &artifact_dir, &container_name);

        let mut artifacts = self
            .artifacts
            .lock()
            .map_err(|_| PlaywrightSandboxError::LockPoisoned)?;
        let (locator, status) = match self.jobs.start(&process) {
            Ok(started) => started,
            Err(error) => {
                if error.outcome_unproven_stage().is_none() {
                    let _ = remove_private_directory(&artifact_dir);
                } else {
                    self.provider_indeterminate.store(true, Ordering::SeqCst);
                }
                return Err(PlaywrightSandboxError::ManagedJob(error));
            }
        };
        if artifacts
            .insert(
                locator.clone(),
                ArtifactRecord {
                    directory: artifact_dir.clone(),
                    container_name,
                    hard_deadline: Instant::now() + Duration::from_millis(request.hard_lifetime_ms),
                    provider_terminal_proven: false,
                    expired: false,
                },
            )
            .is_some()
        {
            drop(artifacts);
            let cleanup = self.stop_provider_aware(&locator, false);
            let _ = remove_private_directory(&artifact_dir);
            return match cleanup {
                Ok(_) => Err(PlaywrightSandboxError::IdentifierCollision),
                Err(error) => Err(error),
            };
        }
        drop(artifacts);

        let monitor = self.clone();
        let monitor_locator = locator.clone();
        if thread::Builder::new()
            .name("cumg-playwright-monitor".into())
            .spawn(move || monitor.monitor_job(monitor_locator))
            .is_err()
        {
            self.stop_provider_aware(&locator, false)?;
            return Err(PlaywrightSandboxError::MonitorUnavailable);
        }

        Ok((locator.clone(), self.decorate_status(&locator, status)?))
    }

    pub fn status(&self, locator: &str) -> Result<PlaywrightTestStatus, PlaywrightSandboxError> {
        let job = self
            .jobs
            .status(locator)
            .map_err(PlaywrightSandboxError::ManagedJob)?;
        if job.state.is_terminal() && !self.provider_terminal_is_proven(locator)? {
            if self.prove_provider_absent(locator).is_err() {
                self.provider_indeterminate.store(true, Ordering::SeqCst);
                return Err(PlaywrightSandboxError::ProviderOutcomeUnproven);
            }
            self.mark_provider_terminal_proven(locator)?;
        }
        self.decorate_status(locator, job)
    }

    pub fn output(
        &self,
        locator: &str,
        stream: ProcessOutputStream,
        offset: u64,
        max_bytes: u64,
    ) -> Result<ManagedJobOutputRange, PlaywrightSandboxError> {
        let max_bytes =
            usize::try_from(max_bytes).map_err(|_| PlaywrightSandboxError::InvalidRequest)?;
        self.jobs
            .output(locator, stream, offset, max_bytes)
            .map_err(PlaywrightSandboxError::ManagedJob)
    }

    pub fn stop(&self, locator: &str) -> Result<PlaywrightTestStatus, PlaywrightSandboxError> {
        self.stop_provider_aware(locator, false)
    }

    pub fn shutdown_jobs(&self) -> Result<(), PlaywrightSandboxError> {
        let locators = self
            .artifacts
            .lock()
            .map_err(|_| PlaywrightSandboxError::LockPoisoned)?
            .keys()
            .cloned()
            .collect::<Vec<_>>();
        let mut first_error = None;
        for locator in locators {
            if let Err(error) = self.stop_provider_aware(&locator, false)
                && first_error.is_none()
            {
                first_error = Some(error);
            }
        }
        let managed_cleanup = self
            .jobs
            .shutdown_all()
            .map_err(PlaywrightSandboxError::ManagedJob);
        if let Some(error) = first_error {
            self.provider_indeterminate.store(true, Ordering::SeqCst);
            let _ = managed_cleanup;
            return Err(error);
        }
        managed_cleanup
    }

    pub fn cleanup_artifacts(&self) -> Result<(), PlaywrightSandboxError> {
        remove_artifact_root_contents(&self.artifact_root)?;
        self.artifacts
            .lock()
            .map_err(|_| PlaywrightSandboxError::LockPoisoned)?
            .clear();
        Ok(())
    }

    pub fn shutdown_all(&self) -> Result<(), PlaywrightSandboxError> {
        self.shutdown_jobs()?;
        self.cleanup_artifacts()
    }

    pub fn has_indeterminate_termination(&self) -> Result<bool, PlaywrightSandboxError> {
        if self.provider_indeterminate.load(Ordering::SeqCst) {
            return Ok(true);
        }
        self.jobs
            .has_indeterminate_termination()
            .map_err(PlaywrightSandboxError::ManagedJob)
    }

    pub fn active_count(&self) -> Result<usize, PlaywrightSandboxError> {
        self.jobs
            .active_count()
            .map_err(PlaywrightSandboxError::ManagedJob)
    }

    fn stop_provider_aware(
        &self,
        locator: &str,
        expired: bool,
    ) -> Result<PlaywrightTestStatus, PlaywrightSandboxError> {
        let container_name = self.container_name(locator)?;
        if expired {
            let mut records = self
                .artifacts
                .lock()
                .map_err(|_| PlaywrightSandboxError::LockPoisoned)?;
            let record = records
                .get_mut(locator)
                .ok_or(PlaywrightSandboxError::UnknownJob)?;
            record.expired = true;
        }

        // Mark the lifecycle intent before provider cleanup. Otherwise an attached
        // runtime client can observe the container removal and race into a natural
        // Completed state even though the caller explicitly requested stop.
        self.jobs
            .request_stop(locator)
            .map_err(PlaywrightSandboxError::ManagedJob)?;

        // First request provider cleanup to reduce the creation/termination race.
        let _ = self.request_provider_remove(&container_name);

        // Reap/terminate the attached runtime client under the existing proven
        // host process-domain contract, but never treat that as container proof.
        let job_result = self.jobs.stop(locator);

        // Repeat provider cleanup after the client is terminal so a container
        // created concurrently with the first request cannot escape supervision.
        let provider_result = self.remove_and_prove_provider_absent(&container_name);

        match (job_result, provider_result) {
            (Ok(job), Ok(())) => {
                self.mark_provider_terminal_proven(locator)?;
                self.decorate_status(locator, job)
            }
            (Err(error), _) if error.outcome_unproven_stage().is_some() => {
                self.provider_indeterminate.store(true, Ordering::SeqCst);
                Err(PlaywrightSandboxError::ManagedJob(error))
            }
            (_, Err(error)) => {
                self.provider_indeterminate.store(true, Ordering::SeqCst);
                Err(error)
            }
            (Err(error), Ok(())) => Err(PlaywrightSandboxError::ManagedJob(error)),
        }
    }

    fn monitor_job(&self, locator: String) {
        loop {
            let (deadline, directory) = match self.record_snapshot(&locator) {
                Ok(record) => (record.0, record.1),
                Err(_) => return,
            };
            if inspect_artifacts(&directory).is_err() {
                if self.stop_provider_aware(&locator, false).is_err() {
                    self.provider_indeterminate.store(true, Ordering::SeqCst);
                }
                return;
            }
            match self.jobs.status(&locator) {
                Ok(status) if status.state.is_terminal() => {
                    let _ = status;
                    let container_name = match self.container_name(&locator) {
                        Ok(name) => name,
                        Err(_) => {
                            self.provider_indeterminate.store(true, Ordering::SeqCst);
                            return;
                        }
                    };
                    if self
                        .remove_and_prove_provider_absent(&container_name)
                        .is_ok()
                    {
                        let _ = self.mark_provider_terminal_proven(&locator);
                    } else {
                        self.provider_indeterminate.store(true, Ordering::SeqCst);
                    }
                    return;
                }
                Ok(_) => {}
                Err(ManagedJobError::UnknownJob) => {
                    let cleanup = self
                        .container_name(&locator)
                        .and_then(|name| self.remove_and_prove_provider_absent(&name));
                    if cleanup.is_err() {
                        self.provider_indeterminate.store(true, Ordering::SeqCst);
                    }
                    return;
                }
                Err(_) => {
                    self.provider_indeterminate.store(true, Ordering::SeqCst);
                    return;
                }
            }
            if Instant::now() >= deadline {
                if self.stop_provider_aware(&locator, true).is_err() {
                    self.provider_indeterminate.store(true, Ordering::SeqCst);
                }
                return;
            }
            thread::sleep(PLAYWRIGHT_MONITOR_POLL);
        }
    }

    fn run_provider_command(
        &self,
        args: Vec<String>,
    ) -> Result<crate::v2_m0::ProcessOutput, PlaywrightSandboxError> {
        let request = ProcessRequest {
            program: self.config.runtime.to_string_lossy().into_owned(),
            args,
            cwd: self.runtime_cwd.to_string_lossy().into_owned(),
            env: vec![],
            timeout_ms: PLAYWRIGHT_PROVIDER_COMMAND_TIMEOUT_MS,
        };
        self.probe_executor
            .execute(&request, &ProcessCancellation::default())
            .map_err(PlaywrightSandboxError::Process)
    }

    fn request_provider_remove(&self, container_name: &str) -> Result<(), PlaywrightSandboxError> {
        let _ = self.run_provider_command(vec![
            "container".into(),
            "rm".into(),
            "-f".into(),
            container_name.to_owned(),
        ])?;
        Ok(())
    }

    fn remove_and_prove_provider_absent(
        &self,
        container_name: &str,
    ) -> Result<(), PlaywrightSandboxError> {
        if self.request_provider_remove(container_name).is_err() {
            return Err(PlaywrightSandboxError::ProviderOutcomeUnproven);
        }
        let deadline = Instant::now() + PLAYWRIGHT_PROVIDER_PROOF_GRACE;
        loop {
            let output = self
                .run_provider_command(vec![
                    "container".into(),
                    "ps".into(),
                    "-a".into(),
                    "--filter".into(),
                    format!("name=^/{container_name}$"),
                    "--format".into(),
                    "{{.ID}}".into(),
                ])
                .map_err(|_| PlaywrightSandboxError::ProviderOutcomeUnproven)?;
            if output.exit_code == Some(0)
                && !output.timed_out
                && !output.cancelled
                && output.stdout.trim().is_empty()
            {
                return Ok(());
            }
            if Instant::now() >= deadline {
                return Err(PlaywrightSandboxError::ProviderOutcomeUnproven);
            }
            thread::sleep(Duration::from_millis(50));
        }
    }

    fn prove_provider_absent(&self, locator: &str) -> Result<(), PlaywrightSandboxError> {
        let container_name = self.container_name(locator)?;
        let output = self
            .run_provider_command(vec![
                "container".into(),
                "ps".into(),
                "-a".into(),
                "--filter".into(),
                format!("name=^/{container_name}$"),
                "--format".into(),
                "{{.ID}}".into(),
            ])
            .map_err(|_| PlaywrightSandboxError::ProviderOutcomeUnproven)?;
        if output.exit_code == Some(0)
            && !output.timed_out
            && !output.cancelled
            && output.stdout.trim().is_empty()
        {
            Ok(())
        } else {
            Err(PlaywrightSandboxError::ProviderOutcomeUnproven)
        }
    }

    fn container_name(&self, locator: &str) -> Result<String, PlaywrightSandboxError> {
        self.artifacts
            .lock()
            .map_err(|_| PlaywrightSandboxError::LockPoisoned)?
            .get(locator)
            .map(|record| record.container_name.clone())
            .ok_or(PlaywrightSandboxError::UnknownJob)
    }

    fn record_snapshot(&self, locator: &str) -> Result<(Instant, PathBuf), PlaywrightSandboxError> {
        self.artifacts
            .lock()
            .map_err(|_| PlaywrightSandboxError::LockPoisoned)?
            .get(locator)
            .map(|record| (record.hard_deadline, record.directory.clone()))
            .ok_or(PlaywrightSandboxError::UnknownJob)
    }

    fn provider_terminal_is_proven(&self, locator: &str) -> Result<bool, PlaywrightSandboxError> {
        self.artifacts
            .lock()
            .map_err(|_| PlaywrightSandboxError::LockPoisoned)?
            .get(locator)
            .map(|record| record.provider_terminal_proven)
            .ok_or(PlaywrightSandboxError::UnknownJob)
    }

    fn mark_provider_terminal_proven(&self, locator: &str) -> Result<(), PlaywrightSandboxError> {
        let mut records = self
            .artifacts
            .lock()
            .map_err(|_| PlaywrightSandboxError::LockPoisoned)?;
        let record = records
            .get_mut(locator)
            .ok_or(PlaywrightSandboxError::UnknownJob)?;
        record.provider_terminal_proven = true;
        Ok(())
    }

    fn decorate_status(
        &self,
        locator: &str,
        mut job: ManagedJobStatus,
    ) -> Result<PlaywrightTestStatus, PlaywrightSandboxError> {
        let records = self
            .artifacts
            .lock()
            .map_err(|_| PlaywrightSandboxError::LockPoisoned)?;
        let record = records
            .get(locator)
            .ok_or(PlaywrightSandboxError::UnknownJob)?;
        let remaining = record
            .hard_deadline
            .saturating_duration_since(Instant::now())
            .as_millis()
            .min(u128::from(u64::MAX)) as u64;
        job.hard_lifetime_remaining_ms = remaining;
        job.lease_remaining_ms = remaining;
        if record.expired && record.provider_terminal_proven && job.state.is_terminal() {
            job.state = crate::v2_managed_job::ManagedJobState::Expired;
        }
        let directory = record.directory.clone();
        drop(records);
        let (artifact_count, artifact_total_bytes) = inspect_artifacts(&directory)?;
        Ok(PlaywrightTestStatus {
            job,
            artifact_count,
            artifact_total_bytes,
        })
    }

    fn validate_request(
        &self,
        request: &PlaywrightTestRequest,
    ) -> Result<PathBuf, PlaywrightSandboxError> {
        if request.hard_lifetime_ms == 0
            || request.hard_lifetime_ms > MAX_PLAYWRIGHT_TEST_LIFETIME_MS
            || request.test_paths.is_empty()
            || request.test_paths.len() > MAX_PLAYWRIGHT_TEST_PATHS
        {
            return Err(PlaywrightSandboxError::InvalidRequest);
        }
        let workspace_path = Path::new(&request.workspace);
        if !workspace_path.is_absolute() {
            return Err(PlaywrightSandboxError::InvalidWorkspace);
        }
        let workspace = fs::canonicalize(workspace_path)
            .map_err(|_| PlaywrightSandboxError::InvalidWorkspace)?;
        if !workspace.is_dir()
            || workspace.to_string_lossy().contains(',')
            || !self
                .config
                .allowed_workspace_roots
                .iter()
                .any(|root| workspace.starts_with(root))
        {
            return Err(PlaywrightSandboxError::InvalidWorkspace);
        }
        for path in &request.test_paths {
            validate_test_path(path)?;
        }
        if let Some(project) = request.project.as_deref() {
            validate_plain_value(project, MAX_PLAYWRIGHT_PROJECT_BYTES)
                .map_err(|_| PlaywrightSandboxError::InvalidProject)?;
        }
        if let Some(grep) = request.grep.as_deref() {
            validate_plain_value(grep, MAX_PLAYWRIGHT_GREP_BYTES)
                .map_err(|_| PlaywrightSandboxError::InvalidGrep)?;
        }
        if request
            .workers
            .is_some_and(|workers| workers == 0 || workers > MAX_PLAYWRIGHT_WORKERS)
        {
            return Err(PlaywrightSandboxError::InvalidWorkers);
        }
        Ok(workspace)
    }

    fn build_process_request(
        &self,
        request: &PlaywrightTestRequest,
        workspace: &Path,
        artifact_dir: &Path,
        container_name: &str,
    ) -> ProcessRequest {
        let mut args = vec![
            "run".to_owned(),
            "--rm".to_owned(),
            "--init".to_owned(),
            "--name".to_owned(),
            container_name.to_owned(),
            "--pull=never".to_owned(),
            "--label".to_owned(),
            PLAYWRIGHT_PROVIDER_LABEL.to_owned(),
            "--label".to_owned(),
            format!(
                "{PLAYWRIGHT_PROVIDER_OWNER_LABEL_KEY}={}",
                self.provider_owner_label
            ),
            "--network".to_owned(),
            "none".to_owned(),
            "--read-only".to_owned(),
            "--cap-drop".to_owned(),
            "ALL".to_owned(),
            "--security-opt".to_owned(),
            "no-new-privileges".to_owned(),
            "--pids-limit".to_owned(),
            PLAYWRIGHT_PIDS_LIMIT.to_owned(),
            "--memory".to_owned(),
            PLAYWRIGHT_MEMORY_LIMIT.to_owned(),
            "--cpus".to_owned(),
            PLAYWRIGHT_CPU_LIMIT.to_owned(),
            "--shm-size".to_owned(),
            PLAYWRIGHT_SHM_SIZE.to_owned(),
            "--user".to_owned(),
            "pwuser".to_owned(),
            "--tmpfs".to_owned(),
            "/tmp:rw,nosuid,nodev,size=512m".to_owned(),
            "--tmpfs".to_owned(),
            format!("{PLAYWRIGHT_CONTAINER_HOME}:rw,nosuid,nodev,size=256m"),
            "--mount".to_owned(),
            format!(
                "type=bind,src={},dst={PLAYWRIGHT_CONTAINER_WORKSPACE},readonly",
                workspace.display()
            ),
            "--mount".to_owned(),
            format!(
                "type=bind,src={},dst={PLAYWRIGHT_CONTAINER_ARTIFACTS}",
                artifact_dir.display()
            ),
            "--workdir".to_owned(),
            PLAYWRIGHT_CONTAINER_WORKSPACE.to_owned(),
            "--env".to_owned(),
            format!("HOME={PLAYWRIGHT_CONTAINER_HOME}"),
            "--env".to_owned(),
            "CI=1".to_owned(),
            "--env".to_owned(),
            "PLAYWRIGHT_BROWSERS_PATH=/ms-playwright".to_owned(),
            self.config.image.clone(),
            PLAYWRIGHT_BINARY.to_owned(),
            "test".to_owned(),
        ];
        args.extend(request.test_paths.iter().cloned());
        if let Some(project) = request.project.as_deref() {
            args.push(format!("--project={project}"));
        }
        if let Some(grep) = request.grep.as_deref() {
            args.push(format!("--grep={grep}"));
        }
        args.push(format!(
            "--workers={}",
            request.workers.unwrap_or(1).min(MAX_PLAYWRIGHT_WORKERS)
        ));
        args.push("--reporter=line".to_owned());
        args.push(format!(
            "--output={PLAYWRIGHT_CONTAINER_ARTIFACTS}/test-results"
        ));

        ProcessRequest {
            program: self.config.runtime.to_string_lossy().into_owned(),
            args,
            cwd: self.runtime_cwd.to_string_lossy().into_owned(),
            env: vec![],
            timeout_ms: request
                .hard_lifetime_ms
                .saturating_add(PLAYWRIGHT_PROVIDER_CLEANUP_BUFFER_MS),
        }
    }

    fn create_artifact_dir(&self) -> Result<PathBuf, PlaywrightSandboxError> {
        for _ in 0..8 {
            let id = random_id("run_");
            let path = self.artifact_root.join(id);
            match fs::create_dir(&path) {
                Ok(()) => {
                    prepare_container_writable_directory(&path)
                        .map_err(|_| PlaywrightSandboxError::ArtifactIo)?;
                    return Ok(path);
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(_) => return Err(PlaywrightSandboxError::ArtifactIo),
            }
        }
        Err(PlaywrightSandboxError::IdentifierCollision)
    }

    fn prune_orphaned_artifacts(&self) -> Result<(), PlaywrightSandboxError> {
        let entries = self
            .artifacts
            .lock()
            .map_err(|_| PlaywrightSandboxError::LockPoisoned)?
            .iter()
            .map(|(locator, record)| {
                (
                    locator.clone(),
                    record.directory.clone(),
                    record.provider_terminal_proven,
                )
            })
            .collect::<Vec<_>>();
        let mut stale = Vec::new();
        for (locator, directory, provider_terminal_proven) in entries {
            match self.jobs.status(&locator) {
                Ok(_) => {}
                Err(ManagedJobError::UnknownJob) if provider_terminal_proven => {
                    stale.push((locator, directory));
                }
                Err(ManagedJobError::UnknownJob) => {
                    self.provider_indeterminate.store(true, Ordering::SeqCst);
                    return Err(PlaywrightSandboxError::ProviderOutcomeUnproven);
                }
                Err(error) => return Err(PlaywrightSandboxError::ManagedJob(error)),
            }
        }
        if stale.is_empty() {
            return Ok(());
        }
        let mut artifacts = self
            .artifacts
            .lock()
            .map_err(|_| PlaywrightSandboxError::LockPoisoned)?;
        for (locator, directory) in stale {
            remove_private_directory(&directory)?;
            artifacts.remove(&locator);
        }
        Ok(())
    }

    #[cfg(test)]
    fn test_build_process_request(
        &self,
        request: &PlaywrightTestRequest,
        artifact_dir: &Path,
    ) -> Result<ProcessRequest, PlaywrightSandboxError> {
        let workspace = self.validate_request(request)?;
        Ok(self.build_process_request(request, &workspace, artifact_dir, "cumg-pw-test"))
    }
}

#[derive(Debug)]
pub enum PlaywrightSandboxError {
    InvalidConfig,
    RuntimeUnavailable,
    InvalidImage,
    InvalidWorkspace,
    InvalidRequest,
    InvalidTestPath,
    InvalidProject,
    InvalidGrep,
    InvalidWorkers,
    UnknownJob,
    IdentifierCollision,
    MonitorUnavailable,
    ProviderOutcomeUnproven,
    ArtifactIo,
    LockPoisoned,
    Process(ProcessError),
    ManagedJob(ManagedJobError),
}

impl PlaywrightSandboxError {
    pub fn outcome_unproven_stage(&self) -> Option<ProcessUnprovenStage> {
        match self {
            Self::ManagedJob(error) => error.outcome_unproven_stage(),
            Self::Process(error) => error.outcome_unproven_stage(),
            _ => None,
        }
    }
}

impl SafeErrorCode for PlaywrightSandboxError {
    fn safe_error_code(&self) -> &'static str {
        match self {
            Self::InvalidConfig => "playwright_sandbox_invalid_config",
            Self::RuntimeUnavailable => "playwright_sandbox_runtime_unavailable",
            Self::InvalidImage => "playwright_sandbox_invalid_image",
            Self::InvalidWorkspace => "playwright_sandbox_invalid_workspace",
            Self::InvalidRequest => "playwright_sandbox_invalid_request",
            Self::InvalidTestPath => "playwright_sandbox_invalid_test_path",
            Self::InvalidProject => "playwright_sandbox_invalid_project",
            Self::InvalidGrep => "playwright_sandbox_invalid_grep",
            Self::InvalidWorkers => "playwright_sandbox_invalid_workers",
            Self::UnknownJob => "playwright_sandbox_unknown_job",
            Self::IdentifierCollision => "playwright_sandbox_identifier_collision",
            Self::MonitorUnavailable => "playwright_sandbox_monitor_unavailable",
            Self::ProviderOutcomeUnproven => "playwright_sandbox_provider_outcome_unproven",
            Self::ArtifactIo => "playwright_sandbox_artifact_io",
            Self::LockPoisoned => "playwright_sandbox_lock_poisoned",
            Self::Process(error) => error.safe_error_code(),
            Self::ManagedJob(error) => error.safe_error_code(),
        }
    }
}

impl std::fmt::Display for PlaywrightSandboxError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.safe_error_code())
    }
}

impl std::error::Error for PlaywrightSandboxError {}

fn validate_image_reference(image: &str) -> Result<(), PlaywrightSandboxError> {
    if image.is_empty() || image.len() > 512 || image.chars().any(char::is_whitespace) {
        return Err(PlaywrightSandboxError::InvalidImage);
    }
    let Some((name, digest)) = image.rsplit_once("@sha256:") else {
        return Err(PlaywrightSandboxError::InvalidImage);
    };
    if name.is_empty()
        || name.starts_with('-')
        || !name.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-' | b'/' | b':')
        })
        || digest.len() != 64
        || !digest
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(PlaywrightSandboxError::InvalidImage);
    }
    Ok(())
}

fn validate_test_path(value: &str) -> Result<(), PlaywrightSandboxError> {
    if value.is_empty()
        || value.len() > MAX_PLAYWRIGHT_TEST_PATH_BYTES
        || value.starts_with('-')
        || value.contains('\0')
    {
        return Err(PlaywrightSandboxError::InvalidTestPath);
    }
    let path = Path::new(value);
    if path.is_absolute()
        || path.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
    {
        return Err(PlaywrightSandboxError::InvalidTestPath);
    }
    Ok(())
}

fn validate_plain_value(value: &str, max_bytes: usize) -> Result<(), ()> {
    if value.is_empty()
        || value.len() > max_bytes
        || value.contains('\0')
        || value.contains('\r')
        || value.contains('\n')
    {
        return Err(());
    }
    Ok(())
}

fn prepare_artifact_root(state_dir: &Path) -> Result<PathBuf, PlaywrightSandboxError> {
    let root = state_dir.join(PLAYWRIGHT_ARTIFACT_ROOT_NAME);
    match fs::symlink_metadata(&root) {
        Ok(metadata) => {
            if metadata.file_type().is_symlink() || !metadata.is_dir() {
                return Err(PlaywrightSandboxError::ArtifactIo);
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            fs::create_dir(&root).map_err(|_| PlaywrightSandboxError::ArtifactIo)?;
        }
        Err(_) => return Err(PlaywrightSandboxError::ArtifactIo),
    }
    harden_directory_permissions(&root).map_err(|_| PlaywrightSandboxError::ArtifactIo)?;
    let canonical = fs::canonicalize(&root).map_err(|_| PlaywrightSandboxError::ArtifactIo)?;
    let canonical_state =
        fs::canonicalize(state_dir).map_err(|_| PlaywrightSandboxError::ArtifactIo)?;
    if canonical.parent() != Some(canonical_state.as_path()) {
        return Err(PlaywrightSandboxError::ArtifactIo);
    }
    Ok(canonical)
}

fn remove_artifact_root_contents(root: &Path) -> Result<(), PlaywrightSandboxError> {
    let entries = fs::read_dir(root).map_err(|_| PlaywrightSandboxError::ArtifactIo)?;
    for entry in entries {
        let entry = entry.map_err(|_| PlaywrightSandboxError::ArtifactIo)?;
        remove_private_directory(&entry.path())?;
    }
    Ok(())
}

fn remove_private_directory(path: &Path) -> Result<(), PlaywrightSandboxError> {
    let metadata = fs::symlink_metadata(path).map_err(|_| PlaywrightSandboxError::ArtifactIo)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(PlaywrightSandboxError::ArtifactIo);
    }
    fs::remove_dir_all(path).map_err(|_| PlaywrightSandboxError::ArtifactIo)
}

fn inspect_artifacts(directory: &Path) -> Result<(u64, u64), PlaywrightSandboxError> {
    let mut count = 0_u64;
    let mut bytes = 0_u64;
    let mut directories = 0_u64;
    let mut pending = vec![directory.to_path_buf()];
    while let Some(current) = pending.pop() {
        directories = directories.saturating_add(1);
        if directories > 4096 {
            return Err(PlaywrightSandboxError::ArtifactIo);
        }
        for entry in fs::read_dir(&current).map_err(|_| PlaywrightSandboxError::ArtifactIo)? {
            let entry = entry.map_err(|_| PlaywrightSandboxError::ArtifactIo)?;
            let metadata = fs::symlink_metadata(entry.path())
                .map_err(|_| PlaywrightSandboxError::ArtifactIo)?;
            if metadata.file_type().is_symlink() {
                return Err(PlaywrightSandboxError::ArtifactIo);
            }
            if metadata.is_dir() {
                pending.push(entry.path());
            } else if metadata.is_file() {
                count = count.saturating_add(1);
                bytes = bytes.saturating_add(metadata.len());
                if count > 4096 || bytes > 512 * 1024 * 1024 {
                    return Err(PlaywrightSandboxError::ArtifactIo);
                }
            } else {
                return Err(PlaywrightSandboxError::ArtifactIo);
            }
        }
    }
    Ok((count, bytes))
}

fn random_id(prefix: &str) -> String {
    let mut bytes = [0_u8; 16];
    OsRng.fill_bytes(&mut bytes);
    let mut value = String::with_capacity(prefix.len() + bytes.len() * 2);
    value.push_str(prefix);
    for byte in bytes {
        let _ = write!(&mut value, "{byte:02x}");
    }
    value
}

#[cfg(unix)]
fn harden_directory_permissions(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt as _;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
}

#[cfg(not(unix))]
fn harden_directory_permissions(_path: &Path) -> std::io::Result<()> {
    Ok(())
}
#[cfg(unix)]
fn prepare_container_writable_directory(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt as _;
    // The parent artifact root is 0700, so other host users cannot traverse it.
    // The run directory itself must be writable by the unprivileged container UID.
    fs::set_permissions(path, fs::Permissions::from_mode(0o777))
}

#[cfg(not(unix))]
fn prepare_container_writable_directory(_path: &Path) -> std::io::Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;

    fn temp_root(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "cumg-playwright-{name}-{}-{}",
            std::process::id(),
            random_id("")
        ));
        fs::create_dir_all(&root).unwrap();
        root
    }

    fn fake_runtime(root: &Path) -> PathBuf {
        let path = root.join(if cfg!(windows) {
            "fake-runtime.exe"
        } else {
            "fake-runtime"
        });
        let mut file = fs::File::create(&path).unwrap();
        file.write_all(b"fake").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
        }
        path
    }

    #[cfg(unix)]
    fn fake_container_runtime(root: &Path) -> PathBuf {
        use std::os::unix::fs::PermissionsExt as _;

        let containers = root.join("fake-containers");
        fs::create_dir_all(&containers).unwrap();
        let path = root.join("fake-container-runtime");
        let script = format!(
            r#"#!/bin/sh
set -e
ROOT="{root}"
cmd="$1"

if [ "$cmd" = "image" ] && [ "$2" = "inspect" ]; then
  echo "sha256:fake"
  exit 0
fi

if [ "$cmd" = "container" ] && [ "$2" = "rm" ]; then
  last=""
  for arg in "$@"; do last="$arg"; done
  /bin/rm -f "$ROOT/$last"
  exit 0
fi

if [ "$cmd" = "container" ] && [ "$2" = "ps" ]; then
  for entry in "$ROOT"/*; do
    if [ -f "$entry" ]; then
      /usr/bin/basename "$entry"
    fi
  done
  exit 0
fi

if [ "$cmd" = "run" ]; then
  name=""
  natural=0
  previous=""
  for arg in "$@"; do
    if [ "$previous" = "--name" ]; then
      name="$arg"
    fi
    case "$arg" in
      *natural.spec.ts) natural=1 ;;
    esac
    previous="$arg"
  done
  [ -n "$name" ] || exit 64
  : > "$ROOT/$name"
  if [ "$natural" = "1" ]; then
    /bin/sleep 0.05
    /bin/rm -f "$ROOT/$name"
    exit 0
  fi
  while [ -f "$ROOT/$name" ]; do
    /bin/sleep 0.02
  done
  exit 0
fi

exit 64
"#,
            root = containers.display()
        );
        fs::write(&path, script).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
        path
    }

    #[cfg(unix)]
    fn provider_runner(root: &Path) -> PlaywrightSandboxRunner {
        let workspace = root.join("workspace");
        let state = root.join("state");
        fs::create_dir_all(&workspace).unwrap();
        fs::create_dir_all(&state).unwrap();
        let config =
            PlaywrightSandboxConfig::new(fake_container_runtime(root), image(), vec![workspace])
                .unwrap();
        let runner = PlaywrightSandboxRunner::new(config, &state, "dev-provider-test").unwrap();
        runner.probe_provider().unwrap();
        runner.recover_provider_orphans().unwrap();
        runner
    }

    #[cfg(unix)]
    fn wait_for_terminal_status(
        runner: &PlaywrightSandboxRunner,
        locator: &str,
        timeout: Duration,
    ) -> PlaywrightTestStatus {
        let deadline = Instant::now() + timeout;
        loop {
            match runner.status(locator) {
                Ok(status) if status.job.state.is_terminal() => return status,
                Ok(_) => {}
                Err(PlaywrightSandboxError::ProviderOutcomeUnproven) => {
                    panic!("provider terminal proof unexpectedly failed")
                }
                Err(error) => panic!("unexpected status error: {}", error.safe_error_code()),
            }
            assert!(
                Instant::now() < deadline,
                "timed out waiting for terminal status"
            );
            thread::sleep(Duration::from_millis(20));
        }
    }

    fn image() -> String {
        format!("example.invalid/playwright@sha256:{}", "a".repeat(64))
    }

    fn runner(root: &Path) -> PlaywrightSandboxRunner {
        let workspace = root.join("workspace");
        let state = root.join("state");
        fs::create_dir_all(&workspace).unwrap();
        fs::create_dir_all(&state).unwrap();
        let config =
            PlaywrightSandboxConfig::new(fake_runtime(root), image(), vec![workspace]).unwrap();
        PlaywrightSandboxRunner::new(config, &state, "dev-test").unwrap()
    }

    fn request(root: &Path) -> PlaywrightTestRequest {
        PlaywrightTestRequest {
            workspace: root.join("workspace").to_string_lossy().into_owned(),
            test_paths: vec!["tests/e2e.spec.ts".into()],
            project: Some("chromium".into()),
            grep: Some("smoke".into()),
            workers: Some(2),
            hard_lifetime_ms: 60_000,
        }
    }

    #[test]
    fn config_requires_digest_pinned_image_and_absolute_regular_runtime() {
        let root = temp_root("config");
        let workspace = root.join("workspace");
        fs::create_dir_all(&workspace).unwrap();
        let runtime = fake_runtime(&root);

        assert!(
            PlaywrightSandboxConfig::new(
                runtime.clone(),
                "mcr.microsoft.com/playwright:v1.63.0".into(),
                vec![workspace.clone()],
            )
            .is_err()
        );
        assert!(PlaywrightSandboxConfig::new(
            PathBuf::from("docker"),
            image(),
            vec![workspace.clone()],
        )
        .is_err());
        assert!(
            PlaywrightSandboxConfig::new(runtime, image(), vec![workspace])
                .unwrap()
                .image()
                .contains("@sha256:")
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn typed_request_rejects_path_option_and_parent_escape() {
        let root = temp_root("paths");
        let runner = runner(&root);
        for bad in ["../secret.spec.ts", "/tmp/test.spec.ts", "--config=evil.ts"] {
            let mut request = request(&root);
            request.test_paths = vec![bad.into()];
            assert!(matches!(
                runner.validate_request(&request),
                Err(PlaywrightSandboxError::InvalidTestPath)
            ));
        }
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn runtime_argv_is_fixed_isolated_and_host_environment_free() {
        let root = temp_root("argv");
        let runner = runner(&root);
        let artifact = root.join("artifact");
        fs::create_dir_all(&artifact).unwrap();
        let process = runner
            .test_build_process_request(&request(&root), &artifact)
            .unwrap();

        assert!(process.env.is_empty());
        assert_eq!(process.program, runner.config.runtime.to_string_lossy());
        assert_eq!(
            process.timeout_ms,
            request(&root).hard_lifetime_ms + PLAYWRIGHT_PROVIDER_CLEANUP_BUFFER_MS
        );
        let joined = process.args.join("\n");
        for required in [
            "--name\ncumg-pw-test",
            "--pull=never",
            "io.cumg.playwright=v1",
            "io.cumg.owner=",
            "--network\nnone",
            "--read-only",
            "--cap-drop\nALL",
            "--security-opt\nno-new-privileges",
            "--user\npwuser",
            "HOME=/home/pwuser",
            "CI=1",
            "PLAYWRIGHT_BROWSERS_PATH=/ms-playwright",
            PLAYWRIGHT_BINARY,
            "--output=/artifacts/test-results",
        ] {
            assert!(joined.contains(required), "missing {required}");
        }
        assert!(joined.contains(&format!(
            "type=bind,src={},dst=/workspace,readonly",
            root.join("workspace").canonicalize().unwrap().display()
        )));
        assert!(joined.contains(&format!(
            "type=bind,src={},dst=/artifacts",
            artifact.display()
        )));
        for forbidden in [
            "SSH_AUTH_SOCK",
            "/.ssh",
            ".docker",
            "--user-data-dir",
            "connect-over-cdp",
            "/var/run/docker.sock",
            "bash",
            "sh -c",
            "--network\nhost",
        ] {
            assert!(!joined.contains(forbidden), "unexpected {forbidden}");
        }
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn workspace_must_be_inside_playwright_specific_root() {
        let root = temp_root("workspace");
        let runner = runner(&root);
        let outside = temp_root("outside");
        let mut request = request(&root);
        request.workspace = outside.to_string_lossy().into_owned();
        assert!(matches!(
            runner.validate_request(&request),
            Err(PlaywrightSandboxError::InvalidWorkspace)
        ));
        let _ = fs::remove_dir_all(outside);
        let comma_workspace = root.join("workspace").join("nested,workspace");
        fs::create_dir_all(&comma_workspace).unwrap();
        request.workspace = comma_workspace.to_string_lossy().into_owned();
        assert!(matches!(
            runner.validate_request(&request),
            Err(PlaywrightSandboxError::InvalidWorkspace)
        ));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn artifact_inspection_rejects_symlinks() {
        let root = temp_root("artifact-symlink");
        let dir = root.join("artifacts");
        fs::create_dir_all(&dir).unwrap();
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink("/tmp", dir.join("escape")).unwrap();
            assert!(inspect_artifacts(&dir).is_err());
        }
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn orphan_prune_preserves_artifacts_without_provider_terminal_proof() {
        let root = temp_root("artifact-proof");
        let runner = runner(&root);
        let directory = runner.create_artifact_dir().unwrap();
        let locator = "missing-managed-job".to_owned();
        runner.artifacts.lock().unwrap().insert(
            locator.clone(),
            ArtifactRecord {
                directory: directory.clone(),
                container_name: "cumg-pw-missing".to_owned(),
                hard_deadline: Instant::now() + Duration::from_secs(60),
                provider_terminal_proven: false,
                expired: false,
            },
        );

        assert!(matches!(
            runner.prune_orphaned_artifacts(),
            Err(PlaywrightSandboxError::ProviderOutcomeUnproven)
        ));
        assert!(directory.exists());
        assert!(runner.has_indeterminate_termination().unwrap());

        runner
            .artifacts
            .lock()
            .unwrap()
            .get_mut(&locator)
            .unwrap()
            .provider_terminal_proven = true;
        runner.prune_orphaned_artifacts().unwrap();
        assert!(!directory.exists());

        let _ = fs::remove_dir_all(root);
    }
    #[cfg(unix)]
    #[test]
    fn provider_lifecycle_explicit_stop_proves_container_absent() {
        let root = temp_root("provider-stop");
        let runner = provider_runner(&root);
        let (locator, started) = runner.start(&request(&root)).unwrap();
        assert_eq!(
            started.job.state,
            crate::v2_managed_job::ManagedJobState::Running
        );

        let stopped = runner.stop(&locator).unwrap();
        assert_eq!(
            stopped.job.state,
            crate::v2_managed_job::ManagedJobState::Stopped
        );
        assert!(!runner.has_indeterminate_termination().unwrap());
        runner.shutdown_all().unwrap();
        let containers = root.join("fake-containers");
        assert_eq!(fs::read_dir(containers).unwrap().count(), 0);
        let _ = fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn provider_lifecycle_natural_exit_requires_provider_absence_proof() {
        let root = temp_root("provider-natural");
        let runner = provider_runner(&root);
        let mut req = request(&root);
        req.test_paths = vec!["tests/natural.spec.ts".into()];
        let (locator, _) = runner.start(&req).unwrap();

        let status = wait_for_terminal_status(&runner, &locator, Duration::from_secs(2));
        assert_eq!(
            status.job.state,
            crate::v2_managed_job::ManagedJobState::Completed
        );
        assert!(!runner.has_indeterminate_termination().unwrap());
        runner.shutdown_all().unwrap();
        let _ = fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn provider_lifecycle_hard_expiry_is_proven_before_expired_status() {
        let root = temp_root("provider-expiry");
        let runner = provider_runner(&root);
        let mut req = request(&root);
        req.hard_lifetime_ms = 100;
        let (locator, _) = runner.start(&req).unwrap();

        let status = wait_for_terminal_status(&runner, &locator, Duration::from_secs(3));
        assert_eq!(
            status.job.state,
            crate::v2_managed_job::ManagedJobState::Expired
        );
        assert!(!runner.has_indeterminate_termination().unwrap());
        runner.shutdown_all().unwrap();
        let _ = fs::remove_dir_all(root);
    }
    #[cfg(unix)]
    #[test]
    fn startup_recovery_removes_only_owned_provider_orphans_before_artifacts() {
        let root = temp_root("provider-recovery");
        let workspace = root.join("workspace");
        let state = root.join("state");
        fs::create_dir_all(&workspace).unwrap();
        fs::create_dir_all(&state).unwrap();
        let runtime = fake_container_runtime(&root);

        let stale_artifact = state.join(PLAYWRIGHT_ARTIFACT_ROOT_NAME).join("stale-run");
        fs::create_dir_all(&stale_artifact).unwrap();

        let containers = root.join("fake-containers");
        fs::write(containers.join("orphan-owned"), b"").unwrap();

        let config = PlaywrightSandboxConfig::new(runtime, image(), vec![workspace]).unwrap();
        let runner = PlaywrightSandboxRunner::new(config, &state, "dev-provider-recovery").unwrap();

        // Constructor preserves old artifacts until provider cleanup is proven.
        assert!(stale_artifact.exists());
        runner.probe_provider().unwrap();
        runner.recover_provider_orphans().unwrap();

        assert_eq!(fs::read_dir(&containers).unwrap().count(), 0);
        assert_eq!(
            fs::read_dir(state.join(PLAYWRIGHT_ARTIFACT_ROOT_NAME))
                .unwrap()
                .count(),
            0
        );
        let _ = fs::remove_dir_all(root);
    }
}
