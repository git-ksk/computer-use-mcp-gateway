//! V2-M1 Agent-native structured process execution.
//!
//! This is deliberately separate from the GUI/backend adapter path. Requests
//! execute an explicit program + argv in an explicit working directory. The
//! structured entrypoint never inserts `sh -c`/`cmd /C`, clears the environment
//! by default, bounds output, and supports cooperative cancellation + hard timeout.
//! The crate-private shell path deliberately invokes a fixed OS shell for the
//! separately-authorized `Shell` capability while reusing process supervision.

use crate::v2_m0::{ProcessEnvVar, ProcessOutput, ProcessRequest, ShellRequest};
use crate::v2_m0_execution::{AgentExecutionGate, ExecutionError, OperationRef};
use crate::v2_observability::SafeErrorCode;
use process_wrap::std::CommandWrap;
#[cfg(windows)]
use process_wrap::std::JobObject;
#[cfg(unix)]
use process_wrap::std::ProcessGroup;
use std::collections::HashSet;
use std::fmt;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::thread;
use std::time::{Duration, Instant};

const DEFAULT_MAX_ARGS: usize = 256;
const DEFAULT_MAX_ENV: usize = 64;
const DEFAULT_MAX_OUTPUT_BYTES: usize = 16 * 1024;
const DEFAULT_MAX_TIMEOUT_MS: u64 = 10 * 60 * 1000;
const DEFAULT_POLL_MS: u64 = 10;

#[derive(Debug, Clone)]
pub struct ProcessPolicy {
    allowed_cwd_roots: Vec<PathBuf>,
    inherited_env_keys: HashSet<String>,
    explicit_env_keys: HashSet<String>,
    denied_program_names: HashSet<String>,
    max_args: usize,
    max_env_entries: usize,
    max_output_bytes_per_stream: usize,
    max_timeout_ms: u64,
    poll_interval: Duration,
}

impl ProcessPolicy {
    pub fn developer_defaults(allowed_cwd_roots: Vec<PathBuf>) -> Result<Self, ProcessError> {
        let inherited_env_keys = [
            "PATH",
            "HOME",
            "USER",
            "LOGNAME",
            "TMPDIR",
            "TMP",
            "TEMP",
            "LANG",
            "LC_ALL",
            "LC_CTYPE",
            "TERM",
            "SSH_AUTH_SOCK",
            "DEVELOPER_DIR",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect();
        let explicit_env_keys = [
            "CI",
            "RUST_LOG",
            "RUST_BACKTRACE",
            "CARGO_HOME",
            "RUSTUP_HOME",
            "NODE_ENV",
            "npm_config_cache",
            "LANG",
            "LC_ALL",
            "LC_CTYPE",
            "DEVELOPER_DIR",
            "FASTLANE_SKIP_UPDATE_CHECK",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect();
        let denied_program_names = [
            "sh",
            "bash",
            "zsh",
            "fish",
            "dash",
            "ksh",
            "csh",
            "tcsh",
            "cmd",
            "cmd.exe",
            "powershell",
            "powershell.exe",
            "pwsh",
            "pwsh.exe",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect();
        Self::new(
            allowed_cwd_roots,
            inherited_env_keys,
            explicit_env_keys,
            denied_program_names,
        )
    }

    pub fn new(
        allowed_cwd_roots: Vec<PathBuf>,
        inherited_env_keys: HashSet<String>,
        explicit_env_keys: HashSet<String>,
        denied_program_names: HashSet<String>,
    ) -> Result<Self, ProcessError> {
        if allowed_cwd_roots.is_empty() {
            return Err(ProcessError::NoAllowedWorkingDirectories);
        }
        let mut canonical_roots = Vec::with_capacity(allowed_cwd_roots.len());
        for root in allowed_cwd_roots {
            let canonical = fs::canonicalize(&root).map_err(ProcessError::Io)?;
            if !canonical.is_dir() {
                return Err(ProcessError::WorkingDirectoryNotDirectory);
            }
            canonical_roots.push(canonical);
        }
        Ok(Self {
            allowed_cwd_roots: canonical_roots,
            inherited_env_keys,
            explicit_env_keys,
            denied_program_names,
            max_args: DEFAULT_MAX_ARGS,
            max_env_entries: DEFAULT_MAX_ENV,
            max_output_bytes_per_stream: DEFAULT_MAX_OUTPUT_BYTES,
            max_timeout_ms: DEFAULT_MAX_TIMEOUT_MS,
            poll_interval: Duration::from_millis(DEFAULT_POLL_MS),
        })
    }

    pub fn with_limits(
        mut self,
        max_args: usize,
        max_env_entries: usize,
        max_output_bytes_per_stream: usize,
        max_timeout_ms: u64,
    ) -> Result<Self, ProcessError> {
        if max_args == 0 || max_output_bytes_per_stream == 0 || max_timeout_ms == 0 {
            return Err(ProcessError::InvalidPolicyLimit);
        }
        self.max_args = max_args;
        self.max_env_entries = max_env_entries;
        self.max_output_bytes_per_stream = max_output_bytes_per_stream;
        self.max_timeout_ms = max_timeout_ms;
        Ok(self)
    }
}

#[derive(Debug, Clone, Default)]
pub struct ProcessCancellation {
    cancelled: Arc<AtomicBool>,
}

impl ProcessCancellation {
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::SeqCst);
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::SeqCst)
    }
}

