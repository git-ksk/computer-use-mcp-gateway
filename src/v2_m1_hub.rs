//! Operator-facing V2-M1 single-device Hub runtime.
//!
//! The Hub accepts one enrolled Agent identity over gRPC/TLS, keeps the V2
//! application messages independently signed, persists admission/generation
//! state before risky transitions, and exposes a small in-process handle that a
//! future authenticated northbound MCP layer can call without depending on gRPC.

use crate::v2_execution_safety::{
    AuthoritativeOperationController, DesktopQuarantine, ExecutionEvidence, ExecutionReceipt,
    IndeterminateReason, OperationOwner, ResolutionRecord,
};
use crate::v2_m0::{
    CONTROL_SCHEMA_VERSION, CommandEnvelope, DeviceCommand, DeviceErrorCode, DeviceRegistry,
    DeviceResult, DirectoryEntry, GrantAuthority, ProcessOutput, ProcessRequest, ShellRequest,
    validate_command_result,
};
use crate::v2_m0_execution::{
    AdmissionDecision, AdmissionLimits, CancellationDecision, CompletionDecision,
    HubOperationState, IndeterminateResolution, OperationRef,
};
use crate::v2_m0_transport::{
    AgentHello, AgentToHub, CancellationDisposition, HubChallenge, HubIdentity, HubToAgent,
    RemoteCancellationAck, RemoteResult, TrustedSessionClock, verify_agent_heartbeat,
    verify_agent_proof, verify_remote_cancellation_ack, verify_remote_result,
};
use crate::v2_m0_trust::{DeviceKeyRotation, apply_device_key_rotation};
use crate::v2_m1_grpc::{
    decode_agent_frame, encode_hub_frame,
    proto::{AgentFrame, HubFrame, agent_control_server::AgentControl},
};
use crate::v2_m1_persistence::{CheckpointStore, HubPersistentState, PersistenceError};
use ed25519_dalek::VerifyingKey;
use opentelemetry::KeyValue;
use rand::{RngCore, rngs::OsRng};
use std::collections::{HashMap, VecDeque};
use std::fmt;
use std::pin::Pin;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::sync::{Mutex, Semaphore, mpsc, oneshot, watch};
use tokio_stream::{Stream, wrappers::ReceiverStream};
use tonic::{Request, Response, Status, Streaming};
use tracing::Instrument as _;

const SESSION_QUEUE_DEPTH: usize = 16;
const DEFAULT_GRANT_TTL_MS: u64 = 30_000;
// Hub and Agent start their monotonic session clocks on opposite sides of the
// SessionAccepted network hop. Backdating only `issued_at` shortens the grant's
// effective remaining life and avoids treating ordinary transport latency as a
// future-dated grant.
const GRANT_ISSUED_AT_SAFETY_MS: u64 = 5_000;

#[derive(Clone)]
pub struct HubProvisionedMaterial {
    pub hub_identity: HubIdentity,
    pub grant_authority: GrantAuthority,
    pub device_verifier: VerifyingKey,
    pub device_rotation: Option<DeviceKeyRotation>,
}

#[derive(Debug, Clone)]
pub struct HubServiceConfig {
    pub state_dir: std::path::PathBuf,
    pub heartbeat_timeout: Duration,
    pub max_queued_per_device: usize,
    pub max_agent_sessions: usize,
    pub max_agent_session_starts_per_minute: usize,
}

impl HubServiceConfig {
    fn admission_limits(&self) -> AdmissionLimits {
        AdmissionLimits {
            max_global_active: 1,
            max_queued_per_device: self.max_queued_per_device,
        }
    }

    fn validate(&self) -> Result<(), HubServiceError> {
        if self.heartbeat_timeout.is_zero() {
            return Err(HubServiceError::InvalidConfig(
                "heartbeat timeout must be non-zero",
            ));
        }
        if self.max_agent_sessions == 0 || self.max_agent_session_starts_per_minute == 0 {
            return Err(HubServiceError::InvalidConfig(
                "Agent session limits must be non-zero",
            ));
        }
        Ok(())
    }
}

struct PersistentHubState {
    registry: DeviceRegistry,
    execution: AuthoritativeOperationController,
}

#[derive(Clone)]
struct LiveSession {
    generation: u64,
    command_tx: mpsc::Sender<HubRequest>,
    supersede: watch::Sender<bool>,
}

struct HubInner {
    config: HubServiceConfig,
    material: HubProvisionedMaterial,
    device_id: String,
    checkpoint: CheckpointStore,
    persistent: Mutex<PersistentHubState>,
    live: Mutex<Option<LiveSession>>,
    session_slots: Arc<Semaphore>,
    session_rate: crate::v2_limits::SlidingWindowRateLimit,
}

#[derive(Clone)]
pub struct SingleDeviceHub {
    inner: Arc<HubInner>,
}

