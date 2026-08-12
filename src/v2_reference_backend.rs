//! Deterministic in-process reference executor for V2 P1 backend portability.
//!
//! This is intentionally not a Cua-shaped MCP mock. It models a process-like
//! backend with an explicit local commit boundary and materially different
//! cancellation contracts: it can sometimes prove not-started or clean local
//! termination, while an unprovable post-commit outcome remains indeterminate.
//! The authoritative Hub operation state machine is not duplicated here.

use crate::v2_m0::{DeviceCapability, DeviceCommand, DeviceResult, ProcessOutput};
use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};
use std::time::Duration;
use tokio::sync::watch;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReferenceCancellationContract {
    /// Once cancellation is observed, the executor can prove that no local work
    /// remains capable of producing another effect.
    ProvenCleanTermination,
    /// Once the commit boundary was crossed, cancellation cannot prove the
    /// external outcome.
    UnprovenAfterCommit,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReferenceExecutionOutcome {
    Completed(DeviceResult),
    /// Cancellation was observed before the deterministic commit boundary.
    ProvenNotStarted,
    /// Local work is proven terminated after the commit boundary.
    ProvenCleanTermination { effect_committed: bool },
    /// The executor cannot prove a terminal outcome after commit.
    Indeterminate { effect_may_have_happened: bool },
}

#[derive(Debug, Clone)]
pub struct DeterministicReferenceExecutor {
    commit_delay: Duration,
    finish_delay: Duration,
    cancellation_contract: ReferenceCancellationContract,
    committed_effects: Arc<AtomicU64>,
}

impl DeterministicReferenceExecutor {
    pub fn new(
        commit_delay: Duration,
        finish_delay: Duration,
        cancellation_contract: ReferenceCancellationContract,
    ) -> Result<Self, ReferenceBackendError> {
        if commit_delay.is_zero() || finish_delay.is_zero() {
            return Err(ReferenceBackendError::InvalidConfig);
        }
        Ok(Self {
            commit_delay,
            finish_delay,
            cancellation_contract,
            committed_effects: Arc::new(AtomicU64::new(0)),
        })
    }

    pub fn committed_effects(&self) -> u64 {
        self.committed_effects.load(Ordering::SeqCst)
    }

    pub fn supported_capability(&self) -> DeviceCapability {
        DeviceCapability::Shell
    }

    pub async fn execute(
        &self,
        command: &DeviceCommand,
        mut cancellation: watch::Receiver<bool>,
    ) -> Result<ReferenceExecutionOutcome, ReferenceBackendError> {
        if !matches!(command, DeviceCommand::Shell { .. }) {
            return Err(ReferenceBackendError::UnsupportedCommand);
        }

        if *cancellation.borrow() {
            return Ok(ReferenceExecutionOutcome::ProvenNotStarted);
        }

        tokio::select! {
            _ = tokio::time::sleep(self.commit_delay) => {}
            changed = cancellation.changed() => {
                if changed.is_err() || *cancellation.borrow() {
                    return Ok(ReferenceExecutionOutcome::ProvenNotStarted);
                }
            }
        }

        self.committed_effects.fetch_add(1, Ordering::SeqCst);

        tokio::select! {
            _ = tokio::time::sleep(self.finish_delay) => {
                Ok(ReferenceExecutionOutcome::Completed(shell_result(
                    false,
                    self.commit_delay + self.finish_delay,
                )))
            }
            changed = cancellation.changed() => {
                if changed.is_err() || *cancellation.borrow() {
                    Ok(match self.cancellation_contract {
                        ReferenceCancellationContract::ProvenCleanTermination => {
                            ReferenceExecutionOutcome::ProvenCleanTermination {
                                effect_committed: true,
                            }
                        }
                        ReferenceCancellationContract::UnprovenAfterCommit => {
                            ReferenceExecutionOutcome::Indeterminate {
                                effect_may_have_happened: true,
                            }
                        }
                    })
                } else {
                    Ok(ReferenceExecutionOutcome::Indeterminate {
                        effect_may_have_happened: true,
                    })
                }
            }
        }
    }
}

fn shell_result(cancelled: bool, duration: Duration) -> DeviceResult {
    DeviceResult::Shell {
        output: ProcessOutput {
            exit_code: if cancelled { None } else { Some(0) },
            stdout: if cancelled {
                String::new()
            } else {
                "reference-completed".into()
            },
            stderr: String::new(),
            stdout_truncated: false,
            stderr_truncated: false,
            timed_out: false,
            cancelled,
            duration_ms: u64::try_from(duration.as_millis()).unwrap_or(u64::MAX),
        },
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReferenceBackendError {
    InvalidConfig,
    UnsupportedCommand,
}

impl std::fmt::Display for ReferenceBackendError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}

impl std::error::Error for ReferenceBackendError {}

#[cfg(test)]
mod tests {
    use super::*;

    fn command() -> DeviceCommand {
        DeviceCommand::Shell {
            request: crate::v2_m0::ShellRequest {
                command: "reference-effect".into(),
                cwd: "/reference".into(),
                env: vec![],
                timeout_ms: 1_000,
            },
        }
    }

    #[tokio::test]
    async fn cancellation_before_commit_is_proven_not_started() {
        let executor = DeterministicReferenceExecutor::new(
            Duration::from_millis(50),
            Duration::from_millis(50),
            ReferenceCancellationContract::UnprovenAfterCommit,
        )
        .unwrap();
        let (cancel_tx, cancel_rx) = watch::channel(false);
        cancel_tx.send(true).unwrap();
        let outcome = executor.execute(&command(), cancel_rx).await.unwrap();
        assert_eq!(outcome, ReferenceExecutionOutcome::ProvenNotStarted);
        assert_eq!(executor.committed_effects(), 0);
    }

    #[tokio::test]
    async fn post_commit_cancel_requires_backend_proof_or_indeterminate() {
        for (contract, terminal) in [
            (ReferenceCancellationContract::ProvenCleanTermination, true),
            (ReferenceCancellationContract::UnprovenAfterCommit, false),
        ] {
            let executor = DeterministicReferenceExecutor::new(
                Duration::from_millis(10),
                Duration::from_millis(100),
                contract,
            )
            .unwrap();
            let (cancel_tx, cancel_rx) = watch::channel(false);
            let worker = {
                let executor = executor.clone();
                tokio::spawn(async move { executor.execute(&command(), cancel_rx).await.unwrap() })
            };
            tokio::time::sleep(Duration::from_millis(30)).await;
            cancel_tx.send(true).unwrap();
            let outcome = worker.await.unwrap();
            assert_eq!(executor.committed_effects(), 1);
            assert_eq!(
                matches!(
                    outcome,
                    ReferenceExecutionOutcome::ProvenCleanTermination { .. }
                ),
                terminal
            );
            assert_eq!(
                matches!(outcome, ReferenceExecutionOutcome::Indeterminate { .. }),
                !terminal
            );
        }
    }
}