#[derive(Debug, Clone)]
pub struct ProcessExecutor {
    policy: ProcessPolicy,
}

impl ProcessExecutor {
    pub fn new(policy: ProcessPolicy) -> Self {
        Self { policy }
    }

    pub fn execute(
        &self,
        request: &ProcessRequest,
        cancellation: &ProcessCancellation,
    ) -> Result<ProcessOutput, ProcessError> {
        let validated = self.validate_request(request)?;
        self.execute_validated(
            &request.program,
            &request.args,
            &validated.cwd,
            &request.env,
            request.timeout_ms,
            cancellation,
        )
    }

    pub(crate) fn execute_shell(
        &self,
        request: &ShellRequest,
        cancellation: &ProcessCancellation,
    ) -> Result<ProcessOutput, ProcessError> {
        let cwd = self.validate_common(&request.cwd, &request.env, request.timeout_ms)?;
        #[cfg(unix)]
        let (program, args) = (
            "/bin/sh".to_owned(),
            vec!["-c".to_owned(), request.command.clone()],
        );
        #[cfg(windows)]
        let (program, args) = (
            "cmd.exe".to_owned(),
            vec![
                "/D".to_owned(),
                "/S".to_owned(),
                "/C".to_owned(),
                request.command.clone(),
            ],
        );
        #[cfg(not(any(unix, windows)))]
        return Err(ProcessError::ShellUnsupportedPlatform);

        #[cfg(any(unix, windows))]
        self.execute_validated(
            &program,
            &args,
            &cwd,
            &request.env,
            request.timeout_ms,
            cancellation,
        )
    }

    fn execute_validated(
        &self,
        program: &str,
        args: &[String],
        cwd: &Path,
        env: &[ProcessEnvVar],
        timeout_ms: u64,
        cancellation: &ProcessCancellation,
    ) -> Result<ProcessOutput, ProcessError> {
        if cancellation.is_cancelled() {
            return Ok(ProcessOutput {
                exit_code: None,
                stdout: String::new(),
                stderr: String::new(),
                stdout_truncated: false,
                stderr_truncated: false,
                timed_out: false,
                cancelled: true,
                duration_ms: 0,
            });
        }

        let mut command = Command::new(program);
        command
            .args(args)
            .current_dir(cwd)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .env_clear();
        for key in &self.policy.inherited_env_keys {
            if let Some(value) = std::env::var_os(key) {
                command.env(key, value);
            }
        }
        for item in env {
            command.env(&item.key, &item.value);
        }

        let started = Instant::now();
        // Supervise the whole process tree, not just the direct child. On Unix
        // the child becomes a process-group leader; on Windows it is assigned
        // to a Job Object. Cancellation/timeout therefore terminates descendants
        // spawned by build tools, package managers, shell pipelines, or tests.
        let mut command = CommandWrap::from(command);
        #[cfg(unix)]
        command.wrap(ProcessGroup::leader());
        #[cfg(windows)]
        command.wrap(JobObject);
        let mut child = command.spawn().map_err(ProcessError::Spawn)?;
        let stdout = child.stdout().take().ok_or(ProcessError::PipeUnavailable)?;
        let stderr = child.stderr().take().ok_or(ProcessError::PipeUnavailable)?;
        let max = self.policy.max_output_bytes_per_stream;
        let stdout_reader = thread::spawn(move || drain_bounded(stdout, max));
        let stderr_reader = thread::spawn(move || drain_bounded(stderr, max));

        let timeout = Duration::from_millis(timeout_ms);
        let mut timed_out = false;
        let mut cancelled = false;
        let status = loop {
            if cancellation.is_cancelled() {
                cancelled = true;
                child.start_kill().map_err(ProcessError::Io)?;
                break child.wait().map_err(ProcessError::Io)?;
            }
            if started.elapsed() >= timeout {
                timed_out = true;
                child.start_kill().map_err(ProcessError::Io)?;
                break child.wait().map_err(ProcessError::Io)?;
            }
            if let Some(status) = child.try_wait().map_err(ProcessError::Io)? {
                // Bounded Agent operations are not service launchers. Clean up
                // anything still attached to the process group / Job Object.
                let _ = child.start_kill();
                break status;
            }
            thread::sleep(self.policy.poll_interval);
        };

        let stdout = stdout_reader
            .join()
            .map_err(|_| ProcessError::ReaderPanicked)??;
        let stderr = stderr_reader
            .join()
            .map_err(|_| ProcessError::ReaderPanicked)??;
        Ok(ProcessOutput {
            exit_code: status.code(),
            stdout: String::from_utf8_lossy(&stdout.bytes).into_owned(),
            stderr: String::from_utf8_lossy(&stderr.bytes).into_owned(),
            stdout_truncated: stdout.truncated,
            stderr_truncated: stderr.truncated,
            timed_out,
            cancelled,
            duration_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
        })
    }

