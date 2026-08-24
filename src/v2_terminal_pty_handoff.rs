#![allow(dead_code)] // #48 experimental/internal until CUMG dogfood admission is complete.

//! Agent-local CUMG dogfood coordinator for experimental Terminal/PTY Handoff (#48).
//!
//! The canonical Handoff runtime owns Agent/Human/none authority, intervention epoch, Done ->
//! verifying, and explicit resume. CUMG owns the real PTY, byte buffering, process state and the
//! consumer-side transition ordering. A single operation mutex closes the check/use race between an
//! authority decision and the PTY operation it protects.

use crate::{
    v2_operator_handoff::{
        HandoffControlError, HandoffInterventionStatus, TerminalPtyHandoffAuthority,
        TerminalPtyHandoffBinding, TerminalPtyHandoffControl, TerminalPtyHandoffResult,
        TerminalPtyInterventionRef, TerminalPtyResumeReceipt,
    },
    v2_terminal_pty::{
        TerminalPtyBinding, TerminalPtyError, TerminalPtyManager, TerminalPtyOutput,
        TerminalPtyProcessState, TerminalPtySpawnConfig,
    },
};
use std::{fmt, sync::Arc};
use tokio::sync::Mutex;

#[derive(Debug)]
pub(crate) enum TerminalPtyDogfoodError {
    Handoff(HandoffControlError),
    Pty(TerminalPtyError),
    InvalidTransition,
    SessionUnavailable,
}

impl fmt::Display for TerminalPtyDogfoodError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Handoff(_) => "terminal Handoff transition failed",
            Self::Pty(_) => "terminal PTY operation failed",
            Self::InvalidTransition => "terminal Handoff returned an invalid transition",
            Self::SessionUnavailable => "terminal PTY session unavailable",
        })
    }
}

impl std::error::Error for TerminalPtyDogfoodError {}

impl From<HandoffControlError> for TerminalPtyDogfoodError {
    fn from(value: HandoffControlError) -> Self {
        Self::Handoff(value)
    }
}

impl From<TerminalPtyError> for TerminalPtyDogfoodError {
    fn from(value: TerminalPtyError) -> Self {
        Self::Pty(value)
    }
}

struct SessionState {
    pty: TerminalPtyManager,
    binding: Option<TerminalPtyBinding>,
    principal_binding: String,
}

pub(crate) struct TerminalPtyDogfoodCoordinator<A: TerminalPtyHandoffAuthority> {
    authority: Arc<A>,
    state: Mutex<SessionState>,
    operation_gate: Mutex<()>,
}

impl<A: TerminalPtyHandoffAuthority> TerminalPtyDogfoodCoordinator<A> {
    pub(crate) fn new(
        authority: Arc<A>,
        principal_binding: String,
    ) -> Result<Self, TerminalPtyDogfoodError> {
        if principal_binding.len() != 64
            || !principal_binding
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
        {
            return Err(TerminalPtyDogfoodError::InvalidTransition);
        }
        Ok(Self {
            authority,
            state: Mutex::new(SessionState {
                pty: TerminalPtyManager::default(),
                binding: None,
                principal_binding,
            }),
            operation_gate: Mutex::new(()),
        })
    }

    pub(crate) async fn spawn(
        &self,
        config: TerminalPtySpawnConfig,
    ) -> Result<TerminalPtyBinding, TerminalPtyDogfoodError> {
        let _operation = self.operation_gate.lock().await;
        let (binding, handoff) = {
            let mut state = self.state.lock().await;
            let binding = state.pty.spawn(config)?;
            let handoff = handoff_binding(&binding, &state.principal_binding)?;
            state.binding = Some(binding.clone());
            (binding, handoff)
        };
        if self
            .authority
            .terminal_pty_control(TerminalPtyHandoffControl::Bind(handoff))
            .await
            .is_err()
        {
            let mut state = self.state.lock().await;
            let _ = state.pty.close(&binding);
            state.binding = None;
            return Err(TerminalPtyDogfoodError::InvalidTransition);
        }
        Ok(binding)
    }