#[derive(Clone)]
pub struct HubHandle {
    inner: Arc<HubInner>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HubCommandResult {
    pub operation_id: String,
    pub result: DeviceResult,
    pub receipt: ExecutionReceipt,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HubProcessResult {
    pub operation_id: String,
    pub output: ProcessOutput,
    pub receipt: ExecutionReceipt,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HubShellResult {
    pub operation_id: String,
    pub output: ProcessOutput,
    pub receipt: ExecutionReceipt,
}

pub struct HubPendingCommand {
    pub operation_id: String,
    reply: oneshot::Receiver<Result<HubCommandResult, HubCommandError>>,
}

impl HubPendingCommand {
    pub async fn wait(self) -> Result<HubCommandResult, HubCommandError> {
        self.reply
            .await
            .map_err(|_| HubCommandError::SessionClosed)?
    }
}

pub struct HubPendingProcess {
    pub operation_id: String,
    pending: HubPendingCommand,
}

impl HubPendingProcess {
    pub async fn wait(self) -> Result<HubProcessResult, HubCommandError> {
        let result = self.pending.wait().await?;
        match result.result {
            DeviceResult::Process { output } => Ok(HubProcessResult {
                operation_id: result.operation_id,
                output,
                receipt: result.receipt,
            }),
            DeviceResult::Error { code } => Err(HubCommandError::Remote(code)),
            _ => Err(HubCommandError::UnexpectedResult),
        }
    }
}

pub struct HubPendingShell {
    pub operation_id: String,
    pending: HubPendingCommand,
}

impl HubPendingShell {
    pub async fn wait(self) -> Result<HubShellResult, HubCommandError> {
        let result = self.pending.wait().await?;
        match result.result {
            DeviceResult::Shell { output } => Ok(HubShellResult {
                operation_id: result.operation_id,
                output,
                receipt: result.receipt,
            }),
            DeviceResult::Error { code } => Err(HubCommandError::Remote(code)),
            _ => Err(HubCommandError::UnexpectedResult),
        }
    }
}

#[derive(Debug)]
enum HubRequest {
    Execute {
        operation_id: String,
        owner: OperationOwner,
        command: DeviceCommand,
        reply: oneshot::Sender<Result<HubCommandResult, HubCommandError>>,
    },
    Cancel {
        operation_id: String,
        owner: OperationOwner,
        reply: oneshot::Sender<Result<CancellationDisposition, HubCommandError>>,
    },
}

struct PendingOperation {
    owner: OperationOwner,
    command: DeviceCommand,
    envelope: Option<CommandEnvelope>,
    reply: oneshot::Sender<Result<HubCommandResult, HubCommandError>>,
}

impl SingleDeviceHub {
    pub fn new(
        config: HubServiceConfig,
        material: HubProvisionedMaterial,
    ) -> Result<(Self, HubHandle), HubServiceError> {
        config.validate()?;
        let checkpoint = CheckpointStore::new(config.state_dir.clone(), "hub")
            .map_err(HubServiceError::Persistence)?;

        let mut identity_registry = DeviceRegistry::default();
        let provisioned_device_id =
            identity_registry.provision_trusted_device(material.device_verifier);
        let (device_id, registry, execution) = match checkpoint.load_latest::<HubPersistentState>()
        {
            Ok(state) => {
                if state.registry.devices.len() != 1 {
                    return Err(HubServiceError::CheckpointDeviceTrustMismatch);
                }
                let device_id = state.registry.devices[0].device_id.clone();
                let (mut registry, execution) = state
                    .restore(config.admission_limits())
                    .map_err(HubServiceError::Persistence)?;
                if registry
                    .device_verifier(&device_id)
                    .map_err(HubServiceError::Control)?
                    != material.device_verifier
                {
                    let rotation = material
                        .device_rotation
                        .as_ref()
                        .ok_or(HubServiceError::CheckpointDeviceTrustMismatch)?;
                    if rotation.device_id != device_id {
                        return Err(HubServiceError::CheckpointDeviceTrustMismatch);
                    }
                    apply_device_key_rotation(&mut registry, rotation, rotation.rotation_epoch)
                        .map_err(HubServiceError::Trust)?;
                    if registry
                        .device_verifier(&device_id)
                        .map_err(HubServiceError::Control)?
                        != material.device_verifier
                    {
                        return Err(HubServiceError::CheckpointDeviceTrustMismatch);
                    }
                }
                (device_id, registry, execution)
            }
            Err(PersistenceError::NoCheckpoint) => (
                provisioned_device_id,
                identity_registry,
                AuthoritativeOperationController::new(config.admission_limits())
                    .map_err(HubServiceError::Execution)?,
            ),
            Err(error) => return Err(HubServiceError::Persistence(error)),
        };

        let session_slots = Arc::new(Semaphore::new(config.max_agent_sessions));
        let session_rate = crate::v2_limits::SlidingWindowRateLimit::new(
            config.max_agent_session_starts_per_minute,
            Duration::from_secs(60),
        )
        .map_err(|_| HubServiceError::InvalidConfig("invalid Agent session rate limit"))?;
        let inner = Arc::new(HubInner {
            config,
            material,
            device_id,
            checkpoint,
            persistent: Mutex::new(PersistentHubState {
                registry,
                execution,
            }),
            live: Mutex::new(None),
            session_slots,
            session_rate,
        });
        let service = Self {
            inner: inner.clone(),
        };
        // Establish a fail-closed baseline before the service accepts a session.
        service.persist_blocking()?;
        Ok((service, HubHandle { inner }))
    }

    pub fn device_id(&self) -> &str {
        &self.inner.device_id
    }

    fn persist_blocking(&self) -> Result<(), HubServiceError> {
        let persistent = self
            .inner
            .persistent
            .try_lock()
            .map_err(|_| HubServiceError::StateBusy)?;
        persist_locked(&self.inner, &persistent)
    }

    async fn run_session(
        &self,
        mut inbound: Streaming<AgentFrame>,
        outbound: mpsc::Sender<Result<HubFrame, Status>>,
    ) -> Result<(), HubServiceError> {
        let hello = match next_agent(&mut inbound).await? {
            AgentToHub::Hello(hello) => hello,
            other => return Err(HubServiceError::UnexpectedMessage(format!("{other:?}"))),
        };
        if hello.device_id != self.inner.device_id {
            return Err(HubServiceError::WrongDevice);
        }
        let challenge = self.inner.material.hub_identity.challenge(&hello)?;
        send_hub(&outbound, HubToAgent::Challenge(challenge.clone())).await?;

        let proof = match next_agent(&mut inbound).await? {
            AgentToHub::Proof(proof) => proof,
            other => return Err(HubServiceError::UnexpectedMessage(format!("{other:?}"))),
        };

        let session = {
            let mut persistent = self.inner.persistent.lock().await;
            verify_agent_proof(&persistent.registry, &hello, &challenge, &proof)?;
            let session = persistent
                .registry
                .connect(&self.inner.device_id, hello.capabilities.clone())?;
            persistent
                .execution
                .prune_terminal_before_generation(&self.inner.device_id, session.generation)?;
            // Generation advancement and safe replay-tombstone pruning must
            // survive a Hub crash before the Agent receives acceptance.
            persist_locked(&self.inner, &persistent)?;
            session
        };

        let accepted_hub_time_ms = unix_time_ms()?;
        let accepted = self.inner.material.hub_identity.accept_session(
            &hello,
            &challenge,
            session.generation,
            session.capabilities.revision,
            accepted_hub_time_ms,
        )?;
        send_hub(&outbound, HubToAgent::Accepted(accepted)).await?;
        // Use the same monotonic-derived Hub clock for all grants in this
        // session. Re-reading wall time for each grant can make issued_at appear
        // a few milliseconds in the future relative to the Agent's signed
        // SessionAccepted clock anchor.
        let session_clock = TrustedSessionClock::new(accepted_hub_time_ms);

        let (command_tx, mut command_rx) = mpsc::channel(SESSION_QUEUE_DEPTH);
        let (supersede_tx, mut supersede_rx) = watch::channel(false);
        let prior = {
            let mut live = self.inner.live.lock().await;
            live.replace(LiveSession {
                generation: session.generation,
                command_tx,
                supersede: supersede_tx,
            })
        };
        if let Some(prior) = prior {
            let _ = prior.supersede.send(true);
        }

        let result = self
            .run_session_loop(
                &mut inbound,
                outbound,
                hello,
                challenge,
                session.generation,
                session.capabilities.revision,
                &session_clock,
                &mut command_rx,
                &mut supersede_rx,
            )
            .await;
        self.cleanup_session(session.generation).await?;
        result
    }

    #[allow(clippy::too_many_arguments)]
    async fn run_session_loop(
        &self,
        inbound: &mut Streaming<AgentFrame>,
        outbound: mpsc::Sender<Result<HubFrame, Status>>,
        hello: AgentHello,
        challenge: HubChallenge,
        generation: u64,
        capability_revision: u64,
        session_clock: &TrustedSessionClock,
        command_rx: &mut mpsc::Receiver<HubRequest>,
        supersede_rx: &mut watch::Receiver<bool>,
    ) -> Result<(), HubServiceError> {
        let mut pending: HashMap<String, PendingOperation> = HashMap::new();
        let mut queue_order: VecDeque<String> = VecDeque::new();
        let mut cancel_waiters: HashMap<
            String,
            oneshot::Sender<Result<CancellationDisposition, HubCommandError>>,
        > = HashMap::new();
        let heartbeat_deadline = tokio::time::sleep(self.inner.config.heartbeat_timeout);
        tokio::pin!(heartbeat_deadline);

        loop {
            tokio::select! {
                _ = &mut heartbeat_deadline => {
                    return Err(HubServiceError::HeartbeatTimeout);
                }
                changed = supersede_rx.changed() => {
                    if changed.is_err() || *supersede_rx.borrow() {
                        return Ok(());
                    }
                }
                request = command_rx.recv() => {
                    let Some(request) = request else {
                        return Ok(());
                    };
                    match request {
                        HubRequest::Execute { operation_id, owner, command, reply } => {
                            if pending.contains_key(&operation_id) {
                                let _ = reply.send(Err(HubCommandError::OperationReplay));
                                continue;
                            }
                            let operation = OperationRef {
                                device_id: self.inner.device_id.clone(),
                                device_generation: generation,
                                operation_id: operation_id.clone(),
                            };
                            let decision = {
                                let mut persistent = self.inner.persistent.lock().await;
                                match persistent.execution.prepare(
                                    operation,
                                    owner.clone(),
                                    command.capability(),
                                    unix_time_ms()?,
                                ) {
                                    Ok(decision) => {
                                        persist_locked(&self.inner, &persistent)?;
                                        Ok(decision)
                                    }
                                    Err(error) => Err(command_error_from_execution(error)),
                                }
                            };
                            let decision = match decision {
                                Ok(decision) => decision,
                                Err(error) => {
                                    let _ = reply.send(Err(error));
                                    continue;
                                }
                            };
                            pending.insert(operation_id.clone(), PendingOperation {
                                owner,
                                command,
                                envelope: None,
                                reply,
                            });
                            match decision {
                                AdmissionDecision::StartNow(operation) => {
                                    self.dispatch_operation(
                                        &outbound,
                                        &hello,
                                        &challenge,
                                        generation,
                                        capability_revision,
                                        session_clock,
                                        &operation.operation_id,
                                        &mut pending,
                                    ).await?;
                                }
                                AdmissionDecision::Queued { .. } => queue_order.push_back(operation_id),
                            }
                        }
                        HubRequest::Cancel { operation_id, owner, reply } => {
                            let decision = {
                                let mut persistent = self.inner.persistent.lock().await;
                                match persistent.execution.request_cancel(
                                    &operation_id,
                                    &owner,
                                    generation,
                                    unix_time_ms()?,
                                ) {
                                    Ok(decision) => {
                                        persist_locked(&self.inner, &persistent)?;
                                        Ok(decision)
                                    }
                                    Err(error) => Err(command_error_from_execution(error)),
                                }
                            };
                            let decision = match decision {
                                Ok(decision) => decision,
                                Err(error) => {
                                    let _ = reply.send(Err(error));
                                    continue;
                                }
                            };
                            match decision {
                                CancellationDecision::CancelledBeforeDispatch { next } => {
                                    if let Some(operation) = pending.remove(&operation_id) {
                                        let _ = operation.reply.send(Err(HubCommandError::CancelledBeforeDispatch));
                                    }
                                    queue_order.retain(|queued| queued != &operation_id);
                                    let _ = reply.send(Ok(CancellationDisposition::CancelledBeforeExecution));
                                    self.dispatch_next(
                                        next,
                                        &outbound,
                                        &hello,
                                        &challenge,
                                        generation,
                                        capability_revision,
                                        session_clock,
                                        &mut pending,
                                        &mut queue_order,
                                    ).await?;
                                }
                                CancellationDecision::SendCancellation(operation) => {
                                    let remote = self.inner.material.hub_identity.remote_cancel(
                                        &hello,
                                        &challenge,
                                        self.inner.device_id.clone(),
                                        generation,
                                        operation.operation_id.clone(),
                                    )?;
                                    cancel_waiters.insert(operation.operation_id, reply);
                                    send_hub(&outbound, HubToAgent::Cancel(remote)).await?;
                                }
                                CancellationDecision::AlreadyTerminal(_) => {
                                    let _ = reply.send(Ok(CancellationDisposition::AlreadyTerminal));
                                }
                            }
                        }
                    }
                }
                message = inbound.message() => {
                    let frame = match message.map_err(HubServiceError::Status)? {
                        Some(frame) => frame,
                        None => {
                            return Ok(());
                        }
                    };
                    self.ensure_current_generation(generation).await?;
                    match decode_agent_frame(frame).map_err(HubServiceError::Carrier)? {
                        AgentToHub::Heartbeat(heartbeat) => {
                            {
                                let persistent = self.inner.persistent.lock().await;
                                verify_agent_heartbeat(&persistent.registry, &hello, &challenge, &heartbeat)?;
                            }
                            if heartbeat.device_generation != generation {
                                return Err(HubServiceError::StaleSession);
                            }
                            let ack = self.inner.material.hub_identity.heartbeat_ack(
                                &hello,
                                &challenge,
                                &heartbeat,
                                session_clock.now_ms(),
                            )?;
                            send_hub(&outbound, HubToAgent::HeartbeatAck(ack)).await?;
                            heartbeat_deadline.as_mut().reset(tokio::time::Instant::now() + self.inner.config.heartbeat_timeout);
                        }
                        AgentToHub::Result(result) => {
                            self.handle_result(
                                result,
                                &outbound,
                                &hello,
                                &challenge,
                                generation,
                                capability_revision,
                                session_clock,
                                &mut pending,
                                &mut queue_order,
                            ).await?;
                        }
                        AgentToHub::CancellationAck(ack) => {
                            self.handle_cancellation_ack(
                                ack,
                                &hello,
                                &challenge,
                                generation,
                                &mut pending,
                                &mut queue_order,
                                &mut cancel_waiters,
                            ).await?;
                        }
                        other => return Err(HubServiceError::UnexpectedMessage(format!("{other:?}"))),
                    }
                }
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    async fn dispatch_operation(
        &self,
        outbound: &mpsc::Sender<Result<HubFrame, Status>>,
        hello: &AgentHello,
        challenge: &HubChallenge,
        generation: u64,
        capability_revision: u64,
        session_clock: &TrustedSessionClock,
        operation_id: &str,
        pending: &mut HashMap<String, PendingOperation>,
    ) -> Result<(), HubServiceError> {
        let operation = pending
            .get_mut(operation_id)
            .ok_or(HubServiceError::PendingOperationMissing)?;
        let command = CommandEnvelope {
            schema_version: CONTROL_SCHEMA_VERSION,
            device_id: self.inner.device_id.clone(),
            device_generation: generation,
            capability_revision,
            operation_id: operation_id.to_owned(),
            command: operation.command.clone(),
        };
        let grant = self
            .inner
            .material
            .grant_authority
            .issue_for_device_capability(
                &self.inner.device_id,
                command.command.capability(),
                session_clock
                    .now_ms()
                    .saturating_sub(GRANT_ISSUED_AT_SAFETY_MS),
                DEFAULT_GRANT_TTL_MS,
            )?;
        let remote = self.inner.material.hub_identity.remote_command(
            hello,
            challenge,
            command.clone(),
            grant,
        )?;

        // Persist `Dispatched` before putting bytes on the network. A crash after
        // this point can conservatively restore as indeterminate, but can never
        // restore a command that may have executed as runnable.
        {
            let mut persistent = self.inner.persistent.lock().await;
            persistent.execution.mark_dispatched(
                operation_id,
                &operation.owner,
                generation,
                unix_time_ms()?,
            )?;
            persist_locked(&self.inner, &persistent)?;
        }
        operation.envelope = Some(command);
        send_hub(outbound, HubToAgent::Command(remote)).await
    }

    #[allow(clippy::too_many_arguments)]
    async fn handle_result(
        &self,
        result: RemoteResult,
        outbound: &mpsc::Sender<Result<HubFrame, Status>>,
        hello: &AgentHello,
        challenge: &HubChallenge,
        generation: u64,
        capability_revision: u64,
        session_clock: &TrustedSessionClock,
        pending: &mut HashMap<String, PendingOperation>,
        queue_order: &mut VecDeque<String>,
    ) -> Result<(), HubServiceError> {
        {
            let persistent = self.inner.persistent.lock().await;
            verify_remote_result(&persistent.registry, hello, challenge, &result)?;
        }
        let operation_id = result.result.operation_id.clone();
        let operation = pending
            .get(&operation_id)
            .ok_or(HubServiceError::PendingOperationMissing)?;
        let command = operation
            .envelope
            .as_ref()
            .ok_or(HubServiceError::PendingOperationMissing)?;
        validate_command_result(command, &result.result)?;
        let owner = operation.owner.clone();
        let device_result = result.result.result.clone();
        let (terminal_state, evidence) = match &device_result {
            DeviceResult::Error { .. } => (
                HubOperationState::Failed,
                ExecutionEvidence::VerifiedRemoteError,
            ),
            DeviceResult::Process { output } | DeviceResult::Shell { output }
                if output.cancelled =>
            {
                (
                    HubOperationState::Cancelled,
                    ExecutionEvidence::ProvenProcessTermination,
                )
            }
            DeviceResult::Process { output } | DeviceResult::Shell { output }
                if output.timed_out =>
            {
                (
                    HubOperationState::Failed,
                    ExecutionEvidence::ProvenProcessTermination,
                )
            }
            _ => (
                HubOperationState::Completed,
                ExecutionEvidence::VerifiedAgentResult,
            ),
        };
        let (next, receipt) = {
            let mut persistent = self.inner.persistent.lock().await;
            let settled = persistent.execution.finalize(
                &operation_id,
                &owner,
                generation,
                terminal_state,
                evidence,
                unix_time_ms()?,
            )?;
            persist_locked(&self.inner, &persistent)?;
            settled
        };
        if let Some(operation) = pending.remove(&operation_id) {
            let response = match device_result {
                DeviceResult::Error { code } => Err(HubCommandError::Remote(code)),
                result => Ok(HubCommandResult {
                    operation_id: operation_id.clone(),
                    result,
                    receipt,
                }),
            };
            let _ = operation.reply.send(response);
        }
        self.dispatch_next(
            next,
            outbound,
            hello,
            challenge,
            generation,
            capability_revision,
            session_clock,
            pending,
            queue_order,
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    async fn dispatch_next(
        &self,
        next: CompletionDecision,
        outbound: &mpsc::Sender<Result<HubFrame, Status>>,
        hello: &AgentHello,
        challenge: &HubChallenge,
        generation: u64,
        capability_revision: u64,
        session_clock: &TrustedSessionClock,
        pending: &mut HashMap<String, PendingOperation>,
        queue_order: &mut VecDeque<String>,
    ) -> Result<(), HubServiceError> {
        if let CompletionDecision::StartNext(operation) = next {
            queue_order.retain(|queued| queued != &operation.operation_id);
            self.dispatch_operation(
                outbound,
                hello,
                challenge,
                generation,
                capability_revision,
                session_clock,
                &operation.operation_id,
                pending,
            )
            .await?;
        }
        Ok(())
    }

    async fn handle_cancellation_ack(
        &self,
        ack: RemoteCancellationAck,
        hello: &AgentHello,
        challenge: &HubChallenge,
        generation: u64,
        pending: &mut HashMap<String, PendingOperation>,
        queue_order: &mut VecDeque<String>,
        cancel_waiters: &mut HashMap<
            String,
            oneshot::Sender<Result<CancellationDisposition, HubCommandError>>,
        >,
    ) -> Result<(), HubServiceError> {
        if ack.device_generation != generation {
            return Err(HubServiceError::StaleSession);
        }
        {
            let persistent = self.inner.persistent.lock().await;
            verify_remote_cancellation_ack(&persistent.registry, hello, challenge, &ack)?;
        }
        if ack.disposition == CancellationDisposition::IndeterminateAfterPropagation
            && !pending.contains_key(&ack.operation_id)
        {
            let state = {
                let persistent = self.inner.persistent.lock().await;
                persistent.execution.state(&ack.operation_id)
            };
            if matches!(
                state,
                Some(
                    HubOperationState::Indeterminate
                        | HubOperationState::Completed
                        | HubOperationState::Failed
                        | HubOperationState::Cancelled
                )
            ) {
                tracing::warn!(
                    event = "v2_hub_late_cancellation_ack_ignored",
                    operation_id = %ack.operation_id,
                    ?state,
                    "verified late/duplicate cancellation acknowledgement cannot mutate terminal or quarantined state"
                );
                if let Some(waiter) = cancel_waiters.remove(&ack.operation_id) {
                    let _ = waiter.send(Ok(ack.disposition));
                }
                return Ok(());
            }
        }
        if ack.disposition == CancellationDisposition::IndeterminateAfterPropagation {
            let owner = pending
                .get(&ack.operation_id)
                .ok_or(HubServiceError::PendingOperationMissing)?
                .owner
                .clone();
            let cancelled_queued = {
                let mut persistent = self.inner.persistent.lock().await;
                if matches!(
                    persistent.execution.state(&ack.operation_id),
                    Some(HubOperationState::Dispatched | HubOperationState::CancelRequested)
                ) {
                    persistent.execution.mark_indeterminate(
                        &ack.operation_id,
                        &owner,
                        generation,
                        IndeterminateReason::CancellationUnproven,
                        unix_time_ms()?,
                    )?;
                }
                let cancelled: Vec<_> = pending
                    .keys()
                    .filter(|operation_id| {
                        operation_id.as_str() != ack.operation_id
                            && persistent.execution.state(operation_id)
                                == Some(HubOperationState::Cancelled)
                    })
                    .cloned()
                    .collect();
                persist_locked(&self.inner, &persistent)?;
                cancelled
            };
            for operation_id in cancelled_queued {
                queue_order.retain(|queued| queued != &operation_id);
                if let Some(operation) = pending.remove(&operation_id) {
                    let _ = operation
                        .reply
                        .send(Err(HubCommandError::CancelledBeforeDispatch));
                }
            }
            if let Some(operation) = pending.remove(&ack.operation_id) {
                let _ = operation
                    .reply
                    .send(Err(HubCommandError::DeviceIndeterminate {
                        operation_id: ack.operation_id.clone(),
                    }));
            }
            tracing::warn!(
                event = "v2_hub_operation_indeterminate",
                operation_id = %ack.operation_id,
                "backend cancellation was propagated but side-effect interruption is unproven; device quarantined"
            );
        }
        if let Some(waiter) = cancel_waiters.remove(&ack.operation_id) {
            let _ = waiter.send(Ok(ack.disposition));
        }
        Ok(())
    }

    async fn ensure_current_generation(&self, generation: u64) -> Result<(), HubServiceError> {
        let persistent = self.inner.persistent.lock().await;
        let current = persistent.registry.current_session(&self.inner.device_id)?;
        if current.generation == generation {
            Ok(())
        } else {
            Err(HubServiceError::StaleSession)
        }
    }

    async fn cleanup_session(&self, generation: u64) -> Result<(), HubServiceError> {
        let is_current = {
            let live = self.inner.live.lock().await;
            live.as_ref()
                .is_some_and(|live| live.generation == generation)
        };
        if is_current {
            let mut live = self.inner.live.lock().await;
            if live
                .as_ref()
                .is_some_and(|live| live.generation == generation)
            {
                *live = None;
            }
        }

        let mut persistent = self.inner.persistent.lock().await;
        // Always settle operations belonging to this transport generation, even
        // if a newer session already superseded it. Dispatched work becomes
        // indeterminate; queued/not-yet-dispatched work is cancelled.
        let operations: Vec<_> = persistent
            .execution
            .snapshot_for_restart()
            .operations
            .into_iter()
            .filter(|operation| operation.operation.device_generation == generation)
            .collect();
        for operation in operations {
            let operation_id = operation.operation.operation_id;
            match persistent.execution.state(&operation_id) {
                Some(HubOperationState::Dispatched | HubOperationState::CancelRequested) => {
                    persistent
                        .execution
                        .mark_connection_lost(&operation_id, unix_time_ms()?)?;
                }
                Some(HubOperationState::Queued | HubOperationState::ActiveNotDispatched) => {
                    let _ = persistent.execution.request_cancel(
                        &operation_id,
                        &operation.owner,
                        generation,
                        unix_time_ms()?,
                    )?;
                }
                _ => {}
            }
        }

        if is_current
            && persistent
                .registry
                .current_session(&self.inner.device_id)
                .is_ok_and(|session| session.generation == generation)
        {
            persistent.registry.disconnect(&self.inner.device_id)?;
        }
        persist_locked(&self.inner, &persistent)
    }
}

impl HubHandle {
    pub fn device_id(&self) -> &str {
        &self.inner.device_id
    }

    pub async fn is_online(&self) -> bool {
        self.inner.live.lock().await.is_some()
    }

    pub async fn current_generation(&self) -> Option<u64> {
        self.inner
            .live
            .lock()
            .await
            .as_ref()
            .map(|session| session.generation)
    }

    pub async fn start_command(
        &self,
        command: DeviceCommand,
    ) -> Result<HubPendingCommand, HubCommandError> {
        self.start_command_as(OperationOwner::local_hub(), command)
            .await
    }

    pub async fn start_command_as(
        &self,
        owner: OperationOwner,
        command: DeviceCommand,
    ) -> Result<HubPendingCommand, HubCommandError> {
        let operation_id = random_operation_id();
        let (reply_tx, reply_rx) = oneshot::channel();
        let tx = {
            let live = self.inner.live.lock().await;
            live.as_ref()
                .map(|session| session.command_tx.clone())
                .ok_or(HubCommandError::AgentOffline)?
        };
        tx.send(HubRequest::Execute {
            operation_id: operation_id.clone(),
            owner,
            command,
            reply: reply_tx,
        })
        .await
        .map_err(|_| HubCommandError::AgentOffline)?;
        Ok(HubPendingCommand {
            operation_id,
            reply: reply_rx,
        })
    }

    pub async fn start_process(
        &self,
        request: ProcessRequest,
    ) -> Result<HubPendingProcess, HubCommandError> {
        let pending = self
            .start_command(DeviceCommand::ExecuteProcess { request })
            .await?;
        Ok(HubPendingProcess {
            operation_id: pending.operation_id.clone(),
            pending,
        })
    }

    pub async fn execute_process(
        &self,
        request: ProcessRequest,
    ) -> Result<HubProcessResult, HubCommandError> {
        self.start_process(request).await?.wait().await
    }

    pub async fn start_shell(
        &self,
        request: ShellRequest,
    ) -> Result<HubPendingShell, HubCommandError> {
        let pending = self.start_command(DeviceCommand::Shell { request }).await?;
        Ok(HubPendingShell {
            operation_id: pending.operation_id.clone(),
            pending,
        })
    }

    pub async fn execute_shell(
        &self,
        request: ShellRequest,
    ) -> Result<HubShellResult, HubCommandError> {
        self.start_shell(request).await?.wait().await
    }

    pub async fn read_file(
        &self,
        path: impl Into<String>,
    ) -> Result<(Vec<u8>, bool), HubCommandError> {
        let result = self
            .start_command(DeviceCommand::ReadFile { path: path.into() })
            .await?
            .wait()
            .await?;
        match result.result {
            DeviceResult::FileContents { bytes, truncated } => Ok((bytes, truncated)),
            _ => Err(HubCommandError::UnexpectedResult),
        }
    }

    pub async fn list_directory(
        &self,
        path: impl Into<String>,
    ) -> Result<(Vec<DirectoryEntry>, bool), HubCommandError> {
        let result = self
            .start_command(DeviceCommand::ListDirectory { path: path.into() })
            .await?
            .wait()
            .await?;
        match result.result {
            DeviceResult::DirectoryEntries { entries, truncated } => Ok((entries, truncated)),
            _ => Err(HubCommandError::UnexpectedResult),
        }
    }

    pub async fn cancel(
        &self,
        operation_id: impl Into<String>,
    ) -> Result<CancellationDisposition, HubCommandError> {
        self.cancel_as(OperationOwner::local_hub(), operation_id)
            .await
    }

    pub async fn cancel_as(
        &self,
        owner: OperationOwner,
        operation_id: impl Into<String>,
    ) -> Result<CancellationDisposition, HubCommandError> {
        let (reply_tx, reply_rx) = oneshot::channel();
        let tx = {
            let live = self.inner.live.lock().await;
            live.as_ref()
                .map(|session| session.command_tx.clone())
                .ok_or(HubCommandError::AgentOffline)?
        };
        tx.send(HubRequest::Cancel {
            operation_id: operation_id.into(),
            owner,
            reply: reply_tx,
        })
        .await
        .map_err(|_| HubCommandError::AgentOffline)?;
        reply_rx.await.map_err(|_| HubCommandError::SessionClosed)?
    }

    pub async fn resolve_indeterminate(
        &self,
        operation_id: &str,
        resolver: OperationOwner,
        decision: IndeterminateResolution,
        evidence: impl Into<String>,
    ) -> Result<ExecutionReceipt, HubCommandError> {
        let mut persistent = self.inner.persistent.lock().await;
        // Resolution is the one transition that re-opens a quarantined desktop.
        // Preserve the exact fail-closed state so a checkpoint failure cannot
        // leave memory reusable while durable state still says unresolved.
        let rollback = persistent.execution.snapshot_for_restart();
        let (_next, receipt) = persistent
            .execution
            .resolve_indeterminate(
                operation_id,
                resolver,
                decision,
                evidence,
                unix_time_ms().map_err(|_| HubCommandError::Rejected)?,
            )
            .map_err(command_error_from_execution)?;
        if persist_locked(&self.inner, &persistent).is_err() {
            persistent.execution = AuthoritativeOperationController::restore_after_restart(
                self.inner.config.admission_limits(),
                rollback,
            )
            .map_err(|_| HubCommandError::Rejected)?;
            return Err(HubCommandError::Rejected);
        }
        Ok(receipt)
    }

    pub async fn desktop_quarantine(&self) -> Option<DesktopQuarantine> {
        let persistent = self.inner.persistent.lock().await;
        persistent
            .execution
            .quarantine(&self.inner.device_id)
            .cloned()
    }

    pub async fn operation_receipt(&self, operation_id: &str) -> Option<ExecutionReceipt> {
        let persistent = self.inner.persistent.lock().await;
        persistent.execution.receipt(operation_id).cloned()
    }

    pub async fn resolution_records(&self) -> Vec<ResolutionRecord> {
        let persistent = self.inner.persistent.lock().await;
        persistent.execution.resolutions().to_vec()
    }
}

#[tonic::async_trait]
impl AgentControl for SingleDeviceHub {
    type OpenSessionStream = Pin<Box<dyn Stream<Item = Result<HubFrame, Status>> + Send + 'static>>;

    async fn open_session(
        &self,
        request: Request<Streaming<AgentFrame>>,
    ) -> Result<Response<Self::OpenSessionStream>, Status> {
        if !self.inner.session_rate.try_acquire() {
            crate::v2_observability::increment_counter(
                "cumg.v2.agent_session_rejected",
                &[KeyValue::new("reason", "rate_limit")],
            );
            return Err(Status::resource_exhausted(
                "Agent session start rate exceeded",
            ));
        }
        let permit = self
            .inner
            .session_slots
            .clone()
            .try_acquire_owned()
            .map_err(|_| {
                crate::v2_observability::increment_counter(
                    "cumg.v2.agent_session_rejected",
                    &[KeyValue::new("reason", "concurrency_limit")],
                );
                Status::resource_exhausted("Agent session concurrency exceeded")
            })?;
        crate::v2_observability::increment_counter("cumg.v2.agent_session_started", &[]);
        let (outbound_tx, outbound_rx) = mpsc::channel(SESSION_QUEUE_DEPTH);
        let service = self.clone();
        tokio::spawn(
            async move {
                let _permit = permit;
                if let Err(error) = service
                    .run_session(request.into_inner(), outbound_tx.clone())
                    .await
                {
                    tracing::warn!(event = "v2_hub_session_error", error = ?error, "V2 Hub Agent session ended with error");
                    let _ = outbound_tx.send(Err(error.grpc_status())).await;
                }
            }
            .instrument(tracing::info_span!("v2_agent_session")),
        );
        Ok(Response::new(Box::pin(ReceiverStream::new(outbound_rx))))
    }
}

fn persist_locked(
    inner: &HubInner,
    persistent: &PersistentHubState,
) -> Result<(), HubServiceError> {
    let state = HubPersistentState::capture(&persistent.registry, &persistent.execution);
    inner
        .checkpoint
        .save(&state)
        .map_err(HubServiceError::Persistence)?;
    Ok(())
}

async fn next_agent(inbound: &mut Streaming<AgentFrame>) -> Result<AgentToHub, HubServiceError> {
    let frame = inbound
        .message()
        .await
        .map_err(HubServiceError::Status)?
        .ok_or(HubServiceError::InboundClosed)?;
    decode_agent_frame(frame).map_err(HubServiceError::Carrier)
}

async fn send_hub(
    sender: &mpsc::Sender<Result<HubFrame, Status>>,
    message: HubToAgent,
) -> Result<(), HubServiceError> {
    sender
        .send(Ok(
            encode_hub_frame(&message).map_err(HubServiceError::Carrier)?
        ))
        .await
        .map_err(|_| HubServiceError::OutboundClosed)
}

fn command_error_from_execution(error: crate::v2_m0_execution::ExecutionError) -> HubCommandError {
    use crate::v2_m0_execution::ExecutionError;
    match error {
        ExecutionError::OperationReplay => HubCommandError::OperationReplay,
        ExecutionError::BackpressureRejected | ExecutionError::AgentBusy => HubCommandError::Busy,
        ExecutionError::DeviceIndeterminate { operation_id } => {
            HubCommandError::DeviceIndeterminate { operation_id }
        }
        ExecutionError::UnknownOperation => HubCommandError::UnknownOperation,
        _ => HubCommandError::Rejected,
    }
}

fn unix_time_ms() -> Result<u64, HubServiceError> {
    Ok(u64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| HubServiceError::SystemClockBeforeEpoch)?
            .as_millis(),
    )
    .unwrap_or(u64::MAX))
}

fn random_operation_id() -> String {
    let mut random = [0_u8; 16];
    OsRng.fill_bytes(&mut random);
    let mut output = String::from("op_");
    for byte in random {
        use std::fmt::Write as _;
        let _ = write!(&mut output, "{byte:02x}");
    }
    output
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HubCommandError {
    AgentOffline,
    SessionSuperseded,
    SessionClosed,
    CancelledBeforeDispatch,
    OperationReplay,
    Busy,
    UnknownOperation,
    DeviceIndeterminate { operation_id: String },
    Rejected,
    Remote(DeviceErrorCode),
    UnexpectedResult,
    Indeterminate,
}

impl fmt::Display for HubCommandError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self:?}")
    }
}

impl std::error::Error for HubCommandError {}

#[derive(Debug)]
pub enum HubServiceError {
    InvalidConfig(&'static str),
    Persistence(PersistenceError),
    Control(crate::v2_m0::ControlError),
    Execution(crate::v2_m0_execution::ExecutionError),
    Transport(crate::v2_m0_transport::TransportError),
    Trust(crate::v2_m0_trust::TrustError),
    Carrier(crate::v2_m1_grpc::GrpcCarrierError),
    Status(Status),
    WrongDevice,
    StaleSession,
    InboundClosed,
    OutboundClosed,
    HeartbeatTimeout,
    UnexpectedMessage(String),
    UnexpectedResultType,
    PendingOperationMissing,
    CheckpointDeviceTrustMismatch,
    StateBusy,
    SystemClockBeforeEpoch,
}

impl From<crate::v2_m0::ControlError> for HubServiceError {
    fn from(error: crate::v2_m0::ControlError) -> Self {
        Self::Control(error)
    }
}

impl From<crate::v2_m0_execution::ExecutionError> for HubServiceError {
    fn from(error: crate::v2_m0_execution::ExecutionError) -> Self {
        Self::Execution(error)
    }
}

impl From<crate::v2_m0_transport::TransportError> for HubServiceError {
    fn from(error: crate::v2_m0_transport::TransportError) -> Self {
        Self::Transport(error)
    }
}

impl HubServiceError {
    fn grpc_status(&self) -> Status {
        match self {
            Self::HeartbeatTimeout
            | Self::InboundClosed
            | Self::OutboundClosed
            | Self::Status(_) => Status::unavailable("Agent session transport unavailable"),
            Self::WrongDevice | Self::CheckpointDeviceTrustMismatch => {
                Status::permission_denied("Agent identity rejected")
            }
            _ => Status::failed_precondition("Agent session rejected"),
        }
    }
}

impl fmt::Display for HubServiceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self:?}")
    }
}

impl std::error::Error for HubServiceError {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::v2_m0::DeviceIdentity;
    use crate::v2_m0_trust::build_device_key_rotation;

    fn test_state_dir(name: &str) -> std::path::PathBuf {
        let path = std::env::temp_dir().join(format!(
            "cumg-v2-hub-{name}-{}-{}",
            std::process::id(),
            rand::random::<u64>()
        ));
        std::fs::create_dir_all(&path).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        path
    }

    #[test]
    fn startup_preserves_stable_device_id_across_dual_signed_device_rotation() {
        let state_dir = test_state_dir("device-rotation");
        let config = HubServiceConfig {
            state_dir: state_dir.clone(),
            heartbeat_timeout: Duration::from_secs(5),
            max_queued_per_device: 1,
            max_agent_sessions: 2,
            max_agent_session_starts_per_minute: 30,
        };
        let old_device = DeviceIdentity::generate();
        let hub_identity = HubIdentity::generate();
        let grants = GrantAuthority::generate();
        let (first, _) = SingleDeviceHub::new(
            config.clone(),
            HubProvisionedMaterial {
                hub_identity: hub_identity.clone(),
                grant_authority: grants.clone(),
                device_verifier: old_device.verifying_key(),
                device_rotation: None,
            },
        )
        .unwrap();
        let stable_device_id = first.device_id().to_owned();
        drop(first);

        let new_device = DeviceIdentity::generate();
        let rotation =
            build_device_key_rotation(&stable_device_id, &old_device, &new_device, 1).unwrap();
        let (rotated, _) = SingleDeviceHub::new(
            config.clone(),
            HubProvisionedMaterial {
                hub_identity: hub_identity.clone(),
                grant_authority: grants.clone(),
                device_verifier: new_device.verifying_key(),
                device_rotation: Some(rotation),
            },
        )
        .unwrap();
        assert_eq!(rotated.device_id(), stable_device_id);
        drop(rotated);

        let (restarted, _) = SingleDeviceHub::new(
            config,
            HubProvisionedMaterial {
                hub_identity,
                grant_authority: grants,
                device_verifier: new_device.verifying_key(),
                device_rotation: None,
            },
        )
        .unwrap();
        assert_eq!(restarted.device_id(), stable_device_id);
        let _ = std::fs::remove_dir_all(state_dir);
    }
}