    pub fn execute_operation(
        &self,
        gate: &mut AgentExecutionGate,
        operation: OperationRef,
        request: &ProcessRequest,
        cancellation: &ProcessCancellation,
    ) -> Result<ProcessOutput, ProcessError> {
        let operation_id = operation.operation_id.clone();
        gate.begin(operation).map_err(ProcessError::Execution)?;
        let result = self.execute(request, cancellation);
        // A process operation is terminal after the direct child has been waited,
        // including timeout/cancellation. Never make the operation ID replayable.
        gate.finish(&operation_id)
            .map_err(ProcessError::Execution)?;
        result
    }

    fn validate_request(&self, request: &ProcessRequest) -> Result<ValidatedRequest, ProcessError> {
        if request.program.trim().is_empty() {
            return Err(ProcessError::InvalidRequest);
        }
        if request.args.len() > self.policy.max_args {
            return Err(ProcessError::TooManyArguments);
        }
        let name = Path::new(&request.program)
            .file_name()
            .and_then(|value| value.to_str())
            .ok_or(ProcessError::InvalidProgram)?
            .to_ascii_lowercase();
        if self.policy.denied_program_names.contains(&name) {
            return Err(ProcessError::ShellProgramDenied);
        }
        let cwd = self.validate_common(&request.cwd, &request.env, request.timeout_ms)?;
        Ok(ValidatedRequest { cwd })
    }

    fn validate_common(
        &self,
        cwd: &str,
        env: &[ProcessEnvVar],
        timeout_ms: u64,
    ) -> Result<PathBuf, ProcessError> {
        if cwd.trim().is_empty() {
            return Err(ProcessError::InvalidRequest);
        }
        if env.len() > self.policy.max_env_entries {
            return Err(ProcessError::TooManyEnvironmentEntries);
        }
        if timeout_ms == 0 || timeout_ms > self.policy.max_timeout_ms {
            return Err(ProcessError::InvalidTimeout);
        }
        let cwd = fs::canonicalize(cwd).map_err(ProcessError::Io)?;
        if !cwd.is_dir() {
            return Err(ProcessError::WorkingDirectoryNotDirectory);
        }
        if !self
            .policy
            .allowed_cwd_roots
            .iter()
            .any(|root| cwd.starts_with(root))
        {
            return Err(ProcessError::WorkingDirectoryDenied);
        }
        let mut seen = HashSet::new();
        for ProcessEnvVar { key, value: _ } in env {
            if key.is_empty() || key.contains('=') || key.contains('\0') || !seen.insert(key) {
                return Err(ProcessError::InvalidEnvironment);
            }
            if !self.policy.explicit_env_keys.contains(key) {
                return Err(ProcessError::EnvironmentKeyDenied(key.clone()));
            }
        }
        Ok(cwd)
    }
}

#[derive(Debug)]
struct ValidatedRequest {
    cwd: PathBuf,
}