    pub(crate) async fn agent_write(&self, bytes: &[u8]) -> Result<(), TerminalPtyDogfoodError> {
        let _operation = self.operation_gate.lock().await;
        let (binding, handoff) = self.bindings().await?;
        expect_ok(
            self.authority
                .terminal_pty_control(TerminalPtyHandoffControl::AgentInput(handoff))
                .await?,
        )?;
        self.state.lock().await.pty.write(&binding, bytes)?;
        Ok(())
    }

    pub(crate) async fn agent_read(
        &self,
        max_bytes: usize,
    ) -> Result<TerminalPtyOutput, TerminalPtyDogfoodError> {
        let _operation = self.operation_gate.lock().await;
        let (binding, handoff) = self.bindings().await?;
        expect_ok(
            self.authority
                .terminal_pty_control(TerminalPtyHandoffControl::AgentObserve(handoff))
                .await?,
        )?;
        Ok(self
            .state
            .lock()
            .await
            .pty
            .read_agent(&binding, max_bytes)?)
    }

    pub(crate) async fn agent_resize(
        &self,
        rows: u16,
        cols: u16,
    ) -> Result<(), TerminalPtyDogfoodError> {
        let _operation = self.operation_gate.lock().await;
        let (binding, handoff) = self.bindings().await?;
        expect_ok(
            self.authority
                .terminal_pty_control(TerminalPtyHandoffControl::AgentResize(handoff))
                .await?,
        )?;
        self.state.lock().await.pty.resize(&binding, rows, cols)?;
        Ok(())
    }

    pub(crate) async fn begin_human(
        &self,
    ) -> Result<TerminalPtyInterventionRef, TerminalPtyDogfoodError> {
        let _operation = self.operation_gate.lock().await;
        let (binding, handoff) = self.bindings().await?;
        let fenced = expect_transition(
            self.authority
                .terminal_pty_control(TerminalPtyHandoffControl::BeginFence(handoff.clone()))
                .await?,
            HandoffInterventionStatus::AwaitingHuman,
        )?;
        let prepare = {
            let state = self.state.lock().await;
            match state.pty.process_state(&binding) {
                Ok(TerminalPtyProcessState::Running) => state
                    .pty
                    .drain_writes(&binding)
                    .and_then(|()| state.pty.begin_human_output(&binding)),
                Ok(TerminalPtyProcessState::Exited { .. } | TerminalPtyProcessState::Closed) => {
                    Err(TerminalPtyError::SessionClosed)
                }
                Err(error) => Err(error),
            }
        };
        if let Err(error) = prepare {
            {
                let state = self.state.lock().await;
                let _ = state.pty.terminate(&binding);
            }
            let _ = self
                .authority
                .terminal_pty_control(TerminalPtyHandoffControl::SessionExit(handoff))
                .await?;
            return Err(TerminalPtyDogfoodError::Pty(error));
        }
        expect_transition(
            self.authority
                .terminal_pty_control(TerminalPtyHandoffControl::ClaimHuman {
                    binding: handoff,
                    intervention_id: fenced.intervention_id.clone(),
                    epoch: fenced.epoch,
                })
                .await?,
            HandoffInterventionStatus::HumanActive,
        )
    }

    pub(crate) async fn human_write(
        &self,
        intervention: &TerminalPtyInterventionRef,
        bytes: &[u8],
    ) -> Result<(), TerminalPtyDogfoodError> {
        let _operation = self.operation_gate.lock().await;
        let (binding, handoff) = self.bindings().await?;
        expect_ok(
            self.authority
                .terminal_pty_control(TerminalPtyHandoffControl::HumanInput {
                    binding: handoff,
                    intervention_id: intervention.intervention_id.clone(),
                    epoch: intervention.epoch,
                })
                .await?,
        )?;
        self.state.lock().await.pty.write(&binding, bytes)?;
        Ok(())
    }

