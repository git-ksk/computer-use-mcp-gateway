//! V2-M1 Agent-native free-form shell execution.
//!
//! This capability is intentionally distinct from structured `ExecuteProcess`.
//! The request accepts shell syntax and therefore invokes a fixed OS shell, but
//! reuses the same cwd/environment policy, bounded output, timeout, process-tree
//! supervision, and cooperative cancellation machinery as structured execution.

use crate::v2_m0::{ProcessOutput, ShellRequest};
use crate::v2_m1_process::{ProcessCancellation, ProcessError, ProcessExecutor, ProcessPolicy};
use crate::v2_observability::SafeErrorCode;
use std::fmt;

const MAX_SHELL_COMMAND_BYTES: usize = 16 * 1024;

#[derive(Debug, Clone)]
pub struct ShellExecutor {
    process: ProcessExecutor,
}

impl ShellExecutor {
    pub fn new(policy: ProcessPolicy) -> Self {
        Self {
            process: ProcessExecutor::new(policy),
        }
    }

    pub fn execute(
        &self,
        request: &ShellRequest,
        cancellation: &ProcessCancellation,
    ) -> Result<ProcessOutput, ShellError> {
        if request.command.trim().is_empty() {
            return Err(ShellError::InvalidCommand);
        }
        if request.command.len() > MAX_SHELL_COMMAND_BYTES {
            return Err(ShellError::CommandTooLarge);
        }
        self.process
            .execute_shell(request, cancellation)
            .map_err(ShellError::Process)
    }
}

pub enum ShellError {
    InvalidCommand,
    CommandTooLarge,
    Process(ProcessError),
}

impl SafeErrorCode for ShellError {
    fn safe_error_code(&self) -> &'static str {
        match self {
            Self::InvalidCommand => "shell_invalid_command",
            Self::CommandTooLarge => "shell_command_too_large",
            Self::Process(_) => "shell_process_error",
        }
    }
}

impl fmt::Debug for ShellError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.safe_error_code())
    }
}

impl fmt::Display for ShellError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.safe_error_code())
    }
}

impl std::error::Error for ShellError {}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn temp_root(name: &str) -> std::path::PathBuf {
        let root = std::env::temp_dir().join(format!(
            "cumg-v2-shell-{name}-{}-{}",
            std::process::id(),
            rand::random::<u64>()
        ));
        fs::create_dir_all(&root).unwrap();
        root
    }

    #[cfg(unix)]
    #[test]
    fn shell_supports_pipeline_and_redirection_inside_allowed_cwd() {
        let root = temp_root("pipeline");
        let executor =
            ShellExecutor::new(ProcessPolicy::developer_defaults(vec![root.clone()]).unwrap());
        let request = ShellRequest {
            command: "printf 'hello\\n' | tr a-z A-Z > result.txt && cat result.txt".into(),
            cwd: root.to_string_lossy().into_owned(),
            env: vec![],
            timeout_ms: 5_000,
        };
        let output = executor
            .execute(&request, &ProcessCancellation::default())
            .unwrap();
        assert_eq!(output.exit_code, Some(0));
        assert_eq!(output.stdout, "HELLO\n");
        assert!(!output.timed_out);
        assert!(!output.cancelled);
        assert_eq!(
            fs::read_to_string(root.join("result.txt")).unwrap(),
            "HELLO\n"
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn shell_rejects_blank_and_oversized_commands() {
        let root = temp_root("bounds");
        let executor =
            ShellExecutor::new(ProcessPolicy::developer_defaults(vec![root.clone()]).unwrap());
        let base = ShellRequest {
            command: " ".into(),
            cwd: root.to_string_lossy().into_owned(),
            env: vec![],
            timeout_ms: 5_000,
        };
        assert!(matches!(
            executor.execute(&base, &ProcessCancellation::default()),
            Err(ShellError::InvalidCommand)
        ));
        let oversized = ShellRequest {
            command: "x".repeat(MAX_SHELL_COMMAND_BYTES + 1),
            ..base
        };
        assert!(matches!(
            executor.execute(&oversized, &ProcessCancellation::default()),
            Err(ShellError::CommandTooLarge)
        ));
        let _ = fs::remove_dir_all(root);
    }
}
