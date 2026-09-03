//! Operator-facing V2-M1 single-device Hub runtime.
//!
//! The Hub accepts one enrolled Agent identity over gRPC/TLS, keeps the V2
//! application messages independently signed, persists admission/generation
//! state before risky transitions, and exposes a small in-process handle that a
//! future authenticated northbound MCP layer can call without depending on gRPC.

use crate::v2_execution_safety::{
    AuthoritativeOperationController, DesktopQuarantine, ExecutionReceipt, IndeterminateReason,
    OperationAdmissionMetadata, OperationDispatchBinding, OperationExecutionLane, OperationOwner,
    OperationRecoverySnapshot, ReconciliationStatus, RecoverableOperationResult, ResolutionRecord,
    RetirementRecord, terminal_evidence_for_device_result,
};
use crate::v2_grant_signer::{GrantSignerError, HubGrantSigner};
use crate::v2_m0::{
    CONTROL_SCHEMA_VERSION, CapabilityAdvertisement, CommandEnvelope, DeviceCapability,
    DeviceCommand, DeviceErrorCode, DeviceRegistry, DeviceResult, DirectoryEntry, ProcessOutput,
    ProcessRequest, ShellRequest, validate_command_result,
};
use crate::v2_m0_execution::{
    AdmissionDecision, AdmissionLimits, CancellationDecision, CompletionDecision,
    HubOperationState, IndeterminateResolution, OperationRef,
};
use crate::v2_m0_transport::{
    AgentHello, AgentToHub, CancellationDisposition, HubChallenge, HubIdentity, HubToAgent,
    RemoteCancellationAck, RemoteHandoffAuthority, RemoteHandoffRequestKind,
    RemoteHandoffResponseKind, RemoteReconciliationReport, RemoteResult, TrustedSessionClock,
    verify_agent_heartbeat, verify_agent_proof, verify_remote_backend_session_ended,
    verify_remote_cancellation_ack, verify_remote_handoff_response,
    verify_remote_reconciliation_report, verify_remote_result,
};
use crate::v2_m0_trust::{DeviceKeyRotation, apply_device_key_rotation};
use crate::v2_m1_grpc::{
    decode_agent_frame, encode_hub_frame,
    proto::{AgentFrame, HubFrame, agent_control_server::AgentControl},
};
use crate::v2_m1_persistence::{
    CheckpointStore, HubPersistentState, MAX_CHECKPOINT_BYTES, PersistenceError,
};
use crate::v2_observability::SafeErrorCode;
use crate::v2_online_recovery::{
    RecoveryAuditAssessment, RecoveryAuthorization, RecoveryChallenge, RecoveryDecision,
    RecoveryError, RecoveryResolved, RecoveryVerifier, build_recovery_challenge,
    build_recovery_resolved, quarantine_fingerprint, recovery_decision_name,
};
use crate::v2_state_lock::{StateDirectoryLock, StateDirectoryLockError};
use ed25519_dalek::VerifyingKey;
use rand::{RngCore, rngs::OsRng};
use std::collections::{HashMap, VecDeque};
use std::fmt;
use std::pin::Pin;
use std::sync::{
    Arc, OnceLock,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::sync::{Mutex, Semaphore, mpsc, oneshot, watch};
use tokio_stream::{Stream, wrappers::ReceiverStream};
use tonic::{Request, Response, Status, Streaming};
use tracing::Instrument as _;

const SESSION_QUEUE_DEPTH: usize = 16;
const DEFAULT_GRANT_TTL_MS: u64 = 30_000;
// Same-generation terminal receipts remain durable for retry/audit semantics, but
// an indefinitely long Agent session would otherwise grow the Hub checkpoint up
// to the hard 1 MiB persistence ceiling. Request a clean authenticated generation
// rollover at half that ceiling, leaving ample headroom for the bounded queue and
// one final in-flight settlement before the existing generation-prune hook runs.
pub const DEFAULT_CHECKPOINT_GENERATION_ROLLOVER_BYTES: usize = (MAX_CHECKPOINT_BYTES as usize) / 2;
// Hub and Agent start their monotonic session clocks on opposite sides of the
// SessionAccepted network hop. Backdating only `issued_at` shortens the grant's
// effective remaining life and avoids treating ordinary transport latency as a
// future-dated grant.
const GRANT_ISSUED_AT_SAFETY_MS: u64 = 5_000;

#[derive(Clone)]
pub struct HubProvisionedMaterial {
    pub hub_identity: HubIdentity,
    pub grant_signer: HubGrantSigner,
    pub device_verifier: VerifyingKey,
    pub device_rotation: Option<DeviceKeyRotation>,
}

#[derive(Debug, Clone)]
pub struct HubServiceConfig {
    pub state_dir: std::path::PathBuf,
    pub heartbeat_timeout: Duration,
    pub max_agent_session_lifetime: Duration,
    pub agent_session_reauth_drain: Duration,
    pub checkpoint_generation_rollover_bytes: usize,
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
        if self.max_agent_session_lifetime.is_zero()
            || self.agent_session_reauth_drain.is_zero()
            || self.agent_session_reauth_drain >= self.max_agent_session_lifetime
        {
            return Err(HubServiceError::InvalidConfig(
                "Agent session lifetime must be non-zero and greater than its reauthentication drain window",
            ));
        }
        if self.checkpoint_generation_rollover_bytes == 0
            || self.checkpoint_generation_rollover_bytes
                > DEFAULT_CHECKPOINT_GENERATION_ROLLOVER_BYTES
        {
            return Err(HubServiceError::InvalidConfig(
                "checkpoint generation rollover bytes must be between 1 and half the checkpoint ceiling",
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
    capability_revision: u64,
    command_tx: mpsc::Sender<HubRequest>,
    supersede: watch::Sender<bool>,
}

#[derive(Default)]
struct RecoveryRuntimeState {
    pending: Option<RecoveryChallenge>,
    last_resolved: Option<(RecoveryAuthorization, RecoveryResolved)>,
}

struct HubInner {
    config: HubServiceConfig,
    material: HubProvisionedMaterial,
    device_id: String,
    checkpoint: CheckpointStore,
    _state_lock: StateDirectoryLock,
    persistent: Mutex<PersistentHubState>,
    live: Mutex<Option<LiveSession>>,
    draining: AtomicBool,
    semantic_constraint_snapshot: OnceLock<SemanticConstraintSnapshotIdentity>,
    last_checkpoint_bytes: AtomicUsize,
    session_slots: Arc<Semaphore>,
    session_rate: crate::v2_limits::SlidingWindowRateLimit,
    recovery_verifier: Option<RecoveryVerifier>,
    recovery_runtime: Mutex<RecoveryRuntimeState>,
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
struct SemanticConstraintSnapshotIdentity {
    revision: u64,
    digest: String,
}

impl SemanticConstraintSnapshotIdentity {
    fn from_evidence(
        evidence: &crate::v2_execution_safety::SemanticConstraintAdmissionEvidence,
    ) -> Self {
        Self {
            revision: evidence.revision,
            digest: evidence.snapshot_digest.clone(),
        }
    }
}

fn semantic_constraint_snapshot_is_current(
    inner: &HubInner,
    expected: Option<&SemanticConstraintSnapshotIdentity>,
) -> bool {
    expected.is_none_or(|expected| inner.semantic_constraint_snapshot.get() == Some(expected))
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
        command: Box<DeviceCommand>,
        metadata: Box<OperationAdmissionMetadata>,
        handoff: Option<RemoteHandoffAuthority>,
        reply: oneshot::Sender<Result<HubCommandResult, HubCommandError>>,
    },
    Handoff {
        request_id: String,
        request: RemoteHandoffRequestKind,
        reply: oneshot::Sender<Result<RemoteHandoffResponseKind, HubCommandError>>,
    },
    Cancel {
        operation_id: String,
        owner: OperationOwner,
        reply: oneshot::Sender<Result<CancellationDisposition, HubCommandError>>,
    },
    EndBackendSession {
        context_id: String,
        reply: oneshot::Sender<Result<bool, HubCommandError>>,
    },
}

struct PendingOperation {
    owner: OperationOwner,
    command: DeviceCommand,
    expected_semantic_constraint_snapshot: Option<SemanticConstraintSnapshotIdentity>,
    handoff: Option<RemoteHandoffAuthority>,
    envelope: Option<CommandEnvelope>,
    reply: oneshot::Sender<Result<HubCommandResult, HubCommandError>>,
}

enum DispatchOutcome {
    Sent,
    Rejected(CompletionDecision),
}

enum CommandSessionFence {
    None,
    Session {
        generation: u64,
        capability_revision: u64,
    },
    Handoff(RemoteHandoffAuthority),
}

impl CommandSessionFence {
    fn expected_session(&self) -> Option<(u64, u64)> {
        match self {
            Self::None => None,
            Self::Session {
                generation,
                capability_revision,
            } => Some((*generation, *capability_revision)),
            Self::Handoff(authority) => Some((authority.generation, authority.capability_revision)),
        }
    }

    fn handoff(self) -> Option<RemoteHandoffAuthority> {
        match self {
            Self::Handoff(authority) => Some(authority),
            Self::None | Self::Session { .. } => None,
        }
    }
}

impl SingleDeviceHub {
    pub fn new(
        config: HubServiceConfig,
        material: HubProvisionedMaterial,
    ) -> Result<(Self, HubHandle), HubServiceError> {
        config.validate()?;
        let state_lock = StateDirectoryLock::acquire(&config.state_dir)
            .map_err(HubServiceError::StateDirectoryLock)?;
        let checkpoint = CheckpointStore::new(config.state_dir.clone(), "hub")
            .map_err(HubServiceError::Persistence)?;
        let recovery_verifier = RecoveryVerifier::load_optional(&config.state_dir)
            .map_err(HubServiceError::OnlineRecovery)?;

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
            _state_lock: state_lock,
            persistent: Mutex::new(PersistentHubState {
                registry,
                execution,
            }),
            live: Mutex::new(None),
            draining: AtomicBool::new(false),
            semantic_constraint_snapshot: OnceLock::new(),
            last_checkpoint_bytes: AtomicUsize::new(0),
            session_slots,
            session_rate,
            recovery_verifier,
            recovery_runtime: Mutex::new(RecoveryRuntimeState::default()),
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
            other => return Err(unexpected_agent_message("hello", &other)),
        };
        if self.inner.draining.load(Ordering::Acquire) {
            tracing::info!(
                event = "v2_agent_session_rejected",
                device_id = %self.inner.device_id,
                outcome = "rejected",
                error_code = "state_busy",
                "Agent session rejected because Hub shutdown drain is active"
            );
            return Err(HubServiceError::StateBusy);
        }
        if hello.device_id != self.inner.device_id {
            crate::v2_observability::agent_session_rejected(
                crate::v2_observability::SessionRejectReason::WrongDevice,
            );
            tracing::warn!(
                event = "v2_agent_session_rejected",
                device_id = %self.inner.device_id,
                outcome = "rejected",
                error_code = "wrong_device",
                "Agent session identity rejected"
            );
            return Err(HubServiceError::WrongDevice);
        }
        tracing::info!(
            event = "v2_agent_session_start",
            device_id = %self.inner.device_id,
            backend = %hello.capabilities.backend,
            outcome = "started",
            "Agent session handshake started"
        );
        let challenge = self.inner.material.hub_identity.challenge(&hello)?;
        send_hub(&outbound, HubToAgent::Challenge(challenge.clone())).await?;

        let proof = match next_agent(&mut inbound).await? {
            AgentToHub::Proof(proof) => proof,
            other => return Err(unexpected_agent_message("proof", &other)),
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
        tracing::info!(
            event = "v2_agent_session_accepted",
            device_id = %self.inner.device_id,
            generation = session.generation,
            backend = %hello.capabilities.backend,
            outcome = "accepted",
            "Agent session accepted"
        );
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
                capability_revision: session.capabilities.revision,
                command_tx,
                supersede: supersede_tx,
            })
        };
        if let Some(prior) = prior {
            tracing::info!(
                event = "v2_agent_session_superseded",
                device_id = %self.inner.device_id,
                generation = prior.generation,
                outcome = "superseded",
                "older Agent session superseded by a newer generation"
            );
            let _ = prior.supersede.send(true);
        }

        self.maybe_send_recovery_challenge(&outbound, session.generation, &session_clock)
            .await?;

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
        let mut backend_session_end_waiters: HashMap<
            String,
            oneshot::Sender<Result<bool, HubCommandError>>,
        > = HashMap::new();
        let mut handoff_waiters: HashMap<
            String,
            oneshot::Sender<Result<RemoteHandoffResponseKind, HubCommandError>>,
        > = HashMap::new();
        let heartbeat_deadline = tokio::time::sleep(self.inner.config.heartbeat_timeout);
        tokio::pin!(heartbeat_deadline);
        let reauth_deadline = tokio::time::sleep(
            self.inner.config.max_agent_session_lifetime
                - self.inner.config.agent_session_reauth_drain,
        );
        tokio::pin!(reauth_deadline);
        let session_hard_deadline =
            tokio::time::sleep(self.inner.config.max_agent_session_lifetime);
        tokio::pin!(session_hard_deadline);
        let mut session_reauth_requested = false;
        let mut generation_rollover_requested = false;

        loop {
            if !generation_rollover_requested
                && self.inner.last_checkpoint_bytes.load(Ordering::Acquire)
                    >= self.inner.config.checkpoint_generation_rollover_bytes
            {
                generation_rollover_requested = true;
                tracing::info!(
                    event = "v2_checkpoint_generation_rollover_requested",
                    device_id = %self.inner.device_id,
                    generation,
                    checkpoint_bytes = self.inner.last_checkpoint_bytes.load(Ordering::Acquire),
                    rollover_bytes = self.inner.config.checkpoint_generation_rollover_bytes,
                    outcome = "drain_existing",
                    "Hub checkpoint reached the generation rollover high-water mark; new operation admission is paused"
                );
            }
            if session_reauth_requested
                && pending.is_empty()
                && cancel_waiters.is_empty()
                && backend_session_end_waiters.is_empty()
                && handoff_waiters.is_empty()
            {
                tracing::info!(
                    event = "v2_agent_session_reauth",
                    device_id = %self.inner.device_id,
                    generation,
                    max_session_lifetime_secs = self.inner.config.max_agent_session_lifetime.as_secs(),
                    outcome = "reconnect",
                    "Agent session reauthentication drain completed; closing transport for a fresh handshake"
                );
                return Ok(());
            }
            if generation_rollover_requested && pending.is_empty() {
                tracing::info!(
                    event = "v2_checkpoint_generation_rollover",
                    device_id = %self.inner.device_id,
                    generation,
                    checkpoint_bytes = self.inner.last_checkpoint_bytes.load(Ordering::Acquire),
                    outcome = "reconnect",
                    "Hub checkpoint high-water drain completed; closing Agent session for a fresh authenticated generation"
                );
                return Ok(());
            }

            tokio::select! {
                _ = &mut reauth_deadline, if !session_reauth_requested => {
                    session_reauth_requested = true;
                    crate::v2_observability::agent_session_reauth_requested();
                    tracing::warn!(
                        event = "v2_agent_session_reauth_requested",
                        device_id = %self.inner.device_id,
                        generation,
                        max_session_lifetime_secs = self.inner.config.max_agent_session_lifetime.as_secs(),
                        drain_window_secs = self.inner.config.agent_session_reauth_drain.as_secs(),
                        outcome = "drain_existing",
                        "Agent session is approaching its maximum lifetime; new operation admission is paused until a fresh handshake"
                    );
                }
                _ = &mut session_hard_deadline => {
                    crate::v2_observability::agent_session_lifetime_exceeded();
                    tracing::error!(
                        event = "v2_agent_session_lifetime_exceeded",
                        device_id = %self.inner.device_id,
                        generation,
                        max_session_lifetime_secs = self.inner.config.max_agent_session_lifetime.as_secs(),
                        pending_operations = pending.len(),
                        outcome = "terminated_fail_closed",
                        error_code = "session_lifetime_exceeded",
                        "Agent session reached its hard maximum lifetime; transport is being terminated"
                    );
                    return Ok(());
                }
                _ = &mut heartbeat_deadline => {
                    tracing::warn!(
                        event = "v2_hub_heartbeat_timeout",
                        device_id = %self.inner.device_id,
                        generation,
                        outcome = "reconnect",
                        error_code = "heartbeat_timeout",
                        "Agent heartbeat deadline expired"
                    );
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
                        HubRequest::Execute {
                            operation_id,
                            owner,
                            command,
                            metadata,
                            handoff,
                            reply,
                        } => {
                            if self.inner.draining.load(Ordering::Acquire) {
                                tracing::info!(
                                    event = "v2_operation_rejected",
                                    operation_id = %operation_id,
                                    device_id = %self.inner.device_id,
                                    generation,
                                    capability = crate::v2_observability::capability_name(command.capability()),
                                    outcome = "draining",
                                    error_code = "hub_draining",
                                    "operation rejected because Hub shutdown drain has started"
                                );
                                let _ = reply.send(Err(HubCommandError::Busy));
                                continue;
                            }
                            if session_reauth_requested {
                                tracing::info!(
                                    event = "v2_operation_rejected",
                                    operation_id = %operation_id,
                                    device_id = %self.inner.device_id,
                                    generation,
                                    capability = crate::v2_observability::capability_name(command.capability()),
                                    outcome = "session_reauthentication",
                                    error_code = "state_busy",
                                    "operation rejected while Hub drains the current Agent session for reauthentication"
                                );
                                let _ = reply.send(Err(HubCommandError::Busy));
                                continue;
                            }
                            if generation_rollover_requested {
                                tracing::info!(
                                    event = "v2_operation_rejected",
                                    operation_id = %operation_id,
                                    device_id = %self.inner.device_id,
                                    generation,
                                    capability = crate::v2_observability::capability_name(command.capability()),
                                    outcome = "generation_rollover",
                                    error_code = "state_busy",
                                    "operation rejected while Hub drains the current generation for checkpoint compaction"
                                );
                                let _ = reply.send(Err(HubCommandError::Busy));
                                continue;
                            }
                            if pending.contains_key(&operation_id) {
                                let _ = reply.send(Err(HubCommandError::OperationReplay));
                                continue;
                            }
                            let operation = OperationRef {
                                device_id: self.inner.device_id.clone(),
                                device_generation: generation,
                                operation_id: operation_id.clone(),
                            };
                            let expected_semantic_constraint_snapshot = metadata
                                .semantic_constraint
                                .as_ref()
                                .map(SemanticConstraintSnapshotIdentity::from_evidence);
                            let decision = {
                                let mut persistent = self.inner.persistent.lock().await;
                                match persistent.execution.prepare_with_metadata(
                                    operation,
                                    owner.clone(),
                                    command.capability(),
                                    *metadata,
                                    unix_time_ms()?,
                                ) {
                                    Ok(decision) => {
                                        let recovery_evidence_read = persistent
                                            .execution
                                            .is_recovery_evidence_read(&operation_id);
                                        persist_locked(&self.inner, &persistent)?;
                                        Ok((decision, recovery_evidence_read))
                                    }
                                    Err(error) => Err(command_error_from_execution(error)),
                                }
                            };
                            let (decision, recovery_evidence_read) = match decision {
                                Ok(decision) => decision,
                                Err(error) => {
                                    tracing::warn!(
                                        event = "v2_operation_rejected",
                                        operation_id = %operation_id,
                                        device_id = %self.inner.device_id,
                                        generation,
                                        capability = crate::v2_observability::capability_name(command.capability()),
                                        outcome = "rejected",
                                        error_code = error.safe_error_code(),
                                        "operation admission rejected"
                                    );
                                    let _ = reply.send(Err(error));
                                    continue;
                                }
                            };
                            let admission_outcome = match &decision {
                                AdmissionDecision::StartNow(_) => "start_now",
                                AdmissionDecision::Queued { .. } => "queued",
                            };
                            tracing::info!(
                                event = "v2_operation_admitted",
                                operation_id = %operation_id,
                                device_id = %self.inner.device_id,
                                generation,
                                capability = crate::v2_observability::capability_name(command.capability()),
                                outcome = admission_outcome,
                                execution_lane = if recovery_evidence_read {
                                    "recovery_evidence_read"
                                } else {
                                    "normal"
                                },
                                "operation admitted"
                            );
                            pending.insert(operation_id.clone(), PendingOperation {
                                owner,
                                command: *command,
                                expected_semantic_constraint_snapshot,
                                handoff,
                                envelope: None,
                                reply,
                            });
                            match decision {
                                AdmissionDecision::StartNow(operation) => {
                                    if let DispatchOutcome::Rejected(next) = self.dispatch_operation(
                                        &outbound,
                                        &hello,
                                        &challenge,
                                        generation,
                                        capability_revision,
                                        session_clock,
                                        &operation.operation_id,
                                        &mut pending,
                                    ).await? {
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
                                }
                                AdmissionDecision::Queued { .. } => queue_order.push_back(operation_id),
                            }
                        }
                        HubRequest::Handoff { request_id, request, reply } => {
                            if handoff_waiters.contains_key(&request_id) {
                                let _ = reply.send(Err(HubCommandError::Rejected));
                                continue;
                            }
                            let remote = self.inner.material.hub_identity.remote_handoff_request(
                                &hello,
                                &challenge,
                                self.inner.device_id.clone(),
                                generation,
                                request_id.clone(),
                                request,
                            )?;
                            handoff_waiters.insert(request_id, reply);
                            send_hub(&outbound, HubToAgent::HandoffRequest(remote)).await?;
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
                                    tracing::info!(
                                        event = "v2_cancellation_requested",
                                        operation_id = %operation_id,
                                        device_id = %self.inner.device_id,
                                        generation,
                                        outcome = "cancelled_before_dispatch",
                                        "operation cancelled before dispatch"
                                    );
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
                                    tracing::info!(
                                        event = "v2_cancellation_requested",
                                        operation_id = %operation.operation_id,
                                        device_id = %self.inner.device_id,
                                        generation,
                                        outcome = "propagated",
                                        "cancellation requested for dispatched operation"
                                    );
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
                        HubRequest::EndBackendSession { context_id, reply } => {
                            if backend_session_end_waiters.contains_key(&context_id) {
                                let _ = reply.send(Err(HubCommandError::Rejected));
                                continue;
                            }
                            let remote = self.inner.material.hub_identity.backend_session_end(
                                &hello,
                                &challenge,
                                self.inner.device_id.clone(),
                                generation,
                                context_id.clone(),
                            )?;
                            backend_session_end_waiters.insert(context_id, reply);
                            send_hub(&outbound, HubToAgent::BackendSessionEnd(remote)).await?;
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
                            self.maybe_send_recovery_challenge(
                                &outbound,
                                generation,
                                session_clock,
                            )
                            .await?;
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
                        AgentToHub::ReconciliationReport(report) => {
                            self.handle_reconciliation_report(
                                report,
                                &hello,
                                &challenge,
                                generation,
                            )
                            .await?;
                        }
                        AgentToHub::BackendSessionEnded(ack) => {
                            {
                                let persistent = self.inner.persistent.lock().await;
                                verify_remote_backend_session_ended(
                                    &persistent.registry,
                                    &hello,
                                    &challenge,
                                    &ack,
                                )?;
                            }
                            if ack.device_generation != generation {
                                return Err(HubServiceError::StaleSession);
                            }
                            let Some(reply) = backend_session_end_waiters.remove(&ack.context_id) else {
                                return Err(HubServiceError::UnexpectedMessage {
                                    expected: "backend_session_end_ack",
                                    got: "unmatched_backend_session_end_ack",
                                });
                            };
                            let _ = reply.send(Ok(ack.ended));
                        }
                        AgentToHub::RecoveryAuthorization(authorization) => {
                            self.handle_recovery_authorization(
                                authorization,
                                &outbound,
                                generation,
                                session_clock,
                            )
                            .await?;
                        }
                        AgentToHub::HandoffResponse(response) => {
                            {
                                let persistent = self.inner.persistent.lock().await;
                                verify_remote_handoff_response(
                                    &persistent.registry,
                                    &hello,
                                    &challenge,
                                    &response,
                                )?;
                            }
                            if response.device_generation != generation {
                                return Err(HubServiceError::StaleSession);
                            }
                            let Some(reply) = handoff_waiters.remove(&response.request_id) else {
                                return Err(HubServiceError::UnexpectedMessage {
                                    expected: "handoff_response",
                                    got: "unmatched_handoff_response",
                                });
                            };
                            let _ = reply.send(Ok(response.response));
                        }
                        AgentToHub::CancellationAck(ack) => {
                            self.handle_cancellation_ack(
                                ack,
                                &outbound,
                                &hello,
                                &challenge,
                                generation,
                                session_clock,
                                &mut pending,
                                &mut queue_order,
                                &mut cancel_waiters,
                            ).await?;
                        }
                        other => return Err(unexpected_agent_message("session_message", &other)),
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
    ) -> Result<DispatchOutcome, HubServiceError> {
        let (owner, device_command, expected_semantic_constraint_snapshot, handoff) = {
            let operation = pending
                .get(operation_id)
                .ok_or(HubServiceError::PendingOperationMissing)?;
            (
                operation.owner.clone(),
                operation.command.clone(),
                operation.expected_semantic_constraint_snapshot.clone(),
                operation.handoff.clone(),
            )
        };
        if self.inner.draining.load(Ordering::Acquire) {
            return self
                .cancel_for_shutdown_drain(operation_id, &owner, generation, pending)
                .await;
        }

        let command = CommandEnvelope {
            schema_version: CONTROL_SCHEMA_VERSION,
            device_id: self.inner.device_id.clone(),
            device_generation: generation,
            capability_revision,
            operation_id: operation_id.to_owned(),
            command: device_command,
        };
        let grant = match self
            .inner
            .material
            .grant_signer
            .issue_for_device_capability(
                &self.inner.device_id,
                command.command.capability(),
                session_clock
                    .now_ms()
                    .saturating_sub(GRANT_ISSUED_AT_SAFETY_MS),
                DEFAULT_GRANT_TTL_MS,
            )
            .await
        {
            Ok(grant) => grant,
            Err(error) => {
                return self
                    .reject_for_grant_signer(operation_id, &owner, generation, pending, &error)
                    .await;
            }
        };
        let dispatch_binding = if command.command.is_read_only() {
            None
        } else {
            Some(OperationDispatchBinding::new(
                capability_revision,
                grant.payload.grant_id.clone(),
            )?)
        };
        let remote = self
            .inner
            .material
            .hub_identity
            .remote_command_with_handoff(hello, challenge, command.clone(), grant, handoff)?;

        // Re-check the drain gate immediately before the durable side-effect boundary.
        if self.inner.draining.load(Ordering::Acquire) {
            return self
                .cancel_for_shutdown_drain(operation_id, &owner, generation, pending)
                .await;
        }

        if !semantic_constraint_snapshot_is_current(
            &self.inner,
            expected_semantic_constraint_snapshot.as_ref(),
        ) {
            return self
                .reject_for_semantic_constraint_snapshot(operation_id, &owner, generation, pending)
                .await;
        }

        // Persist `Dispatched` before putting bytes on the network. A crash after
        // this point can conservatively restore as indeterminate, but can never
        // restore a command that may have executed as runnable.
        {
            let mut persistent = self.inner.persistent.lock().await;
            persistent.execution.mark_dispatched_with_binding(
                operation_id,
                &owner,
                generation,
                dispatch_binding,
                unix_time_ms()?,
            )?;
            persist_locked(&self.inner, &persistent)?;
        }
        let capability = command.command.capability();
        pending
            .get_mut(operation_id)
            .ok_or(HubServiceError::PendingOperationMissing)?
            .envelope = Some(command);
        send_hub(outbound, HubToAgent::Command(remote)).await?;
        tracing::info!(
            event = "v2_operation_dispatched",
            operation_id,
            device_id = %self.inner.device_id,
            generation,
            capability = crate::v2_observability::capability_name(capability),
            outcome = "dispatched",
            "operation dispatched to Agent"
        );
        Ok(DispatchOutcome::Sent)
    }

    async fn reject_for_semantic_constraint_snapshot(
        &self,
        operation_id: &str,
        owner: &OperationOwner,
        generation: u64,
        pending: &mut HashMap<String, PendingOperation>,
    ) -> Result<DispatchOutcome, HubServiceError> {
        let next = {
            let mut persistent = self.inner.persistent.lock().await;
            let decision = persistent.execution.request_cancel(
                operation_id,
                owner,
                generation,
                unix_time_ms()?,
            )?;
            persist_locked(&self.inner, &persistent)?;
            match decision {
                CancellationDecision::CancelledBeforeDispatch { next } => next,
                CancellationDecision::AlreadyTerminal(_) => CompletionDecision::Idle,
                CancellationDecision::SendCancellation(_) => {
                    return Err(HubServiceError::StateBusy);
                }
            }
        };
        if let Some(operation) = pending.remove(operation_id) {
            let _ = operation
                .reply
                .send(Err(HubCommandError::SemanticConstraintSnapshotStale));
        }
        tracing::warn!(
            event = "v2_semantic_constraint_snapshot_stale",
            operation_id,
            device_id = %self.inner.device_id,
            generation,
            outcome = "cancelled_before_dispatch",
            error_code = "semantic_constraint_snapshot_stale",
            "semantic constraint snapshot changed before provider dispatch"
        );
        Ok(DispatchOutcome::Rejected(next))
    }

    async fn reject_for_grant_signer(
        &self,
        operation_id: &str,
        owner: &OperationOwner,
        generation: u64,
        pending: &mut HashMap<String, PendingOperation>,
        error: &GrantSignerError,
    ) -> Result<DispatchOutcome, HubServiceError> {
        let next = {
            let mut persistent = self.inner.persistent.lock().await;
            let decision = persistent.execution.request_cancel(
                operation_id,
                owner,
                generation,
                unix_time_ms()?,
            )?;
            persist_locked(&self.inner, &persistent)?;
            match decision {
                CancellationDecision::CancelledBeforeDispatch { next } => next,
                CancellationDecision::AlreadyTerminal(_) => CompletionDecision::Idle,
                CancellationDecision::SendCancellation(_) => {
                    return Err(HubServiceError::StateBusy);
                }
            }
        };
        if let Some(operation) = pending.remove(operation_id) {
            let _ = operation
                .reply
                .send(Err(HubCommandError::GrantSigningUnavailable));
        }
        crate::v2_observability::grant_signing_failed();
        tracing::error!(
            event = "v2_grant_signing_failed",
            operation_id,
            device_id = %self.inner.device_id,
            generation,
            outcome = "cancelled_before_dispatch",
            error_code = error.safe_error_code(),
            "grant signer did not authorize a token; operation cancelled before Agent dispatch"
        );
        Ok(DispatchOutcome::Rejected(next))
    }

    async fn cancel_for_shutdown_drain(
        &self,
        operation_id: &str,
        owner: &OperationOwner,
        generation: u64,
        pending: &mut HashMap<String, PendingOperation>,
    ) -> Result<DispatchOutcome, HubServiceError> {
        let next = {
            let mut persistent = self.inner.persistent.lock().await;
            let decision = persistent.execution.request_cancel(
                operation_id,
                owner,
                generation,
                unix_time_ms()?,
            )?;
            persist_locked(&self.inner, &persistent)?;
            match decision {
                CancellationDecision::CancelledBeforeDispatch { next } => next,
                CancellationDecision::AlreadyTerminal(_) => CompletionDecision::Idle,
                CancellationDecision::SendCancellation(_) => {
                    return Err(HubServiceError::StateBusy);
                }
            }
        };
        if let Some(operation) = pending.remove(operation_id) {
            let _ = operation
                .reply
                .send(Err(HubCommandError::CancelledBeforeDispatch));
        }
        tracing::info!(
            event = "v2_operation_rejected",
            operation_id,
            device_id = %self.inner.device_id,
            generation,
            outcome = "draining",
            error_code = "hub_draining",
            "undispatched operation cancelled during Hub shutdown drain"
        );
        Ok(DispatchOutcome::Rejected(next))
    }

    async fn handle_reconciliation_report(
        &self,
        report: RemoteReconciliationReport,
        hello: &AgentHello,
        challenge: &HubChallenge,
        generation: u64,
    ) -> Result<(), HubServiceError> {
        if report.reporting_generation != generation {
            return Err(HubServiceError::StaleSession);
        }
        let now_ms = unix_time_ms()?;
        let mut persistent = self.inner.persistent.lock().await;
        verify_remote_reconciliation_report(&persistent.registry, hello, challenge, &report)?;

        // Reconciliation is deliberately transactional with respect to durable Hub
        // state. The live authoritative controller is not changed until a complete
        // candidate state has been committed to disk.
        let mut candidate = persistent.execution.clone();
        let mut resolved = Vec::new();
        let mut operator_required = Vec::new();

        for evidence in &report.terminal_evidence {
            let operation_id = evidence.operation.operation_id.as_str();
            if candidate.state(operation_id) != Some(HubOperationState::Indeterminate) {
                // Old evidence can remain in the bounded Agent journal across later
                // reconnects. A terminal/unknown operation is therefore an
                // idempotent stale duplicate, never a reason to replay or mutate.
                continue;
            }

            if evidence.operation.device_id != self.inner.device_id
                || evidence.operation.device_generation >= generation
            {
                candidate.mark_reconciliation_operator_required(operation_id)?;
                operator_required.push(operation_id.to_owned());
                continue;
            }

            match candidate.reconcile_authoritative_terminal(evidence, now_ms) {
                Ok((_next, receipt)) => {
                    resolved.push((
                        operation_id.to_owned(),
                        evidence.capability,
                        receipt.terminal_state,
                    ));
                }
                Err(crate::v2_m0_execution::ExecutionError::OwnershipFenceMismatch)
                | Err(crate::v2_m0_execution::ExecutionError::InvalidOperation)
                | Err(crate::v2_m0_execution::ExecutionError::InvalidTransition) => {
                    // A signed Agent claim that does not exactly bind to the Hub's
                    // operation/device/generation/fence/capability record is not
                    // authoritative for this quarantine. Escalate; never retry.
                    candidate.mark_reconciliation_operator_required(operation_id)?;
                    operator_required.push(operation_id.to_owned());
                }
                Err(error) => return Err(HubServiceError::Execution(error)),
            }
        }

        // The report is the Agent's complete bounded authoritative journal for
        // this newly authenticated session. If an auto-reconciling quarantine has
        // no exact proof in it, no protocol-defined proof source remains in this
        // Agent state; retain quarantine and surface an explicit evidence gap.
        let remaining = candidate.quarantine_inspections()?;
        let mut evidence_gaps = Vec::new();
        for inspection in remaining {
            if inspection.operation.device_id == self.inner.device_id
                && inspection.reconciliation_status == ReconciliationStatus::AutoReconciling
            {
                candidate.mark_reconciliation_evidence_gap(&inspection.operation.operation_id)?;
                evidence_gaps.push(inspection.operation.operation_id);
            }
        }

        let state = HubPersistentState::capture(&persistent.registry, &candidate);
        let checkpoint_bytes = match self.inner.checkpoint.save_with_size(&state) {
            Ok((_, checkpoint_bytes)) => checkpoint_bytes,
            Err(error) => {
                crate::v2_observability::persistence_failure(
                    crate::v2_observability::PersistenceComponent::Hub,
                );
                tracing::error!(
                    event = "v2_persistence_failure",
                    device_id = %self.inner.device_id,
                    outcome = "failed",
                    error_code = error.safe_error_code(),
                    component = "hub",
                    "Hub reconciliation checkpoint persistence failed"
                );
                return Err(HubServiceError::Persistence(error));
            }
        };
        self.inner
            .last_checkpoint_bytes
            .store(checkpoint_bytes, Ordering::Release);
        // Only now can quarantine disappear from the live authoritative state.
        persistent.execution = candidate;
        drop(persistent);

        for (operation_id, capability, terminal_state) in resolved {
            tracing::info!(
                event = "v2_operation_auto_resolved",
                operation_id,
                device_id = %self.inner.device_id,
                generation,
                capability = crate::v2_observability::capability_name(capability),
                terminal_state = ?terminal_state,
                outcome = "auto_resolved",
                "authoritative Agent terminal evidence reconciled quarantined operation without replay"
            );
        }
        for operation_id in operator_required {
            tracing::warn!(
                event = "v2_operation_reconciliation_operator_required",
                operation_id,
                device_id = %self.inner.device_id,
                generation,
                outcome = "operator_required",
                error_code = "reconciliation_binding_mismatch",
                "signed reconciliation evidence did not match the exact Hub authority binding; quarantine retained"
            );
        }
        for operation_id in evidence_gaps {
            tracing::warn!(
                event = "v2_operation_reconciliation_evidence_gap",
                operation_id,
                device_id = %self.inner.device_id,
                generation,
                outcome = "unrecoverable_evidence_gap",
                error_code = "reconciliation_evidence_unavailable",
                "Agent journal has no authoritative terminal proof for quarantined operation; quarantine retained"
            );
        }
        Ok(())
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
        if !pending.contains_key(&operation_id) {
            crate::v2_observability::stale_result_rejected();
            tracing::warn!(
                event = "v2_stale_result_rejected",
                operation_id = %operation_id,
                device_id = %self.inner.device_id,
                generation,
                outcome = "rejected",
                error_code = "pending_operation_missing",
                "verified result has no pending operation in this session"
            );
            return Err(HubServiceError::PendingOperationMissing);
        }
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
        let capability = operation.command.capability();

        if matches!(
            device_result,
            DeviceResult::Error {
                code: crate::v2_m0::DeviceErrorCode::BackendOutcomeIndeterminate
            }
        ) {
            let recovery_evidence_read = {
                let persistent = self.inner.persistent.lock().await;
                persistent
                    .execution
                    .is_recovery_evidence_read(&operation_id)
            };
            if recovery_evidence_read {
                let (next, _receipt) = {
                    let mut persistent = self.inner.persistent.lock().await;
                    let settled = persistent
                        .execution
                        .mark_recovery_read_interrupted(&operation_id, unix_time_ms()?)?;
                    persist_locked(&self.inner, &persistent)?;
                    settled
                };
                crate::v2_observability::operation_completed(
                    capability,
                    crate::v2_observability::OperationOutcome::Failed,
                );
                tracing::warn!(
                    event = "v2_recovery_evidence_read_failed",
                    operation_id = %operation_id,
                    device_id = %self.inner.device_id,
                    generation,
                    capability = crate::v2_observability::capability_name(capability),
                    outcome = "failed_safe",
                    error_code = "backend_outcome_unproven",
                    "recovery evidence read failed without changing the existing quarantine"
                );
                if let Some(operation) = pending.remove(&operation_id) {
                    let _ = operation.reply.send(Err(HubCommandError::Remote(
                        crate::v2_m0::DeviceErrorCode::BackendOutcomeIndeterminate,
                    )));
                }
                return self
                    .dispatch_next(
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
                    .await;
            }
            let cancelled_queued = {
                let mut persistent = self.inner.persistent.lock().await;
                persistent.execution.mark_indeterminate(
                    &operation_id,
                    &owner,
                    generation,
                    IndeterminateReason::BackendOutcomeUnproven,
                    unix_time_ms()?,
                )?;
                let cancelled: Vec<_> = pending
                    .keys()
                    .filter(|pending_id| {
                        pending_id.as_str() != operation_id
                            && persistent.execution.state(pending_id)
                                == Some(HubOperationState::Cancelled)
                    })
                    .cloned()
                    .collect();
                persist_locked(&self.inner, &persistent)?;
                cancelled
            };
            for cancelled_id in cancelled_queued {
                queue_order.retain(|queued| queued != &cancelled_id);
                if let Some(operation) = pending.remove(&cancelled_id) {
                    let _ = operation
                        .reply
                        .send(Err(HubCommandError::CancelledBeforeDispatch));
                }
            }
            if let Some(operation) = pending.remove(&operation_id) {
                let _ = operation
                    .reply
                    .send(Err(HubCommandError::DeviceIndeterminate {
                        operation_id: operation_id.clone(),
                    }));
            }
            crate::v2_observability::operation_indeterminate(
                IndeterminateReason::BackendOutcomeUnproven,
            );
            emit_quarantine_created_alert(
                &operation_id,
                &self.inner.device_id,
                generation,
                Some(capability),
                IndeterminateReason::BackendOutcomeUnproven,
            );
            tracing::warn!(
                event = "v2_operation_indeterminate",
                operation_id = %operation_id,
                device_id = %self.inner.device_id,
                generation,
                capability = crate::v2_observability::capability_name(capability),
                outcome = "quarantined",
                indeterminate_reason = crate::v2_observability::indeterminate_reason_name(
                    IndeterminateReason::BackendOutcomeUnproven
                ),
                error_code = "backend_outcome_unproven",
                "backend returned no proof of completion after a mutating dispatch; device quarantined"
            );
            self.maybe_send_recovery_challenge(outbound, generation, session_clock)
                .await?;
            return Ok(());
        }

        let (terminal_state, evidence) = terminal_evidence_for_device_result(&device_result)
            .ok_or(HubServiceError::UnexpectedResultType)?;
        let recoverable_result = recoverable_result_for(capability, &device_result);
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
            if let Some(result) = recoverable_result {
                persistent.execution.attach_recoverable_result(
                    &operation_id,
                    &owner,
                    generation,
                    result,
                )?;
            }
            persist_locked(&self.inner, &persistent)?;
            settled
        };
        let outcome = match terminal_state {
            HubOperationState::Completed => crate::v2_observability::OperationOutcome::Completed,
            HubOperationState::Cancelled => crate::v2_observability::OperationOutcome::Cancelled,
            _ => crate::v2_observability::OperationOutcome::Failed,
        };
        crate::v2_observability::operation_completed(capability, outcome);
        tracing::info!(
            event = if terminal_state == HubOperationState::Completed {
                "v2_operation_completed"
            } else {
                "v2_operation_failed"
            },
            operation_id = %operation_id,
            device_id = %self.inner.device_id,
            generation,
            capability = crate::v2_observability::capability_name(capability),
            outcome = match outcome {
                crate::v2_observability::OperationOutcome::Completed => "completed",
                crate::v2_observability::OperationOutcome::Failed => "failed",
                crate::v2_observability::OperationOutcome::Cancelled => "cancelled",
            },
            "operation reached a durable terminal state"
        );
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
        let mut next = next;
        while let CompletionDecision::StartNext(operation) = next {
            queue_order.retain(|queued| queued != &operation.operation_id);
            match self
                .dispatch_operation(
                    outbound,
                    hello,
                    challenge,
                    generation,
                    capability_revision,
                    session_clock,
                    &operation.operation_id,
                    pending,
                )
                .await?
            {
                DispatchOutcome::Sent => break,
                DispatchOutcome::Rejected(following) => next = following,
            }
        }
        Ok(())
    }

    // Cancellation verification needs both authenticated-session context and the two
    // operation waiter maps; keep that state explicit instead of hiding it in a broad context bag.
    #[allow(clippy::too_many_arguments)]
    async fn handle_cancellation_ack(
        &self,
        ack: RemoteCancellationAck,
        outbound: &mpsc::Sender<Result<HubFrame, Status>>,
        hello: &AgentHello,
        challenge: &HubChallenge,
        generation: u64,
        session_clock: &TrustedSessionClock,
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
        tracing::info!(
            event = "v2_cancellation_acknowledged",
            operation_id = %ack.operation_id,
            device_id = %self.inner.device_id,
            generation,
            outcome = crate::v2_observability::cancellation_disposition_name(&ack.disposition),
            "Agent cancellation acknowledgement verified"
        );
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
            let recovery_capability = pending
                .get(&ack.operation_id)
                .ok_or(HubServiceError::PendingOperationMissing)?
                .command
                .capability();
            let recovery_evidence_read = {
                let persistent = self.inner.persistent.lock().await;
                persistent
                    .execution
                    .is_recovery_evidence_read(&ack.operation_id)
            };
            let cancelled_queued = {
                let mut persistent = self.inner.persistent.lock().await;
                if matches!(
                    persistent.execution.state(&ack.operation_id),
                    Some(HubOperationState::Dispatched | HubOperationState::CancelRequested)
                ) {
                    if recovery_evidence_read {
                        let _ = persistent
                            .execution
                            .mark_recovery_read_interrupted(&ack.operation_id, unix_time_ms()?)?;
                    } else {
                        persistent.execution.mark_indeterminate(
                            &ack.operation_id,
                            &owner,
                            generation,
                            IndeterminateReason::CancellationUnproven,
                            unix_time_ms()?,
                        )?;
                    }
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
                let response = if recovery_evidence_read {
                    Err(HubCommandError::Remote(
                        crate::v2_m0::DeviceErrorCode::BackendOutcomeIndeterminate,
                    ))
                } else {
                    Err(HubCommandError::DeviceIndeterminate {
                        operation_id: ack.operation_id.clone(),
                    })
                };
                let _ = operation.reply.send(response);
            }
            if recovery_evidence_read {
                crate::v2_observability::operation_completed(
                    recovery_capability,
                    crate::v2_observability::OperationOutcome::Failed,
                );
                tracing::warn!(
                    event = "v2_recovery_evidence_read_failed",
                    operation_id = %ack.operation_id,
                    device_id = %self.inner.device_id,
                    generation,
                    outcome = "failed_safe",
                    error_code = "cancellation_unproven",
                    "recovery evidence read cancellation remained side-effect safe; existing quarantine retained"
                );
            } else {
                crate::v2_observability::operation_indeterminate(
                    IndeterminateReason::CancellationUnproven,
                );
                emit_quarantine_created_alert(
                    &ack.operation_id,
                    &self.inner.device_id,
                    generation,
                    None,
                    IndeterminateReason::CancellationUnproven,
                );
                tracing::warn!(
                    event = "v2_operation_indeterminate",
                    operation_id = %ack.operation_id,
                    device_id = %self.inner.device_id,
                    generation,
                    outcome = "quarantined",
                    indeterminate_reason = crate::v2_observability::indeterminate_reason_name(
                        IndeterminateReason::CancellationUnproven
                    ),
                    error_code = "cancellation_unproven",
                    "backend cancellation was propagated but side-effect interruption is unproven; device quarantined"
                );
                self.maybe_send_recovery_challenge(outbound, generation, session_clock)
                    .await?;
            }
        }
        if let Some(waiter) = cancel_waiters.remove(&ack.operation_id) {
            let _ = waiter.send(Ok(ack.disposition));
        }
        Ok(())
    }

    async fn maybe_send_recovery_challenge(
        &self,
        outbound: &mpsc::Sender<Result<HubFrame, Status>>,
        generation: u64,
        session_clock: &TrustedSessionClock,
    ) -> Result<(), HubServiceError> {
        if self.inner.recovery_verifier.is_none() {
            return Ok(());
        }
        let quarantine = {
            let persistent = self.inner.persistent.lock().await;
            persistent
                .execution
                .quarantine(&self.inner.device_id)
                .cloned()
        };
        let Some(quarantine) = quarantine else {
            return Ok(());
        };
        let now_ms = session_clock.now_ms();
        let fingerprint = quarantine_fingerprint(&quarantine);
        {
            let runtime = self.inner.recovery_runtime.lock().await;
            if runtime.pending.as_ref().is_some_and(|pending| {
                pending.current_generation == generation
                    && pending.quarantine_fingerprint == fingerprint
                    && now_ms <= pending.expires_at_ms
            }) {
                return Ok(());
            }
        }
        let challenge = build_recovery_challenge(
            &self.inner.material.hub_identity,
            &quarantine,
            generation,
            now_ms,
        )
        .map_err(HubServiceError::OnlineRecovery)?;
        {
            let mut runtime = self.inner.recovery_runtime.lock().await;
            runtime.pending = Some(challenge.clone());
            runtime.last_resolved = None;
        }
        send_hub(outbound, HubToAgent::RecoveryChallenge(challenge.clone())).await?;
        tracing::warn!(
            event = "v2_recovery_challenge_issued",
            operation_id = %challenge.operation_id,
            device_id = %challenge.device_id,
            generation,
            quarantine_generation = challenge.quarantine_generation,
            outcome = "local_user_action_required",
            "online recovery challenge issued for quarantined desktop"
        );
        Ok(())
    }

    async fn handle_recovery_authorization(
        &self,
        authorization: RecoveryAuthorization,
        outbound: &mpsc::Sender<Result<HubFrame, Status>>,
        generation: u64,
        session_clock: &TrustedSessionClock,
    ) -> Result<(), HubServiceError> {
        let verifier =
            self.inner
                .recovery_verifier
                .clone()
                .ok_or(HubServiceError::OnlineRecovery(
                    RecoveryError::KeyUnavailable,
                ))?;

        let (pending, duplicate_ack) = {
            let runtime = self.inner.recovery_runtime.lock().await;
            let duplicate = runtime
                .last_resolved
                .as_ref()
                .filter(|(accepted, _)| accepted.request_id == authorization.request_id)
                .cloned();
            (runtime.pending.clone(), duplicate)
        };
        if let Some((accepted, ack)) = duplicate_ack {
            if accepted != authorization {
                return Err(HubServiceError::OnlineRecovery(
                    RecoveryError::ChallengeMismatch,
                ));
            }
            send_hub(outbound, HubToAgent::RecoveryResolved(ack)).await?;
            return Ok(());
        }
        let pending = pending.ok_or(HubServiceError::OnlineRecovery(
            RecoveryError::ChallengeMismatch,
        ))?;
        verifier
            .verify_authorization(&pending, &authorization, session_clock.now_ms())
            .map_err(HubServiceError::OnlineRecovery)?;
        if authorization.current_generation != generation {
            return Err(HubServiceError::StaleSession);
        }

        let resolved_at_ms = session_clock.now_ms();
        let mut retirement_capacity = None;
        {
            let mut persistent = self.inner.persistent.lock().await;
            let quarantine = persistent
                .execution
                .quarantine(&self.inner.device_id)
                .cloned()
                .ok_or(HubServiceError::OnlineRecovery(
                    RecoveryError::ChallengeMismatch,
                ))?;
            if quarantine.operation_id != authorization.operation_id
                || quarantine.device_generation != authorization.quarantine_generation
                || quarantine_fingerprint(&quarantine) != authorization.quarantine_fingerprint
            {
                return Err(HubServiceError::OnlineRecovery(
                    RecoveryError::ChallengeMismatch,
                ));
            }
            let rollback = persistent.execution.snapshot_for_restart();
            match authorization.decision {
                RecoveryDecision::ConfirmedCompleted => {
                    let resolver =
                        OperationOwner::new("cumg://local-user-recovery", verifier.key_id())?;
                    persistent.execution.resolve_indeterminate(
                        &authorization.operation_id,
                        resolver,
                        IndeterminateResolution::ConfirmedCompleted,
                        authorization.evidence.clone(),
                        resolved_at_ms,
                    )?;
                }
                RecoveryDecision::ConfirmedNotExecuted => {
                    let resolver =
                        OperationOwner::new("cumg://local-user-recovery", verifier.key_id())?;
                    persistent.execution.resolve_indeterminate(
                        &authorization.operation_id,
                        resolver,
                        IndeterminateResolution::ConfirmedNotExecuted,
                        authorization.evidence.clone(),
                        resolved_at_ms,
                    )?;
                }
                RecoveryDecision::CurrentStateAccepted => {
                    let current_session = persistent
                        .registry
                        .current_session(&self.inner.device_id)
                        .map_err(HubServiceError::Control)?;
                    if current_session.generation != authorization.current_generation {
                        return Err(HubServiceError::StaleSession);
                    }
                    let policy = authorization.current_state_policy.ok_or(
                        HubServiceError::OnlineRecovery(RecoveryError::ChallengeMismatch),
                    )?;
                    match persistent.execution.accept_current_state(
                        &authorization.operation_id,
                        policy,
                        authorization.evidence.clone(),
                        authorization.current_generation,
                        resolved_at_ms,
                    ) {
                        Ok(_) => {
                            retirement_capacity = Some(persistent.execution.retirement_capacity());
                        }
                        Err(
                            crate::v2_m0_execution::ExecutionError::RetirementCapacityExhausted,
                        ) => {
                            crate::v2_observability::retirement_capacity_exhausted();
                            tracing::warn!(
                                event = "v2_retirement_capacity_exhausted",
                                outcome = "rejected",
                                "current-state acceptance refused because permanent replay tombstone capacity is exhausted"
                            );
                            return Err(HubServiceError::Execution(
                                crate::v2_m0_execution::ExecutionError::RetirementCapacityExhausted,
                            ));
                        }
                        Err(error) => return Err(HubServiceError::Execution(error)),
                    }
                }
            }
            if let Err(error) = persist_locked(&self.inner, &persistent) {
                persistent.execution = AuthoritativeOperationController::restore_after_restart(
                    self.inner.config.admission_limits(),
                    rollback,
                )?;
                return Err(error);
            }
        }

        let ack = build_recovery_resolved(
            &self.inner.material.hub_identity,
            &authorization,
            resolved_at_ms,
        )
        .map_err(HubServiceError::OnlineRecovery)?;
        {
            let mut runtime = self.inner.recovery_runtime.lock().await;
            runtime.pending = None;
            runtime.last_resolved = Some((authorization.clone(), ack.clone()));
        }
        if let Some(capacity) = retirement_capacity {
            crate::v2_observability::retirement_capacity_observed(capacity);
        }
        crate::v2_observability::quarantine_resolved();
        tracing::info!(
            event = "v2_quarantine_resolved_online",
            operation_id = %authorization.operation_id,
            device_id = %authorization.device_id,
            generation,
            quarantine_generation = authorization.quarantine_generation,
            recovery_key_id = %verifier.key_id(),
            audit_assessment = match authorization.audit_assessment {
                RecoveryAuditAssessment::Completed => "completed",
                RecoveryAuditAssessment::NotExecuted => "not_executed",
                RecoveryAuditAssessment::Inconclusive => "inconclusive",
            },
            outcome = recovery_decision_name(authorization.decision),
            "local-user-authorized online recovery durably cleared desktop quarantine without replay"
        );
        send_hub(outbound, HubToAgent::RecoveryResolved(ack)).await
    }

    async fn ensure_current_generation(&self, generation: u64) -> Result<(), HubServiceError> {
        let persistent = self.inner.persistent.lock().await;
        let current = persistent.registry.current_session(&self.inner.device_id)?;
        if current.generation == generation {
            Ok(())
        } else {
            tracing::warn!(
                event = "v2_stale_session_rejected",
                device_id = %self.inner.device_id,
                generation,
                outcome = "rejected",
                error_code = "generation_mismatch",
                "message from stale Agent session generation rejected"
            );
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
        let mut connection_lost_indeterminate = Vec::new();
        let mut recovery_reads_interrupted = Vec::new();
        for operation in operations {
            let operation_id = operation.operation.operation_id;
            match persistent.execution.state(&operation_id) {
                Some(HubOperationState::Dispatched | HubOperationState::CancelRequested) => {
                    persistent
                        .execution
                        .mark_connection_lost(&operation_id, unix_time_ms()?)?;
                    if operation.execution_lane == OperationExecutionLane::RecoveryEvidenceRead {
                        recovery_reads_interrupted
                            .push((operation_id.clone(), operation.capability));
                    } else {
                        connection_lost_indeterminate
                            .push((operation_id.clone(), operation.capability));
                    }
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
        persist_locked(&self.inner, &persistent)?;
        for (operation_id, capability) in recovery_reads_interrupted {
            crate::v2_observability::operation_completed(
                capability,
                crate::v2_observability::OperationOutcome::Failed,
            );
            tracing::warn!(
                event = "v2_recovery_evidence_read_failed",
                operation_id = %operation_id,
                device_id = %self.inner.device_id,
                generation,
                capability = crate::v2_observability::capability_name(capability),
                outcome = "failed_safe",
                error_code = "connection_lost",
                "recovery evidence read lost transport without changing the existing quarantine"
            );
        }
        for (operation_id, capability) in connection_lost_indeterminate {
            crate::v2_observability::operation_indeterminate(IndeterminateReason::ConnectionLost);
            emit_quarantine_created_alert(
                &operation_id,
                &self.inner.device_id,
                generation,
                Some(capability),
                IndeterminateReason::ConnectionLost,
            );
            tracing::warn!(
                event = "v2_operation_indeterminate",
                operation_id = %operation_id,
                device_id = %self.inner.device_id,
                generation,
                capability = crate::v2_observability::capability_name(capability),
                outcome = "quarantined",
                indeterminate_reason = crate::v2_observability::indeterminate_reason_name(
                    IndeterminateReason::ConnectionLost
                ),
                error_code = "connection_lost",
                "connection loss left dispatched operation indeterminate; device quarantined"
            );
        }
        tracing::info!(
            event = "v2_agent_session_ended",
            device_id = %self.inner.device_id,
            generation,
            outcome = if is_current { "disconnected" } else { "superseded" },
            "Agent session cleanup completed"
        );
        Ok(())
    }
}

impl HubHandle {
    pub fn device_id(&self) -> &str {
        &self.inner.device_id
    }

    /// Close the Hub admission gate before a planned shutdown. Existing work
    /// that has already crossed the dispatch boundary remains connected so the
    /// Agent can provide terminal evidence during the bounded drain window.
    pub fn begin_shutdown_drain(&self) -> bool {
        !self.inner.draining.swap(true, Ordering::AcqRel)
    }

    pub fn is_shutdown_draining(&self) -> bool {
        self.inner.draining.load(Ordering::Acquire)
    }

    /// Wait until all already-admitted work reaches a durable terminal or
    /// indeterminate state. The binary wraps this in an operator-configured
    /// timeout; timeout never changes the fail-closed restart semantics.
    pub async fn wait_for_shutdown_drain(&self) {
        loop {
            let unsettled = {
                let persistent = self.inner.persistent.lock().await;
                persistent
                    .execution
                    .has_unsettled_work(&self.inner.device_id)
            };
            if !unsettled {
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    /// Close the currently authenticated Agent stream after a planned shutdown
    /// drain has completed (or its bounded timeout has expired). The session
    /// cleanup path remains authoritative: if work is still dispatched, closing
    /// the stream preserves the existing fail-closed Indeterminate + quarantine
    /// semantics instead of treating transport shutdown as terminal evidence.
    pub async fn close_live_session_for_shutdown(&self) -> bool {
        let supersede = {
            let live = self.inner.live.lock().await;
            live.as_ref().map(|session| session.supersede.clone())
        };
        supersede.is_some_and(|sender| sender.send(true).is_ok())
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

    /// Return the currently connected Agent's versioned capability advertisement.
    ///
    /// `None` means the Agent is offline or its live registry entry is unavailable.
    /// Callers must still rely on command-session validation at dispatch time because
    /// a reconnect can change generation/revision after this observation.
    pub async fn current_capabilities(&self) -> Option<CapabilityAdvertisement> {
        let persistent = self.inner.persistent.lock().await;
        persistent
            .registry
            .current_session(&self.inner.device_id)
            .ok()
            .map(|session| session.capabilities)
    }

    /// Atomically observe the current Agent generation and capability advertisement.
    /// Stateful northbound workflow bindings must use this rather than reading the
    /// generation and capability revision in separate lock acquisitions.
    pub async fn current_session_binding(&self) -> Option<(u64, CapabilityAdvertisement)> {
        let persistent = self.inner.persistent.lock().await;
        persistent
            .registry
            .current_session(&self.inner.device_id)
            .ok()
            .map(|session| (session.generation, session.capabilities))
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
        self.start_command_as_with_id(owner, random_operation_id(), command)
            .await
    }

    /// Installs one immutable semantic-constraint snapshot identity for this Hub runtime.
    /// Re-installing the exact revision+digest is idempotent; any different snapshot
    /// requires a reviewed Hub restart and cannot hot-widen the running authority.
    pub fn install_semantic_constraint_snapshot(
        &self,
        revision: u64,
        digest: &str,
    ) -> Result<(), HubCommandError> {
        if revision == 0
            || digest.len() != 64
            || !digest.bytes().all(|byte| byte.is_ascii_hexdigit())
        {
            return Err(HubCommandError::Rejected);
        }
        let requested = SemanticConstraintSnapshotIdentity {
            revision,
            digest: digest.to_owned(),
        };
        if let Some(current) = self.inner.semantic_constraint_snapshot.get() {
            return if current == &requested {
                Ok(())
            } else {
                Err(HubCommandError::Rejected)
            };
        }
        match self
            .inner
            .semantic_constraint_snapshot
            .set(requested.clone())
        {
            Ok(()) => Ok(()),
            Err(_) if self.inner.semantic_constraint_snapshot.get() == Some(&requested) => Ok(()),
            Err(_) => Err(HubCommandError::Rejected),
        }
    }

    /// Allocates a CUMG logical operation identity before command admission.
    pub fn new_operation_id(&self) -> String {
        random_operation_id()
    }

    pub async fn start_command_as_with_id(
        &self,
        owner: OperationOwner,
        operation_id: String,
        command: DeviceCommand,
    ) -> Result<HubPendingCommand, HubCommandError> {
        self.start_command_as_with_id_and_metadata(
            owner,
            operation_id,
            command,
            OperationAdmissionMetadata::empty(),
        )
        .await
    }

    pub async fn start_command_as_with_id_and_metadata(
        &self,
        owner: OperationOwner,
        operation_id: String,
        command: DeviceCommand,
        metadata: OperationAdmissionMetadata,
    ) -> Result<HubPendingCommand, HubCommandError> {
        self.start_command_as_with_id_and_metadata_inner(
            owner,
            operation_id,
            command,
            metadata,
            CommandSessionFence::None,
        )
        .await
    }

    /// Starts only if the live session still matches the generation/revision that an external
    /// authority gate just observed. CUMG admission and exact-capability grants remain authoritative.
    pub async fn start_command_as_with_id_and_metadata_for_session(
        &self,
        owner: OperationOwner,
        operation_id: String,
        command: DeviceCommand,
        metadata: OperationAdmissionMetadata,
        expected_session: (u64, u64),
    ) -> Result<HubPendingCommand, HubCommandError> {
        self.start_command_as_with_id_and_metadata_inner(
            owner,
            operation_id,
            command,
            metadata,
            CommandSessionFence::Session {
                generation: expected_session.0,
                capability_revision: expected_session.1,
            },
        )
        .await
    }

    pub async fn start_command_as_with_id_and_metadata_for_handoff(
        &self,
        owner: OperationOwner,
        operation_id: String,
        command: DeviceCommand,
        metadata: OperationAdmissionMetadata,
        handoff: RemoteHandoffAuthority,
    ) -> Result<HubPendingCommand, HubCommandError> {
        self.start_command_as_with_id_and_metadata_inner(
            owner,
            operation_id,
            command,
            metadata,
            CommandSessionFence::Handoff(handoff),
        )
        .await
    }

    pub(crate) async fn handoff_request(
        &self,
        request: RemoteHandoffRequestKind,
    ) -> Result<RemoteHandoffResponseKind, HubCommandError> {
        if request.requires_agent_idle() {
            let persistent = self.inner.persistent.lock().await;
            if request.starts_human_control()
                && let Some(quarantine) = persistent.execution.quarantine(&self.inner.device_id)
            {
                return Err(HubCommandError::DeviceIndeterminate {
                    operation_id: quarantine.operation_id.clone(),
                });
            }
            if persistent
                .execution
                .has_unsettled_work(&self.inner.device_id)
            {
                return Err(HubCommandError::Busy);
            }
        }
        let (reply_tx, reply_rx) = oneshot::channel();
        let request_id = random_handoff_request_id();
        let tx = {
            let live = self.inner.live.lock().await;
            live.as_ref()
                .map(|session| session.command_tx.clone())
                .ok_or(HubCommandError::AgentOffline)?
        };
        tx.send(HubRequest::Handoff {
            request_id,
            request,
            reply: reply_tx,
        })
        .await
        .map_err(|_| HubCommandError::AgentOffline)?;
        reply_rx.await.map_err(|_| HubCommandError::SessionClosed)?
    }

    async fn start_command_as_with_id_and_metadata_inner(
        &self,
        owner: OperationOwner,
        operation_id: String,
        command: DeviceCommand,
        metadata: OperationAdmissionMetadata,
        session_fence: CommandSessionFence,
    ) -> Result<HubPendingCommand, HubCommandError> {
        if self.inner.draining.load(Ordering::Acquire) {
            return Err(HubCommandError::Busy);
        }
        if operation_id.is_empty() {
            return Err(HubCommandError::Rejected);
        }
        let (reply_tx, reply_rx) = oneshot::channel();
        let tx = {
            let live = self.inner.live.lock().await;
            let session = live.as_ref().ok_or(HubCommandError::AgentOffline)?;
            if session_fence
                .expected_session()
                .is_some_and(|(generation, revision)| {
                    session.generation != generation || session.capability_revision != revision
                })
            {
                return Err(HubCommandError::SessionSuperseded);
            }
            session.command_tx.clone()
        };
        let handoff = session_fence.handoff();
        tx.send(HubRequest::Execute {
            operation_id: operation_id.clone(),
            owner,
            command: Box::new(command),
            metadata: Box::new(metadata),
            handoff,
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

    /// End backend-owned interaction state without creating a semantic device
    /// operation, grant, replay identity, or quarantine transition. Callers must
    /// validate CUMG context ownership before invoking this lifecycle control.
    pub(crate) async fn end_backend_interaction_session(
        &self,
        context_id: String,
    ) -> Result<bool, HubCommandError> {
        let (reply_tx, reply_rx) = oneshot::channel();
        let tx = {
            let live = self.inner.live.lock().await;
            live.as_ref()
                .map(|session| session.command_tx.clone())
                .ok_or(HubCommandError::AgentOffline)?
        };
        tx.send(HubRequest::EndBackendSession {
            context_id,
            reply: reply_tx,
        })
        .await
        .map_err(|_| HubCommandError::AgentOffline)?;
        reply_rx.await.map_err(|_| HubCommandError::SessionClosed)?
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
                decision.clone(),
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
        crate::v2_observability::quarantine_resolved();
        tracing::info!(
            event = "v2_quarantine_resolved",
            operation_id,
            device_id = %self.inner.device_id,
            generation = receipt.operation.device_generation,
            capability = crate::v2_observability::capability_name(receipt.capability),
            outcome = crate::v2_observability::resolution_name(&decision),
            "indeterminate operation explicitly resolved; quarantine cleared"
        );
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

    pub async fn operation_recovery_as(
        &self,
        owner: OperationOwner,
        operation_id: &str,
    ) -> Result<OperationRecoverySnapshot, HubCommandError> {
        let persistent = self.inner.persistent.lock().await;
        persistent
            .execution
            .recovery_for_owner(operation_id, &owner)
            .map_err(command_error_from_execution)
    }

    pub async fn resolution_records(&self) -> Vec<ResolutionRecord> {
        let persistent = self.inner.persistent.lock().await;
        persistent.execution.resolutions().to_vec()
    }

    pub async fn retirement_records(&self) -> Vec<RetirementRecord> {
        let persistent = self.inner.persistent.lock().await;
        persistent.execution.retirements().to_vec()
    }
}

#[cfg(test)]
#[path = "v2_m1_hub_online_recovery_tests.rs"]
mod online_recovery_tests;

#[tonic::async_trait]
impl AgentControl for SingleDeviceHub {
    type OpenSessionStream = Pin<Box<dyn Stream<Item = Result<HubFrame, Status>> + Send + 'static>>;

    async fn open_session(
        &self,
        request: Request<Streaming<AgentFrame>>,
    ) -> Result<Response<Self::OpenSessionStream>, Status> {
        if !self.inner.session_rate.try_acquire() {
            crate::v2_observability::agent_session_rejected(
                crate::v2_observability::SessionRejectReason::RateLimit,
            );
            tracing::warn!(
                event = "v2_agent_session_rejected",
                outcome = "rejected",
                error_code = "rate_limit",
                "Agent session start rate exceeded"
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
                crate::v2_observability::agent_session_rejected(
                    crate::v2_observability::SessionRejectReason::ConcurrencyLimit,
                );
                tracing::warn!(
                    event = "v2_agent_session_rejected",
                    outcome = "rejected",
                    error_code = "concurrency_limit",
                    "Agent session concurrency exceeded"
                );
                Status::resource_exhausted("Agent session concurrency exceeded")
            })?;
        crate::v2_observability::agent_session_started();
        let (outbound_tx, outbound_rx) = mpsc::channel(SESSION_QUEUE_DEPTH);
        let service = self.clone();
        tokio::spawn(
            async move {
                let _permit = permit;
                if let Err(error) = service
                    .run_session(request.into_inner(), outbound_tx.clone())
                    .await
                {
                    tracing::warn!(
                        event = "v2_hub_session_error",
                        device_id = %service.inner.device_id,
                        outcome = "ended",
                        error_code = error.safe_error_code(),
                        "V2 Hub Agent session ended with error"
                    );
                    let _ = outbound_tx.send(Err(error.grpc_status())).await;
                }
            }
            .instrument(tracing::info_span!("v2_agent_session")),
        );
        Ok(Response::new(Box::pin(ReceiverStream::new(outbound_rx))))
    }
}

fn emit_quarantine_created_alert(
    operation_id: &str,
    device_id: &str,
    generation: u64,
    capability: Option<DeviceCapability>,
    reason: IndeterminateReason,
) {
    crate::v2_observability::quarantine_created();
    tracing::error!(
        event = "v2_quarantine_created",
        operation_id,
        device_id,
        generation,
        capability = capability
            .map(crate::v2_observability::capability_name)
            .unwrap_or("unknown"),
        outcome = "operator_action_required",
        indeterminate_reason = crate::v2_observability::indeterminate_reason_name(reason),
        error_code = "device_indeterminate",
        "device quarantined; explicit operator resolution is required before reuse"
    );
}

fn persist_locked(
    inner: &HubInner,
    persistent: &PersistentHubState,
) -> Result<(), HubServiceError> {
    let state = HubPersistentState::capture(&persistent.registry, &persistent.execution);
    let checkpoint_bytes = match inner.checkpoint.save_with_size(&state) {
        Ok((_, checkpoint_bytes)) => checkpoint_bytes,
        Err(error) => {
            crate::v2_observability::persistence_failure(
                crate::v2_observability::PersistenceComponent::Hub,
            );
            tracing::error!(
                event = "v2_persistence_failure",
                device_id = %inner.device_id,
                outcome = "failed",
                error_code = error.safe_error_code(),
                component = "hub",
                "Hub checkpoint persistence failed"
            );
            return Err(HubServiceError::Persistence(error));
        }
    };
    inner
        .last_checkpoint_bytes
        .store(checkpoint_bytes, Ordering::Release);
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

fn unexpected_agent_message(expected: &'static str, message: &AgentToHub) -> HubServiceError {
    let got = message.kind();
    tracing::warn!(
        event = "v2_protocol_message_rejected",
        outcome = "rejected",
        error_code = "unexpected_message",
        expected_message = expected,
        message_kind = got,
        "unexpected Agent protocol message rejected"
    );
    HubServiceError::UnexpectedMessage { expected, got }
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

fn recoverable_result_for(
    capability: DeviceCapability,
    result: &DeviceResult,
) -> Option<RecoverableOperationResult> {
    match (capability, result) {
        (DeviceCapability::ExecuteProcess, DeviceResult::Process { output }) => {
            Some(RecoverableOperationResult::Process {
                output: output.clone(),
            })
        }
        (DeviceCapability::Shell, DeviceResult::Shell { output }) => {
            Some(RecoverableOperationResult::Shell {
                output: output.clone(),
            })
        }
        (
            DeviceCapability::ExecuteProcess | DeviceCapability::Shell,
            DeviceResult::Error { code },
        ) => Some(RecoverableOperationResult::Error { code: *code }),
        (capability, _)
            if !matches!(capability.class(), crate::v2_m0::CapabilityClass::Observe) =>
        {
            Some(RecoverableOperationResult::EffectfulStatus)
        }
        _ => None,
    }
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

fn random_handoff_request_id() -> String {
    let mut random = [0_u8; 16];
    OsRng.fill_bytes(&mut random);
    let mut output = String::from("handoff_req_");
    for byte in random {
        use std::fmt::Write as _;
        let _ = write!(&mut output, "{byte:02x}");
    }
    output
}

#[derive(Clone, PartialEq, Eq)]
pub enum HubCommandError {
    AgentOffline,
    SessionSuperseded,
    SessionClosed,
    CancelledBeforeDispatch,
    SemanticConstraintSnapshotStale,
    OperationReplay,
    Busy,
    UnknownOperation,
    DeviceIndeterminate { operation_id: String },
    Rejected,
    GrantSigningUnavailable,
    Remote(DeviceErrorCode),
    UnexpectedResult,
    Indeterminate,
}

impl SafeErrorCode for HubCommandError {
    fn safe_error_code(&self) -> &'static str {
        match self {
            Self::AgentOffline => "agent_offline",
            Self::SessionSuperseded => "session_superseded",
            Self::SessionClosed => "session_closed",
            Self::CancelledBeforeDispatch => "cancelled_before_dispatch",
            Self::SemanticConstraintSnapshotStale => "semantic_constraint_snapshot_stale",
            Self::OperationReplay => "operation_replay",
            Self::Busy => "busy",
            Self::UnknownOperation => "unknown_operation",
            Self::DeviceIndeterminate { .. } | Self::Indeterminate => "device_indeterminate",
            Self::Rejected => "rejected",
            Self::GrantSigningUnavailable => "grant_signing_unavailable",
            Self::Remote(_) => "remote_error",
            Self::UnexpectedResult => "unexpected_result",
        }
    }
}

impl fmt::Debug for HubCommandError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.safe_error_code())
    }
}

impl fmt::Display for HubCommandError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.safe_error_code())
    }
}

impl std::error::Error for HubCommandError {}

pub enum HubServiceError {
    InvalidConfig(&'static str),
    Persistence(PersistenceError),
    OnlineRecovery(RecoveryError),
    StateDirectoryLock(StateDirectoryLockError),
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
    UnexpectedMessage {
        expected: &'static str,
        got: &'static str,
    },
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

impl SafeErrorCode for HubServiceError {
    fn safe_error_code(&self) -> &'static str {
        match self {
            Self::InvalidConfig(_) => "invalid_config",
            Self::Persistence(error) => error.safe_error_code(),
            Self::OnlineRecovery(error) => error.safe_code(),
            Self::StateDirectoryLock(StateDirectoryLockError::Busy) => "state_directory_busy",
            Self::StateDirectoryLock(_) => "state_directory_lock_error",
            Self::Control(_) => "control_error",
            Self::Execution(_) => "execution_error",
            Self::Transport(_) => "protocol_error",
            Self::Trust(_) => "trust_error",
            Self::Carrier(_) => "carrier_error",
            Self::Status(_) => "grpc_status",
            Self::WrongDevice => "wrong_device",
            Self::StaleSession => "stale_session",
            Self::InboundClosed => "inbound_closed",
            Self::OutboundClosed => "outbound_closed",
            Self::HeartbeatTimeout => "heartbeat_timeout",
            Self::UnexpectedMessage { .. } => "unexpected_message",
            Self::UnexpectedResultType => "unexpected_result_type",
            Self::PendingOperationMissing => "pending_operation_missing",
            Self::CheckpointDeviceTrustMismatch => "checkpoint_device_trust_mismatch",
            Self::StateBusy => "state_busy",
            Self::SystemClockBeforeEpoch => "system_clock_before_epoch",
        }
    }
}

impl fmt::Debug for HubServiceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnexpectedMessage { expected, got } => f
                .debug_struct("unexpected_message")
                .field("expected", expected)
                .field("got", got)
                .finish(),
            _ => f.write_str(self.safe_error_code()),
        }
    }
}

impl fmt::Display for HubServiceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self, f)
    }
}

impl std::error::Error for HubServiceError {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::v2_m0::DeviceIdentity;
    use crate::v2_m0::GrantAuthority;
    use crate::v2_m0_transport::RemoteHandoffOperatorCommand;
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
    fn session_lifetime_requires_a_strictly_smaller_nonzero_reauth_drain() {
        let base = HubServiceConfig {
            state_dir: test_state_dir("session-lifetime-config"),
            heartbeat_timeout: Duration::from_secs(5),
            max_agent_session_lifetime: Duration::from_secs(60),
            agent_session_reauth_drain: Duration::from_secs(30),
            checkpoint_generation_rollover_bytes: 512 * 1024,
            max_queued_per_device: 1,
            max_agent_sessions: 2,
            max_agent_session_starts_per_minute: 30,
        };
        assert!(base.validate().is_ok());

        let mut equal = base.clone();
        equal.agent_session_reauth_drain = equal.max_agent_session_lifetime;
        assert!(matches!(
            equal.validate(),
            Err(HubServiceError::InvalidConfig(_))
        ));

        let mut zero = base.clone();
        zero.agent_session_reauth_drain = Duration::ZERO;
        assert!(matches!(
            zero.validate(),
            Err(HubServiceError::InvalidConfig(_))
        ));

        let _ = std::fs::remove_dir_all(base.state_dir);
    }

    #[tokio::test]
    async fn semantic_constraint_snapshot_is_immutable_and_stale_admission_cancels_before_dispatch()
    {
        use crate::v2_execution_safety::{
            OperationAdmissionMetadata, SemanticConstraintAdmissionEvidence,
        };

        let immutable_state = test_state_dir("semantic-snapshot-immutable");
        let device = DeviceIdentity::generate();
        let (_hub, immutable_handle) = SingleDeviceHub::new(
            HubServiceConfig {
                state_dir: immutable_state.clone(),
                heartbeat_timeout: Duration::from_secs(5),
                max_agent_session_lifetime: Duration::from_secs(60 * 60),
                agent_session_reauth_drain: Duration::from_secs(30),
                checkpoint_generation_rollover_bytes: 512 * 1024,
                max_queued_per_device: 1,
                max_agent_sessions: 2,
                max_agent_session_starts_per_minute: 30,
            },
            HubProvisionedMaterial {
                hub_identity: HubIdentity::generate(),
                grant_signer: GrantAuthority::generate().into(),
                device_verifier: device.verifying_key(),
                device_rotation: None,
            },
        )
        .unwrap();
        let digest_a = "a".repeat(64);
        let digest_b = "b".repeat(64);
        immutable_handle
            .install_semantic_constraint_snapshot(7, &digest_a)
            .unwrap();
        immutable_handle
            .install_semantic_constraint_snapshot(7, &digest_a)
            .unwrap();
        assert_eq!(
            immutable_handle.install_semantic_constraint_snapshot(7, &digest_b),
            Err(HubCommandError::Rejected)
        );
        assert_eq!(
            immutable_handle.install_semantic_constraint_snapshot(8, &digest_a),
            Err(HubCommandError::Rejected)
        );
        drop(immutable_handle);
        let _ = std::fs::remove_dir_all(immutable_state);

        let state_dir = test_state_dir("semantic-snapshot-stale");
        let device = DeviceIdentity::generate();
        let (hub, handle) = SingleDeviceHub::new(
            HubServiceConfig {
                state_dir: state_dir.clone(),
                heartbeat_timeout: Duration::from_secs(5),
                max_agent_session_lifetime: Duration::from_secs(60 * 60),
                agent_session_reauth_drain: Duration::from_secs(30),
                checkpoint_generation_rollover_bytes: 512 * 1024,
                max_queued_per_device: 1,
                max_agent_sessions: 2,
                max_agent_session_starts_per_minute: 30,
            },
            HubProvisionedMaterial {
                hub_identity: HubIdentity::generate(),
                grant_signer: GrantAuthority::generate().into(),
                device_verifier: device.verifying_key(),
                device_rotation: None,
            },
        )
        .unwrap();

        let operation_id = "op-semantic-snapshot-stale".to_owned();
        let owner = OperationOwner::local_hub();
        let admitted = SemanticConstraintAdmissionEvidence {
            revision: 7,
            snapshot_digest: digest_a.clone(),
            kind: "type_text_max_utf8_bytes".into(),
            rule_id: "text-small".into(),
        };
        let expected = SemanticConstraintSnapshotIdentity::from_evidence(&admitted);
        let metadata = OperationAdmissionMetadata {
            audit: Default::default(),
            request_fingerprint: None,
            evidence_envelope: None,
            semantic_constraint: Some(admitted),
        };
        {
            let mut persistent = handle.inner.persistent.lock().await;
            persistent
                .execution
                .prepare_with_metadata(
                    OperationRef {
                        device_id: handle.device_id().to_owned(),
                        device_generation: 1,
                        operation_id: operation_id.clone(),
                    },
                    owner.clone(),
                    DeviceCapability::TypeText,
                    metadata,
                    1,
                )
                .unwrap();
            persist_locked(&handle.inner, &persistent).unwrap();
        }
        handle
            .install_semantic_constraint_snapshot(8, &digest_b)
            .unwrap();
        assert!(!semantic_constraint_snapshot_is_current(
            &hub.inner,
            Some(&expected),
        ));

        let (reply_tx, reply_rx) = oneshot::channel();
        let mut pending = HashMap::from([(
            operation_id.clone(),
            PendingOperation {
                owner: owner.clone(),
                command: DeviceCommand::TypeText {
                    text: "approved".into(),
                },
                expected_semantic_constraint_snapshot: Some(expected),
                handoff: None,
                envelope: None,
                reply: reply_tx,
            },
        )]);
        let outcome = hub
            .reject_for_semantic_constraint_snapshot(&operation_id, &owner, 1, &mut pending)
            .await
            .unwrap();
        assert!(matches!(outcome, DispatchOutcome::Rejected(_)));
        assert!(pending.is_empty());
        assert_eq!(
            reply_rx.await.unwrap(),
            Err(HubCommandError::SemanticConstraintSnapshotStale)
        );
        let persistent = handle.inner.persistent.lock().await;
        assert_eq!(
            persistent.execution.state(&operation_id),
            Some(HubOperationState::Cancelled)
        );
        assert!(
            persistent
                .execution
                .quarantine(handle.device_id())
                .is_none()
        );
        let snapshot = persistent.execution.snapshot_for_restart();
        let record = snapshot
            .operations
            .iter()
            .find(|record| record.operation.operation_id == operation_id)
            .unwrap();
        assert_eq!(record.dispatched_at_ms, None);
        assert!(record.dispatch_binding.is_none());
        assert_eq!(record.semantic_constraint.as_ref().unwrap().revision, 7);
        assert_eq!(
            record.semantic_constraint.as_ref().unwrap().snapshot_digest,
            digest_a
        );
        drop(persistent);
        drop(hub);
        drop(handle);
        let _ = std::fs::remove_dir_all(state_dir);
    }

    #[test]
    fn startup_exclusively_owns_the_state_directory_until_all_handles_drop() {
        let state_dir = test_state_dir("state-lock");
        let config = HubServiceConfig {
            state_dir: state_dir.clone(),
            heartbeat_timeout: Duration::from_secs(5),
            max_agent_session_lifetime: Duration::from_secs(60 * 60),
            agent_session_reauth_drain: Duration::from_secs(30),
            checkpoint_generation_rollover_bytes: 512 * 1024,
            max_queued_per_device: 1,
            max_agent_sessions: 2,
            max_agent_session_starts_per_minute: 30,
        };
        let material = HubProvisionedMaterial {
            hub_identity: HubIdentity::generate(),
            grant_signer: GrantAuthority::generate().into(),
            device_verifier: DeviceIdentity::generate().verifying_key(),
            device_rotation: None,
        };
        let (first, first_handle) = SingleDeviceHub::new(config.clone(), material.clone()).unwrap();
        assert!(matches!(
            SingleDeviceHub::new(config.clone(), material.clone()),
            Err(HubServiceError::StateDirectoryLock(
                StateDirectoryLockError::Busy
            ))
        ));
        drop(first);
        assert!(matches!(
            SingleDeviceHub::new(config.clone(), material.clone()),
            Err(HubServiceError::StateDirectoryLock(
                StateDirectoryLockError::Busy
            ))
        ));
        drop(first_handle);
        let (restarted, restarted_handle) = SingleDeviceHub::new(config, material).unwrap();
        drop(restarted);
        drop(restarted_handle);
        let _ = std::fs::remove_dir_all(state_dir);
    }

    #[test]
    fn startup_preserves_stable_device_id_across_dual_signed_device_rotation() {
        let state_dir = test_state_dir("device-rotation");
        let config = HubServiceConfig {
            state_dir: state_dir.clone(),
            heartbeat_timeout: Duration::from_secs(5),
            max_agent_session_lifetime: Duration::from_secs(60 * 60),
            agent_session_reauth_drain: Duration::from_secs(30),
            checkpoint_generation_rollover_bytes: 512 * 1024,
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
                grant_signer: grants.clone().into(),
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
                grant_signer: grants.clone().into(),
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
                grant_signer: grants.into(),
                device_verifier: new_device.verifying_key(),
                device_rotation: None,
            },
        )
        .unwrap();
        assert_eq!(restarted.device_id(), stable_device_id);
        let _ = std::fs::remove_dir_all(state_dir);
    }

    #[tokio::test]
    async fn handoff_begin_never_bypasses_desktop_quarantine() {
        let state_dir = test_state_dir("handoff-operator-quarantine");
        let device = DeviceIdentity::generate();
        let (_hub, handle) = SingleDeviceHub::new(
            HubServiceConfig {
                state_dir: state_dir.clone(),
                heartbeat_timeout: Duration::from_secs(5),
                max_agent_session_lifetime: Duration::from_secs(60 * 60),
                agent_session_reauth_drain: Duration::from_secs(30),
                checkpoint_generation_rollover_bytes: 512 * 1024,
                max_queued_per_device: 1,
                max_agent_sessions: 2,
                max_agent_session_starts_per_minute: 30,
            },
            HubProvisionedMaterial {
                hub_identity: HubIdentity::generate(),
                grant_signer: GrantAuthority::generate().into(),
                device_verifier: device.verifying_key(),
                device_rotation: None,
            },
        )
        .unwrap();
        let owner = OperationOwner::local_hub();
        let operation_id = "op-handoff-quarantine".to_owned();
        {
            let mut persistent = handle.inner.persistent.lock().await;
            persistent
                .execution
                .prepare(
                    OperationRef {
                        device_id: handle.device_id().to_owned(),
                        device_generation: 1,
                        operation_id: operation_id.clone(),
                    },
                    owner.clone(),
                    crate::v2_m0::DeviceCapability::Shell,
                    1,
                )
                .unwrap();
            persistent
                .execution
                .mark_dispatched(&operation_id, &owner, 1, 2)
                .unwrap();
            persistent
                .execution
                .mark_connection_lost(&operation_id, 3)
                .unwrap();
            persist_locked(&handle.inner, &persistent).unwrap();
        }

        assert_eq!(
            handle
                .handoff_request(RemoteHandoffRequestKind::Operator {
                    command: RemoteHandoffOperatorCommand::Begin,
                    authority: None,
                })
                .await,
            Err(HubCommandError::DeviceIndeterminate { operation_id })
        );
        let _ = std::fs::remove_dir_all(state_dir);
    }

    #[tokio::test]
    async fn handoff_lifecycle_control_is_rejected_while_device_work_is_active() {
        let state_dir = test_state_dir("handoff-operator-busy");
        let device = DeviceIdentity::generate();
        let (_hub, handle) = SingleDeviceHub::new(
            HubServiceConfig {
                state_dir: state_dir.clone(),
                heartbeat_timeout: Duration::from_secs(5),
                max_agent_session_lifetime: Duration::from_secs(60 * 60),
                agent_session_reauth_drain: Duration::from_secs(30),
                checkpoint_generation_rollover_bytes: 512 * 1024,
                max_queued_per_device: 1,
                max_agent_sessions: 2,
                max_agent_session_starts_per_minute: 30,
            },
            HubProvisionedMaterial {
                hub_identity: HubIdentity::generate(),
                grant_signer: GrantAuthority::generate().into(),
                device_verifier: device.verifying_key(),
                device_rotation: None,
            },
        )
        .unwrap();
        let owner = OperationOwner::local_hub();
        {
            let mut persistent = handle.inner.persistent.lock().await;
            persistent
                .execution
                .prepare(
                    OperationRef {
                        device_id: handle.device_id().to_owned(),
                        device_generation: 1,
                        operation_id: "op-handoff-operator-busy".into(),
                    },
                    owner,
                    crate::v2_m0::DeviceCapability::Shell,
                    1,
                )
                .unwrap();
            persist_locked(&handle.inner, &persistent).unwrap();
        }

        assert_eq!(
            handle
                .handoff_request(RemoteHandoffRequestKind::Operator {
                    command: RemoteHandoffOperatorCommand::Begin,
                    authority: None,
                })
                .await,
            Err(HubCommandError::Busy)
        );
        let _ = std::fs::remove_dir_all(state_dir);
    }

    #[tokio::test]
    async fn shutdown_drain_closes_new_admission_gate() {
        let state_dir = test_state_dir("shutdown-drain-admission");
        let device = DeviceIdentity::generate();
        let (_hub, handle) = SingleDeviceHub::new(
            HubServiceConfig {
                state_dir: state_dir.clone(),
                heartbeat_timeout: Duration::from_secs(5),
                max_agent_session_lifetime: Duration::from_secs(60 * 60),
                agent_session_reauth_drain: Duration::from_secs(30),
                checkpoint_generation_rollover_bytes: 512 * 1024,
                max_queued_per_device: 1,
                max_agent_sessions: 2,
                max_agent_session_starts_per_minute: 30,
            },
            HubProvisionedMaterial {
                hub_identity: HubIdentity::generate(),
                grant_signer: GrantAuthority::generate().into(),
                device_verifier: device.verifying_key(),
                device_rotation: None,
            },
        )
        .unwrap();

        assert!(handle.begin_shutdown_drain());
        assert!(handle.is_shutdown_draining());
        assert!(!handle.begin_shutdown_drain());
        assert!(matches!(
            handle.start_command(DeviceCommand::ScreenGeometry).await,
            Err(HubCommandError::Busy)
        ));
        let _ = std::fs::remove_dir_all(state_dir);
    }

    #[tokio::test]
    async fn shutdown_drain_waits_for_already_admitted_work_to_settle() {
        let state_dir = test_state_dir("shutdown-drain-wait");
        let device = DeviceIdentity::generate();
        let (_hub, handle) = SingleDeviceHub::new(
            HubServiceConfig {
                state_dir: state_dir.clone(),
                heartbeat_timeout: Duration::from_secs(5),
                max_agent_session_lifetime: Duration::from_secs(60 * 60),
                agent_session_reauth_drain: Duration::from_secs(30),
                checkpoint_generation_rollover_bytes: 512 * 1024,
                max_queued_per_device: 1,
                max_agent_sessions: 2,
                max_agent_session_starts_per_minute: 30,
            },
            HubProvisionedMaterial {
                hub_identity: HubIdentity::generate(),
                grant_signer: GrantAuthority::generate().into(),
                device_verifier: device.verifying_key(),
                device_rotation: None,
            },
        )
        .unwrap();
        let owner = OperationOwner::local_hub();
        let operation_id = "op-shutdown-drain".to_owned();
        {
            let mut persistent = handle.inner.persistent.lock().await;
            persistent
                .execution
                .prepare(
                    OperationRef {
                        device_id: handle.device_id().to_owned(),
                        device_generation: 1,
                        operation_id: operation_id.clone(),
                    },
                    owner.clone(),
                    crate::v2_m0::DeviceCapability::ScreenGeometry,
                    1,
                )
                .unwrap();
            persist_locked(&handle.inner, &persistent).unwrap();
        }

        handle.begin_shutdown_drain();
        assert!(
            tokio::time::timeout(Duration::from_millis(40), handle.wait_for_shutdown_drain())
                .await
                .is_err(),
            "drain must wait while admitted work is non-terminal"
        );

        {
            let mut persistent = handle.inner.persistent.lock().await;
            let decision = persistent
                .execution
                .request_cancel(&operation_id, &owner, 1, 2)
                .unwrap();
            assert!(matches!(
                decision,
                CancellationDecision::CancelledBeforeDispatch { .. }
            ));
            persist_locked(&handle.inner, &persistent).unwrap();
        }
        tokio::time::timeout(Duration::from_millis(200), handle.wait_for_shutdown_drain())
            .await
            .expect("drain should finish after durable settlement");
        let _ = std::fs::remove_dir_all(state_dir);
    }

    #[test]
    fn process_policy_error_code_survives_hub_recovery_boundary() {
        for capability in [DeviceCapability::ExecuteProcess, DeviceCapability::Shell] {
            let result = DeviceResult::Error {
                code: DeviceErrorCode::WorkingDirectoryDenied,
            };
            assert_eq!(
                recoverable_result_for(capability, &result),
                Some(RecoverableOperationResult::Error {
                    code: DeviceErrorCode::WorkingDirectoryDenied,
                })
            );
        }
    }

    #[test]
    fn effectful_recovery_marker_never_copies_gui_or_browser_payloads() {
        assert_eq!(
            recoverable_result_for(DeviceCapability::TypeText, &DeviceResult::TypeTextCompleted,),
            Some(RecoverableOperationResult::EffectfulStatus)
        );
        assert_eq!(
            recoverable_result_for(
                DeviceCapability::PointerClick,
                &DeviceResult::PointerClickCompleted,
            ),
            Some(RecoverableOperationResult::EffectfulStatus)
        );
        assert_eq!(
            recoverable_result_for(
                DeviceCapability::ScreenGeometry,
                &DeviceResult::ScreenGeometry {
                    width_points: 100,
                    height_points: 100,
                    scale_factor_milli: 1000,
                },
            ),
            None
        );
        let encoded = serde_json::to_string(&RecoverableOperationResult::EffectfulStatus).unwrap();
        assert_eq!(encoded, r#"{"type":"effectful_status"}"#);
    }

    #[derive(Clone)]
    struct CaptureWriter(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);

    struct CaptureSink(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);

    impl std::io::Write for CaptureSink {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for CaptureWriter {
        type Writer = CaptureSink;

        fn make_writer(&'a self) -> Self::Writer {
            CaptureSink(self.0.clone())
        }
    }

    #[test]
    fn unexpected_protocol_message_log_and_error_are_payload_free() {
        let marker = "RAW_STDOUT_SECRET_DO_NOT_LOG";
        let stderr_marker = "RAW_STDERR_SECRET_DO_NOT_LOG";
        let message = AgentToHub::Result(RemoteResult {
            schema_version: crate::v2_m0_transport::HUB_AGENT_SCHEMA_VERSION,
            result: crate::v2_m0::CommandResultEnvelope {
                schema_version: CONTROL_SCHEMA_VERSION,
                device_id: "dev-fixture".into(),
                device_generation: 7,
                capability_revision: 3,
                operation_id: "op-fixture".into(),
                result: DeviceResult::Process {
                    output: ProcessOutput {
                        exit_code: Some(1),
                        stdout: marker.into(),
                        stderr: stderr_marker.into(),
                        stdout_truncated: false,
                        stderr_truncated: false,
                        timed_out: false,
                        cancelled: false,
                        duration_ms: 1,
                    },
                },
            },
            signature: b"RAW_SIGNATURE_SECRET_DO_NOT_LOG".to_vec(),
        });
        let bytes = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let subscriber = tracing_subscriber::fmt()
            .without_time()
            .with_writer(CaptureWriter(bytes.clone()))
            .finish();
        let error = tracing::subscriber::with_default(subscriber, || {
            unexpected_agent_message("hello", &message)
        });
        let log = String::from_utf8(bytes.lock().unwrap().clone()).unwrap();
        let rendered = format!("{error:?} {error}");
        for forbidden in [marker, stderr_marker, "RAW_SIGNATURE_SECRET_DO_NOT_LOG"] {
            assert!(!log.contains(forbidden), "log leaked marker {forbidden}");
            assert!(
                !rendered.contains(forbidden),
                "safe error leaked marker {forbidden}"
            );
        }
        assert!(log.contains("message_kind"));
        assert!(log.contains("result"));
        assert!(rendered.contains("got: \"result\"") || rendered.contains("got=result"));
    }

    #[test]
    fn quarantine_creation_emits_dedicated_error_alert_with_safe_correlation() {
        let bytes = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let subscriber = tracing_subscriber::fmt()
            .with_ansi(false)
            .without_time()
            .with_writer(CaptureWriter(bytes.clone()))
            .finish();
        tracing::subscriber::with_default(subscriber, || {
            emit_quarantine_created_alert(
                "op-alert-fixture",
                "dev-alert-fixture",
                7,
                Some(DeviceCapability::Shell),
                IndeterminateReason::ConnectionLost,
            );
        });
        let log = String::from_utf8(bytes.lock().unwrap().clone()).unwrap();
        for expected in [
            "ERROR",
            "v2_quarantine_created",
            "op-alert-fixture",
            "dev-alert-fixture",
            "operator_action_required",
            "connection_lost",
        ] {
            assert!(
                log.contains(expected),
                "missing quarantine alert field {expected}"
            );
        }
    }

    #[test]
    fn indeterminate_and_resolution_events_keep_correlation_fields() {
        let source = include_str!("v2_m1_hub.rs");
        for event in ["v2_operation_indeterminate", "v2_quarantine_resolved"] {
            let start = source.find(event).expect("event exists");
            let end = (start + 1_200).min(source.len());
            let block = &source[start..end];
            for field in ["operation_id", "device_id", "generation"] {
                assert!(block.contains(field), "{event} missing {field}");
            }
        }
    }

    #[test]
    fn reconciliation_commits_candidate_before_live_clear_and_has_no_dispatch_path() {
        let source = include_str!("v2_m1_hub.rs");
        let start = source
            .find("async fn handle_reconciliation_report(")
            .unwrap();
        let end = source[start..]
            .find("async fn handle_result(")
            .map(|offset| start + offset)
            .unwrap();
        let block = &source[start..end];
        let durable = block.find("checkpoint.save_with_size(&state)").unwrap();
        let live_swap = block.find("persistent.execution = candidate").unwrap();
        assert!(durable < live_swap);
        for forbidden in [
            "dispatch_operation(",
            "HubToAgent::Command",
            "start_command(",
            "request_fingerprint",
            "compare_request_fingerprint",
        ] {
            assert!(
                !block.contains(forbidden),
                "reconciliation must not contain {forbidden}"
            );
        }
        assert!(block.contains("ReconciliationStatus::AutoReconciling"));
        assert!(block.contains("mark_reconciliation_evidence_gap"));
        assert!(block.contains("mark_reconciliation_operator_required"));
    }
}