    pub(crate) async fn human_read(
        &self,
        intervention: &TerminalPtyInterventionRef,
        max_bytes: usize,
    ) -> Result<TerminalPtyOutput, TerminalPtyDogfoodError> {
        let _operation = self.operation_gate.lock().await;
        let (binding, handoff) = self.bindings().await?;
        expect_ok(
            self.authority
                .terminal_pty_control(TerminalPtyHandoffControl::HumanObserve {
                    binding: handoff,
                    intervention_id: intervention.intervention_id.clone(),
                    epoch: intervention.epoch,
                })
                .await?,
        )?;
        Ok(self
            .state
            .lock()
            .await
            .pty
            .read_human(&binding, max_bytes)?)
    }

    pub(crate) async fn human_resize(
        &self,
        intervention: &TerminalPtyInterventionRef,
        rows: u16,
        cols: u16,
    ) -> Result<(), TerminalPtyDogfoodError> {
        let _operation = self.operation_gate.lock().await;
        let (binding, handoff) = self.bindings().await?;
        expect_ok(
            self.authority
                .terminal_pty_control(TerminalPtyHandoffControl::HumanResize {
                    binding: handoff,
                    intervention_id: intervention.intervention_id.clone(),
                    epoch: intervention.epoch,
                })
                .await?,
        )?;
        self.state.lock().await.pty.resize(&binding, rows, cols)?;
        Ok(())
    }

    pub(crate) async fn human_disconnect(
        &self,
        intervention: &TerminalPtyInterventionRef,
    ) -> Result<(), TerminalPtyDogfoodError> {
        let _operation = self.operation_gate.lock().await;
        let (_, handoff) = self.bindings().await?;
        expect_status(
            self.authority
                .terminal_pty_control(TerminalPtyHandoffControl::HumanDisconnect {
                    binding: handoff,
                    intervention_id: intervention.intervention_id.clone(),
                    epoch: intervention.epoch,
                })
                .await?,
        )?;
        Ok(())
    }

    pub(crate) async fn human_done(
        &self,
        intervention: &TerminalPtyInterventionRef,
    ) -> Result<TerminalPtyInterventionRef, TerminalPtyDogfoodError> {
        let _operation = self.operation_gate.lock().await;
        let (binding, handoff) = self.bindings().await?;
        let verifying = expect_transition(
            self.authority
                .terminal_pty_control(TerminalPtyHandoffControl::DoneFence {
                    binding: handoff.clone(),
                    intervention_id: intervention.intervention_id.clone(),
                    epoch: intervention.epoch,
                })
                .await?,
            HandoffInterventionStatus::Verifying,
        )?;
        let session_alive = {
            let state = self.state.lock().await;
            let drain = state.pty.drain_writes(&binding);
            let process = state.pty.process_state(&binding);
            let alive = drain.is_ok() && matches!(process, Ok(TerminalPtyProcessState::Running));
            if !alive {
                state.pty.terminate(&binding)?;
            }
            state.pty.finish_human_output(&binding)?;
            alive
        };
        if !session_alive {
            let _ = self
                .authority
                .terminal_pty_control(TerminalPtyHandoffControl::SessionExit(handoff.clone()))
                .await?;
        }
        expect_transition(
            self.authority
                .terminal_pty_control(TerminalPtyHandoffControl::ConfirmHumanDrain {
                    binding: handoff,
                    intervention_id: verifying.intervention_id.clone(),
                    epoch: verifying.epoch,
                })
                .await?,
            HandoffInterventionStatus::Verifying,
        )
    }

    pub(crate) async fn process_state(
        &self,
    ) -> Result<TerminalPtyProcessState, TerminalPtyDogfoodError> {
        let _operation = self.operation_gate.lock().await;
        let (binding, handoff) = self.bindings().await?;
        let observed = self.state.lock().await.pty.process_state(&binding);
        match observed {
            Ok(state) => {
                if !matches!(state, TerminalPtyProcessState::Running) {
                    let _ = self
                        .authority
                        .terminal_pty_control(TerminalPtyHandoffControl::SessionExit(handoff))
                        .await?;
                }
                Ok(state)
            }
            Err(error) => {
                if error.closes_session() {
                    {
                        let state = self.state.lock().await;
                        let _ = state.pty.terminate(&binding);
                    }
                    let _ = self
                        .authority
                        .terminal_pty_control(TerminalPtyHandoffControl::SessionExit(handoff))
                        .await?;
                }
                Err(TerminalPtyDogfoodError::Pty(error))
            }
        }
    }