#[derive(Debug)]
struct BoundedBytes {
    bytes: Vec<u8>,
    truncated: bool,
}

fn drain_bounded<R: Read>(mut reader: R, max: usize) -> Result<BoundedBytes, ProcessError> {
    let mut kept = Vec::with_capacity(max.min(4096));
    let mut truncated = false;
    let mut buffer = [0_u8; 4096];
    loop {
        let read = reader.read(&mut buffer).map_err(ProcessError::Io)?;
        if read == 0 {
            break;
        }
        let remaining = max.saturating_sub(kept.len());
        let copy = read.min(remaining);
        kept.extend_from_slice(&buffer[..copy]);
        if copy < read {
            truncated = true;
        }
    }
    Ok(BoundedBytes {
        bytes: kept,
        truncated,
    })
}

pub enum ProcessError {
    NoAllowedWorkingDirectories,
    InvalidPolicyLimit,
    InvalidRequest,
    InvalidProgram,
    ShellProgramDenied,
    ShellUnsupportedPlatform,
    TooManyArguments,
    TooManyEnvironmentEntries,
    InvalidTimeout,
    WorkingDirectoryNotDirectory,
    WorkingDirectoryDenied,
    InvalidEnvironment,
    EnvironmentKeyDenied(String),
    Spawn(std::io::Error),
    PipeUnavailable,
    ReaderPanicked,
    Io(std::io::Error),
    Execution(ExecutionError),
}

impl SafeErrorCode for ProcessError {
    fn safe_error_code(&self) -> &'static str {
        match self {
            Self::NoAllowedWorkingDirectories => "process_no_allowed_working_directories",
            Self::InvalidPolicyLimit => "process_invalid_policy_limit",
            Self::InvalidRequest => "process_invalid_request",
            Self::InvalidProgram => "process_invalid_program",
            Self::ShellProgramDenied => "process_shell_program_denied",
            Self::ShellUnsupportedPlatform => "process_shell_unsupported_platform",
            Self::TooManyArguments => "process_too_many_arguments",
            Self::TooManyEnvironmentEntries => "process_too_many_environment_entries",
            Self::InvalidTimeout => "process_invalid_timeout",
            Self::WorkingDirectoryNotDirectory => "process_working_directory_not_directory",
            Self::WorkingDirectoryDenied => "process_working_directory_denied",
            Self::InvalidEnvironment => "process_invalid_environment",
            Self::EnvironmentKeyDenied(_) => "process_environment_key_denied",
            Self::Spawn(_) => "process_spawn_failed",
            Self::PipeUnavailable => "process_pipe_unavailable",
            Self::ReaderPanicked => "process_reader_panicked",
            Self::Io(_) => "process_io",
            Self::Execution(_) => "process_execution",
        }
    }
}

impl fmt::Debug for ProcessError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.safe_error_code())
    }
}

impl fmt::Display for ProcessError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.safe_error_code())
    }
}

impl std::error::Error for ProcessError {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::v2_m0::ProcessRequest;
    use std::sync::mpsc;

