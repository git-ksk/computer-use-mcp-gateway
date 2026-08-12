//! Operator-facing V2-M1 single-device Hub runtime.
//!
//! The Hub accepts one enrolled Agent identity over gRPC/TLS, keeps the V2
//! application messages independently signed, persists admission/generation
//! state before risky transitions, and exposes a small in-process handle that a
//! future authenticated northbound MCP layer can call without depending on gRPC.

use crate::v2_m0::{
    CONTROL_SCHEMA_VERSION, CommandEnvelope, DeviceCommand, DeviceErrorCode, DeviceRegistry,
    DeviceResult, DirectoryEntry, GrantAuthority, ProcessOutput, ProcessRequest,
    validate_command_result,
};
use crate::v2_m0_execution::{
    AdmissionDecision, AdmissionLimits, CancellationDecision, CompletionDecision,
    HubAdmissionController, HubOperationState, OperationRef,
};
use crate::v2_m0_transport::{
    AgentHello, AgentToHub, CancellationDisposition, HubChallenge, HubIdentity, HubToAgent,
    RemoteCancellationAck, RemoteResult, TrustedSessionClock, verify_agent_heartbeat,
    verify_agent_proof, verify_remote_cancellation_ack, verify_remote_result,
};
use crate::v2_m1_grpc::{
    decode_agent_frame, encode_hub_frame,
    proto::{AgentFrame, HubFrame, agent_control_server::AgentControl},
};
use crate::v2_m1_persistence::{CheckpointStore, HubPersistentState, PersistenceError};
use ed25519_dalek::VerifyingKey;
use rand::{RngCore, rngs::OsRng};
use std::collections::{HashMap, VecDeque};
use std::fmt;
use std::pin::Pin;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::sync::{Mutex, mpsc, oneshot, watch};
use tokio_stream::{Stream, wrappers::ReceiverStream};
use tonic::{Request, Response, Status, Streaming};

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
}

#[derive(Debug, Clone)]
pub struct HubServiceConfig {
    pub state_dir: std::path::PathBuf,
    pub heartbeat_timeout: Duration,
    pub max_queued_per_device: usize,
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
        Ok(())
    }
}

struct PersistentHubState {
    registry: DeviceRegistry,
    admission: HubAdmissionController,
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
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HubProcessResult {
    pub operation_id: String,
    pub output: ProcessOutput,
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
        command: DeviceCommand,
        reply: oneshot::Sender<Result<HubCommandResult, HubCommandError>>,
    },
    Cancel {
        operation_id: String,
        reply: oneshot::Sender<Result<CancellationDisposition, HubCommandError>>,
    },
}

struct PendingOperation {
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
        let device_id = identity_registry.provision_trusted_device(material.device_verifier);
        let (registry, admission) = match checkpoint.load_latest::<HubPersistentState>() {
            Ok(state) => {
                let (registry, admission) = state
                    .restore(config.admission_limits())
                    .map_err(HubServiceError::Persistence)?;
                if registry
                    .device_verifier(&device_id)
                    .map_err(HubServiceError::Control)?
                    != material.device_verifier
                {
                    return Err(HubServiceError::CheckpointDeviceTrustMismatch);
                }
                (registry, admission)
            }
            Err(PersistenceError::NoCheckpoint) => (
                identity_registry,
                HubAdmissionController::new(config.admission_limits())
                    .map_err(HubServiceError::Execution)?,
            ),
            Err(error) => return Err(HubServiceError::Persistence(error)),
        };

        let inner = Arc::new(HubInner {
            config,
            material,
            device_id,
            checkpoint,
            persistent: Mutex::new(PersistentHubState {
                registry,
                admission,
            }),
            live: Mutex::new(None),
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
                .admission
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
                        HubRequest::Execute { operation_id, command, reply } => {
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
                                match persistent.admission.admit(operation) {
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
                        HubRequest::Cancel { operation_id, reply } => {
                            let decision = {
                                let mut persistent = self.inner.persistent.lock().await;
                                match persistent.admission.cancel(&operation_id) {
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
            persistent.admission.mark_dispatched(operation_id)?;
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
        let device_result = result.result.result.clone();
        let cancelled = matches!(
            &device_result,
            DeviceResult::Process { output } if output.cancelled
        );
        let next = {
            let mut persistent = self.inner.persistent.lock().await;
            let next = persistent
                .admission
                .complete(&operation_id, cancelled)
                .map_err(HubServiceError::Execution)?;
            persist_locked(&self.inner, &persistent)?;
            next
        };
        if let Some(operation) = pending.remove(&operation_id) {
            let response = match device_result {
                DeviceResult::Error { code } => Err(HubCommandError::Remote(code)),
                result => Ok(HubCommandResult {
                    operation_id: operation_id.clone(),
                    result,
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
        let operation_ids: Vec<_> = persistent
            .admission
            .snapshot_for_restart()
            .operations
            .into_iter()
            .filter(|operation| operation.operation.device_generation == generation)
            .map(|operation| operation.operation.operation_id)
            .collect();
        for operation_id in operation_ids {
            match persistent.admission.state(&operation_id) {
                Some(HubOperationState::Dispatched | HubOperationState::CancelRequested) => {
                    let _ = persistent.admission.mark_connection_lost(&operation_id);
                }
                Some(HubOperationState::Queued | HubOperationState::ActiveNotDispatched) => {
                    let _ = persistent.admission.cancel(&operation_id);
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

    pub async fn start_command(
        &self,
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
        let (reply_tx, reply_rx) = oneshot::channel();
        let tx = {
            let live = self.inner.live.lock().await;
            live.as_ref()
                .map(|session| session.command_tx.clone())
                .ok_or(HubCommandError::AgentOffline)?
        };
        tx.send(HubRequest::Cancel {
            operation_id: operation_id.into(),
            reply: reply_tx,
        })
        .await
        .map_err(|_| HubCommandError::AgentOffline)?;
        reply_rx.await.map_err(|_| HubCommandError::SessionClosed)?
    }
}

#[tonic::async_trait]
impl AgentControl for SingleDeviceHub {
    type OpenSessionStream = Pin<Box<dyn Stream<Item = Result<HubFrame, Status>> + Send + 'static>>;

    async fn open_session(
        &self,
        request: Request<Streaming<AgentFrame>>,
    ) -> Result<Response<Self::OpenSessionStream>, Status> {
        let (outbound_tx, outbound_rx) = mpsc::channel(SESSION_QUEUE_DEPTH);
        let service = self.clone();
        tokio::spawn(async move {
            if let Err(error) = service
                .run_session(request.into_inner(), outbound_tx.clone())
                .await
            {
                tracing::warn!(event = "v2_hub_session_error", error = ?error, "V2 Hub Agent session ended with error");
                let _ = outbound_tx.send(Err(error.grpc_status())).await;
            }
        });
        Ok(Response::new(Box::pin(ReceiverStream::new(outbound_rx))))
    }
}

fn persist_locked(
    inner: &HubInner,
    persistent: &PersistentHubState,
) -> Result<(), HubServiceError> {
    let state = HubPersistentState::capture(&persistent.registry, &persistent.admission);
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