    pub(crate) async fn report_verification(
        &self,
        intervention: &TerminalPtyInterventionRef,
        satisfied: bool,
    ) -> Result<TerminalPtyInterventionRef, TerminalPtyDogfoodError> {
        let _operation = self.operation_gate.lock().await;
        let (_, handoff) = self.bindings().await?;
        expect_transition(
            self.authority
                .terminal_pty_control(TerminalPtyHandoffControl::Verify {
                    binding: handoff,
                    intervention_id: intervention.intervention_id.clone(),
                    epoch: intervention.epoch,
                    satisfied,
                })
                .await?,
            if satisfied {
                HandoffInterventionStatus::ReadyToResume
            } else {
                HandoffInterventionStatus::Verifying
            },
        )
    }

    pub(crate) async fn resume(
        &self,
        intervention: &TerminalPtyInterventionRef,
    ) -> Result<TerminalPtyResumeReceipt, TerminalPtyDogfoodError> {
        let _operation = self.operation_gate.lock().await;
        let (_, handoff) = self.bindings().await?;
        match self
            .authority
            .terminal_pty_control(TerminalPtyHandoffControl::Resume {
                binding: handoff,
                intervention_id: intervention.intervention_id.clone(),
                epoch: intervention.epoch,
            })
            .await?
        {
            TerminalPtyHandoffResult::Resume(receipt) => Ok(receipt),
            _ => Err(TerminalPtyDogfoodError::InvalidTransition),
        }
    }

    pub(crate) async fn close_session(&self) -> Result<(), TerminalPtyDogfoodError> {
        let _operation = self.operation_gate.lock().await;
        let (binding, handoff) = self.bindings().await?;
        {
            let state = self.state.lock().await;
            state.pty.terminate(&binding)?;
        }
        let _ = self
            .authority
            .terminal_pty_control(TerminalPtyHandoffControl::SessionExit(handoff.clone()))
            .await?;
        expect_ok(
            self.authority
                .terminal_pty_control(TerminalPtyHandoffControl::ReleaseClosed(handoff))
                .await?,
        )?;
        let mut state = self.state.lock().await;
        state.pty.release(&binding)?;
        state.binding = None;
        Ok(())
    }

    /// The initial prototype does not cache cwd/env/job/prompt state. Calling this means the
    /// consumer explicitly discarded all pre-Handoff assumptions; it is not inferred from output.
    pub(crate) async fn acknowledge_state_invalidated(
        &self,
    ) -> Result<(), TerminalPtyDogfoodError> {
        let _operation = self.operation_gate.lock().await;
        let (_, handoff) = self.bindings().await?;
        expect_ok(
            self.authority
                .terminal_pty_control(TerminalPtyHandoffControl::AckStateSync(handoff))
                .await?,
        )
    }

    async fn bindings(
        &self,
    ) -> Result<(TerminalPtyBinding, TerminalPtyHandoffBinding), TerminalPtyDogfoodError> {
        let state = self.state.lock().await;
        let binding = state
            .binding
            .clone()
            .ok_or(TerminalPtyDogfoodError::SessionUnavailable)?;
        let handoff = handoff_binding(&binding, &state.principal_binding)?;
        Ok((binding, handoff))
    }
}

fn handoff_binding(
    binding: &TerminalPtyBinding,
    principal_binding: &str,
) -> Result<TerminalPtyHandoffBinding, TerminalPtyDogfoodError> {
    Ok(TerminalPtyHandoffBinding::new(
        binding.session_id().to_owned(),
        binding.generation(),
        principal_binding.to_owned(),
    )?)
}