    fn temp_root(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "cumg-v2-process-{name}-{}-{}",
            std::process::id(),
            rand::random::<u64>()
        ));
        fs::create_dir_all(&root).unwrap();
        root
    }

    #[cfg(unix)]
    fn request(program: &str, root: &Path, args: &[&str]) -> ProcessRequest {
        ProcessRequest {
            program: program.into(),
            args: args.iter().map(|value| (*value).to_owned()).collect(),
            cwd: root.to_string_lossy().into_owned(),
            env: vec![],
            timeout_ms: 5_000,
        }
    }

    #[cfg(unix)]
    #[test]
    fn structured_argv_executes_without_shell_and_bounds_output() {
        let root = temp_root("argv");
        let policy = ProcessPolicy::developer_defaults(vec![root.clone()])
            .unwrap()
            .with_limits(32, 8, 8, 5_000)
            .unwrap();
        let executor = ProcessExecutor::new(policy);
        let output = executor
            .execute(
                &request("/bin/echo", &root, &["0123456789abcdef"]),
                &ProcessCancellation::default(),
            )
            .unwrap();
        assert_eq!(output.exit_code, Some(0));
        assert_eq!(output.stdout, "01234567");
        assert!(output.stdout_truncated);
        assert!(!output.timed_out && !output.cancelled);
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn shell_interpreters_are_separate_and_denied_by_process_executor() {
        let root = temp_root("shell-deny");
        let executor =
            ProcessExecutor::new(ProcessPolicy::developer_defaults(vec![root.clone()]).unwrap());
        let result = executor.execute(
            &request("/bin/sh", &root, &["-c", "echo should-not-run"]),
            &ProcessCancellation::default(),
        );
        assert!(matches!(result, Err(ProcessError::ShellProgramDenied)));
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn cwd_cannot_escape_the_allowed_root_even_through_symlink_resolution() {
        let root = temp_root("cwd");
        let outside = temp_root("outside");
        let link = root.join("escape");
        std::os::unix::fs::symlink(&outside, &link).unwrap();
        let executor =
            ProcessExecutor::new(ProcessPolicy::developer_defaults(vec![root.clone()]).unwrap());
        let mut req = request("/bin/echo", &root, &["nope"]);
        req.cwd = link.to_string_lossy().into_owned();
        assert!(matches!(
            executor.execute(&req, &ProcessCancellation::default()),
            Err(ProcessError::WorkingDirectoryDenied)
        ));
        fs::remove_dir_all(root).unwrap();
        fs::remove_dir_all(outside).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn timeout_kills_and_waits_for_the_direct_child() {
        let root = temp_root("timeout");
        let executor =
            ProcessExecutor::new(ProcessPolicy::developer_defaults(vec![root.clone()]).unwrap());
        let mut req = request("/bin/sleep", &root, &["2"]);
        req.timeout_ms = 40;
        let started = Instant::now();
        let output = executor
            .execute(&req, &ProcessCancellation::default())
            .unwrap();
        assert!(output.timed_out && !output.cancelled);
        assert!(started.elapsed() < Duration::from_secs(1));
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn normal_parent_exit_does_not_leave_background_descendants() {
        use std::os::unix::fs::PermissionsExt;

        let root = temp_root("normal-exit-tree");
        let script = root.join("spawn-background");
        let pid_file = root.join("background.pid");
        fs::write(
            &script,
            format!(
                "#!/bin/sh\nsleep 30 </dev/null >/dev/null 2>&1 &\necho $! > {}\nexit 0\n",
                pid_file.display()
            ),
        )
        .unwrap();
        fs::set_permissions(&script, fs::Permissions::from_mode(0o700)).unwrap();
        let executor =
            ProcessExecutor::new(ProcessPolicy::developer_defaults(vec![root.clone()]).unwrap());
        let started = Instant::now();
        let output = executor
            .execute(
                &request(script.to_str().unwrap(), &root, &[]),
                &ProcessCancellation::default(),
            )
            .unwrap();
        assert_eq!(output.exit_code, Some(0));
        assert!(started.elapsed() < Duration::from_secs(2));
        let child_pid = fs::read_to_string(&pid_file).unwrap().trim().to_owned();
        thread::sleep(Duration::from_millis(30));
        let still_alive = Command::new("/bin/kill")
            .args(["-0", &child_pid])
            .status()
            .map(|status| status.success())
            .unwrap_or(false);
        assert!(
            !still_alive,
            "background descendant survived operation completion"
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn cancellation_terminates_descendant_processes_too() {
        use std::os::unix::fs::PermissionsExt;

        let root = temp_root("cancel-tree");
        let script = root.join("spawn-child");
        let pid_file = root.join("child.pid");
        fs::write(
            &script,
            format!(
                "#!/bin/sh\nsleep 30 &\necho $! > {}\nwait\n",
                pid_file.display()
            ),
        )
        .unwrap();
        fs::set_permissions(&script, fs::Permissions::from_mode(0o700)).unwrap();

        let executor =
            ProcessExecutor::new(ProcessPolicy::developer_defaults(vec![root.clone()]).unwrap());
        let req = request(script.to_str().unwrap(), &root, &[]);
        let cancellation = ProcessCancellation::default();
        let worker_cancel = cancellation.clone();
        let worker = thread::spawn(move || executor.execute(&req, &worker_cancel).unwrap());

        let deadline = Instant::now() + Duration::from_secs(2);
        while !pid_file.exists() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(10));
        }
        let child_pid = fs::read_to_string(&pid_file).unwrap().trim().to_owned();
        cancellation.cancel();
        let output = worker.join().unwrap();
        assert!(output.cancelled);
        thread::sleep(Duration::from_millis(30));
        let still_alive = Command::new("/bin/kill")
            .args(["-0", &child_pid])
            .status()
            .map(|status| status.success())
            .unwrap_or(false);
        assert!(!still_alive, "descendant process survived cancellation");
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn cancellation_terminates_process_and_operation_id_remains_terminal() {
        let root = temp_root("cancel");
        let executor =
            ProcessExecutor::new(ProcessPolicy::developer_defaults(vec![root.clone()]).unwrap());
        let req = request("/bin/sleep", &root, &["2"]);
        let cancellation = ProcessCancellation::default();
        let cancellation_for_thread = cancellation.clone();
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            thread::sleep(Duration::from_millis(30));
            cancellation_for_thread.cancel();
            tx.send(()).unwrap();
        });
        let mut gate = AgentExecutionGate::default();
        let operation = OperationRef {
            device_id: "dev-test".into(),
            device_generation: 1,
            operation_id: "op-process-cancel".into(),
        };
        let output = executor
            .execute_operation(&mut gate, operation.clone(), &req, &cancellation)
            .unwrap();
        rx.recv_timeout(Duration::from_secs(1)).unwrap();
        assert!(output.cancelled && !output.timed_out);
        assert_eq!(gate.begin(operation), Err(ExecutionError::OperationReplay));
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn explicit_utf8_locale_is_available_without_inherited_locale() {
        let root = temp_root("explicit-locale");
        let defaults = ProcessPolicy::developer_defaults(vec![root.clone()]).unwrap();
        for key in ["LANG", "LC_ALL", "LC_CTYPE"] {
            assert!(defaults.explicit_env_keys.contains(key));
        }

        let inherited = HashSet::new();
        let explicit = ["LANG", "LC_ALL", "LC_CTYPE"]
            .into_iter()
            .map(str::to_owned)
            .collect();
        let policy =
            ProcessPolicy::new(vec![root.clone()], inherited, explicit, HashSet::new()).unwrap();
        let executor = ProcessExecutor::new(policy);
        let locale = "en_US.UTF-8";
        let env = ["LANG", "LC_ALL", "LC_CTYPE"]
            .into_iter()
            .map(|key| ProcessEnvVar {
                key: key.into(),
                value: locale.into(),
            })
            .collect::<Vec<_>>();

        let mut process = request("/usr/bin/env", &root, &[]);
        process.env = env.clone();
        let process_output = executor
            .execute(&process, &ProcessCancellation::default())
            .unwrap();
        for key in ["LANG", "LC_ALL", "LC_CTYPE"] {
            assert!(
                process_output
                    .stdout
                    .lines()
                    .any(|line| line == format!("{key}={locale}"))
            );
        }

        let shell = ShellRequest {
            command: r#"printf '%s\n' "$LANG" "$LC_ALL" "$LC_CTYPE""#.into(),
            cwd: root.to_string_lossy().into_owned(),
            env,
            timeout_ms: 5_000,
        };
        let shell_output = executor
            .execute_shell(&shell, &ProcessCancellation::default())
            .unwrap();
        assert_eq!(
            shell_output.stdout.lines().collect::<Vec<_>>(),
            vec![locale, locale, locale]
        );

        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn explicit_environment_is_allowlisted_and_environment_is_cleared() {
        let root = temp_root("env");
        let mut inherited = HashSet::new();
        inherited.insert("PATH".into());
        let mut explicit = HashSet::new();
        explicit.insert("CI".into());
        let denied = HashSet::new();
        let policy = ProcessPolicy::new(vec![root.clone()], inherited, explicit, denied).unwrap();
        let executor = ProcessExecutor::new(policy);
        let mut req = request("/usr/bin/env", &root, &[]);
        req.env = vec![ProcessEnvVar {
            key: "CI".into(),
            value: "1".into(),
        }];
        let output = executor
            .execute(&req, &ProcessCancellation::default())
            .unwrap();
        assert!(output.stdout.lines().any(|line| line == "CI=1"));
        assert!(
            !output
                .stdout
                .lines()
                .any(|line| line.starts_with("SECRET_DO_NOT_INHERIT="))
        );
        req.env.push(ProcessEnvVar {
            key: "AWS_SECRET_ACCESS_KEY".into(),
            value: "x".into(),
        });
        assert!(matches!(
            executor.execute(&req, &ProcessCancellation::default()),
            Err(ProcessError::EnvironmentKeyDenied(key)) if key == "AWS_SECRET_ACCESS_KEY"
        ));
        fs::remove_dir_all(root).unwrap();
    }
}