fn expect_ok(result: TerminalPtyHandoffResult) -> Result<(), TerminalPtyDogfoodError> {
    match result {
        TerminalPtyHandoffResult::Ok => Ok(()),
        _ => Err(TerminalPtyDogfoodError::InvalidTransition),
    }
}

fn expect_status(result: TerminalPtyHandoffResult) -> Result<(), TerminalPtyDogfoodError> {
    match result {
        TerminalPtyHandoffResult::Status(Some(_)) => Ok(()),
        _ => Err(TerminalPtyDogfoodError::InvalidTransition),
    }
}

fn expect_transition(
    result: TerminalPtyHandoffResult,
    expected: HandoffInterventionStatus,
) -> Result<TerminalPtyInterventionRef, TerminalPtyDogfoodError> {
    match result {
        TerminalPtyHandoffResult::Transition(intervention) if intervention.status == expected => {
            Ok(intervention)
        }
        _ => Err(TerminalPtyDogfoodError::InvalidTransition),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::v2_operator_handoff::TerminalPtyHandoffStatus;
    use async_trait::async_trait;
    use std::{collections::VecDeque, ffi::OsString, path::Path};

    struct FakeAuthority {
        replies: Mutex<VecDeque<TerminalPtyHandoffResult>>,
        calls: Mutex<Vec<TerminalPtyHandoffControl>>,
    }

    impl FakeAuthority {
        fn new(replies: Vec<TerminalPtyHandoffResult>) -> Self {
            Self {
                replies: Mutex::new(replies.into()),
                calls: Mutex::new(Vec::new()),
            }
        }
    }

    #[async_trait]
    impl TerminalPtyHandoffAuthority for FakeAuthority {
        async fn terminal_pty_control(
            &self,
            control: TerminalPtyHandoffControl,
        ) -> Result<TerminalPtyHandoffResult, HandoffControlError> {
            self.calls.lock().await.push(control);
            self.replies
                .lock()
                .await
                .pop_front()
                .ok_or(HandoffControlError::Protocol)
        }
    }

    fn transition(status: HandoffInterventionStatus, epoch: u64) -> TerminalPtyHandoffResult {
        TerminalPtyHandoffResult::Transition(TerminalPtyInterventionRef {
            intervention_id: "intervention-1".into(),
            epoch,
            status,
        })
    }

    fn config() -> TerminalPtySpawnConfig {
        TerminalPtySpawnConfig {
            program: Path::new("/bin/cat").to_path_buf(),
            args: Vec::new(),
            cwd: Path::new("/tmp").to_path_buf(),
            env: vec![(OsString::from("TERM"), OsString::from("xterm-256color"))],
            rows: 24,
            cols: 80,
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn coordinator_orders_fence_drain_claim_and_done_drain_before_verification() {
        if !Path::new("/bin/cat").exists() {
            return;
        }
        let authority = Arc::new(FakeAuthority::new(vec![
            TerminalPtyHandoffResult::Status(Some(TerminalPtyHandoffStatus {
                authority: crate::v2_operator_handoff::TerminalPtyExecutionAuthority::Agent,
                intervention_status: None,
                intervention_epoch: None,
                session_generation: 1,
                session_alive: true,
                human_disconnected: false,
                agent_state_synchronization_required: false,
            })),
            transition(HandoffInterventionStatus::AwaitingHuman, 1),
            transition(HandoffInterventionStatus::HumanActive, 1),
            TerminalPtyHandoffResult::Ok,
            transition(HandoffInterventionStatus::Verifying, 2),
            transition(HandoffInterventionStatus::Verifying, 2),
            transition(HandoffInterventionStatus::ReadyToResume, 2),
            TerminalPtyHandoffResult::Resume(TerminalPtyResumeReceipt {
                epoch: 2,
                session_alive: true,
                agent_state_sync_required: true,
            }),
            TerminalPtyHandoffResult::Ok,
            TerminalPtyHandoffResult::Ok,
        ]));
        let coordinator =
            TerminalPtyDogfoodCoordinator::new(authority.clone(), "a".repeat(64)).unwrap();
        coordinator.spawn(config()).await.unwrap();
        let human = coordinator.begin_human().await.unwrap();
        coordinator
            .human_write(&human, b"human-period\n")
            .await
            .unwrap();
        let verifying = coordinator.human_done(&human).await.unwrap();
        let ready = coordinator
            .report_verification(&verifying, true)
            .await
            .unwrap();
        let receipt = coordinator.resume(&ready).await.unwrap();
        assert!(receipt.agent_state_sync_required);
        coordinator.acknowledge_state_invalidated().await.unwrap();
        coordinator.agent_write(b"agent-after\n").await.unwrap();

        let calls = authority.calls.lock().await;
        assert!(matches!(calls[1], TerminalPtyHandoffControl::BeginFence(_)));
        assert!(matches!(
            calls[2],
            TerminalPtyHandoffControl::ClaimHuman { .. }
        ));
        assert!(matches!(
            calls[4],
            TerminalPtyHandoffControl::DoneFence { .. }
        ));
        assert!(matches!(
            calls[5],
            TerminalPtyHandoffControl::ConfirmHumanDrain { .. }
        ));
        assert!(matches!(calls[6], TerminalPtyHandoffControl::Verify { .. }));
        assert!(matches!(calls[7], TerminalPtyHandoffControl::Resume { .. }));
        assert!(matches!(
            calls[8],
            TerminalPtyHandoffControl::AckStateSync(_)
        ));
        assert!(matches!(calls[9], TerminalPtyHandoffControl::AgentInput(_)));
    }

    #[cfg(unix)]
    #[tokio::test]
    #[ignore = "real local Handoff runtime + PTY dogfood; run explicitly with CUMG_V2_HANDOFF_ROOT and CUMG_V2_NODE"]
    async fn real_handoff_runtime_and_real_pty_dogfood() {
        use crate::v2_operator_handoff::{
            ManagedHandoffRuntimeConfig, ManagedOperatorHandoffAuthority,
        };
        use rand::{RngCore, rngs::OsRng};
        use std::{env, fs, os::unix::fs::PermissionsExt, path::PathBuf, time::Duration};
        use tokio::time::sleep;

        let handoff_root =
            PathBuf::from(env::var_os("CUMG_V2_HANDOFF_ROOT").expect("CUMG_V2_HANDOFF_ROOT"));
        let node = PathBuf::from(env::var_os("CUMG_V2_NODE").expect("CUMG_V2_NODE"));
        assert!(
            handoff_root
                .join("dist/experimental/terminal-pty.js")
                .is_file()
        );
        assert!(node.is_absolute());

        let mut nonce = [0_u8; 8];
        OsRng.fill_bytes(&mut nonce);
        let temp = env::temp_dir().join(format!(
            "cumg-v2-terminal-pty-real-{}-{}",
            std::process::id(),
            u64::from_le_bytes(nonce),
        ));
        fs::create_dir(&temp).unwrap();
        fs::set_permissions(&temp, fs::Permissions::from_mode(0o700)).unwrap();
        let key = temp.join("checkpoint.key");
        fs::write(&key, [0x51_u8; 32]).unwrap();
        fs::set_permissions(&key, fs::Permissions::from_mode(0o600)).unwrap();
        let checkpoint = temp.join("checkpoint.json");
        let env_file = temp.join("managed-runtime.env");
        fs::write(
            &env_file,
            format!(
                "CUMG_V2_HANDOFF_ROOT={}\nCUMG_V2_HANDOFF_CHECKPOINT_FILE={}\nCUMG_V2_HANDOFF_CHECKPOINT_KEY_FILE={}\n",
                handoff_root.display(),
                checkpoint.display(),
                key.display(),
            ),
        )
        .unwrap();
        fs::set_permissions(&env_file, fs::Permissions::from_mode(0o600)).unwrap();
        let script =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("scripts/v2_handoff_runtime.mjs");
        let runtime_config =
            ManagedHandoffRuntimeConfig::new(node, script, env_file, Duration::from_secs(5))
                .unwrap();
        let runtime = Arc::new(
            ManagedOperatorHandoffAuthority::spawn(runtime_config)
                .await
                .unwrap(),
        );
        let coordinator =
            TerminalPtyDogfoodCoordinator::new(runtime.clone(), "a".repeat(64)).unwrap();

        // A PTY that exits after the Agent fence but before Human claim must never grant Human
        // authority or wedge the single-session slot. Explicit closed release permits only a fresh
        // generation; it does not revive or replay the exited PTY.
        let exited_binding = coordinator
            .spawn(TerminalPtySpawnConfig {
                program: Path::new("/usr/bin/true").to_path_buf(),
                args: Vec::new(),
                cwd: Path::new("/tmp").to_path_buf(),
                env: Vec::new(),
                rows: 24,
                cols: 80,
            })
            .await
            .unwrap();
        sleep(Duration::from_millis(80)).await;
        assert!(coordinator.begin_human().await.is_err());
        coordinator.close_session().await.unwrap();

        let binding = coordinator.spawn(config()).await.unwrap();
        assert_eq!(binding.generation(), exited_binding.generation() + 1);

        coordinator.agent_write(b"agent-before\n").await.unwrap();
        sleep(Duration::from_millis(80)).await;
        let before = coordinator.agent_read(64 * 1024).await.unwrap();
        assert!(
            before
                .as_bytes()
                .windows(b"agent-before".len())
                .any(|w| w == b"agent-before")
        );

        let human = coordinator.begin_human().await.unwrap();
        assert!(coordinator.agent_write(b"must-not-run\n").await.is_err());
        coordinator.human_resize(&human, 30, 100).await.unwrap();
        coordinator
            .human_write(&human, b"human-period-private\n")
            .await
            .unwrap();
        sleep(Duration::from_millis(80)).await;
        let human_output = coordinator.human_read(&human, 64 * 1024).await.unwrap();
        assert!(
            human_output
                .as_bytes()
                .windows(b"human-period-private".len())
                .any(|w| w == b"human-period-private")
        );

        coordinator.human_disconnect(&human).await.unwrap();
        assert!(
            coordinator
                .agent_write(b"still-must-not-run\n")
                .await
                .is_err()
        );
        let verifying = coordinator.human_done(&human).await.unwrap();
        assert!(
            coordinator
                .agent_write(b"verifying-must-not-run\n")
                .await
                .is_err()
        );
        assert_eq!(
            coordinator.process_state().await.unwrap(),
            TerminalPtyProcessState::Running
        );

        let ready = coordinator
            .report_verification(&verifying, true)
            .await
            .unwrap();
        assert!(
            coordinator
                .agent_write(b"ready-must-not-run\n")
                .await
                .is_err()
        );
        let receipt = coordinator.resume(&ready).await.unwrap();
        assert!(receipt.session_alive);
        assert!(receipt.agent_state_sync_required);
        assert!(coordinator.agent_write(b"sync-required\n").await.is_err());
        coordinator.acknowledge_state_invalidated().await.unwrap();

        let after_resume = coordinator.agent_read(64 * 1024).await.unwrap();
        assert!(
            !after_resume
                .as_bytes()
                .windows(b"human-period-private".len())
                .any(|w| w == b"human-period-private")
        );
        coordinator.agent_write(b"agent-after\n").await.unwrap();
        sleep(Duration::from_millis(80)).await;
        let after = coordinator.agent_read(64 * 1024).await.unwrap();
        assert!(
            after
                .as_bytes()
                .windows(b"agent-after".len())
                .any(|w| w == b"agent-after")
        );

        coordinator.close_session().await.unwrap();
        runtime.shutdown().await;
        assert!(
            !checkpoint.exists(),
            "Terminal prototype must not persist generic Handoff checkpoint state"
        );
        fs::remove_dir_all(temp).unwrap();
    }
}
