//! Operator-facing V2-M1 outbound Agent runtime over gRPC bidirectional streaming.
//!
//! The runtime keeps the V2 application protocol independently signed. gRPC/TLS
//! supplies the long-lived full-duplex carrier while the Agent owns heartbeat,
//! reconnect, grant validation, replay barriers, direct process execution, and
//! cancellation. An optional external Cua MCP adapter adds typed GUI capabilities.

use crate::mutation_authority::{MutationAuthorityGate, MutationAuthorityRole};
use crate::v2_agent_handoff::{AgentHandoffCoordinator, AgentHandoffSessionFence};
use crate::v2_browser_execute::BrowserRefusalReason;
use crate::v2_browser_staging::{
    BrowserDownloadStagingBroker, BrowserDownloadStagingError, BrowserStagingStartupError,
    BrowserUploadStagingBroker, BrowserUploadStagingError,
};
use crate::v2_ephemeral_data_refs::{
    AgentEphemeralBinding, AgentEphemeralDataError, AgentEphemeralDataLimits,
    AgentEphemeralDataStore, EphemeralDataKind,
};
use crate::v2_execution_safety::{
    AgentTerminalEvidence, BackendExecutionReceipt, BackendReceiptTargetBinding,
    MAX_AGENT_BACKEND_EXECUTION_RECEIPTS, MAX_AGENT_TERMINAL_EVIDENCE_ENTRIES,
    terminal_evidence_for_device_result,
};
use crate::v2_linux_cgroup::LinuxCgroupV2Containment;
use crate::v2_m0::{
    AgentProcessOutputLocator, AgentProcessOutputLocators, CAPABILITY_SCHEMA_VERSION,
    CONTROL_SCHEMA_VERSION, CapabilityAdvertisement, CapabilityClass, CommandResultEnvelope,
    DeviceCapability, DeviceCommand, DeviceErrorCode, DeviceResult, DeviceSession, GrantLedger,
    ProcessOutputStream, VerificationStatus,
};
use crate::v2_m0_execution::{AgentExecutionGate, OperationRef};
use crate::v2_m0_transport::{
    AgentHello, AgentToHub, CancellationDisposition, HubChallenge, HubToAgent,
    RemoteBackendSessionEnd, RemoteHandoffErrorCode, RemoteHandoffResponseKind,
    RemoteIndeterminateCause, TrustedSessionClock, build_agent_heartbeat, build_agent_proof,
    build_remote_backend_session_ended, build_remote_cancellation_ack,
    build_remote_handoff_response, build_remote_indeterminate_ack,
    build_remote_reconciliation_report, build_remote_result, verify_hub_challenge,
    verify_hub_heartbeat_ack, verify_remote_backend_session_end, verify_remote_cancel,
    verify_remote_command, verify_remote_handoff_request, verify_session_accepted,
};
use crate::v2_m0_trust::TrustedHubIdentity;
use crate::v2_m1::ReconnectPolicy;
use crate::v2_m1_backend::{
    BackendExecutionOutcome, BackendExecutionReceiptContext, ComputerUseBackendAdapter,
    CuaMcpAdapter, M1BackendError,
};
use crate::v2_m1_filesystem::{FilesystemError, FilesystemExecutor, FilesystemPolicy};
use crate::v2_m1_grpc::{
    decode_hub_frame, encode_agent_frame, proto::agent_control_client::AgentControlClient,
};
use crate::v2_m1_keys::AgentProvisionedMaterial;
use crate::v2_m1_persistence::{AgentPersistentState, CheckpointStore, PersistenceError};
use crate::v2_m1_process::{
    CapturedProcessOutput, ProcessCancellation, ProcessError, ProcessExecutor, ProcessPolicy,
    ProcessUnprovenStage,
};
use crate::v2_m1_shell::{ShellError, ShellExecutor};
use crate::v2_m1_workspace_mutation::{
    WorkspaceMutationError, WorkspaceMutationExecutor, WorkspaceMutationPolicy, sha256_hex,
};
use crate::v2_managed_job::{ManagedJobError, ManagedJobLimits, ManagedJobManager};
use crate::v2_observability::SafeErrorCode;
use crate::v2_online_recovery::{
    RecoveryAuthorization, RecoveryError, RecoveryPhase, RecoveryResolved, clear_authorization,
    clear_recovery_handoff, clear_recovery_resolved, load_authorization, load_challenge,
    recovery_decision_name, recovery_phase_name, store_challenge, store_recovery_resolved,
    validate_authorization_against_challenge, verify_recovery_challenge,
    verify_recovery_resolved_for_authorization,
};
use crate::v2_operator_handoff::{
    VerificationToken, is_exact_verification_candidate, is_phase1_protected_command,
};
use crate::v2_playwright_sandbox::{
    PlaywrightSandboxConfig, PlaywrightSandboxError, PlaywrightSandboxRunner,
};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use std::collections::VecDeque;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::sync::{mpsc, watch};
use tokio_stream::wrappers::ReceiverStream;
use tonic::Code;
use tonic::transport::{Certificate, Channel, ClientTlsConfig, Endpoint};

const GRPC_QUEUE_DEPTH: usize = 8;
const MANAGED_JOB_SAFETY_POLL_INTERVAL: Duration = Duration::from_millis(100);

fn persist_verified_recovery_completion(
    state_dir: &Path,
    trusted_hub: &ed25519_dalek::VerifyingKey,
    expected_authorization: &RecoveryAuthorization,
    resolved: &RecoveryResolved,
) -> Result<(), AgentServiceError> {
    verify_recovery_resolved_for_authorization(resolved, trusted_hub, expected_authorization)
        .map_err(AgentServiceError::OnlineRecovery)?;
    store_recovery_resolved(state_dir, resolved).map_err(AgentServiceError::OnlineRecovery)?;
    clear_recovery_handoff(state_dir).map_err(AgentServiceError::OnlineRecovery)
}

#[derive(Debug, Clone)]
pub struct CuaAgentConfig {
    pub command: String,
    pub args: Vec<String>,
    pub backend_version: String,
    pub platform: String,
    pub revision: u64,
    pub connect_timeout: Duration,
    pub tool_timeout: Duration,
    pub reconnect_attempts: u32,
    pub reconnect_backoff: Duration,
    pub mutation_authority_dir: Option<PathBuf>,
}

impl CuaAgentConfig {
    fn validate(&self) -> Result<(), AgentServiceError> {
        if self.command.trim().is_empty()
            || self.backend_version.trim().is_empty()
            || self.platform.trim().is_empty()
            || self.revision == 0
            || self.connect_timeout.is_zero()
            || self.tool_timeout.is_zero()
            || self.reconnect_attempts == 0
            || self.reconnect_backoff.is_zero()
        {
            return Err(AgentServiceError::InvalidConfig(
                "invalid Cua backend settings",
            ));
        }
        Ok(())
    }

    fn adapter(&self) -> CuaMcpAdapter {
        let adapter = CuaMcpAdapter::new(
            self.command.clone(),
            self.args.clone(),
            self.backend_version.clone(),
            self.platform.clone(),
            self.revision,
            self.connect_timeout,
            self.tool_timeout,
            self.reconnect_attempts,
            self.reconnect_backoff,
        );
        match &self.mutation_authority_dir {
            Some(directory) => adapter.with_mutation_authority(MutationAuthorityGate::new(
                directory,
                MutationAuthorityRole::V2,
            )),
            None => adapter,
        }
    }
}

#[derive(Debug, Clone)]
pub struct AgentServiceConfig {
    pub hub_endpoint: String,
    pub hub_domain: String,
    pub device_id: String,
    pub allowed_cwd_roots: Vec<PathBuf>,
    pub allowed_file_roots: Vec<PathBuf>,
    /// Effectful workspace mutation roots. Empty disables the capability.
    /// These never inherit from cwd or read-only roots.
    pub allowed_write_roots: Vec<PathBuf>,
    /// Explicit deny subpaths inside writable roots. Deny wins over allow.
    pub denied_write_subpaths: Vec<PathBuf>,
    pub state_dir: PathBuf,
    /// Dedicated non-authoritative storage parent for short-lived workspace/process data.
    /// None disables retrievable extended process output until packaging configures it.
    pub ephemeral_data_parent: Option<PathBuf>,
    pub heartbeat_interval: Duration,
    pub reconnect: ReconnectPolicy,
    pub cua: Option<CuaAgentConfig>,
}

impl AgentServiceConfig {
    pub fn validate(&self) -> Result<(), AgentServiceError> {
        if !self.hub_endpoint.starts_with("https://") {
            return Err(AgentServiceError::InvalidConfig(
                "Hub endpoint must use https://",
            ));
        }
        if self.hub_domain.trim().is_empty() || self.device_id.trim().is_empty() {
            return Err(AgentServiceError::InvalidConfig(
                "Hub domain and device id must be non-empty",
            ));
        }
        if self.heartbeat_interval.is_zero() {
            return Err(AgentServiceError::InvalidConfig(
                "heartbeat interval must be non-zero",
            ));
        }
        self.reconnect
            .validate()
            .map_err(|_| AgentServiceError::InvalidConfig("invalid reconnect policy"))?;
        if self.allowed_cwd_roots.is_empty() {
            return Err(AgentServiceError::InvalidConfig(
                "at least one allowed cwd root is required",
            ));
        }
        if self.allowed_file_roots.is_empty() {
            return Err(AgentServiceError::InvalidConfig(
                "at least one allowed file root is required",
            ));
        }
        if self.allowed_write_roots.is_empty() && !self.denied_write_subpaths.is_empty() {
            return Err(AgentServiceError::InvalidConfig(
                "workspace mutation deny subpaths require at least one writable root",
            ));
        }
        if let Some(cua) = &self.cua {
            cua.validate()?;
        }
        Ok(())
    }
}

pub struct AgentService {
    config: AgentServiceConfig,
    material: AgentProvisionedMaterial,
    executor: ProcessExecutor,
    managed_jobs: ManagedJobManager,
    playwright: Option<PlaywrightSandboxRunner>,
    managed_job_fail_closed: bool,
    shell: ShellExecutor,
    filesystem: FilesystemExecutor,
    workspace_mutation: Option<WorkspaceMutationExecutor>,
    ephemeral_data: Option<Arc<StdMutex<AgentEphemeralDataStore>>>,
    browser_upload_staging: BrowserUploadStagingBroker,
    browser_download_staging: BrowserDownloadStagingBroker,
    computer_use: Option<Arc<dyn ComputerUseBackendAdapter>>,
    handoff: Option<Arc<AgentHandoffCoordinator>>,
    trusted_hub: TrustedHubIdentity,
    grants: GrantLedger,
    execution: AgentExecutionGate,
    terminal_evidence: VecDeque<AgentTerminalEvidence>,
    backend_execution_receipts: VecDeque<BackendExecutionReceipt>,
    checkpoint: CheckpointStore,
}

fn reconcile_grant_verifiers(grants: &mut GrantLedger, material: &AgentProvisionedMaterial) {
    let mut configured = vec![material.grant_verifier];
    configured.extend(material.additional_grant_verifiers.iter().copied());
    let configured_ids: std::collections::HashSet<_> = configured
        .iter()
        .map(crate::v2_m0::verifying_key_id)
        .collect();
    for verifier in configured {
        grants.trust_verifier(verifier);
    }
    let existing = grants.snapshot().verifier_keys;
    for key in existing {
        if let Ok(verifier) = ed25519_dalek::VerifyingKey::from_bytes(&key) {
            let key_id = crate::v2_m0::verifying_key_id(&verifier);
            if !configured_ids.contains(&key_id) {
                grants.retire_verifier(&key_id);
            }
        }
    }
}

impl AgentService {
    pub fn new(
        config: AgentServiceConfig,
        material: AgentProvisionedMaterial,
    ) -> Result<Self, AgentServiceError> {
        config.validate()?;
        let computer_use = config
            .cua
            .as_ref()
            .map(|cua| Arc::new(cua.adapter()) as Arc<dyn ComputerUseBackendAdapter>);
        Self::build(config, material, computer_use)
    }

    pub fn new_with_computer_use_backend(
        config: AgentServiceConfig,
        material: AgentProvisionedMaterial,
        backend: Arc<dyn ComputerUseBackendAdapter>,
    ) -> Result<Self, AgentServiceError> {
        if config.cua.is_some() {
            return Err(AgentServiceError::InvalidConfig(
                "custom Computer Use backend conflicts with Cua config",
            ));
        }
        config.validate()?;
        Self::build(config, material, Some(backend))
    }

    fn build(
        config: AgentServiceConfig,
        material: AgentProvisionedMaterial,
        computer_use: Option<Arc<dyn ComputerUseBackendAdapter>>,
    ) -> Result<Self, AgentServiceError> {
        let executor = ProcessExecutor::new(
            ProcessPolicy::developer_defaults(config.allowed_cwd_roots.clone())
                .map_err(AgentServiceError::Process)?,
        );
        let managed_jobs = ManagedJobManager::new(executor.clone(), ManagedJobLimits::default())
            .map_err(AgentServiceError::ManagedJob)?;
        let shell = ShellExecutor::new(
            ProcessPolicy::developer_defaults(config.allowed_cwd_roots.clone())
                .map_err(AgentServiceError::Process)?,
        );
        let filesystem = FilesystemExecutor::new(
            FilesystemPolicy::new(config.allowed_file_roots.clone())
                .map_err(AgentServiceError::Filesystem)?,
        );
        let workspace_mutation = if config.allowed_write_roots.is_empty() {
            None
        } else {
            Some(WorkspaceMutationExecutor::new(
                WorkspaceMutationPolicy::new(
                    config.allowed_write_roots.clone(),
                    config.denied_write_subpaths.clone(),
                )
                .map_err(AgentServiceError::WorkspaceMutationStartup)?,
            ))
        };
        // Loading the checkpoint first establishes/hardens the private state root.
        // Browser transfer staging is created only after that reviewed root exists.
        let checkpoint = CheckpointStore::new(config.state_dir.clone(), "agent")
            .map_err(AgentServiceError::Persistence)?;
        let (
            trusted_hub,
            grants,
            execution,
            terminal_evidence,
            backend_execution_receipts,
            managed_job_fail_closed,
        ) = match checkpoint.load_latest::<AgentPersistentState>() {
            Ok(state) => {
                let restored = state
                    .restore_with_runtime_safety()
                    .map_err(AgentServiceError::Persistence)?;
                if restored.device_id != config.device_id {
                    return Err(AgentServiceError::CheckpointIdentityMismatch);
                }
                let mut trusted_hub = restored.trusted_hub;
                let mut grants = restored.grant_ledger;
                if trusted_hub.verifier() != material.trusted_hub {
                    let rotation = material
                        .hub_rotation
                        .as_ref()
                        .ok_or(AgentServiceError::CheckpointTrustMismatch)?;
                    trusted_hub
                        .apply_rotation(rotation)
                        .map_err(AgentServiceError::Trust)?;
                    if trusted_hub.verifier() != material.trusted_hub {
                        return Err(AgentServiceError::CheckpointTrustMismatch);
                    }
                }
                reconcile_grant_verifiers(&mut grants, &material);
                (
                    trusted_hub,
                    grants,
                    restored.execution,
                    restored.terminal_evidence.into_iter().collect(),
                    restored.backend_execution_receipts.into_iter().collect(),
                    restored.managed_job_fail_closed,
                )
            }
            Err(PersistenceError::NoCheckpoint) => {
                let mut grants = GrantLedger::new(material.grant_verifier);
                reconcile_grant_verifiers(&mut grants, &material);
                (
                    TrustedHubIdentity::new(material.trusted_hub),
                    grants,
                    AgentExecutionGate::default(),
                    VecDeque::new(),
                    VecDeque::new(),
                    false,
                )
            }
            Err(error) => return Err(AgentServiceError::Persistence(error)),
        };
        let ephemeral_data = match config.ephemeral_data_parent.as_deref() {
            Some(parent) => Some(Arc::new(StdMutex::new(
                AgentEphemeralDataStore::new(parent, AgentEphemeralDataLimits::default())
                    .map_err(AgentServiceError::EphemeralDataStartup)?,
            ))),
            None => None,
        };
        let browser_upload_staging = match BrowserUploadStagingBroker::new(&config.state_dir) {
            Ok(staging) => staging,
            Err(error) => {
                emit_browser_staging_startup_failure(
                    "upload",
                    error.upload_safe_error_code(),
                    &error,
                );
                return Err(AgentServiceError::BrowserUploadStagingStartup(error));
            }
        };
        let browser_download_staging = match BrowserDownloadStagingBroker::new(&config.state_dir) {
            Ok(staging) => staging,
            Err(error) => {
                emit_browser_staging_startup_failure(
                    "download",
                    error.download_safe_error_code(),
                    &error,
                );
                return Err(AgentServiceError::BrowserDownloadStagingStartup(error));
            }
        };
        let service = Self {
            config,
            material,
            executor,
            managed_jobs,
            playwright: None,
            managed_job_fail_closed,
            shell,
            filesystem,
            workspace_mutation,
            ephemeral_data,
            browser_upload_staging,
            browser_download_staging,
            computer_use,
            handoff: None,
            trusted_hub,
            grants,
            execution,
            terminal_evidence,
            backend_execution_receipts,
            checkpoint,
        };
        // Establish a baseline checkpoint before accepting any command.
        service.persist_state()?;
        Ok(service)
    }

    pub fn with_handoff_coordinator(mut self, coordinator: Arc<AgentHandoffCoordinator>) -> Self {
        self.handoff = Some(coordinator);
        self
    }

    pub fn with_linux_cgroup_v2(mut self, root: PathBuf) -> Result<Self, AgentServiceError> {
        let containment = Arc::new(LinuxCgroupV2Containment::new(root).map_err(|error| {
            AgentServiceError::Process(ProcessError::LinuxCgroupUnavailable(error))
        })?);
        let policy = ProcessPolicy::developer_defaults(self.config.allowed_cwd_roots.clone())
            .map_err(AgentServiceError::Process)?;
        self.executor =
            ProcessExecutor::new(policy.clone()).with_linux_cgroup_v2(containment.clone());
        self.shell = ShellExecutor::from_process_executor(
            ProcessExecutor::new(policy).with_linux_cgroup_v2(containment),
        );
        Ok(self)
    }

    pub fn with_playwright_sandbox(
        mut self,
        config: PlaywrightSandboxConfig,
    ) -> Result<Self, AgentServiceError> {
        let runner =
            PlaywrightSandboxRunner::new(config, &self.config.state_dir, &self.config.device_id)
                .map_err(AgentServiceError::PlaywrightSandbox)?;
        runner
            .probe_provider()
            .map_err(AgentServiceError::PlaywrightSandbox)?;
        runner
            .recover_provider_orphans()
            .map_err(AgentServiceError::PlaywrightSandbox)?;
        self.playwright = Some(runner);
        Ok(self)
    }

    fn persist_state(&self) -> Result<(), AgentServiceError> {
        let terminal_evidence: Vec<_> = self.terminal_evidence.iter().cloned().collect();
        let backend_execution_receipts: Vec<_> = self
            .backend_execution_receipts
            .iter()
            .filter(|receipt| {
                terminal_evidence
                    .iter()
                    .any(|terminal| terminal == &receipt.as_terminal_evidence())
            })
            .cloned()
            .collect();
        let state = AgentPersistentState::capture_with_runtime_evidence(
            self.config.device_id.clone(),
            &self.trusted_hub,
            &self.grants,
            &self.execution,
            &terminal_evidence,
            &backend_execution_receipts,
            self.managed_job_fail_closed,
        )
        .map_err(|error| {
            record_agent_persistence_failure(&self.config.device_id, &error);
            AgentServiceError::Persistence(error)
        })?;
        if let Err(error) = self.checkpoint.save(&state) {
            record_agent_persistence_failure(&self.config.device_id, &error);
            return Err(AgentServiceError::Persistence(error));
        }
        Ok(())
    }

    pub fn capabilities(&self) -> CapabilityAdvertisement {
        let mut supported = vec![
            DeviceCapability::ExecuteProcess,
            DeviceCapability::Shell,
            DeviceCapability::ManagedJobControl,
            DeviceCapability::ManagedJobObserve,
            DeviceCapability::ReadFile,
            DeviceCapability::ListDirectory,
        ];
        if self.workspace_mutation.is_some() {
            supported.push(DeviceCapability::WriteWorkspaceFile);
        }
        if self.playwright.is_some() {
            supported.push(DeviceCapability::PlaywrightTestControl);
            supported.push(DeviceCapability::PlaywrightTestObserve);
        }
        if self.ephemeral_data.is_some() {
            supported.push(DeviceCapability::ReadProcessOutput);
        }
        let backend = if let Some(computer_use) = &self.computer_use {
            let advertisement = computer_use.advertisement();
            let backend = format!("agent-native+{}", advertisement.backend);
            for capability in advertisement.supported {
                if !supported.contains(&capability) {
                    supported.push(capability);
                }
            }
            backend
        } else {
            "agent-native".to_owned()
        };
        CapabilityAdvertisement {
            backend,
            backend_version: env!("CARGO_PKG_VERSION").into(),
            platform: format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH),
            capability_schema_version: CAPABILITY_SCHEMA_VERSION,
            revision: 8,
            supported,
        }
    }

    /// Run until shutdown or a bounded sequence of connection/session failures is exhausted.
    /// A successfully authenticated session resets the reconnect failure streak.
    pub async fn run(
        &mut self,
        mut shutdown: watch::Receiver<bool>,
    ) -> Result<(), AgentServiceError> {
        if let Some(computer_use) = &self.computer_use {
            if let Err(error) = computer_use.connect().await {
                tracing::error!(
                    event = "v2_backend_failure",
                    device_id = %self.config.device_id,
                    backend = %computer_use.advertisement().backend,
                    outcome = "failed",
                    error_code = error.safe_error_code(),
                    "Computer Use backend connection failed"
                );
                return Err(AgentServiceError::Backend(error));
            }
        }
        let result = self.run_connected_lifecycle(&mut shutdown).await;
        if let Some(computer_use) = &self.computer_use {
            let shutdown_result = computer_use
                .shutdown()
                .await
                .map_err(AgentServiceError::Backend);
            if result.is_ok() {
                shutdown_result?;
            }
        }
        result
    }

    async fn run_connected_lifecycle(
        &mut self,
        shutdown: &mut watch::Receiver<bool>,
    ) -> Result<(), AgentServiceError> {
        let mut failures = 0_u32;
        loop {
            if *shutdown.borrow() {
                return Ok(());
            }

            let channel = match self.connect_channel().await {
                Ok(channel) => channel,
                Err(error) => {
                    failures = failures.saturating_add(1);
                    if failures >= self.config.reconnect.max_attempts {
                        crate::v2_observability::reconnect_exhausted();
                        tracing::error!(
                            event = "v2_agent_reconnect_exhausted",
                            device_id = %self.config.device_id,
                            reconnect_attempt = failures,
                            outcome = "exhausted",
                            error_code = error.safe_error_code(),
                            "Agent Hub connection retries exhausted"
                        );
                        return Err(error);
                    }
                    crate::v2_observability::reconnect_attempt();
                    tracing::warn!(
                        event = "v2_agent_reconnect",
                        device_id = %self.config.device_id,
                        reconnect_attempt = failures,
                        outcome = "scheduled",
                        error_code = error.safe_error_code(),
                        "Agent Hub connection failed; reconnect scheduled"
                    );
                    let delay = self
                        .config
                        .reconnect
                        .delay_for_attempt(failures.saturating_sub(1))
                        .map_err(|_| AgentServiceError::ReconnectExhausted)?;
                    if sleep_or_shutdown(delay, shutdown).await {
                        return Ok(());
                    }
                    continue;
                }
            };

            match self.run_authenticated_session(channel, shutdown).await {
                Ok(SessionExit::Shutdown) => return Ok(()),
                Ok(SessionExit::Reconnect) => {
                    failures = 0;
                    crate::v2_observability::reconnect_attempt();
                    tracing::warn!(
                        event = "v2_agent_reconnect",
                        device_id = %self.config.device_id,
                        reconnect_attempt = 1_u32,
                        outcome = "scheduled",
                        error_code = "authenticated_session_reconnect",
                        "authenticated Agent session requested reconnect"
                    );
                    if sleep_or_shutdown(self.config.reconnect.initial_delay, shutdown).await {
                        return Ok(());
                    }
                }
                Err(error) if error.reconnectable() => {
                    failures = failures.saturating_add(1);
                    if failures >= self.config.reconnect.max_attempts {
                        crate::v2_observability::reconnect_exhausted();
                        tracing::error!(
                            event = "v2_agent_reconnect_exhausted",
                            device_id = %self.config.device_id,
                            reconnect_attempt = failures,
                            outcome = "exhausted",
                            error_code = error.safe_error_code(),
                            "Agent session reconnect retries exhausted"
                        );
                        return Err(error);
                    }
                    crate::v2_observability::reconnect_attempt();
                    tracing::warn!(
                        event = "v2_agent_reconnect",
                        device_id = %self.config.device_id,
                        reconnect_attempt = failures,
                        outcome = "scheduled",
                        error_code = error.safe_error_code(),
                        "Agent session transport failed; reconnect scheduled"
                    );
                    let delay = self
                        .config
                        .reconnect
                        .delay_for_attempt(failures.saturating_sub(1))
                        .map_err(|_| AgentServiceError::ReconnectExhausted)?;
                    if sleep_or_shutdown(delay, shutdown).await {
                        return Ok(());
                    }
                }
                Err(error) => {
                    tracing::error!(
                        event = "v2_agent_session_error",
                        device_id = %self.config.device_id,
                        outcome = "ended",
                        error_code = error.safe_error_code(),
                        "Agent session ended with a non-reconnectable error"
                    );
                    return Err(error);
                }
            }
        }
    }

    async fn connect_channel(&self) -> Result<Channel, AgentServiceError> {
        let root_pem = der_certificate_to_pem(&self.material.tls_root_der);
        let tls = ClientTlsConfig::new()
            .ca_certificate(Certificate::from_pem(root_pem))
            .domain_name(self.config.hub_domain.clone());
        Endpoint::from_shared(self.config.hub_endpoint.clone())
            .map_err(AgentServiceError::Transport)?
            .tls_config(tls)
            .map_err(AgentServiceError::Transport)?
            .connect()
            .await
            .map_err(AgentServiceError::Transport)
    }

    async fn run_authenticated_session(
        &mut self,
        channel: Channel,
        shutdown: &mut watch::Receiver<bool>,
    ) -> Result<SessionExit, AgentServiceError> {
        let mut client = AgentControlClient::new(channel)
            .max_decoding_message_size(crate::v2_m1_grpc::MAX_GRPC_TRANSPORT_MESSAGE_BYTES)
            .max_encoding_message_size(crate::v2_m1_grpc::MAX_GRPC_TRANSPORT_MESSAGE_BYTES);
        let (outbound_tx, outbound_rx) = mpsc::channel(GRPC_QUEUE_DEPTH);
        let mut inbound = client
            .open_session(ReceiverStream::new(outbound_rx))
            .await
            .map_err(AgentServiceError::Status)?
            .into_inner();

        let hello = AgentHello::new(self.config.device_id.clone(), self.capabilities());
        send_agent(&outbound_tx, AgentToHub::Hello(hello.clone())).await?;
        let challenge = match next_hub(&mut inbound).await? {
            HubToAgent::Challenge(challenge) => challenge,
            other => return Err(unexpected_hub_message("challenge", &other)),
        };
        verify_hub_challenge(&hello, &challenge, &self.trusted_hub.verifier())
            .map_err(AgentServiceError::Protocol)?;
        let proof = build_agent_proof(&self.material.device_identity, &hello, &challenge)
            .map_err(AgentServiceError::Protocol)?;
        send_agent(&outbound_tx, AgentToHub::Proof(proof)).await?;

        let accepted = match next_hub(&mut inbound).await? {
            HubToAgent::Accepted(accepted) => accepted,
            other => return Err(unexpected_hub_message("accepted", &other)),
        };
        verify_session_accepted(&hello, &challenge, &accepted, &self.trusted_hub.verifier())
            .map_err(AgentServiceError::Protocol)?;
        tracing::info!(
            event = "v2_agent_session_accepted",
            device_id = %self.config.device_id,
            generation = accepted.device_generation,
            backend = %hello.capabilities.backend,
            outcome = "accepted",
            "Hub accepted Agent session"
        );
        let session = DeviceSession {
            device_id: self.config.device_id.clone(),
            generation: accepted.device_generation,
            capabilities: hello.capabilities.clone(),
        };
        if self.managed_job_fail_closed {
            tracing::error!(
                event = "v2_managed_job_fail_closed",
                device_id = %session.device_id,
                generation = session.generation,
                outcome = "refused",
                error_code = "managed_job_fail_closed",
                "persisted managed-job termination ambiguity blocks Agent work"
            );
            return Err(AgentServiceError::ManagedJobFailClosed);
        }
        self.execution
            .prepare_generation(session.generation)
            .map_err(AgentServiceError::Execution)?;
        // Persist the generation rollover before accepting work so replay
        // tombstones from prior generations can be safely discarded on disk.
        self.persist_state()?;
        let report = build_remote_reconciliation_report(
            &self.material.device_identity,
            &hello,
            &challenge,
            session.generation,
            self.terminal_evidence.iter().cloned().collect(),
        )
        .map_err(AgentServiceError::Protocol)?;
        send_agent(&outbound_tx, AgentToHub::ReconciliationReport(report)).await?;
        // Staged upload handles never survive an Agent transport generation.
        self.browser_upload_staging
            .cleanup_all()
            .map_err(AgentServiceError::BrowserUploadStaging)?;
        self.browser_download_staging
            .cleanup_all()
            .map_err(AgentServiceError::BrowserDownloadStaging)?;
        // Recovery challenges are generation-bound. Never carry an authorization
        // across an authenticated reconnect; a fresh Hub challenge is required.
        clear_recovery_handoff(&self.config.state_dir)
            .map_err(AgentServiceError::OnlineRecovery)?;
        let trusted_clock = TrustedSessionClock::new(accepted.hub_time_ms);

        let session_result = self
            .run_session_loop(
                inbound,
                outbound_tx,
                hello,
                challenge,
                session,
                trusted_clock,
                shutdown,
            )
            .await;

        let managed_jobs = self.managed_jobs.clone();
        let playwright = self.playwright.clone();
        let cleanup = tokio::task::spawn_blocking(move || {
            let generic = managed_jobs.shutdown_all();
            let playwright_jobs = playwright
                .as_ref()
                .map(PlaywrightSandboxRunner::shutdown_jobs)
                .transpose();
            let artifact_cleanup: Result<Option<()>, PlaywrightSandboxError> =
                if playwright_jobs.is_ok() {
                    playwright
                        .as_ref()
                        .map(PlaywrightSandboxRunner::cleanup_artifacts)
                        .transpose()
                } else {
                    Ok(None)
                };
            (generic, playwright_jobs, artifact_cleanup)
        })
        .await
        .map_err(|_| AgentServiceError::ManagedJobFailClosed)?;
        let (generic_cleanup, playwright_cleanup, artifact_cleanup) = cleanup;
        if let Err(error) = &artifact_cleanup {
            tracing::warn!(
                event = "v2_playwright_artifact_cleanup_failed",
                device_id = %self.config.device_id,
                error_code = error.safe_error_code(),
                "Playwright artifact cleanup failed after session teardown"
            );
        }
        let termination_cleanup_failed = generic_cleanup.is_err() || playwright_cleanup.is_err();
        if termination_cleanup_failed {
            let error_code = if generic_cleanup.is_err() {
                "managed_job_cleanup_failed"
            } else {
                "playwright_cleanup_failed"
            };
            self.managed_job_fail_closed = true;
            self.persist_state()?;
            tracing::error!(
                event = "v2_managed_execution_cleanup_indeterminate",
                device_id = %self.config.device_id,
                outcome = "fail_closed",
                error_code,
                "managed execution cleanup could not be proven safe; persisted fail-closed state"
            );
            return Err(AgentServiceError::ManagedJobFailClosed);
        }
        session_result
    }

    #[allow(clippy::too_many_arguments)]
    async fn run_session_loop(
        &mut self,
        mut inbound: tonic::Streaming<crate::v2_m1_grpc::proto::HubFrame>,
        outbound_tx: mpsc::Sender<crate::v2_m1_grpc::proto::AgentFrame>,
        hello: AgentHello,
        challenge: HubChallenge,
        session: DeviceSession,
        trusted_clock: TrustedSessionClock,
        shutdown: &mut watch::Receiver<bool>,
    ) -> Result<SessionExit, AgentServiceError> {
        let mut heartbeat = tokio::time::interval(self.config.heartbeat_interval);
        heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let mut managed_job_safety_poll = tokio::time::interval(MANAGED_JOB_SAFETY_POLL_INTERVAL);
        managed_job_safety_poll.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        // The immediate first tick establishes liveness as soon as authentication finishes.
        let mut heartbeat_sequence = 0_u64;
        let mut pending_heartbeat: Option<(u64, Instant)> = None;
        let heartbeat_deadline = self.config.heartbeat_interval.saturating_mul(3);

        let (operation_done_tx, mut operation_done_rx) = mpsc::channel::<OperationCompletion>(1);
        let mut active: Option<ActiveOperation> = None;
        let mut pending_backend_session_ends = VecDeque::<RemoteBackendSessionEnd>::new();
        let mut recovery_poll = tokio::time::interval(Duration::from_millis(250));
        recovery_poll.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let mut pending_recovery_request: Option<RecoveryAuthorization> = None;
        let mut last_recovery_warning: Option<(RecoveryPhase, [u8; 32])> = None;

        let session_result = async {
            loop {
                tokio::select! {
                changed = shutdown.changed() => {
                    if changed.is_err() || *shutdown.borrow() {
                        terminate_active(
                            &mut active,
                            &mut operation_done_rx,
                            &mut self.execution,
                            &mut self.terminal_evidence,
                            &mut self.backend_execution_receipts,
                            &session.device_id,
                        )
                        .await?;
                        self.persist_state()?;
                        return Ok(SessionExit::Shutdown);
                    }
                }
                _ = managed_job_safety_poll.tick() => {
                    let generic_indeterminate = self
                        .managed_jobs
                        .has_indeterminate_termination()
                        .map_err(AgentServiceError::ManagedJob)?;
                    let playwright_indeterminate = match &self.playwright {
                        Some(playwright) => playwright
                            .has_indeterminate_termination()
                            .map_err(AgentServiceError::PlaywrightSandbox)?,
                        None => false,
                    };
                    if generic_indeterminate || playwright_indeterminate {
                        self.managed_job_fail_closed = true;
                        self.persist_state()?;
                        tracing::error!(
                            event = "v2_managed_job_async_indeterminate",
                            device_id = %session.device_id,
                            generation = session.generation,
                            outcome = "fail_closed",
                            error_code = "managed_job_fail_closed",
                            "managed execution expiry/cleanup lost terminal proof; refusing further Agent work"
                        );
                        return Err(AgentServiceError::ManagedJobFailClosed);
                    }
                }
                _ = heartbeat.tick() => {
                    if pending_heartbeat.as_ref().is_some_and(|(_, sent)| sent.elapsed() >= heartbeat_deadline) {
                        terminate_active(
                            &mut active,
                            &mut operation_done_rx,
                            &mut self.execution,
                            &mut self.terminal_evidence,
                            &mut self.backend_execution_receipts,
                            &session.device_id,
                        )
                        .await?;
                        self.persist_state()?;
                        tracing::warn!(
                            event = "v2_agent_heartbeat_timeout",
                            device_id = %session.device_id,
                            generation = session.generation,
                            outcome = "reconnect",
                            error_code = "heartbeat_timeout",
                            "Hub heartbeat acknowledgement deadline expired"
                        );
                        return Ok(SessionExit::Reconnect);
                    }
                    if pending_heartbeat.is_none() {
                        heartbeat_sequence = heartbeat_sequence.saturating_add(1);
                        let message = build_agent_heartbeat(
                            &self.material.device_identity,
                            &hello,
                            &challenge,
                            session.generation,
                            heartbeat_sequence,
                        ).map_err(AgentServiceError::Protocol)?;
                        send_agent(&outbound_tx, AgentToHub::Heartbeat(message)).await?;
                        pending_heartbeat = Some((heartbeat_sequence, Instant::now()));
                    }
                }
                completion = operation_done_rx.recv(), if active.is_some() => {
                    let completion = completion.ok_or(AgentServiceError::OperationWorkerClosed)?;
                    let expected = active.take().ok_or(AgentServiceError::OperationStateMismatch)?;
                    if completion.operation_id != expected.operation_id
                        || completion.device_generation != expected.device_generation
                        || completion.device_generation != session.generation
                    {
                        return Err(AgentServiceError::OperationStateMismatch);
                    }
                    self.execution.finish(&completion.operation_id)
                        .map_err(AgentServiceError::Execution)?;
                    match completion.outcome {
                        AgentOperationOutcome::Result(result) => {
                            let mut device_result = match result {
                                Ok(result) => result,
                                Err(error) => {
                                    tracing::warn!(
                                        event = "v2_agent_operation_failed",
                                        operation_id = %completion.operation_id,
                                        device_id = %session.device_id,
                                        generation = session.generation,
                                        outcome = "failed",
                                        error_code = agent_operation_error_code(&error),
                                        "Agent operation failed without logging command or result payload"
                                    );
                                    DeviceResult::Error {
                                        code: operation_error_code(&error),
                                    }
                                }
                            };
                            if let Some(verification) = expected.handoff_verification.as_ref() {
                                let satisfied = matches!(
                                    device_result,
                                    DeviceResult::UiStateVerification {
                                        status: VerificationStatus::Satisfied,
                                        ..
                                    }
                                );
                                let reported = match self.handoff.as_ref() {
                                    Some(coordinator) => coordinator
                                        .report_verification_local(
                                            verification.authority.clone(),
                                            verification.token.clone(),
                                            satisfied,
                                            AgentHandoffSessionFence::for_device(
                                                    &session.device_id,
                                                    session.generation,
                                                    session.capabilities.revision,
                                                ),
                                        )
                                        .await
                                        .is_ok(),
                                    None => false,
                                };
                                if !reported
                                    && !matches!(
                                        device_result,
                                        DeviceResult::Error {
                                            code: DeviceErrorCode::BackendOutcomeIndeterminate
                                        }
                                    )
                                {
                                    device_result = DeviceResult::Error {
                                        code: DeviceErrorCode::HandoffVerificationUnavailable,
                                    };
                                }
                            }
                            record_terminal_evidence(
                                &mut self.terminal_evidence,
                                &session.device_id,
                                &expected,
                                &device_result,
                            )?;
                            // Persist both the replay tombstone and payload-free terminal proof
                            // before attempting result delivery. A lost transport can therefore
                            // recover evidence without re-executing the operation.
                            self.persist_state()?;
                            let result = CommandResultEnvelope {
                                schema_version: CONTROL_SCHEMA_VERSION,
                                device_id: session.device_id.clone(),
                                device_generation: session.generation,
                                capability_revision: session.capabilities.revision,
                                operation_id: completion.operation_id,
                                result: device_result,
                            };
                            let signed = build_remote_result(
                                &self.material.device_identity,
                                &hello,
                                &challenge,
                                result,
                            ).map_err(AgentServiceError::Protocol)?;
                            send_agent(&outbound_tx, AgentToHub::Result(signed)).await?;
                        }
                        AgentOperationOutcome::BackendReceipt(receipt) => {
                            record_backend_execution_receipt(
                                &mut self.backend_execution_receipts,
                                &mut self.terminal_evidence,
                                receipt,
                            )?;
                            // Receipt provenance and the derived #124 terminal proof are
                            // committed in one Agent checkpoint before transport churn.
                            self.persist_state()?;
                            tracing::info!(
                                event = "v2_agent_backend_receipt_committed",
                                operation_id = %completion.operation_id,
                                device_id = %session.device_id,
                                generation = session.generation,
                                outcome = "authoritative_receipt_persisted",
                                "authoritative backend receipt persisted; reconnecting for existing reconciliation path"
                            );
                            return Ok(SessionExit::Reconnect);
                        }
                        AgentOperationOutcome::Indeterminate(cause) => {
                            // No authoritative terminal proof exists for an indeterminate
                            // outcome. Persist only the replay barrier; never manufacture
                            // reconciliation evidence from timeout/cancellation heuristics.
                            self.persist_state()?;
                            match cause {
                                AgentIndeterminateCause::CancellationPropagated => {
                                    // The cancellation branch already queued a signed
                                    // IndeterminateAfterPropagation acknowledgement. Keep
                                    // this transport generation alive so that acknowledgement
                                    // cannot be lost merely because the local worker completed
                                    // first; the Hub quarantine blocks all further work.
                                    crate::v2_observability::backend_failure(
                                        crate::v2_observability::BackendFailureReason::AmbiguousOutcome,
                                    );
                                    tracing::warn!(
                                        event = "v2_agent_backend_indeterminate",
                                        operation_id = %completion.operation_id,
                                        device_id = %session.device_id,
                                        generation = session.generation,
                                        outcome = "indeterminate",
                                        indeterminate_reason = "cancellation_unproven",
                                        error_code = "backend_cancellation_ambiguous",
                                        "backend cancellation outcome is ambiguous; keeping session alive after signed quarantine acknowledgement"
                                    );
                                }
                                AgentIndeterminateCause::BackendTimedOut => {
                                    // Preserve only the payload-free ambiguity cause. This signed
                                    // acknowledgement is diagnostic/quarantine metadata, never
                                    // terminal evidence and never replay authority.
                                    let ack = build_remote_indeterminate_ack(
                                        &self.material.device_identity,
                                        &hello,
                                        &challenge,
                                        session.generation,
                                        completion.operation_id.clone(),
                                        RemoteIndeterminateCause::BackendTimedOut,
                                    )
                                    .map_err(AgentServiceError::Protocol)?;
                                    send_agent(&outbound_tx, AgentToHub::IndeterminateAck(ack))
                                        .await?;
                                    tracing::warn!(
                                        event = "v2_agent_backend_indeterminate",
                                        operation_id = %completion.operation_id,
                                        device_id = %session.device_id,
                                        generation = session.generation,
                                        outcome = "indeterminate",
                                        indeterminate_reason = "backend_timed_out",
                                        error_code = "backend_timeout_ambiguous",
                                        "backend timed out with ambiguous outcome; signed cause acknowledgement sent before reconnect"
                                    );
                                    return Ok(SessionExit::Reconnect);
                                }
                                AgentIndeterminateCause::ProcessOutcomeUnproven(stage) => {
                                    // The process/shell operation was dispatched to its local
                                    // worker, but the Agent cannot prove the spawn/terminal boundary
                                    // strongly enough to publish an ordinary result. Close
                                    // this authenticated generation so the Hub's existing
                                    // connection-loss path durably quarantines the operation.
                                    tracing::warn!(
                                        event = "v2_agent_process_indeterminate",
                                        operation_id = %completion.operation_id,
                                        device_id = %session.device_id,
                                        generation = session.generation,
                                        outcome = "indeterminate",
                                        indeterminate_reason = "process_outcome_unproven",
                                        failure_stage = stage.as_str(),
                                        error_code = "process_outcome_unproven",
                                        "process/shell terminality is unproven after spawn; reconnecting without a terminal result"
                                    );
                                    return Ok(SessionExit::Reconnect);
                                }
                                AgentIndeterminateCause::PlaywrightProviderOutcomeUnproven => {
                                    tracing::warn!(
                                        event = "v2_agent_playwright_provider_indeterminate",
                                        operation_id = %completion.operation_id,
                                        device_id = %session.device_id,
                                        generation = session.generation,
                                        outcome = "indeterminate",
                                        indeterminate_reason = "playwright_provider_outcome_unproven",
                                        error_code = "playwright_sandbox_provider_outcome_unproven",
                                        "Playwright container terminality is unproven; reconnecting without a terminal result"
                                    );
                                    return Ok(SessionExit::Reconnect);
                                }
                                AgentIndeterminateCause::WorkspaceMutationOutcomeUnproven => {
                                    tracing::warn!(
                                        event = "v2_agent_workspace_mutation_indeterminate",
                                        operation_id = %completion.operation_id,
                                        device_id = %session.device_id,
                                        generation = session.generation,
                                        outcome = "indeterminate",
                                        indeterminate_reason = "workspace_mutation_outcome_unproven",
                                        error_code = "workspace_mutation_outcome_unproven",
                                        "workspace mutation publication is unproven; reconnecting without a terminal result"
                                    );
                                    return Ok(SessionExit::Reconnect);
                                }
                            }
                        }
                    }
                    drain_backend_session_ends(
                        &mut pending_backend_session_ends,
                        self.computer_use.as_ref(),
                        &self.browser_upload_staging,
                        &self.browser_download_staging,
                        &self.material.device_identity,
                        &hello,
                        &challenge,
                        &outbound_tx,
                    ).await?;
                }
                _ = recovery_poll.tick() => {
                    if pending_recovery_request.is_none() {
                        if let Some(authorization) = load_authorization(&self.config.state_dir)
                            .map_err(AgentServiceError::OnlineRecovery)?
                        {
                            let challenge = match load_challenge(&self.config.state_dir)
                                .map_err(AgentServiceError::OnlineRecovery)?
                            {
                                Some(challenge) => challenge,
                                None => {
                                    clear_authorization(&self.config.state_dir)
                                        .map_err(AgentServiceError::OnlineRecovery)?;
                                    tracing::warn!(
                                        event = "v2_recovery_authorization_rejected",
                                        device_id = %session.device_id,
                                        generation = session.generation,
                                        outcome = "rejected",
                                        error_code = "recovery_challenge_missing",
                                        "local recovery authorization had no current Hub challenge"
                                    );
                                    continue;
                                }
                            };
                            if let Err(error) = validate_authorization_against_challenge(
                                &challenge,
                                &authorization,
                                trusted_clock.now_ms(),
                            ) {
                                clear_authorization(&self.config.state_dir)
                                    .map_err(AgentServiceError::OnlineRecovery)?;
                                tracing::warn!(
                                    event = "v2_recovery_authorization_rejected",
                                    device_id = %session.device_id,
                                    generation = session.generation,
                                    outcome = "rejected",
                                    error_code = error.safe_code(),
                                    "stale or mismatched local recovery authorization rejected before relay"
                                );
                                continue;
                            }
                            // Preserve the prior phase acknowledgement across reconnect so the
                            // Human can verify it, but once a fresh authorization is ready to relay,
                            // remove that stale acknowledgement before waiting for the exact new one.
                            clear_recovery_resolved(&self.config.state_dir)
                                .map_err(AgentServiceError::OnlineRecovery)?;
                            send_agent(
                                &outbound_tx,
                                AgentToHub::RecoveryAuthorization(authorization.clone()),
                            )
                            .await?;
                            pending_recovery_request = Some(authorization);
                        }
                    }
                }
                message = inbound.message() => {
                    let frame = match message.map_err(AgentServiceError::Status)? {
                        Some(frame) => frame,
                        None => {
                            terminate_active(
                            &mut active,
                            &mut operation_done_rx,
                            &mut self.execution,
                            &mut self.terminal_evidence,
                            &mut self.backend_execution_receipts,
                            &session.device_id,
                        )
                        .await?;
                            self.persist_state()?;
                            return Ok(SessionExit::Reconnect);
                        }
                    };
                    match decode_hub_frame(frame).map_err(AgentServiceError::Carrier)? {
                        HubToAgent::HandoffRequest(remote) => {
                            verify_remote_handoff_request(
                                &hello,
                                &challenge,
                                &remote,
                                &self.trusted_hub.verifier(),
                            )
                            .map_err(AgentServiceError::Protocol)?;
                            if remote.device_generation != session.generation {
                                return Err(AgentServiceError::Control(
                                    crate::v2_m0::ControlError::StaleDeviceGeneration {
                                        expected: session.generation,
                                        got: remote.device_generation,
                                    },
                                ));
                            }
                            let requires_idle = remote.request.requires_agent_idle();
                            let response = if requires_idle && active.is_some() {
                                RemoteHandoffResponseKind::Rejected {
                                    code: RemoteHandoffErrorCode::Rejected,
                                }
                            } else if let Some(coordinator) = self.handoff.as_ref() {
                                coordinator
                                    .handle_remote(
                                        remote.request,
                                        AgentHandoffSessionFence::for_device(
                                                    &session.device_id,
                                                    session.generation,
                                                    session.capabilities.revision,
                                                ),
                                    )
                                    .await
                            } else {
                                RemoteHandoffResponseKind::Rejected {
                                    code: RemoteHandoffErrorCode::Unsupported,
                                }
                            };
                            let response = build_remote_handoff_response(
                                &self.material.device_identity,
                                &hello,
                                &challenge,
                                session.device_id.clone(),
                                session.generation,
                                remote.request_id,
                                response,
                            )
                            .map_err(AgentServiceError::Protocol)?;
                            send_agent(&outbound_tx, AgentToHub::HandoffResponse(response)).await?;
                        }
                        HubToAgent::HeartbeatAck(ack) => {
                            verify_hub_heartbeat_ack(&hello, &challenge, &ack, &self.trusted_hub.verifier())
                                .map_err(AgentServiceError::Protocol)?;
                            let Some((expected, _)) = pending_heartbeat else {
                                return Err(AgentServiceError::HeartbeatAckMismatch);
                            };
                            if ack.device_generation != session.generation || ack.sequence != expected {
                                return Err(AgentServiceError::HeartbeatAckMismatch);
                            }
                            pending_heartbeat = None;
                        }
                        HubToAgent::RecoveryChallenge(recovery) => {
                            verify_recovery_challenge(
                                &recovery,
                                &self.trusted_hub.verifier(),
                                &session.device_id,
                                session.generation,
                                trusted_clock.now_ms(),
                            )
                            .map_err(AgentServiceError::OnlineRecovery)?;
                            clear_authorization(&self.config.state_dir)
                                .map_err(AgentServiceError::OnlineRecovery)?;
                            // Preserve the last signed durable acknowledgement until a newer
                            // one replaces it. Exact request/phase verification prevents stale
                            // acknowledgements from satisfying a new recovery request, while
                            // keeping the first-stage receipt observable across the forced
                            // generation rollover used by PointerClick recovery.
                            store_challenge(&self.config.state_dir, &recovery)
                                .map_err(AgentServiceError::OnlineRecovery)?;
                            pending_recovery_request = None;
                            let subject = (recovery.phase, recovery.quarantine_fingerprint);
                            if last_recovery_warning != Some(subject) {
                                last_recovery_warning = Some(subject);
                                tracing::warn!(
                                    event = "v2_recovery_challenge_received",
                                    operation_id = %recovery.operation_id,
                                    device_id = %session.device_id,
                                    generation = session.generation,
                                    quarantine_generation = recovery.quarantine_generation,
                                    recovery_phase = recovery_phase_name(recovery.phase),
                                    outcome = if recovery.phase == RecoveryPhase::MutationResume {
                                        "mutation_resume_required"
                                    } else {
                                        "local_user_action_required"
                                    },
                                    "Hub-signed online recovery challenge published for local user inspection"
                                );
                            } else {
                                tracing::debug!(
                                    event = "v2_recovery_challenge_refreshed",
                                    operation_id = %recovery.operation_id,
                                    device_id = %session.device_id,
                                    generation = session.generation,
                                    recovery_phase = recovery_phase_name(recovery.phase),
                                    outcome = "unchanged_challenge_refreshed",
                                    "unchanged recovery challenge refreshed without repeated operator warning"
                                );
                            }
                        }
                        HubToAgent::RecoveryResolved(resolved) => {
                            let expected_authorization = pending_recovery_request
                                .as_ref()
                                .ok_or(AgentServiceError::RecoveryResultMismatch)?;
                            persist_verified_recovery_completion(
                                &self.config.state_dir,
                                &self.trusted_hub.verifier(),
                                expected_authorization,
                                &resolved,
                            )?;
                            pending_recovery_request = None;
                            tracing::info!(
                                event = "v2_recovery_resolved",
                                operation_id = %resolved.operation_id,
                                device_id = %session.device_id,
                                generation = session.generation,
                                recovery_phase = recovery_phase_name(resolved.phase),
                                outcome = recovery_decision_name(resolved.decision),
                                "Hub durably completed local-user recovery without replaying the old operation"
                            );
                        }
                        HubToAgent::BackendSessionEnd(remote) => {
                            verify_remote_backend_session_end(
                                &hello,
                                &challenge,
                                &remote,
                                &self.trusted_hub.verifier(),
                            )
                            .map_err(AgentServiceError::Protocol)?;
                            if remote.device_generation != session.generation {
                                return Err(AgentServiceError::Control(
                                    crate::v2_m0::ControlError::StaleDeviceGeneration {
                                        expected: session.generation,
                                        got: remote.device_generation,
                                    },
                                ));
                            }
                            pending_backend_session_ends.push_back(remote);
                            if active.is_none() {
                                drain_backend_session_ends(
                                    &mut pending_backend_session_ends,
                                    self.computer_use.as_ref(),
                                    &self.browser_upload_staging,
                                    &self.browser_download_staging,
                                    &self.material.device_identity,
                                    &hello,
                                    &challenge,
                                    &outbound_tx,
                                )
                                .await?;
                            }
                        }
                        HubToAgent::Command(remote) => {
                            verify_remote_command(&hello, &challenge, &remote, &self.trusted_hub.verifier())
                                .map_err(AgentServiceError::Protocol)?;
                            crate::v2_m0::validate_command_session(&remote.command, &session)
                                .map_err(AgentServiceError::Control)?;
                            let capability = remote.command.command.capability();
                            let capability_revision = remote.command.capability_revision;
                            let dispatch_grant_id = remote.grant.payload.grant_id.clone();
                            self.grants.authorize_device_capability_once(
                                &remote.grant,
                                &session.device_id,
                                capability,
                                trusted_clock.now_ms(),
                            ).map_err(AgentServiceError::Control)?;
                            self.persist_state()?;
                            if active.is_some() {
                                return Err(AgentServiceError::AgentBusy);
                            }
                            let operation = OperationRef {
                                device_id: session.device_id.clone(),
                                device_generation: session.generation,
                                operation_id: remote.command.operation_id.clone(),
                            };
                            self.execution.begin(operation).map_err(AgentServiceError::Execution)?;
                            // Persist the replay barrier before consulting Handoff. A crash or
                            // runtime failure after this point can never make the Hub-dispatched
                            // operation runnable again on this Agent generation.
                            self.persist_state()?;
                            let operation_id = remote.command.operation_id.clone();
                            let worker_operation_id = operation_id.clone();
                            let worker_generation = session.generation;
                            let done = operation_done_tx.clone();

                            // Agent-owned final authority gate. This is intentionally after grant
                            // consumption/replay persistence but before any local Computer Use
                            // backend call, closing the Hub-admission -> Human-claim TOCTOU window.
                            let protected = is_phase1_protected_command(&remote.command.command);
                            let handoff_verification = if protected {
                                match (self.handoff.as_ref(), remote.handoff.clone()) {
                                    (Some(coordinator), Some(authority)) => {
                                        match coordinator
                                            .final_admit(
                                                authority.clone(),
                                                &remote.command.command,
                                                AgentHandoffSessionFence::for_device(
                                                    &session.device_id,
                                                    session.generation,
                                                    session.capabilities.revision,
                                                ),
                                            )
                                            .await
                                        {
                                            Ok(crate::v2_operator_handoff::AgentAuthorityDecision::Allow) => None,
                                            Ok(crate::v2_operator_handoff::AgentAuthorityDecision::Verification(token))
                                                if is_exact_verification_candidate(&remote.command.command) =>
                                            {
                                                Some(PendingHandoffVerification { authority, token })
                                            }
                                            Ok(crate::v2_operator_handoff::AgentAuthorityDecision::Deny)
                                            | Ok(crate::v2_operator_handoff::AgentAuthorityDecision::Verification(_)) => {
                                                let terminal = ActiveOperation {
                                                    operation_id: operation_id.clone(),
                                                    device_generation: session.generation,
                                                    capability_revision,
                                                    capability,
                                                    dispatch_grant_id: dispatch_grant_id.clone(),
                                                    handoff_verification: None,
                                                    cancellation: ActiveCancellation::None,
                                                };
                                                self.execution.finish(&operation_id)
                                                    .map_err(AgentServiceError::Execution)?;
                                                let device_result = DeviceResult::Error {
                                                    code: DeviceErrorCode::HandoffAuthoritySuspended,
                                                };
                                                record_terminal_evidence(
                                                    &mut self.terminal_evidence,
                                                    &session.device_id,
                                                    &terminal,
                                                    &device_result,
                                                )?;
                                                self.persist_state()?;
                                                send_terminal_result(
                                                    &self.material.device_identity,
                                                    &hello,
                                                    &challenge,
                                                    &outbound_tx,
                                                    &session,
                                                    operation_id,
                                                    device_result,
                                                ).await?;
                                                continue;
                                            }
                                            Err(_) => {
                                                let terminal = ActiveOperation {
                                                    operation_id: operation_id.clone(),
                                                    device_generation: session.generation,
                                                    capability_revision,
                                                    capability,
                                                    dispatch_grant_id: dispatch_grant_id.clone(),
                                                    handoff_verification: None,
                                                    cancellation: ActiveCancellation::None,
                                                };
                                                self.execution.finish(&operation_id)
                                                    .map_err(AgentServiceError::Execution)?;
                                                let device_result = DeviceResult::Error {
                                                    code: DeviceErrorCode::HandoffRuntimeUnavailable,
                                                };
                                                record_terminal_evidence(
                                                    &mut self.terminal_evidence,
                                                    &session.device_id,
                                                    &terminal,
                                                    &device_result,
                                                )?;
                                                self.persist_state()?;
                                                send_terminal_result(
                                                    &self.material.device_identity,
                                                    &hello,
                                                    &challenge,
                                                    &outbound_tx,
                                                    &session,
                                                    operation_id,
                                                    device_result,
                                                ).await?;
                                                continue;
                                            }
                                        }
                                    }
                                    (Some(_), None) | (None, Some(_)) => {
                                        let terminal = ActiveOperation {
                                            operation_id: operation_id.clone(),
                                            device_generation: session.generation,
                                            capability_revision,
                                            capability,
                                            dispatch_grant_id: dispatch_grant_id.clone(),
                                            handoff_verification: None,
                                            cancellation: ActiveCancellation::None,
                                        };
                                        self.execution.finish(&operation_id)
                                            .map_err(AgentServiceError::Execution)?;
                                        let device_result = DeviceResult::Error {
                                            code: DeviceErrorCode::HandoffRuntimeUnavailable,
                                        };
                                        record_terminal_evidence(
                                            &mut self.terminal_evidence,
                                            &session.device_id,
                                            &terminal,
                                            &device_result,
                                        )?;
                                        self.persist_state()?;
                                        send_terminal_result(
                                            &self.material.device_identity,
                                            &hello,
                                            &challenge,
                                            &outbound_tx,
                                            &session,
                                            operation_id,
                                            device_result,
                                        ).await?;
                                        continue;
                                    }
                                    (None, None) => None,
                                }
                            } else {
                                None
                            };

                            let cancellation = match remote.command.command {
                                DeviceCommand::ExecuteProcess { request } => {
                                    let cancellation = ProcessCancellation::default();
                                    let worker_cancel = cancellation.clone();
                                    let executor = self.executor.clone();
                                    let ephemeral_data = self.ephemeral_data.clone();
                                    let worker_device_id = session.device_id.clone();
                                    let worker_revision = session.capabilities.revision;
                                    let source_operation_id = worker_operation_id.clone();
                                    tokio::spawn(async move {
                                        let result = tokio::task::spawn_blocking(move || {
                                            if ephemeral_data.is_some() {
                                                executor
                                                    .execute_captured(&request, &worker_cancel)
                                                    .map(|captured| {
                                                        stage_captured_process_result(
                                                            captured,
                                                            ephemeral_data.as_ref(),
                                                            &worker_device_id,
                                                            worker_generation,
                                                            worker_revision,
                                                            &source_operation_id,
                                                            false,
                                                        )
                                                    })
                                            } else {
                                                executor.execute(&request, &worker_cancel).map(|output| {
                                                    DeviceResult::Process {
                                                        output,
                                                        output_refs: None,
                                                        agent_output_locators: None,
                                                    }
                                                })
                                            }
                                        })
                                        .await;
                                        let _ = done
                                            .send(OperationCompletion {
                                                operation_id: worker_operation_id,
                                                device_generation: worker_generation,
                                                outcome: process_operation_outcome(result),
                                            })
                                            .await;
                                    });
                                    ActiveCancellation::Process(cancellation)
                                }
                                DeviceCommand::Shell { request } => {
                                    let cancellation = ProcessCancellation::default();
                                    let worker_cancel = cancellation.clone();
                                    let shell = self.shell.clone();
                                    let ephemeral_data = self.ephemeral_data.clone();
                                    let worker_device_id = session.device_id.clone();
                                    let worker_revision = session.capabilities.revision;
                                    let source_operation_id = worker_operation_id.clone();
                                    tokio::spawn(async move {
                                        let result = tokio::task::spawn_blocking(move || {
                                            if ephemeral_data.is_some() {
                                                shell.execute_captured(&request, &worker_cancel).map(
                                                    |captured| {
                                                        stage_captured_process_result(
                                                            captured,
                                                            ephemeral_data.as_ref(),
                                                            &worker_device_id,
                                                            worker_generation,
                                                            worker_revision,
                                                            &source_operation_id,
                                                            true,
                                                        )
                                                    },
                                                )
                                            } else {
                                                shell.execute(&request, &worker_cancel).map(|output| {
                                                    DeviceResult::Shell {
                                                        output,
                                                        output_refs: None,
                                                        agent_output_locators: None,
                                                    }
                                                })
                                            }
                                        })
                                        .await;
                                        let _ = done
                                            .send(OperationCompletion {
                                                operation_id: worker_operation_id,
                                                device_generation: worker_generation,
                                                outcome: shell_operation_outcome(result),
                                            })
                                            .await;
                                    });
                                    ActiveCancellation::Process(cancellation)
                                }
                                DeviceCommand::ReadProcessOutput {
                                    locator,
                                    source_operation_id,
                                    stream,
                                    offset,
                                    max_bytes,
                                } => {
                                    let ephemeral_data = self.ephemeral_data.clone();
                                    let worker_device_id = session.device_id.clone();
                                    let worker_revision = session.capabilities.revision;
                                    tokio::spawn(async move {
                                        let result = tokio::task::spawn_blocking(move || {
                                            let Some(store) = ephemeral_data else {
                                                return DeviceResult::Error {
                                                    code: DeviceErrorCode::InvalidRequest,
                                                };
                                            };
                                            let max_bytes = match usize::try_from(max_bytes) {
                                                Ok(max_bytes) => max_bytes,
                                                Err(_) => {
                                                    return DeviceResult::Error {
                                                        code: DeviceErrorCode::InvalidRequest,
                                                    };
                                                }
                                            };
                                            let kind = match stream {
                                                ProcessOutputStream::Stdout => EphemeralDataKind::ProcessStdout,
                                                ProcessOutputStream::Stderr => EphemeralDataKind::ProcessStderr,
                                            };
                                            let Some(now_ms) = agent_unix_time_ms() else {
                                                return DeviceResult::Error {
                                                    code: DeviceErrorCode::IoFailure,
                                                };
                                            };
                                            let range = match store.lock() {
                                                Ok(mut store) => store.read_range(
                                                    &locator,
                                                    &worker_device_id,
                                                    worker_generation,
                                                    worker_revision,
                                                    Some(&source_operation_id),
                                                    kind,
                                                    offset,
                                                    max_bytes,
                                                    now_ms,
                                                ),
                                                Err(_) => {
                                                    return DeviceResult::Error {
                                                        code: DeviceErrorCode::IoFailure,
                                                    };
                                                }
                                            };
                                            match range {
                                                Ok(range) => DeviceResult::ProcessOutputRange {
                                                    bytes: range.bytes,
                                                    stream,
                                                    source_operation_id,
                                                    offset: range.offset,
                                                    next_offset: range.next_offset,
                                                    total_bytes: range.total_bytes,
                                                    eof: range.eof,
                                                },
                                                Err(error) => DeviceResult::Error {
                                                    code: ephemeral_data_error_code(&error),
                                                },
                                            }
                                        })
                                        .await
                                        .map_err(|_| AgentOperationError::WorkerPanicked);
                                        let outcome = match result {
                                            Ok(result) => AgentOperationOutcome::Result(Ok(result)),
                                            Err(error) => AgentOperationOutcome::Result(Err(error)),
                                        };
                                        let _ = done
                                            .send(OperationCompletion {
                                                operation_id: worker_operation_id,
                                                device_generation: worker_generation,
                                                outcome,
                                            })
                                            .await;
                                    });
                                    ActiveCancellation::None
                                }
                                DeviceCommand::ManagedJobStart { request } => {
                                    let managed_jobs = self.managed_jobs.clone();
                                    tokio::spawn(async move {
                                        let result = tokio::task::spawn_blocking(move || {
                                            managed_jobs.start(&request).map(|(agent_locator, status)| {
                                                DeviceResult::ManagedJobStarted {
                                                    agent_locator,
                                                    status,
                                                }
                                            })
                                        })
                                        .await;
                                        let _ = done
                                            .send(OperationCompletion {
                                                operation_id: worker_operation_id,
                                                device_generation: worker_generation,
                                                outcome: managed_job_operation_outcome(result),
                                            })
                                            .await;
                                    });
                                    ActiveCancellation::None
                                }
                                DeviceCommand::ManagedJobStatus { locator } => {
                                    let managed_jobs = self.managed_jobs.clone();
                                    tokio::spawn(async move {
                                        let result = tokio::task::spawn_blocking(move || {
                                            managed_jobs
                                                .status(&locator)
                                                .map(|status| DeviceResult::ManagedJobStatus { status })
                                        })
                                        .await;
                                        let _ = done
                                            .send(OperationCompletion {
                                                operation_id: worker_operation_id,
                                                device_generation: worker_generation,
                                                outcome: managed_job_operation_outcome(result),
                                            })
                                            .await;
                                    });
                                    ActiveCancellation::None
                                }
                                DeviceCommand::ManagedJobOutput {
                                    locator,
                                    stream,
                                    offset,
                                    max_bytes,
                                } => {
                                    let managed_jobs = self.managed_jobs.clone();
                                    tokio::spawn(async move {
                                        let result = tokio::task::spawn_blocking(move || {
                                            let max_bytes = usize::try_from(max_bytes)
                                                .map_err(|_| ManagedJobError::InvalidRead)?;
                                            managed_jobs
                                                .output(&locator, stream, offset, max_bytes)
                                                .map(|range| DeviceResult::ManagedJobOutput {
                                                    stream,
                                                    range,
                                                })
                                        })
                                        .await;
                                        let _ = done
                                            .send(OperationCompletion {
                                                operation_id: worker_operation_id,
                                                device_generation: worker_generation,
                                                outcome: managed_job_operation_outcome(result),
                                            })
                                            .await;
                                    });
                                    ActiveCancellation::None
                                }
                                DeviceCommand::ManagedJobRenew { locator, lease_ms } => {
                                    let managed_jobs = self.managed_jobs.clone();
                                    tokio::spawn(async move {
                                        let result = tokio::task::spawn_blocking(move || {
                                            managed_jobs
                                                .renew(&locator, lease_ms)
                                                .map(|status| DeviceResult::ManagedJobRenewed { status })
                                        })
                                        .await;
                                        let _ = done
                                            .send(OperationCompletion {
                                                operation_id: worker_operation_id,
                                                device_generation: worker_generation,
                                                outcome: managed_job_operation_outcome(result),
                                            })
                                            .await;
                                    });
                                    ActiveCancellation::None
                                }
                                DeviceCommand::ManagedJobStop { locator } => {
                                    let managed_jobs = self.managed_jobs.clone();
                                    tokio::spawn(async move {
                                        let result = tokio::task::spawn_blocking(move || {
                                            managed_jobs
                                                .stop(&locator)
                                                .map(|status| DeviceResult::ManagedJobStopped { status })
                                        })
                                        .await;
                                        let _ = done
                                            .send(OperationCompletion {
                                                operation_id: worker_operation_id,
                                                device_generation: worker_generation,
                                                outcome: managed_job_operation_outcome(result),
                                            })
                                            .await;
                                    });
                                    ActiveCancellation::None
                                }
                                DeviceCommand::PlaywrightTestStart { request } => {
                                    let playwright = self.playwright.clone();
                                    tokio::spawn(async move {
                                        let result = tokio::task::spawn_blocking(move || {
                                            let runner = playwright.ok_or(
                                                PlaywrightSandboxError::InvalidConfig,
                                            )?;
                                            runner.start(&request).map(
                                                |(agent_locator, status)| {
                                                    DeviceResult::PlaywrightTestStarted {
                                                        agent_locator,
                                                        status,
                                                    }
                                                },
                                            )
                                        })
                                        .await;
                                        let _ = done
                                            .send(OperationCompletion {
                                                operation_id: worker_operation_id,
                                                device_generation: worker_generation,
                                                outcome: playwright_operation_outcome(result),
                                            })
                                            .await;
                                    });
                                    ActiveCancellation::None
                                }
                                DeviceCommand::PlaywrightTestStatus { locator } => {
                                    let playwright = self.playwright.clone();
                                    tokio::spawn(async move {
                                        let result = tokio::task::spawn_blocking(move || {
                                            let runner = playwright.ok_or(
                                                PlaywrightSandboxError::InvalidConfig,
                                            )?;
                                            runner.status(&locator).map(|status| {
                                                DeviceResult::PlaywrightTestStatus { status }
                                            })
                                        })
                                        .await;
                                        let _ = done
                                            .send(OperationCompletion {
                                                operation_id: worker_operation_id,
                                                device_generation: worker_generation,
                                                outcome: playwright_operation_outcome(result),
                                            })
                                            .await;
                                    });
                                    ActiveCancellation::None
                                }
                                DeviceCommand::PlaywrightTestOutput {
                                    locator,
                                    stream,
                                    offset,
                                    max_bytes,
                                } => {
                                    let playwright = self.playwright.clone();
                                    tokio::spawn(async move {
                                        let result = tokio::task::spawn_blocking(move || {
                                            let runner = playwright.ok_or(
                                                PlaywrightSandboxError::InvalidConfig,
                                            )?;
                                            runner
                                                .output(&locator, stream, offset, max_bytes)
                                                .map(|range| DeviceResult::PlaywrightTestOutput {
                                                    stream,
                                                    range,
                                                })
                                        })
                                        .await;
                                        let _ = done
                                            .send(OperationCompletion {
                                                operation_id: worker_operation_id,
                                                device_generation: worker_generation,
                                                outcome: playwright_operation_outcome(result),
                                            })
                                            .await;
                                    });
                                    ActiveCancellation::None
                                }
                                DeviceCommand::PlaywrightTestStop { locator } => {
                                    let playwright = self.playwright.clone();
                                    tokio::spawn(async move {
                                        let result = tokio::task::spawn_blocking(move || {
                                            let runner = playwright.ok_or(
                                                PlaywrightSandboxError::InvalidConfig,
                                            )?;
                                            runner.stop(&locator).map(|status| {
                                                DeviceResult::PlaywrightTestStopped { status }
                                            })
                                        })
                                        .await;
                                        let _ = done
                                            .send(OperationCompletion {
                                                operation_id: worker_operation_id,
                                                device_generation: worker_generation,
                                                outcome: playwright_operation_outcome(result),
                                            })
                                            .await;
                                    });
                                    ActiveCancellation::None
                                }
                                DeviceCommand::StageBrowserUploadFile {
                                    context_id,
                                    file_name,
                                    data_base64,
                                    expected_bytes,
                                } => {
                                    let staging = self.browser_upload_staging.clone();
                                    let revision = session.capabilities.revision;
                                    tokio::spawn(async move {
                                        let result = tokio::task::spawn_blocking(move || {
                                            staging.stage(
                                                &context_id,
                                                worker_generation,
                                                revision,
                                                &file_name,
                                                data_base64.as_str(),
                                                expected_bytes,
                                            )
                                        })
                                        .await
                                        .map_err(|_| AgentOperationError::WorkerPanicked)
                                        .and_then(|result| {
                                            result
                                                .map(|staged| DeviceResult::BrowserUploadStaged {
                                                    backend_file_handle: staged.handle,
                                                    bytes: staged.bytes,
                                                })
                                                .map_err(AgentOperationError::BrowserUploadStaging)
                                        });
                                        let _ = done
                                            .send(OperationCompletion {
                                                operation_id: worker_operation_id,
                                                device_generation: worker_generation,
                                                outcome: AgentOperationOutcome::Result(result),
                                            })
                                            .await;
                                    });
                                    ActiveCancellation::None
                                }
                                DeviceCommand::ReadFile {
                                    path,
                                    offset,
                                    max_bytes,
                                } => {
                                    let filesystem = self.filesystem.clone();
                                    tokio::spawn(async move {
                                        let result = tokio::task::spawn_blocking(move || {
                                            filesystem.read_file_range(&path, offset, max_bytes)
                                        })
                                        .await
                                        .map_err(|_| AgentOperationError::WorkerPanicked)
                                        .and_then(|result| result.map_err(AgentOperationError::Filesystem));
                                        let _ = done.send(OperationCompletion {
                                            operation_id: worker_operation_id,
                                            device_generation: worker_generation,
                                            outcome: AgentOperationOutcome::Result(result),
                                        }).await;
                                    });
                                    ActiveCancellation::None
                                }
                                DeviceCommand::ListDirectory { path, after } => {
                                    let filesystem = self.filesystem.clone();
                                    tokio::spawn(async move {
                                        let result = tokio::task::spawn_blocking(move || {
                                            filesystem.list_directory_page(&path, after.as_deref())
                                        })
                                        .await
                                        .map_err(|_| AgentOperationError::WorkerPanicked)
                                        .and_then(|result| result.map_err(AgentOperationError::Filesystem));
                                        let _ = done.send(OperationCompletion {
                                            operation_id: worker_operation_id,
                                            device_generation: worker_generation,
                                            outcome: AgentOperationOutcome::Result(result),
                                        }).await;
                                    });
                                    ActiveCancellation::None
                                }
                                DeviceCommand::WriteWorkspaceFile {
                                    path,
                                    data_base64,
                                    expected_bytes,
                                    content_sha256,
                                    precondition,
                                } => {
                                    let workspace_mutation = self.workspace_mutation.clone();
                                    tokio::spawn(async move {
                                        let result = tokio::task::spawn_blocking(move || {
                                            let Some(workspace_mutation) = workspace_mutation else {
                                                return Err(WorkspaceMutationError::NoAllowedRoots);
                                            };
                                            let bytes = STANDARD
                                                .decode(data_base64.as_str())
                                                .map_err(|_| WorkspaceMutationError::InvalidPayload)?;
                                            if u64::try_from(bytes.len()).ok() != Some(expected_bytes)
                                                || sha256_hex(&bytes) != content_sha256
                                            {
                                                return Err(WorkspaceMutationError::InvalidPayload);
                                            }
                                            workspace_mutation.write_file(
                                                path.as_str(),
                                                &bytes,
                                                &precondition,
                                            )
                                        })
                                        .await;
                                        let outcome = workspace_mutation_operation_outcome(result);
                                        let _ = done.send(OperationCompletion {
                                            operation_id: worker_operation_id,
                                            device_generation: worker_generation,
                                            outcome,
                                        }).await;
                                    });
                                    ActiveCancellation::None
                                }
                                command @ (DeviceCommand::ListApplications
                                | DeviceCommand::ScreenGeometry
                                | DeviceCommand::Screenshot
                                | DeviceCommand::ScreenshotContextual { .. }
                                | DeviceCommand::PointerClick { .. }
                                | DeviceCommand::PointerClickAdvanced { .. }
                                | DeviceCommand::PointerDrag { .. }
                                | DeviceCommand::PointerDragAdvanced { .. }
                                | DeviceCommand::TypeText { .. }
                                | DeviceCommand::TypeTextAdvanced { .. }
                                | DeviceCommand::ListWindows { .. }
                                | DeviceCommand::LaunchApplication { .. }
                                | DeviceCommand::InspectWindow { .. }
                                | DeviceCommand::InspectWindowContextual { .. }
                                | DeviceCommand::VerifyUiState { .. }
                                | DeviceCommand::VerifyUiStateContextual { .. }
                                | DeviceCommand::TerminateApplication { .. }
                                | DeviceCommand::ActivateWindow { .. }
                                | DeviceCommand::SetWindowFrame { .. }
                                | DeviceCommand::InvokeMenu { .. }
                                | DeviceCommand::KeyboardInput { .. }
                                | DeviceCommand::Scroll { .. }
                                | DeviceCommand::ClipboardRead { .. }
                                | DeviceCommand::ClipboardWrite { .. }
                                | DeviceCommand::PointerPosition { .. }
                                | DeviceCommand::MovePointer { .. }
                                | DeviceCommand::SetUiValue { .. }
                                | DeviceCommand::CaptureRegion { .. }
                                | DeviceCommand::ExpandInteractionScope { .. }
                                | DeviceCommand::Browser { .. }) => {
                                    let computer_use = self
                                        .computer_use
                                        .clone()
                                        .ok_or(AgentServiceError::UnsupportedCommand)?;
                                    let upload_staging = self.browser_upload_staging.clone();
                                    let download_staging = self.browser_download_staging.clone();
                                    let capability_revision = session.capabilities.revision;
                                    let advertisement = computer_use.advertisement();
                                    let receipt_context = BackendExecutionReceiptContext {
                                        operation: OperationRef {
                                            device_id: session.device_id.clone(),
                                            device_generation: worker_generation,
                                            operation_id: worker_operation_id.clone(),
                                        },
                                        capability_revision,
                                        capability,
                                        dispatch_grant_id: dispatch_grant_id.clone(),
                                        backend: advertisement.backend,
                                        backend_version: advertisement.backend_version,
                                        target_binding: BackendReceiptTargetBinding::for_command(&command),
                                    };
                                    let (cancel_tx, cancel_rx) = watch::channel(false);
                                    tokio::spawn(async move {
                                        let outcome = execute_computer_use_operation(
                                            computer_use,
                                            command,
                                            upload_staging,
                                            download_staging,
                                            receipt_context,
                                            cancel_rx,
                                        )
                                        .await;
                                        let _ = done.send(OperationCompletion {
                                            operation_id: worker_operation_id,
                                            device_generation: worker_generation,
                                            outcome,
                                        }).await;
                                    });
                                    ActiveCancellation::Backend(cancel_tx)
                                }
                            };
                            active = Some(ActiveOperation {
                                operation_id,
                                device_generation: session.generation,
                                capability_revision,
                                capability,
                                dispatch_grant_id,
                                handoff_verification,
                                cancellation,
                            });
                        }
                        HubToAgent::Cancel(cancel) => {
                            verify_remote_cancel(&hello, &challenge, &cancel, &self.trusted_hub.verifier())
                                .map_err(AgentServiceError::Protocol)?;
                            if cancel.device_generation != session.generation {
                                return Err(AgentServiceError::CancellationMismatch);
                            }
                            let disposition = match active.as_ref() {
                                Some(operation) if operation.operation_id == cancel.operation_id => {
                                    self.execution.request_cancel(&cancel.operation_id)
                                        .map_err(AgentServiceError::Execution)?;
                                    self.persist_state()?;
                                    match &operation.cancellation {
                                        ActiveCancellation::Process(cancellation) => {
                                            cancellation.cancel();
                                            CancellationDisposition::CancellationRequested
                                        }
                                        ActiveCancellation::Backend(cancellation) => {
                                            let _ = cancellation.send(true);
                                            // MCP cancellation propagation is not proof that a desktop
                                            // side effect stopped. The Hub must quarantine this operation.
                                            CancellationDisposition::IndeterminateAfterPropagation
                                        }
                                        ActiveCancellation::None => {
                                            // Bounded filesystem observation has no mutation side effect,
                                            // but std filesystem calls are not asynchronously interruptible.
                                            CancellationDisposition::IndeterminateAfterPropagation
                                        }
                                    }
                                }
                                Some(_) => return Err(AgentServiceError::CancellationMismatch),
                                None => return Err(AgentServiceError::CancellationMismatch),
                            };
                            let ack = build_remote_cancellation_ack(
                                &self.material.device_identity,
                                &hello,
                                &challenge,
                                &cancel,
                                disposition,
                            ).map_err(AgentServiceError::Protocol)?;
                            send_agent(&outbound_tx, AgentToHub::CancellationAck(ack)).await?;
                        }
                        other => return Err(unexpected_hub_message("session_message", &other)),
                    }
                }
            }
        }
        }.await;

        // No session exit path may orphan a process. Protocol errors, stream
        // errors, and policy failures are just as capable of tearing down the
        // session as an explicit reconnect. Always cancel + wait the direct
        // child before returning an error, then persist the terminal replay
        // barrier before the outer lifecycle can reconnect or exit.
        if session_result.is_err() && active.is_some() {
            terminate_active(
                &mut active,
                &mut operation_done_rx,
                &mut self.execution,
                &mut self.terminal_evidence,
                &mut self.backend_execution_receipts,
                &session.device_id,
            )
            .await?;
            self.persist_state()?;
        }
        session_result
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SessionExit {
    Reconnect,
    Shutdown,
}

struct ActiveOperation {
    operation_id: String,
    device_generation: u64,
    capability_revision: u64,
    capability: DeviceCapability,
    dispatch_grant_id: String,
    handoff_verification: Option<PendingHandoffVerification>,
    cancellation: ActiveCancellation,
}

struct PendingHandoffVerification {
    authority: crate::v2_m0_transport::RemoteHandoffAuthority,
    token: VerificationToken,
}

enum ActiveCancellation {
    Process(ProcessCancellation),
    Backend(watch::Sender<bool>),
    None,
}

#[derive(Debug)]
enum AgentOperationOutcome {
    Result(Result<DeviceResult, AgentOperationError>),
    BackendReceipt(BackendExecutionReceipt),
    Indeterminate(AgentIndeterminateCause),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AgentIndeterminateCause {
    CancellationPropagated,
    BackendTimedOut,
    ProcessOutcomeUnproven(ProcessUnprovenStage),
    PlaywrightProviderOutcomeUnproven,
    WorkspaceMutationOutcomeUnproven,
}

#[derive(Debug)]
struct OperationCompletion {
    operation_id: String,
    device_generation: u64,
    outcome: AgentOperationOutcome,
}

fn agent_unix_time_ms() -> Option<u64> {
    let elapsed = SystemTime::now().duration_since(UNIX_EPOCH).ok()?;
    u64::try_from(elapsed.as_millis()).ok()
}

fn stage_captured_process_result(
    captured: CapturedProcessOutput,
    ephemeral_data: Option<&Arc<StdMutex<AgentEphemeralDataStore>>>,
    device_id: &str,
    device_generation: u64,
    capability_revision: u64,
    operation_id: &str,
    shell: bool,
) -> DeviceResult {
    let mut locators = AgentProcessOutputLocators {
        stdout: None,
        stderr: None,
    };
    if let (Some(store), Some(now_ms)) = (ephemeral_data, agent_unix_time_ms()) {
        match store.lock() {
            Ok(mut store) => {
                if let Some(retained) = captured.stdout_retained.as_ref() {
                    let binding = AgentEphemeralBinding::new(
                        device_id.to_owned(),
                        device_generation,
                        capability_revision,
                        Some(operation_id.to_owned()),
                        EphemeralDataKind::ProcessStdout,
                    );
                    if let Ok(binding) = binding {
                        match store.stage(binding, &retained.bytes, now_ms) {
                            Ok(staged) => {
                                locators.stdout = Some(AgentProcessOutputLocator {
                                    locator: staged.locator().to_owned(),
                                    retained_bytes: staged.bytes,
                                    complete: retained.complete,
                                });
                            }
                            Err(error) => tracing::warn!(
                                event = "v2_process_output_spool_refused",
                                stream = "stdout",
                                error_code = error.safe_error_code(),
                                "extended process output was not retained; inline result remains authoritative"
                            ),
                        }
                    }
                }
                if let Some(retained) = captured.stderr_retained.as_ref() {
                    let binding = AgentEphemeralBinding::new(
                        device_id.to_owned(),
                        device_generation,
                        capability_revision,
                        Some(operation_id.to_owned()),
                        EphemeralDataKind::ProcessStderr,
                    );
                    if let Ok(binding) = binding {
                        match store.stage(binding, &retained.bytes, now_ms) {
                            Ok(staged) => {
                                locators.stderr = Some(AgentProcessOutputLocator {
                                    locator: staged.locator().to_owned(),
                                    retained_bytes: staged.bytes,
                                    complete: retained.complete,
                                });
                            }
                            Err(error) => tracing::warn!(
                                event = "v2_process_output_spool_refused",
                                stream = "stderr",
                                error_code = error.safe_error_code(),
                                "extended process output was not retained; inline result remains authoritative"
                            ),
                        }
                    }
                }
            }
            Err(_) => tracing::warn!(
                event = "v2_process_output_spool_refused",
                error_code = "ephemeral_data_lock_poisoned",
                "extended process output store unavailable; inline result remains authoritative"
            ),
        }
    }
    let agent_output_locators =
        (locators.stdout.is_some() || locators.stderr.is_some()).then_some(Box::new(locators));
    if shell {
        DeviceResult::Shell {
            output: captured.output,
            output_refs: None,
            agent_output_locators,
        }
    } else {
        DeviceResult::Process {
            output: captured.output,
            output_refs: None,
            agent_output_locators,
        }
    }
}

fn process_operation_outcome(
    result: Result<Result<DeviceResult, ProcessError>, tokio::task::JoinError>,
) -> AgentOperationOutcome {
    match result {
        Ok(Ok(result)) => AgentOperationOutcome::Result(Ok(result)),
        Ok(Err(error)) => match error.outcome_unproven_stage() {
            Some(stage) => AgentOperationOutcome::Indeterminate(
                AgentIndeterminateCause::ProcessOutcomeUnproven(stage),
            ),
            None => AgentOperationOutcome::Result(Err(AgentOperationError::Process(error))),
        },
        Err(_) => AgentOperationOutcome::Indeterminate(
            AgentIndeterminateCause::ProcessOutcomeUnproven(ProcessUnprovenStage::Worker),
        ),
    }
}

fn managed_job_operation_outcome(
    result: Result<Result<DeviceResult, ManagedJobError>, tokio::task::JoinError>,
) -> AgentOperationOutcome {
    match result {
        Ok(Ok(result)) => AgentOperationOutcome::Result(Ok(result)),
        Ok(Err(error)) => match error.outcome_unproven_stage() {
            Some(stage) => AgentOperationOutcome::Indeterminate(
                AgentIndeterminateCause::ProcessOutcomeUnproven(stage),
            ),
            None => AgentOperationOutcome::Result(Err(AgentOperationError::ManagedJob(error))),
        },
        Err(_) => AgentOperationOutcome::Indeterminate(
            AgentIndeterminateCause::ProcessOutcomeUnproven(ProcessUnprovenStage::Worker),
        ),
    }
}

fn playwright_operation_outcome(
    result: Result<Result<DeviceResult, PlaywrightSandboxError>, tokio::task::JoinError>,
) -> AgentOperationOutcome {
    match result {
        Ok(Ok(result)) => AgentOperationOutcome::Result(Ok(result)),
        Ok(Err(PlaywrightSandboxError::ProviderOutcomeUnproven)) => {
            AgentOperationOutcome::Indeterminate(
                AgentIndeterminateCause::PlaywrightProviderOutcomeUnproven,
            )
        }
        Ok(Err(error)) => match error.outcome_unproven_stage() {
            Some(stage) => AgentOperationOutcome::Indeterminate(
                AgentIndeterminateCause::ProcessOutcomeUnproven(stage),
            ),
            None => {
                AgentOperationOutcome::Result(Err(AgentOperationError::PlaywrightSandbox(error)))
            }
        },
        Err(_) => AgentOperationOutcome::Indeterminate(
            AgentIndeterminateCause::ProcessOutcomeUnproven(ProcessUnprovenStage::Worker),
        ),
    }
}

fn shell_operation_outcome(
    result: Result<Result<DeviceResult, ShellError>, tokio::task::JoinError>,
) -> AgentOperationOutcome {
    match result {
        Ok(Ok(result)) => AgentOperationOutcome::Result(Ok(result)),
        Ok(Err(error)) => match error.outcome_unproven_stage() {
            Some(stage) => AgentOperationOutcome::Indeterminate(
                AgentIndeterminateCause::ProcessOutcomeUnproven(stage),
            ),
            None => AgentOperationOutcome::Result(Err(AgentOperationError::Shell(error))),
        },
        Err(_) => AgentOperationOutcome::Indeterminate(
            AgentIndeterminateCause::ProcessOutcomeUnproven(ProcessUnprovenStage::Worker),
        ),
    }
}

fn workspace_mutation_operation_outcome(
    result: Result<Result<DeviceResult, WorkspaceMutationError>, tokio::task::JoinError>,
) -> AgentOperationOutcome {
    match result {
        Ok(Ok(result)) => AgentOperationOutcome::Result(Ok(result)),
        Ok(Err(error)) if error.outcome_unproven() => AgentOperationOutcome::Indeterminate(
            AgentIndeterminateCause::WorkspaceMutationOutcomeUnproven,
        ),
        Ok(Err(error)) => {
            AgentOperationOutcome::Result(Err(AgentOperationError::WorkspaceMutation(error)))
        }
        Err(_) => AgentOperationOutcome::Indeterminate(
            AgentIndeterminateCause::WorkspaceMutationOutcomeUnproven,
        ),
    }
}

#[derive(Debug)]
pub enum AgentOperationError {
    Process(ProcessError),
    ManagedJob(ManagedJobError),
    PlaywrightSandbox(PlaywrightSandboxError),
    Shell(ShellError),
    Filesystem(FilesystemError),
    WorkspaceMutation(WorkspaceMutationError),
    BrowserUploadStaging(BrowserUploadStagingError),
    BrowserDownloadStaging(BrowserDownloadStagingError),
    Backend(M1BackendError),
    WorkerPanicked,
}

fn normalized_device_result(result: &Result<DeviceResult, AgentOperationError>) -> DeviceResult {
    match result {
        Ok(result) => result.clone(),
        Err(error) => DeviceResult::Error {
            code: operation_error_code(error),
        },
    }
}

fn record_terminal_evidence(
    journal: &mut VecDeque<AgentTerminalEvidence>,
    device_id: &str,
    operation: &ActiveOperation,
    device_result: &DeviceResult,
) -> Result<(), AgentServiceError> {
    if operation.capability.class() == CapabilityClass::Observe {
        return Ok(());
    }
    let Some((terminal_state, evidence)) = terminal_evidence_for_device_result(device_result)
    else {
        return Ok(());
    };
    let entry = AgentTerminalEvidence {
        operation: OperationRef {
            device_id: device_id.to_owned(),
            device_generation: operation.device_generation,
            operation_id: operation.operation_id.clone(),
        },
        capability_revision: operation.capability_revision,
        capability: operation.capability,
        dispatch_grant_id: operation.dispatch_grant_id.clone(),
        terminal_state,
        evidence,
    };
    entry.validate().map_err(AgentServiceError::Execution)?;
    if journal
        .iter()
        .any(|existing| existing.operation.operation_id == entry.operation.operation_id)
    {
        return Ok(());
    }
    journal.push_back(entry);
    while journal.len() > MAX_AGENT_TERMINAL_EVIDENCE_ENTRIES {
        journal.pop_front();
    }
    Ok(())
}

fn record_backend_execution_receipt(
    receipt_journal: &mut VecDeque<BackendExecutionReceipt>,
    terminal_journal: &mut VecDeque<AgentTerminalEvidence>,
    receipt: BackendExecutionReceipt,
) -> Result<(), AgentServiceError> {
    receipt.validate().map_err(AgentServiceError::Execution)?;
    if let Some(existing) = receipt_journal
        .iter()
        .find(|existing| existing.operation.operation_id == receipt.operation.operation_id)
    {
        if existing == &receipt {
            return Ok(());
        }
        return Err(AgentServiceError::Execution(
            crate::v2_m0_execution::ExecutionError::InvalidOperation,
        ));
    }
    if receipt_journal.iter().any(|existing| {
        existing.backend == receipt.backend
            && existing.backend_version == receipt.backend_version
            && existing.sequence >= receipt.sequence
    }) {
        return Err(AgentServiceError::Execution(
            crate::v2_m0_execution::ExecutionError::InvalidOperation,
        ));
    }
    let terminal = receipt.as_terminal_evidence();
    if let Some(existing) = terminal_journal
        .iter()
        .find(|existing| existing.operation.operation_id == terminal.operation.operation_id)
    {
        if existing != &terminal {
            return Err(AgentServiceError::Execution(
                crate::v2_m0_execution::ExecutionError::InvalidOperation,
            ));
        }
    } else {
        terminal_journal.push_back(terminal);
        while terminal_journal.len() > MAX_AGENT_TERMINAL_EVIDENCE_ENTRIES {
            terminal_journal.pop_front();
        }
    }
    receipt_journal.push_back(receipt);
    while receipt_journal.len() > MAX_AGENT_BACKEND_EXECUTION_RECEIPTS {
        receipt_journal.pop_front();
    }
    Ok(())
}
async fn terminate_active(
    active: &mut Option<ActiveOperation>,
    operation_done_rx: &mut mpsc::Receiver<OperationCompletion>,
    execution: &mut AgentExecutionGate,
    terminal_evidence: &mut VecDeque<AgentTerminalEvidence>,
    backend_execution_receipts: &mut VecDeque<BackendExecutionReceipt>,
    device_id: &str,
) -> Result<(), AgentServiceError> {
    let Some(operation) = active.take() else {
        return Ok(());
    };
    match &operation.cancellation {
        ActiveCancellation::Process(cancellation) => cancellation.cancel(),
        ActiveCancellation::Backend(cancellation) => {
            let _ = cancellation.send(true);
        }
        ActiveCancellation::None => {}
    }
    let completion = tokio::time::timeout(Duration::from_secs(5), operation_done_rx.recv())
        .await
        .map_err(|_| AgentServiceError::OperationTerminationTimeout)?
        .ok_or(AgentServiceError::OperationWorkerClosed)?;
    if completion.operation_id != operation.operation_id
        || completion.device_generation != operation.device_generation
    {
        return Err(AgentServiceError::OperationStateMismatch);
    }
    // If the worker already reached the same terminal result that the normal
    // protocol would accept, retain only its payload-free proof. Otherwise the
    // disconnect remains ambiguous and no evidence is manufactured.
    match &completion.outcome {
        AgentOperationOutcome::Result(result) => {
            let device_result = normalized_device_result(result);
            record_terminal_evidence(terminal_evidence, device_id, &operation, &device_result)?;
        }
        AgentOperationOutcome::BackendReceipt(receipt) => {
            record_backend_execution_receipt(
                backend_execution_receipts,
                terminal_evidence,
                receipt.clone(),
            )?;
        }
        AgentOperationOutcome::Indeterminate(_) => {}
    }
    // Process descendants have been killed/waited by ProcessExecutor. Read-only
    // filesystem operations are bounded and awaited before the replay ID is
    // terminalized.
    execution
        .abandon_on_disconnect(&operation.operation_id)
        .map_err(AgentServiceError::Execution)?;
    Ok(())
}

fn ephemeral_data_error_code(error: &AgentEphemeralDataError) -> DeviceErrorCode {
    match error {
        AgentEphemeralDataError::UnknownLocator | AgentEphemeralDataError::Expired => {
            DeviceErrorCode::NotFound
        }
        AgentEphemeralDataError::DeviceMismatch
        | AgentEphemeralDataError::GenerationMismatch
        | AgentEphemeralDataError::CapabilityRevisionMismatch
        | AgentEphemeralDataError::KindMismatch
        | AgentEphemeralDataError::OperationMismatch => DeviceErrorCode::PermissionDenied,
        AgentEphemeralDataError::ReadTooLarge
        | AgentEphemeralDataError::InvalidRange
        | AgentEphemeralDataError::InvalidBinding
        | AgentEphemeralDataError::InvalidObject
        | AgentEphemeralDataError::InvalidLimits
        | AgentEphemeralDataError::InvalidPrivateRoot
        | AgentEphemeralDataError::ObjectTooLarge
        | AgentEphemeralDataError::ObjectLimitExceeded
        | AgentEphemeralDataError::ByteLimitExceeded
        | AgentEphemeralDataError::IdentifierCollision => DeviceErrorCode::InvalidRequest,
        AgentEphemeralDataError::Io => DeviceErrorCode::IoFailure,
    }
}

fn agent_operation_error_code(error: &AgentOperationError) -> &'static str {
    match error {
        AgentOperationError::Process(error) => error.safe_error_code(),
        AgentOperationError::ManagedJob(error) => error.safe_error_code(),
        AgentOperationError::PlaywrightSandbox(error) => error.safe_error_code(),
        AgentOperationError::Shell(error) => error.safe_error_code(),
        AgentOperationError::Filesystem(error) => error.safe_error_code(),
        AgentOperationError::WorkspaceMutation(error) => error.safe_error_code(),
        AgentOperationError::BrowserUploadStaging(error) => error.safe_error_code(),
        AgentOperationError::BrowserDownloadStaging(error) => error.safe_error_code(),
        AgentOperationError::Backend(error) => error.safe_error_code(),
        AgentOperationError::WorkerPanicked => "operation_worker_panicked",
    }
}

fn operation_error_code(error: &AgentOperationError) -> DeviceErrorCode {
    match error {
        AgentOperationError::ManagedJob(error) => managed_job_error_code(error),
        AgentOperationError::PlaywrightSandbox(error) => playwright_error_code(error),
        AgentOperationError::WorkspaceMutation(WorkspaceMutationError::PreconditionFailed) => {
            DeviceErrorCode::WorkspacePreconditionFailed
        }
        AgentOperationError::WorkspaceMutation(WorkspaceMutationError::PayloadTooLarge) => {
            DeviceErrorCode::WorkspacePayloadTooLarge
        }
        AgentOperationError::WorkspaceMutation(
            WorkspaceMutationError::PathDenied | WorkspaceMutationError::HardLinkDenied,
        ) => DeviceErrorCode::PermissionDenied,
        AgentOperationError::WorkspaceMutation(WorkspaceMutationError::Io(_)) => {
            DeviceErrorCode::IoFailure
        }
        AgentOperationError::WorkspaceMutation(WorkspaceMutationError::OutcomeUnproven(_)) => {
            DeviceErrorCode::BackendOutcomeIndeterminate
        }
        AgentOperationError::Filesystem(FilesystemError::PathDenied) => {
            DeviceErrorCode::PermissionDenied
        }
        AgentOperationError::Filesystem(
            FilesystemError::InvalidPath | FilesystemError::NotFile | FilesystemError::NotDirectory,
        ) => DeviceErrorCode::InvalidRequest,
        AgentOperationError::Filesystem(FilesystemError::Io(error))
            if error.kind() == std::io::ErrorKind::NotFound =>
        {
            DeviceErrorCode::NotFound
        }
        AgentOperationError::Process(ProcessError::OutcomeUnproven(_))
        | AgentOperationError::Shell(ShellError::Process(ProcessError::OutcomeUnproven(_))) => {
            // Defense in depth: normal process/shell workers intercept this before
            // constructing a DeviceResult. If a future path bypasses that helper,
            // the wire fallback must still enter Hub indeterminate/quarantine.
            DeviceErrorCode::BackendOutcomeIndeterminate
        }
        AgentOperationError::Filesystem(FilesystemError::Io(_))
        | AgentOperationError::Process(ProcessError::Io(_))
        | AgentOperationError::Shell(ShellError::Process(ProcessError::Io(_))) => {
            DeviceErrorCode::IoFailure
        }
        AgentOperationError::Process(ProcessError::Spawn(_))
        | AgentOperationError::Shell(ShellError::Process(ProcessError::Spawn(_))) => {
            DeviceErrorCode::ProcessSpawnFailed
        }
        AgentOperationError::Process(ProcessError::EnvironmentKeyDenied(_))
        | AgentOperationError::Shell(ShellError::Process(ProcessError::EnvironmentKeyDenied(_))) => {
            DeviceErrorCode::EnvironmentKeyDenied
        }
        AgentOperationError::Process(ProcessError::InvalidEnvironment)
        | AgentOperationError::Shell(ShellError::Process(ProcessError::InvalidEnvironment)) => {
            DeviceErrorCode::InvalidEnvironment
        }
        AgentOperationError::Process(ProcessError::TooManyEnvironmentEntries)
        | AgentOperationError::Shell(ShellError::Process(
            ProcessError::TooManyEnvironmentEntries,
        )) => DeviceErrorCode::TooManyEnvironmentEntries,
        AgentOperationError::Process(ProcessError::WorkingDirectoryDenied)
        | AgentOperationError::Shell(ShellError::Process(ProcessError::WorkingDirectoryDenied)) => {
            DeviceErrorCode::WorkingDirectoryDenied
        }
        AgentOperationError::Process(ProcessError::WorkingDirectoryNotDirectory)
        | AgentOperationError::Shell(ShellError::Process(
            ProcessError::WorkingDirectoryNotDirectory,
        )) => DeviceErrorCode::WorkingDirectoryInvalid,
        AgentOperationError::Process(ProcessError::InvalidTimeout)
        | AgentOperationError::Shell(ShellError::Process(ProcessError::InvalidTimeout)) => {
            DeviceErrorCode::InvalidTimeout
        }
        AgentOperationError::Process(ProcessError::InvalidProgram)
        | AgentOperationError::Shell(ShellError::Process(ProcessError::InvalidProgram)) => {
            DeviceErrorCode::InvalidProgram
        }
        AgentOperationError::Process(ProcessError::ShellProgramDenied)
        | AgentOperationError::Shell(ShellError::Process(ProcessError::ShellProgramDenied)) => {
            DeviceErrorCode::ProgramDenied
        }
        AgentOperationError::Process(ProcessError::TooManyArguments)
        | AgentOperationError::Shell(ShellError::Process(ProcessError::TooManyArguments)) => {
            DeviceErrorCode::TooManyArguments
        }
        AgentOperationError::Process(ProcessError::InvalidRequest)
        | AgentOperationError::Shell(ShellError::InvalidCommand | ShellError::CommandTooLarge)
        | AgentOperationError::Shell(ShellError::Process(ProcessError::InvalidRequest))
        | AgentOperationError::Filesystem(_)
        | AgentOperationError::WorkspaceMutation(_) => DeviceErrorCode::InvalidRequest,
        AgentOperationError::Process(
            ProcessError::NoAllowedWorkingDirectories
            | ProcessError::InvalidPolicyLimit
            | ProcessError::ShellUnsupportedPlatform
            | ProcessError::PipeUnavailable
            | ProcessError::ReaderPanicked
            | ProcessError::LinuxCgroupUnavailable(_)
            | ProcessError::Execution(_),
        )
        | AgentOperationError::Shell(ShellError::Process(
            ProcessError::NoAllowedWorkingDirectories
            | ProcessError::InvalidPolicyLimit
            | ProcessError::ShellUnsupportedPlatform
            | ProcessError::PipeUnavailable
            | ProcessError::ReaderPanicked
            | ProcessError::LinuxCgroupUnavailable(_)
            | ProcessError::Execution(_),
        )) => DeviceErrorCode::InternalFailure,
        AgentOperationError::BrowserUploadStaging(
            BrowserUploadStagingError::UnknownHandle
            | BrowserUploadStagingError::ContextMismatch
            | BrowserUploadStagingError::GenerationMismatch
            | BrowserUploadStagingError::CapabilityRevisionMismatch,
        ) => DeviceErrorCode::BrowserRefStale,
        AgentOperationError::BrowserUploadStaging(
            BrowserUploadStagingError::Io | BrowserUploadStagingError::InvalidRoot,
        ) => DeviceErrorCode::IoFailure,
        AgentOperationError::BrowserUploadStaging(_) => DeviceErrorCode::InvalidRequest,
        AgentOperationError::BrowserDownloadStaging(
            BrowserDownloadStagingError::UnknownOperation
            | BrowserDownloadStagingError::ContextMismatch
            | BrowserDownloadStagingError::GenerationMismatch
            | BrowserDownloadStagingError::CapabilityRevisionMismatch,
        ) => DeviceErrorCode::BrowserRefStale,
        AgentOperationError::BrowserDownloadStaging(
            BrowserDownloadStagingError::Io | BrowserDownloadStagingError::InvalidRoot,
        ) => DeviceErrorCode::IoFailure,
        AgentOperationError::BrowserDownloadStaging(_) => DeviceErrorCode::InvalidRequest,
        AgentOperationError::Backend(M1BackendError::ExecutionBudgetExceeded) => {
            DeviceErrorCode::ExecutionBudgetExceeded
        }
        AgentOperationError::Backend(M1BackendError::BrowserRefused(reason)) => {
            browser_refusal_error_code(*reason)
        }
        AgentOperationError::Backend(M1BackendError::MutationAuthority(_)) => {
            DeviceErrorCode::MutationAuthorityUnavailable
        }
        AgentOperationError::Backend(_) | AgentOperationError::WorkerPanicked => {
            DeviceErrorCode::InternalFailure
        }
    }
}

fn managed_job_error_code(error: &ManagedJobError) -> DeviceErrorCode {
    match error {
        ManagedJobError::UnknownJob => DeviceErrorCode::NotFound,
        ManagedJobError::CapacityExceeded => DeviceErrorCode::ExecutionBudgetExceeded,
        ManagedJobError::InvalidLifetime
        | ManagedJobError::InvalidLease
        | ManagedJobError::InvalidRead
        | ManagedJobError::InvalidLimits
        | ManagedJobError::TerminalJob => DeviceErrorCode::InvalidRequest,
        ManagedJobError::LockPoisoned | ManagedJobError::ThreadSpawn(_) => {
            DeviceErrorCode::InternalFailure
        }
        ManagedJobError::Process(ProcessError::WorkingDirectoryDenied) => {
            DeviceErrorCode::WorkingDirectoryDenied
        }
        ManagedJobError::Process(ProcessError::WorkingDirectoryNotDirectory) => {
            DeviceErrorCode::WorkingDirectoryInvalid
        }
        ManagedJobError::Process(ProcessError::InvalidTimeout) => DeviceErrorCode::InvalidTimeout,
        ManagedJobError::Process(ProcessError::InvalidProgram) => DeviceErrorCode::InvalidProgram,
        ManagedJobError::Process(ProcessError::ShellProgramDenied) => {
            DeviceErrorCode::ProgramDenied
        }
        ManagedJobError::Process(ProcessError::TooManyArguments) => {
            DeviceErrorCode::TooManyArguments
        }
        ManagedJobError::Process(ProcessError::EnvironmentKeyDenied(_)) => {
            DeviceErrorCode::EnvironmentKeyDenied
        }
        ManagedJobError::Process(ProcessError::InvalidEnvironment) => {
            DeviceErrorCode::InvalidEnvironment
        }
        ManagedJobError::Process(ProcessError::TooManyEnvironmentEntries) => {
            DeviceErrorCode::TooManyEnvironmentEntries
        }
        ManagedJobError::Process(ProcessError::Spawn(_)) => DeviceErrorCode::ProcessSpawnFailed,
        ManagedJobError::Process(ProcessError::OutcomeUnproven(_)) => {
            DeviceErrorCode::BackendOutcomeIndeterminate
        }
        ManagedJobError::Process(ProcessError::InvalidRequest) => DeviceErrorCode::InvalidRequest,
        ManagedJobError::Process(_) => DeviceErrorCode::InternalFailure,
    }
}

fn playwright_error_code(error: &PlaywrightSandboxError) -> DeviceErrorCode {
    match error {
        PlaywrightSandboxError::UnknownJob => DeviceErrorCode::NotFound,
        PlaywrightSandboxError::InvalidWorkspace => DeviceErrorCode::WorkingDirectoryDenied,
        PlaywrightSandboxError::InvalidRequest
        | PlaywrightSandboxError::InvalidTestPath
        | PlaywrightSandboxError::InvalidProject
        | PlaywrightSandboxError::InvalidGrep
        | PlaywrightSandboxError::InvalidWorkers
        | PlaywrightSandboxError::InvalidConfig
        | PlaywrightSandboxError::InvalidImage => DeviceErrorCode::InvalidRequest,
        PlaywrightSandboxError::RuntimeUnavailable | PlaywrightSandboxError::ArtifactIo => {
            DeviceErrorCode::IoFailure
        }
        PlaywrightSandboxError::IdentifierCollision
        | PlaywrightSandboxError::MonitorUnavailable
        | PlaywrightSandboxError::LockPoisoned => DeviceErrorCode::InternalFailure,
        PlaywrightSandboxError::ProviderOutcomeUnproven => {
            DeviceErrorCode::BackendOutcomeIndeterminate
        }
        PlaywrightSandboxError::Process(error) => match error {
            ProcessError::OutcomeUnproven(_) => DeviceErrorCode::BackendOutcomeIndeterminate,
            ProcessError::Spawn(_) => DeviceErrorCode::ProcessSpawnFailed,
            ProcessError::Io(_) => DeviceErrorCode::IoFailure,
            _ => DeviceErrorCode::InvalidRequest,
        },
        PlaywrightSandboxError::ManagedJob(error) => managed_job_error_code(error),
    }
}

fn browser_refusal_error_code(reason: BrowserRefusalReason) -> DeviceErrorCode {
    match reason {
        BrowserRefusalReason::RouteUnavailable => DeviceErrorCode::BrowserRouteUnavailable,
        BrowserRefusalReason::RequiresSetup => DeviceErrorCode::BrowserRequiresSetup,
        BrowserRefusalReason::BindingAmbiguous => DeviceErrorCode::BrowserBindingAmbiguous,
        BrowserRefusalReason::BindingStale => DeviceErrorCode::BrowserBindingStale,
        BrowserRefusalReason::WrongTarget => DeviceErrorCode::BrowserWrongTargetRefused,
        BrowserRefusalReason::TabRequired => DeviceErrorCode::BrowserTabRequired,
        BrowserRefusalReason::TabNotFound => DeviceErrorCode::BrowserTabNotFound,
        BrowserRefusalReason::RefStale => DeviceErrorCode::BrowserRefStale,
        BrowserRefusalReason::InputTrustUnavailable => {
            DeviceErrorCode::BrowserInputTrustUnavailable
        }
        BrowserRefusalReason::EndpointOwnerMismatch => {
            DeviceErrorCode::BrowserEndpointOwnerMismatch
        }
        BrowserRefusalReason::ConsentRequired => DeviceErrorCode::BrowserConsentRequired,
        BrowserRefusalReason::ConsentRevoked => DeviceErrorCode::BrowserConsentRevoked,
        BrowserRefusalReason::ReconnectExhausted => DeviceErrorCode::BrowserReconnectExhausted,
        BrowserRefusalReason::InputIncomplete => DeviceErrorCode::BrowserInputIncomplete,
        BrowserRefusalReason::ActionUnavailable => DeviceErrorCode::BrowserActionUnavailable,
        BrowserRefusalReason::OriginOutsideScope => DeviceErrorCode::BrowserOriginOutsideScope,
        BrowserRefusalReason::Other => DeviceErrorCode::BrowserRefused,
    }
}

async fn recover_authoritative_backend_receipt(
    computer_use: &Arc<dyn ComputerUseBackendAdapter>,
    context: &BackendExecutionReceiptContext,
    fallback: AgentOperationOutcome,
) -> AgentOperationOutcome {
    if context.capability.class() == CapabilityClass::Observe {
        return fallback;
    }
    match computer_use.recover_execution_receipt(context).await {
        Ok(Some(receipt)) if context.matches_receipt(&receipt) => {
            AgentOperationOutcome::BackendReceipt(receipt)
        }
        // Missing, malformed, stale, mismatched, or provider-error evidence is
        // never promoted. Preserve the original ambiguous outcome so Hub
        // quarantine/no-replay behavior remains authoritative.
        Ok(Some(_)) | Ok(None) | Err(_) => fallback,
    }
}

async fn execute_computer_use_operation(
    computer_use: Arc<dyn ComputerUseBackendAdapter>,
    command: DeviceCommand,
    upload_staging: BrowserUploadStagingBroker,
    download_staging: BrowserDownloadStagingBroker,
    receipt_context: BackendExecutionReceiptContext,
    cancellation: watch::Receiver<bool>,
) -> AgentOperationOutcome {
    use crate::v2_browser_runtime::{BrowserBackendCommand, BrowserBackendResult};

    let device_generation = receipt_context.operation.device_generation;
    let capability_revision = receipt_context.capability_revision;

    match &command {
        DeviceCommand::Browser {
            command:
                browser_command @ BrowserBackendCommand::Upload {
                    context_id,
                    staged_files,
                    ..
                },
        } => {
            let handles = staged_files
                .iter()
                .map(|file| file.backend_file_handle.clone())
                .collect::<Vec<_>>();
            let mut resolved = Vec::with_capacity(staged_files.len());
            for file in staged_files {
                match upload_staging.resolve(
                    &file.backend_file_handle,
                    context_id,
                    device_generation,
                    capability_revision,
                ) {
                    Ok(file) => resolved.push(file),
                    Err(error) => {
                        let _ = upload_staging.consume_handles(
                            &handles,
                            context_id,
                            device_generation,
                            capability_revision,
                        );
                        return AgentOperationOutcome::Result(Err(
                            AgentOperationError::BrowserUploadStaging(error),
                        ));
                    }
                }
            }
            let result = computer_use
                .execute_browser_upload(browser_command, &resolved, cancellation)
                .await;
            match result {
                Ok(BackendExecutionOutcome::Completed(result)) => {
                    if let Err(error) = upload_staging.consume_handles(
                        &handles,
                        context_id,
                        device_generation,
                        capability_revision,
                    ) {
                        return AgentOperationOutcome::Result(Err(
                            AgentOperationError::BrowserUploadStaging(error),
                        ));
                    }
                    AgentOperationOutcome::Result(Ok(result))
                }
                Ok(BackendExecutionOutcome::CancellationPropagatedIndeterminate) => {
                    recover_authoritative_backend_receipt(
                        &computer_use,
                        &receipt_context,
                        AgentOperationOutcome::Indeterminate(
                            AgentIndeterminateCause::CancellationPropagated,
                        ),
                    )
                    .await
                }
                Ok(BackendExecutionOutcome::TimedOutIndeterminate) => {
                    recover_authoritative_backend_receipt(
                        &computer_use,
                        &receipt_context,
                        AgentOperationOutcome::Indeterminate(
                            AgentIndeterminateCause::BackendTimedOut,
                        ),
                    )
                    .await
                }
                Ok(BackendExecutionOutcome::BackendOutcomeIndeterminate) => {
                    recover_authoritative_backend_receipt(
                        &computer_use,
                        &receipt_context,
                        AgentOperationOutcome::Result(Ok(DeviceResult::Error {
                            code: DeviceErrorCode::BackendOutcomeIndeterminate,
                        })),
                    )
                    .await
                }
                Err(error) => {
                    let _ = upload_staging.consume_handles(
                        &handles,
                        context_id,
                        device_generation,
                        capability_revision,
                    );
                    AgentOperationOutcome::Result(Err(AgentOperationError::Backend(error)))
                }
            }
        }
        DeviceCommand::Browser {
            command:
                browser_command @ BrowserBackendCommand::Download {
                    context_id,
                    destination_name,
                    max_bytes,
                    overwrite,
                    ..
                },
        } => {
            let prepared = match download_staging.prepare(
                context_id,
                device_generation,
                capability_revision,
                destination_name,
                *max_bytes,
                *overwrite,
            ) {
                Ok(prepared) => prepared,
                Err(error) => {
                    return AgentOperationOutcome::Result(Err(
                        AgentOperationError::BrowserDownloadStaging(error),
                    ));
                }
            };
            let result = computer_use
                .execute_browser_download(browser_command, &prepared, cancellation)
                .await;
            match result {
                Ok(BackendExecutionOutcome::Completed(DeviceResult::Browser {
                    result:
                        BrowserBackendResult::DownloadStaged {
                            backend_download_id,
                            bytes_written,
                        },
                })) => match download_staging.finalize(
                    prepared.operation_handle(),
                    context_id,
                    device_generation,
                    capability_revision,
                    &backend_download_id,
                    bytes_written,
                ) {
                    Ok(finalized) => AgentOperationOutcome::Result(Ok(DeviceResult::Browser {
                        result: BrowserBackendResult::DownloadCompleted {
                            backend_download_handle: finalized.backend_download_handle,
                            destination_name: finalized.destination_name,
                            bytes_written: finalized.bytes,
                            data_base64: finalized.data_base64,
                        },
                    })),
                    Err(error) => AgentOperationOutcome::Result(Err(
                        AgentOperationError::BrowserDownloadStaging(error),
                    )),
                },
                Ok(BackendExecutionOutcome::Completed(_)) => {
                    let _ = download_staging.abort(
                        prepared.operation_handle(),
                        context_id,
                        device_generation,
                        capability_revision,
                    );
                    AgentOperationOutcome::Result(Err(AgentOperationError::Backend(
                        M1BackendError::MalformedResponse(
                            "browser download adapter returned an unexpected result",
                        ),
                    )))
                }
                Ok(BackendExecutionOutcome::CancellationPropagatedIndeterminate) => {
                    // The backend may still be writing. Leave this private operation staged;
                    // execution safety quarantines the interaction until explicit resolution,
                    // and context teardown removes the private directory.
                    recover_authoritative_backend_receipt(
                        &computer_use,
                        &receipt_context,
                        AgentOperationOutcome::Indeterminate(
                            AgentIndeterminateCause::CancellationPropagated,
                        ),
                    )
                    .await
                }
                Ok(BackendExecutionOutcome::TimedOutIndeterminate) => {
                    recover_authoritative_backend_receipt(
                        &computer_use,
                        &receipt_context,
                        AgentOperationOutcome::Indeterminate(
                            AgentIndeterminateCause::BackendTimedOut,
                        ),
                    )
                    .await
                }
                Ok(BackendExecutionOutcome::BackendOutcomeIndeterminate) => {
                    recover_authoritative_backend_receipt(
                        &computer_use,
                        &receipt_context,
                        AgentOperationOutcome::Result(Ok(DeviceResult::Error {
                            code: DeviceErrorCode::BackendOutcomeIndeterminate,
                        })),
                    )
                    .await
                }
                Err(error) => {
                    let _ = download_staging.abort(
                        prepared.operation_handle(),
                        context_id,
                        device_generation,
                        capability_revision,
                    );
                    AgentOperationOutcome::Result(Err(AgentOperationError::Backend(error)))
                }
            }
        }
        _ => match computer_use.execute(&command, cancellation).await {
            Ok(BackendExecutionOutcome::Completed(result)) => {
                AgentOperationOutcome::Result(Ok(result))
            }
            Ok(BackendExecutionOutcome::CancellationPropagatedIndeterminate) => {
                recover_authoritative_backend_receipt(
                    &computer_use,
                    &receipt_context,
                    AgentOperationOutcome::Indeterminate(
                        AgentIndeterminateCause::CancellationPropagated,
                    ),
                )
                .await
            }
            Ok(BackendExecutionOutcome::TimedOutIndeterminate) => {
                recover_authoritative_backend_receipt(
                    &computer_use,
                    &receipt_context,
                    AgentOperationOutcome::Indeterminate(AgentIndeterminateCause::BackendTimedOut),
                )
                .await
            }
            Ok(BackendExecutionOutcome::BackendOutcomeIndeterminate) => {
                recover_authoritative_backend_receipt(
                    &computer_use,
                    &receipt_context,
                    AgentOperationOutcome::Result(Ok(DeviceResult::Error {
                        code: DeviceErrorCode::BackendOutcomeIndeterminate,
                    })),
                )
                .await
            }
            Err(error) => AgentOperationOutcome::Result(Err(AgentOperationError::Backend(error))),
        },
    }
}

#[allow(clippy::too_many_arguments)]
async fn drain_backend_session_ends(
    pending: &mut VecDeque<RemoteBackendSessionEnd>,
    computer_use: Option<&Arc<dyn ComputerUseBackendAdapter>>,
    browser_upload_staging: &BrowserUploadStagingBroker,
    browser_download_staging: &BrowserDownloadStagingBroker,
    identity: &crate::v2_m0::DeviceIdentity,
    hello: &AgentHello,
    challenge: &HubChallenge,
    outbound: &mpsc::Sender<crate::v2_m1_grpc::proto::AgentFrame>,
) -> Result<(), AgentServiceError> {
    while let Some(request) = pending.pop_front() {
        let staging_ended = match browser_upload_staging.cleanup_context(&request.context_id) {
            Ok(()) => true,
            Err(error) => {
                tracing::warn!(
                    event = "v2_browser_upload_staging_cleanup_failed",
                    outcome = "failed",
                    error_code = error.safe_error_code(),
                    "browser upload staging cleanup failed without logging context identity"
                );
                false
            }
        };
        let download_staging_ended =
            match browser_download_staging.cleanup_context(&request.context_id) {
                Ok(()) => true,
                Err(error) => {
                    tracing::warn!(
                        event = "v2_browser_download_staging_cleanup_failed",
                        outcome = "failed",
                        error_code = error.safe_error_code(),
                        "browser download staging cleanup failed without logging context identity"
                    );
                    false
                }
            };
        let backend_ended = match computer_use {
            Some(adapter) => match adapter.end_interaction_session(&request.context_id).await {
                Ok(()) => true,
                Err(error) => {
                    tracing::warn!(
                        event = "v2_backend_session_cleanup_failed",
                        outcome = "failed",
                        error_code = error.safe_error_code(),
                        "backend interaction-session cleanup failed without logging context identity"
                    );
                    false
                }
            },
            None => true,
        };
        let ended = staging_ended && download_staging_ended && backend_ended;
        let ack = build_remote_backend_session_ended(identity, hello, challenge, &request, ended)
            .map_err(AgentServiceError::Protocol)?;
        send_agent(outbound, AgentToHub::BackendSessionEnded(ack)).await?;
    }
    Ok(())
}

fn unexpected_hub_message(expected: &'static str, message: &HubToAgent) -> AgentServiceError {
    let got = message.kind();
    tracing::warn!(
        event = "v2_protocol_message_rejected",
        outcome = "rejected",
        error_code = "unexpected_message",
        expected_message = expected,
        message_kind = got,
        "unexpected Hub protocol message rejected"
    );
    AgentServiceError::UnexpectedMessage { expected, got }
}

fn record_agent_persistence_failure(device_id: &str, error: &PersistenceError) {
    crate::v2_observability::persistence_failure(
        crate::v2_observability::PersistenceComponent::Agent,
    );
    tracing::error!(
        event = "v2_persistence_failure",
        device_id,
        outcome = "failed",
        error_code = error.safe_error_code(),
        io_class = error.io_class(),
        resource_exhausted = error.is_resource_exhaustion(),
        component = "agent",
        agent_availability = "fail_closed_exit",
        service_manager_retry = "external",
        "Agent checkpoint persistence failed"
    );
}

async fn send_terminal_result(
    identity: &crate::v2_m0::DeviceIdentity,
    hello: &AgentHello,
    challenge: &HubChallenge,
    sender: &mpsc::Sender<crate::v2_m1_grpc::proto::AgentFrame>,
    session: &DeviceSession,
    operation_id: String,
    device_result: DeviceResult,
) -> Result<(), AgentServiceError> {
    let result = CommandResultEnvelope {
        schema_version: CONTROL_SCHEMA_VERSION,
        device_id: session.device_id.clone(),
        device_generation: session.generation,
        capability_revision: session.capabilities.revision,
        operation_id,
        result: device_result,
    };
    let signed = build_remote_result(identity, hello, challenge, result)
        .map_err(AgentServiceError::Protocol)?;
    send_agent(sender, AgentToHub::Result(signed)).await
}

async fn send_agent(
    sender: &mpsc::Sender<crate::v2_m1_grpc::proto::AgentFrame>,
    message: AgentToHub,
) -> Result<(), AgentServiceError> {
    sender
        .send(encode_agent_frame(&message).map_err(AgentServiceError::Carrier)?)
        .await
        .map_err(|_| AgentServiceError::OutboundClosed)
}

async fn next_hub(
    inbound: &mut tonic::Streaming<crate::v2_m1_grpc::proto::HubFrame>,
) -> Result<HubToAgent, AgentServiceError> {
    let frame = inbound
        .message()
        .await
        .map_err(AgentServiceError::Status)?
        .ok_or(AgentServiceError::InboundClosed)?;
    decode_hub_frame(frame).map_err(AgentServiceError::Carrier)
}

async fn sleep_or_shutdown(duration: Duration, shutdown: &mut watch::Receiver<bool>) -> bool {
    tokio::select! {
        _ = tokio::time::sleep(duration) => false,
        result = shutdown.changed() => result.is_err() || *shutdown.borrow(),
    }
}

fn der_certificate_to_pem(der: &[u8]) -> Vec<u8> {
    let encoded = STANDARD.encode(der);
    let mut pem = String::from("-----BEGIN CERTIFICATE-----\n");
    for chunk in encoded.as_bytes().chunks(64) {
        pem.push_str(std::str::from_utf8(chunk).expect("base64 is ASCII"));
        pem.push('\n');
    }
    pem.push_str("-----END CERTIFICATE-----\n");
    pem.into_bytes()
}

fn emit_browser_staging_startup_failure(
    staging_direction: &'static str,
    io_safe_code: &'static str,
    error: &BrowserStagingStartupError,
) {
    let safe_error_code = io_safe_code;
    tracing::error!(
        event = "v2_agent_browser_staging_startup_failed",
        staging_direction,
        stage = error.stage(),
        failure_class = error.failure_class(),
        io_class = error.io_class(),
        error_code = safe_error_code,
        agent_startup = "refused",
        service_manager_retry = "external",
        "browser staging initialization failed; Agent startup remains fail-closed"
    );
}

pub enum AgentServiceError {
    InvalidConfig(&'static str),
    Transport(tonic::transport::Error),
    Status(tonic::Status),
    Carrier(crate::v2_m1_grpc::GrpcCarrierError),
    Protocol(crate::v2_m0_transport::TransportError),
    Trust(crate::v2_m0_trust::TrustError),
    Control(crate::v2_m0::ControlError),
    Execution(crate::v2_m0_execution::ExecutionError),
    Process(ProcessError),
    ManagedJob(ManagedJobError),
    PlaywrightSandbox(PlaywrightSandboxError),
    ManagedJobFailClosed,
    Filesystem(FilesystemError),
    WorkspaceMutationStartup(WorkspaceMutationError),
    EphemeralDataStartup(AgentEphemeralDataError),
    BrowserUploadStagingStartup(BrowserStagingStartupError),
    BrowserDownloadStagingStartup(BrowserStagingStartupError),
    BrowserUploadStaging(BrowserUploadStagingError),
    BrowserDownloadStaging(BrowserDownloadStagingError),
    Backend(M1BackendError),
    Operation(AgentOperationError),
    Persistence(PersistenceError),
    OnlineRecovery(RecoveryError),
    UnexpectedMessage {
        expected: &'static str,
        got: &'static str,
    },
    HeartbeatAckMismatch,
    RecoveryResultMismatch,
    CancellationMismatch,
    AgentBusy,
    UnsupportedCommand,
    InboundClosed,
    OutboundClosed,
    OperationWorkerClosed,
    OperationStateMismatch,
    OperationTerminationTimeout,
    ReconnectExhausted,
    CheckpointIdentityMismatch,
    CheckpointTrustMismatch,
    CheckpointGrantTrustMismatch,
}

fn grpc_status_safe_error_code(status: &tonic::Status) -> &'static str {
    match status.code() {
        Code::FailedPrecondition
            if status.message() == crate::v2_m1_grpc::HUB_AGENT_SCHEMA_INCOMPATIBLE_MESSAGE =>
        {
            "hub_agent_schema_incompatible"
        }
        Code::PermissionDenied => "hub_agent_identity_rejected",
        Code::Unauthenticated => "hub_authentication_rejected",
        Code::ResourceExhausted => "hub_session_resource_exhausted",
        Code::Unavailable => "hub_transport_unavailable",
        Code::Cancelled => "hub_transport_cancelled",
        Code::DeadlineExceeded => "hub_transport_deadline_exceeded",
        _ => "grpc_status",
    }
}
impl AgentServiceError {
    fn reconnectable(&self) -> bool {
        match self {
            Self::Transport(_) | Self::InboundClosed | Self::OutboundClosed => true,
            Self::Status(status) => matches!(
                status.code(),
                Code::Unavailable | Code::Cancelled | Code::DeadlineExceeded | Code::Unknown
            ),
            _ => false,
        }
    }
}

impl SafeErrorCode for AgentServiceError {
    fn safe_error_code(&self) -> &'static str {
        match self {
            Self::InvalidConfig(_) => "invalid_config",
            Self::Transport(_) => "hub_transport_connect_failed",
            Self::Status(status) => grpc_status_safe_error_code(status),
            Self::Carrier(_) => "carrier_error",
            Self::Protocol(_) => "protocol_error",
            Self::Trust(_) => "trust_error",
            Self::Control(_) => "control_error",
            Self::Execution(_) => "execution_error",
            Self::Process(_) => "process_error",
            Self::ManagedJob(error) => error.safe_error_code(),
            Self::PlaywrightSandbox(error) => error.safe_error_code(),
            Self::ManagedJobFailClosed => "managed_job_fail_closed",
            Self::Filesystem(_) => "filesystem_error",
            Self::WorkspaceMutationStartup(error) => error.safe_error_code(),
            Self::EphemeralDataStartup(error) => error.safe_error_code(),
            Self::BrowserUploadStagingStartup(error) => error.upload_safe_error_code(),
            Self::BrowserDownloadStagingStartup(error) => error.download_safe_error_code(),
            Self::BrowserUploadStaging(error) => error.safe_error_code(),
            Self::BrowserDownloadStaging(error) => error.safe_error_code(),
            Self::Backend(error) => error.safe_error_code(),
            Self::Operation(_) => "operation_error",
            Self::Persistence(error) => error.safe_error_code(),
            Self::OnlineRecovery(error) => error.safe_code(),
            Self::UnexpectedMessage { .. } => "unexpected_message",
            Self::HeartbeatAckMismatch => "heartbeat_ack_mismatch",
            Self::RecoveryResultMismatch => "recovery_result_mismatch",
            Self::CancellationMismatch => "cancellation_mismatch",
            Self::AgentBusy => "agent_busy",
            Self::UnsupportedCommand => "unsupported_command",
            Self::InboundClosed => "inbound_closed",
            Self::OutboundClosed => "outbound_closed",
            Self::OperationWorkerClosed => "operation_worker_closed",
            Self::OperationStateMismatch => "operation_state_mismatch",
            Self::OperationTerminationTimeout => "operation_termination_timeout",
            Self::ReconnectExhausted => "reconnect_exhausted",
            Self::CheckpointIdentityMismatch => "checkpoint_identity_mismatch",
            Self::CheckpointTrustMismatch => "checkpoint_trust_mismatch",
            Self::CheckpointGrantTrustMismatch => "checkpoint_grant_trust_mismatch",
        }
    }
}

impl fmt::Debug for AgentServiceError {
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

impl fmt::Display for AgentServiceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self, f)
    }
}

impl std::error::Error for AgentServiceError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recovery_completion_receipt_is_persisted_before_handoff_cleanup() {
        use crate::v2_execution_safety::{DesktopQuarantine, IndeterminateReason, OperationOwner};
        use crate::v2_m0_execution::IndeterminateResolution;
        use crate::v2_m0_transport::HubIdentity;
        use crate::v2_online_recovery::{
            RecoveryAuditAssessment, build_recovery_challenge, build_recovery_resolved,
            load_authorization, load_challenge, load_recovery_resolved, new_authorization,
            store_authorization, store_challenge,
        };
        let state_dir = std::env::temp_dir().join(format!(
            "cumg-agent-recovery-receipt-{}-{}",
            std::process::id(),
            rand::random::<u64>()
        ));
        std::fs::create_dir_all(&state_dir).unwrap();
        let quarantine = DesktopQuarantine {
            operation_id: "op_recovery_receipt".into(),
            device_id: "dev_recovery_receipt".into(),
            device_generation: 4,
            owner: OperationOwner::new("issuer", "subject").unwrap(),
            reason: IndeterminateReason::ConnectionLost,
            since_ms: 100,
        };
        let hub = HubIdentity::generate();
        let challenge = build_recovery_challenge(&hub, &quarantine, 5, 110).unwrap();
        let authorization = new_authorization(
            &challenge,
            RecoveryAuditAssessment::Inconclusive,
            IndeterminateResolution::ConfirmedNotExecuted,
            "operator-reviewed",
        )
        .unwrap();
        let resolved = build_recovery_resolved(&hub, &authorization, 120).unwrap();
        store_challenge(&state_dir, &challenge).unwrap();
        store_authorization(&state_dir, &authorization).unwrap();

        persist_verified_recovery_completion(
            &state_dir,
            &hub.verifier(),
            &authorization,
            &resolved,
        )
        .unwrap();

        assert_eq!(load_recovery_resolved(&state_dir).unwrap(), Some(resolved));
        assert!(load_challenge(&state_dir).unwrap().is_none());
        assert!(load_authorization(&state_dir).unwrap().is_none());
        let _ = std::fs::remove_dir_all(state_dir);
    }

    #[test]
    fn grpc_status_classification_is_bounded_and_distinguishes_schema_skew() {
        let mismatch = AgentServiceError::Status(tonic::Status::failed_precondition(
            crate::v2_m1_grpc::HUB_AGENT_SCHEMA_INCOMPATIBLE_MESSAGE,
        ));
        assert_eq!(mismatch.safe_error_code(), "hub_agent_schema_incompatible");
        assert!(!mismatch.reconnectable());

        let identity = AgentServiceError::Status(tonic::Status::permission_denied(
            "remote detail must not be copied",
        ));
        assert_eq!(identity.safe_error_code(), "hub_agent_identity_rejected");
        assert!(!format!("{identity:?}").contains("remote detail"));

        let unknown_marker = "REMOTE_SECRET_DO_NOT_LOG";
        let unknown = AgentServiceError::Status(tonic::Status::failed_precondition(unknown_marker));
        assert_eq!(unknown.safe_error_code(), "grpc_status");
        assert!(!format!("{unknown:?}").contains(unknown_marker));
    }
    #[test]
    fn storage_full_persistence_failure_is_bounded_and_forces_fail_closed_exit() {
        let error = AgentServiceError::Persistence(PersistenceError::Io(std::io::Error::from(
            std::io::ErrorKind::StorageFull,
        )));
        assert_eq!(error.safe_error_code(), "persistence_resource_exhausted");
        assert!(!error.reconnectable());
        assert_eq!(format!("{error:?}"), "persistence_resource_exhausted");
    }

    #[test]
    fn process_terminal_proof_classification_controls_indeterminate_routing() {
        let ordinary = process_operation_outcome(Ok(Err(ProcessError::Io(std::io::Error::other(
            "terminal reader failure",
        )))));
        assert!(matches!(
            ordinary,
            AgentOperationOutcome::Result(Err(AgentOperationError::Process(_)))
        ));

        let unproven = process_operation_outcome(Ok(Err(ProcessError::OutcomeUnproven(
            ProcessUnprovenStage::Wait,
        ))));
        assert!(matches!(
            unproven,
            AgentOperationOutcome::Indeterminate(AgentIndeterminateCause::ProcessOutcomeUnproven(
                ProcessUnprovenStage::Wait
            ))
        ));

        let provider_unproven =
            playwright_operation_outcome(Ok(Err(PlaywrightSandboxError::ProviderOutcomeUnproven)));
        assert!(matches!(
            provider_unproven,
            AgentOperationOutcome::Indeterminate(
                AgentIndeterminateCause::PlaywrightProviderOutcomeUnproven
            )
        ));

        let shell_unproven = shell_operation_outcome(Ok(Err(ShellError::Process(
            ProcessError::OutcomeUnproven(ProcessUnprovenStage::Termination),
        ))));
        assert!(matches!(
            shell_unproven,
            AgentOperationOutcome::Indeterminate(AgentIndeterminateCause::ProcessOutcomeUnproven(
                ProcessUnprovenStage::Termination
            ))
        ));
        let leaked =
            AgentOperationError::Process(ProcessError::OutcomeUnproven(ProcessUnprovenStage::Poll));
        assert_eq!(
            operation_error_code(&leaked),
            DeviceErrorCode::BackendOutcomeIndeterminate
        );
    }

    #[tokio::test]
    async fn process_worker_panic_is_fail_closed_indeterminate() {
        let joined = tokio::task::spawn_blocking(|| -> Result<DeviceResult, ProcessError> {
            panic!("injected process worker panic");
        })
        .await;
        assert!(matches!(
            process_operation_outcome(joined),
            AgentOperationOutcome::Indeterminate(AgentIndeterminateCause::ProcessOutcomeUnproven(
                ProcessUnprovenStage::Worker
            ))
        ));
    }

    #[test]
    fn process_policy_errors_preserve_safe_categories_through_shell_wrapping() {
        let cases = [
            (
                AgentOperationError::Process(ProcessError::WorkingDirectoryDenied),
                DeviceErrorCode::WorkingDirectoryDenied,
            ),
            (
                AgentOperationError::Shell(ShellError::Process(
                    ProcessError::WorkingDirectoryDenied,
                )),
                DeviceErrorCode::WorkingDirectoryDenied,
            ),
            (
                AgentOperationError::Process(ProcessError::InvalidTimeout),
                DeviceErrorCode::InvalidTimeout,
            ),
            (
                AgentOperationError::Process(ProcessError::ShellProgramDenied),
                DeviceErrorCode::ProgramDenied,
            ),
            (
                AgentOperationError::Process(ProcessError::Spawn(std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    "PRIVATE_HOST_PATH",
                ))),
                DeviceErrorCode::ProcessSpawnFailed,
            ),
        ];
        for (error, expected) in cases {
            assert_eq!(operation_error_code(&error), expected);
            let rendered = format!("{error:?}");
            assert!(!rendered.contains("PRIVATE_HOST_PATH"));
        }
    }

    #[test]
    fn execution_budget_rejection_is_a_terminal_typed_remote_error() {
        let error = AgentOperationError::Backend(M1BackendError::ExecutionBudgetExceeded);
        assert_eq!(
            operation_error_code(&error),
            DeviceErrorCode::ExecutionBudgetExceeded
        );
        assert_eq!(
            terminal_evidence_for_device_result(&DeviceResult::Error {
                code: DeviceErrorCode::ExecutionBudgetExceeded,
            }),
            Some((
                crate::v2_m0_execution::HubOperationState::Failed,
                crate::v2_execution_safety::ExecutionEvidence::VerifiedRemoteError,
            ))
        );
    }

    #[test]
    fn executor_internal_failures_remain_generic_fail_closed() {
        for error in [
            AgentOperationError::Process(ProcessError::PipeUnavailable),
            AgentOperationError::Process(ProcessError::ReaderPanicked),
            AgentOperationError::Process(ProcessError::InvalidPolicyLimit),
        ] {
            assert_eq!(
                operation_error_code(&error),
                DeviceErrorCode::InternalFailure
            );
        }
    }

    #[test]
    fn environment_policy_errors_map_to_stable_device_codes_without_values() {
        let denied = AgentOperationError::Process(ProcessError::EnvironmentKeyDenied(
            "AWS_SECRET_ACCESS_KEY".into(),
        ));
        assert_eq!(
            operation_error_code(&denied),
            DeviceErrorCode::EnvironmentKeyDenied
        );
        assert_eq!(
            operation_error_code(&AgentOperationError::Process(
                ProcessError::InvalidEnvironment
            )),
            DeviceErrorCode::InvalidEnvironment
        );
        assert_eq!(
            operation_error_code(&AgentOperationError::Shell(ShellError::Process(
                ProcessError::TooManyEnvironmentEntries,
            ))),
            DeviceErrorCode::TooManyEnvironmentEntries
        );
        assert_eq!(
            operation_error_code(&AgentOperationError::Process(ProcessError::InvalidRequest)),
            DeviceErrorCode::InvalidRequest
        );
        let rendered = format!("{denied:?}");
        assert!(!rendered.contains("AWS_SECRET_ACCESS_KEY"));
        assert!(!rendered.contains("secret-value"));
    }

    #[test]
    fn config_requires_https_and_bounded_liveness_settings() {
        let config = AgentServiceConfig {
            hub_endpoint: "http://localhost:7443".into(),
            hub_domain: "localhost".into(),
            device_id: "dev-a".into(),
            allowed_file_roots: vec![std::env::current_dir().unwrap()],
            allowed_write_roots: vec![],
            denied_write_subpaths: vec![],
            allowed_cwd_roots: vec![std::env::current_dir().unwrap()],
            state_dir: std::env::temp_dir().join("cumg-v2-agent-config-test"),
            ephemeral_data_parent: None,
            heartbeat_interval: Duration::from_secs(5),
            reconnect: ReconnectPolicy {
                initial_delay: Duration::from_millis(10),
                max_delay: Duration::from_secs(1),
                max_attempts: 3,
            },
            cua: None,
        };
        assert!(matches!(
            config.validate(),
            Err(AgentServiceError::InvalidConfig(_))
        ));
    }

    #[test]
    fn filesystem_observation_roots_are_independent_from_process_cwd_roots() {
        use crate::v2_m0::{DeviceIdentity, GrantAuthority, ProcessRequest};
        use crate::v2_m0_transport::HubIdentity;

        let base = std::env::temp_dir().join(format!(
            "cumg-v2-agent-file-roots-{}-{}",
            std::process::id(),
            rand::random::<u64>()
        ));
        let file_root = base.join("observable");
        let process_only = base.join("process-only");
        let state_dir = base.join("state");
        std::fs::create_dir_all(&file_root).unwrap();
        std::fs::create_dir_all(&process_only).unwrap();
        std::fs::write(file_root.join("visible.txt"), b"visible").unwrap();
        std::fs::write(
            process_only.join("not-readable.txt"),
            b"private-to-file-api",
        )
        .unwrap();

        let config = AgentServiceConfig {
            hub_endpoint: "https://localhost:7443".into(),
            hub_domain: "localhost".into(),
            device_id: "dev-file-roots".into(),
            allowed_cwd_roots: vec![base.clone()],
            allowed_file_roots: vec![file_root.clone()],
            allowed_write_roots: vec![],
            denied_write_subpaths: vec![],
            state_dir: state_dir.clone(),
            ephemeral_data_parent: None,
            heartbeat_interval: Duration::from_secs(5),
            reconnect: ReconnectPolicy {
                initial_delay: Duration::from_millis(10),
                max_delay: Duration::from_secs(1),
                max_attempts: 3,
            },
            cua: None,
        };
        let device = DeviceIdentity::generate();
        let hub = HubIdentity::generate();
        let grant = GrantAuthority::generate();
        let service = AgentService::new(
            config,
            AgentProvisionedMaterial {
                device_identity: device,
                trusted_hub: hub.verifier(),
                grant_verifier: grant.verifier(),
                additional_grant_verifiers: vec![],
                hub_rotation: None,
                tls_root_der: vec![1],
            },
        )
        .unwrap();

        assert!(matches!(
            service
                .filesystem
                .read_file(file_root.join("visible.txt").to_str().unwrap()),
            Ok(DeviceResult::FileContents { .. })
        ));
        assert!(matches!(
            service
                .filesystem
                .read_file(process_only.join("not-readable.txt").to_str().unwrap()),
            Err(FilesystemError::PathDenied)
        ));
        assert!(matches!(
            service
                .filesystem
                .list_directory(process_only.to_str().unwrap()),
            Err(FilesystemError::PathDenied)
        ));

        let missing_program = process_only.join("missing-program");
        let process = ProcessRequest {
            program: missing_program.to_string_lossy().into_owned(),
            args: vec![],
            cwd: process_only.to_string_lossy().into_owned(),
            env: vec![],
            timeout_ms: 5_000,
        };
        assert!(matches!(
            service
                .executor
                .execute(&process, &ProcessCancellation::default()),
            Err(ProcessError::Spawn(_))
        ));

        let _ = std::fs::remove_dir_all(base);
    }

    #[test]
    fn missing_file_roots_fail_closed_without_cwd_fallback() {
        let mut config = AgentServiceConfig {
            hub_endpoint: "https://localhost:7443".into(),
            hub_domain: "localhost".into(),
            device_id: "dev-file-roots-missing".into(),
            allowed_cwd_roots: vec![std::env::current_dir().unwrap()],
            allowed_file_roots: vec![std::env::current_dir().unwrap()],
            allowed_write_roots: vec![],
            denied_write_subpaths: vec![],
            state_dir: std::env::temp_dir().join("cumg-v2-agent-file-roots-missing"),
            ephemeral_data_parent: None,
            heartbeat_interval: Duration::from_secs(5),
            reconnect: ReconnectPolicy {
                initial_delay: Duration::from_millis(10),
                max_delay: Duration::from_secs(1),
                max_attempts: 3,
            },
            cua: None,
        };
        config.allowed_file_roots.clear();
        assert!(matches!(
            config.validate(),
            Err(AgentServiceError::InvalidConfig(
                "at least one allowed file root is required"
            ))
        ));
    }

    #[derive(Debug)]
    struct FakeComputerUseBackend;

    #[async_trait::async_trait]
    impl ComputerUseBackendAdapter for FakeComputerUseBackend {
        fn advertisement(&self) -> CapabilityAdvertisement {
            CapabilityAdvertisement {
                backend: "fake-cu".into(),
                backend_version: "1".into(),
                platform: "test".into(),
                capability_schema_version: CAPABILITY_SCHEMA_VERSION,
                revision: 1,
                supported: vec![
                    DeviceCapability::ScreenGeometry,
                    DeviceCapability::Screenshot,
                    DeviceCapability::TypeText,
                ],
            }
        }

        async fn connect(&self) -> Result<(), M1BackendError> {
            Ok(())
        }

        async fn shutdown(&self) -> Result<(), M1BackendError> {
            Ok(())
        }

        async fn execute(
            &self,
            command: &DeviceCommand,
            _cancellation: watch::Receiver<bool>,
        ) -> Result<BackendExecutionOutcome, M1BackendError> {
            match command {
                DeviceCommand::ScreenGeometry => Ok(BackendExecutionOutcome::Completed(
                    DeviceResult::ScreenGeometry {
                        width_points: 1,
                        height_points: 1,
                        scale_factor_milli: 1_000,
                    },
                )),
                _ => Err(M1BackendError::UnsupportedCommand(command.capability())),
            }
        }
    }

    #[derive(Debug)]
    struct ReceiptAwareComputerUseBackend {
        receipt: Option<BackendExecutionReceipt>,
    }

    #[async_trait::async_trait]
    impl ComputerUseBackendAdapter for ReceiptAwareComputerUseBackend {
        fn advertisement(&self) -> CapabilityAdvertisement {
            CapabilityAdvertisement {
                backend: "receipt-cu".into(),
                backend_version: "2".into(),
                platform: "test".into(),
                capability_schema_version: CAPABILITY_SCHEMA_VERSION,
                revision: 1,
                supported: vec![DeviceCapability::TerminateApplication],
            }
        }

        async fn connect(&self) -> Result<(), M1BackendError> {
            Ok(())
        }

        async fn shutdown(&self) -> Result<(), M1BackendError> {
            Ok(())
        }

        async fn execute(
            &self,
            _command: &DeviceCommand,
            _cancellation: watch::Receiver<bool>,
        ) -> Result<BackendExecutionOutcome, M1BackendError> {
            Ok(BackendExecutionOutcome::BackendOutcomeIndeterminate)
        }

        async fn recover_execution_receipt(
            &self,
            _context: &BackendExecutionReceiptContext,
        ) -> Result<Option<BackendExecutionReceipt>, M1BackendError> {
            Ok(self.receipt.clone())
        }
    }

    fn terminate_receipt_context(operation_id: &str) -> BackendExecutionReceiptContext {
        BackendExecutionReceiptContext {
            operation: OperationRef {
                device_id: "dev-receipt".into(),
                device_generation: 41,
                operation_id: operation_id.into(),
            },
            capability_revision: 9,
            capability: DeviceCapability::TerminateApplication,
            dispatch_grant_id: "grant_receipt_fence".into(),
            backend: "receipt-cu".into(),
            backend_version: "2".into(),
            target_binding: Some(BackendReceiptTargetBinding::ApplicationProcess {
                process_id: 4242,
            }),
        }
    }

    fn exact_backend_receipt(
        context: &BackendExecutionReceiptContext,
        sequence: u64,
    ) -> BackendExecutionReceipt {
        BackendExecutionReceipt {
            schema_version: crate::v2_execution_safety::BACKEND_EXECUTION_RECEIPT_SCHEMA_VERSION,
            operation: context.operation.clone(),
            capability_revision: context.capability_revision,
            capability: context.capability,
            dispatch_grant_id: context.dispatch_grant_id.clone(),
            backend: context.backend.clone(),
            backend_version: context.backend_version.clone(),
            provider_contract_schema_version:
                crate::v2_execution_safety::BACKEND_EXECUTION_RECEIPT_SCHEMA_VERSION,
            sequence,
            target_binding: context.target_binding.clone(),
            terminal_outcome:
                crate::v2_execution_safety::BackendReceiptTerminalOutcome::EffectCommitted,
        }
    }

    #[tokio::test]
    async fn exact_backend_receipt_recovers_response_loss_without_replay() {
        let root = std::env::temp_dir().join(format!(
            "cumg-agent-backend-receipt-{}-{}",
            std::process::id(),
            rand::random::<u64>()
        ));
        let upload = BrowserUploadStagingBroker::new(&root).unwrap();
        let download = BrowserDownloadStagingBroker::new(&root).unwrap();
        let context = terminate_receipt_context("op-backend-receipt");
        let receipt = exact_backend_receipt(&context, 7);
        let backend: Arc<dyn ComputerUseBackendAdapter> =
            Arc::new(ReceiptAwareComputerUseBackend {
                receipt: Some(receipt.clone()),
            });
        let (_cancel_tx, cancel_rx) = watch::channel(false);

        let outcome = execute_computer_use_operation(
            backend,
            DeviceCommand::TerminateApplication { process_id: 4242 },
            upload,
            download,
            context,
            cancel_rx,
        )
        .await;

        assert!(matches!(
            outcome,
            AgentOperationOutcome::BackendReceipt(ref recovered) if recovered == &receipt
        ));
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn missing_or_mismatched_backend_receipt_keeps_response_loss_indeterminate() {
        let root = std::env::temp_dir().join(format!(
            "cumg-agent-backend-receipt-missing-{}-{}",
            std::process::id(),
            rand::random::<u64>()
        ));
        let context = terminate_receipt_context("op-backend-receipt-missing");
        let exact = exact_backend_receipt(&context, 8);
        let mut cases: Vec<(&str, Option<BackendExecutionReceipt>)> = vec![("missing", None)];

        let mut operation = exact.clone();
        operation.operation.operation_id = "op-cross-operation-substitution".into();
        cases.push(("operation_id", Some(operation)));

        let mut device = exact.clone();
        device.operation.device_id = "dev-other".into();
        cases.push(("device", Some(device)));

        let mut generation = exact.clone();
        generation.operation.device_generation += 1;
        cases.push(("generation", Some(generation)));

        let mut capability_revision = exact.clone();
        capability_revision.capability_revision += 1;
        cases.push(("capability_revision", Some(capability_revision)));

        let mut capability = exact.clone();
        capability.capability = DeviceCapability::LaunchApplication;
        capability.target_binding = Some(BackendReceiptTargetBinding::ApplicationLaunch {
            identifier: Some("com.example.other".into()),
            name: None,
        });
        cases.push(("capability", Some(capability)));

        let mut dispatch = exact.clone();
        dispatch.dispatch_grant_id = "grant_other_dispatch".into();
        cases.push(("dispatch_grant", Some(dispatch)));

        let mut backend = exact.clone();
        backend.backend = "unknown-provider".into();
        cases.push(("backend_provenance", Some(backend)));

        let mut backend_version = exact.clone();
        backend_version.backend_version = "other-version".into();
        cases.push(("backend_version", Some(backend_version)));

        let mut receipt_schema = exact.clone();
        receipt_schema.schema_version = 0;
        cases.push(("receipt_schema", Some(receipt_schema)));

        let mut provider_schema = exact.clone();
        provider_schema.provider_contract_schema_version = 0;
        cases.push(("provider_schema", Some(provider_schema)));

        let mut sequence = exact.clone();
        sequence.sequence = 0;
        cases.push(("sequence", Some(sequence)));

        let mut missing_target = exact.clone();
        missing_target.target_binding = None;
        cases.push(("missing_target", Some(missing_target)));

        let mut target = exact.clone();
        target.target_binding =
            Some(BackendReceiptTargetBinding::ApplicationProcess { process_id: 9999 });
        cases.push(("target", Some(target)));

        for (case, receipt) in cases {
            let backend: Arc<dyn ComputerUseBackendAdapter> =
                Arc::new(ReceiptAwareComputerUseBackend { receipt });
            let (_cancel_tx, cancel_rx) = watch::channel(false);
            let outcome = execute_computer_use_operation(
                backend,
                DeviceCommand::TerminateApplication { process_id: 4242 },
                BrowserUploadStagingBroker::new(&root).unwrap(),
                BrowserDownloadStagingBroker::new(&root).unwrap(),
                context.clone(),
                cancel_rx,
            )
            .await;
            assert!(
                matches!(
                    outcome,
                    AgentOperationOutcome::Result(Ok(DeviceResult::Error {
                        code: DeviceErrorCode::BackendOutcomeIndeterminate
                    }))
                ),
                "case={case}"
            );
        }
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn backend_receipt_journal_is_idempotent_and_rejects_conflicts_or_stale_sequence() {
        let context = terminate_receipt_context("op-receipt-journal");
        let receipt = exact_backend_receipt(&context, 11);
        let mut receipts = VecDeque::new();
        let mut terminal = VecDeque::new();
        record_backend_execution_receipt(&mut receipts, &mut terminal, receipt.clone()).unwrap();
        record_backend_execution_receipt(&mut receipts, &mut terminal, receipt.clone()).unwrap();
        assert_eq!(receipts.len(), 1);
        assert_eq!(terminal, VecDeque::from([receipt.as_terminal_evidence()]));

        let mut conflict = receipt.clone();
        conflict.sequence = 12;
        assert!(record_backend_execution_receipt(&mut receipts, &mut terminal, conflict).is_err());

        let mut stale_context = terminate_receipt_context("op-receipt-stale");
        stale_context.operation.operation_id = "op-receipt-stale".into();
        let stale = exact_backend_receipt(&stale_context, 10);
        assert!(record_backend_execution_receipt(&mut receipts, &mut terminal, stale).is_err());
    }

    #[test]
    fn custom_computer_use_backend_is_injected_without_changing_native_capabilities() {
        use crate::v2_m0::{DeviceIdentity, GrantAuthority};
        use crate::v2_m0_transport::HubIdentity;

        let state_dir = std::env::temp_dir().join(format!(
            "cumg-v2-agent-custom-backend-{}-{}",
            std::process::id(),
            rand::random::<u64>()
        ));
        let config = AgentServiceConfig {
            hub_endpoint: "https://localhost:7443".into(),
            hub_domain: "localhost".into(),
            device_id: "dev-custom-backend".into(),
            allowed_file_roots: vec![std::env::current_dir().unwrap()],
            allowed_write_roots: vec![],
            denied_write_subpaths: vec![],
            allowed_cwd_roots: vec![std::env::current_dir().unwrap()],
            state_dir: state_dir.clone(),
            ephemeral_data_parent: None,
            heartbeat_interval: Duration::from_secs(5),
            reconnect: ReconnectPolicy {
                initial_delay: Duration::from_millis(10),
                max_delay: Duration::from_secs(1),
                max_attempts: 3,
            },
            cua: None,
        };
        let device = DeviceIdentity::generate();
        let hub = HubIdentity::generate();
        let grant = GrantAuthority::generate();
        let material = AgentProvisionedMaterial {
            device_identity: device,
            trusted_hub: hub.verifier(),
            grant_verifier: grant.verifier(),
            additional_grant_verifiers: vec![],
            hub_rotation: None,
            tls_root_der: vec![1],
        };
        let service = AgentService::new_with_computer_use_backend(
            config,
            material,
            Arc::new(FakeComputerUseBackend),
        )
        .unwrap();
        let capabilities = service.capabilities();
        assert_eq!(capabilities.backend, "agent-native+fake-cu");
        assert!(
            capabilities
                .supported
                .contains(&DeviceCapability::ExecuteProcess)
        );
        assert!(capabilities.supported.contains(&DeviceCapability::Shell));
        assert!(capabilities.supported.contains(&DeviceCapability::ReadFile));
        assert!(
            capabilities
                .supported
                .contains(&DeviceCapability::ListDirectory)
        );
        assert!(
            capabilities
                .supported
                .contains(&DeviceCapability::ScreenGeometry)
        );
        assert!(
            capabilities
                .supported
                .contains(&DeviceCapability::Screenshot)
        );
        assert!(capabilities.supported.contains(&DeviceCapability::TypeText));
        assert!(
            !capabilities
                .supported
                .contains(&DeviceCapability::PlaywrightTestControl)
        );
        assert!(
            !capabilities
                .supported
                .contains(&DeviceCapability::PlaywrightTestObserve)
        );
        let _ = std::fs::remove_dir_all(state_dir);
    }

    #[test]
    fn startup_applies_signed_hub_rotation_and_reconciles_grant_overlap() {
        use crate::v2_m0::{DeviceIdentity, GrantAuthority};
        use crate::v2_m0_transport::HubIdentity;
        use crate::v2_m0_trust::build_hub_key_rotation;

        let state_dir = std::env::temp_dir().join(format!(
            "cumg-v2-agent-rotation-{}-{}",
            std::process::id(),
            rand::random::<u64>()
        ));
        let config = AgentServiceConfig {
            hub_endpoint: "https://localhost:7443".into(),
            hub_domain: "localhost".into(),
            device_id: "dev-rotation".into(),
            allowed_file_roots: vec![std::env::current_dir().unwrap()],
            allowed_write_roots: vec![],
            denied_write_subpaths: vec![],
            allowed_cwd_roots: vec![std::env::current_dir().unwrap()],
            state_dir: state_dir.clone(),
            ephemeral_data_parent: None,
            heartbeat_interval: Duration::from_secs(5),
            reconnect: ReconnectPolicy {
                initial_delay: Duration::from_millis(10),
                max_delay: Duration::from_secs(1),
                max_attempts: 3,
            },
            cua: None,
        };
        let device = DeviceIdentity::generate();
        let old_hub = HubIdentity::generate();
        let old_grant = GrantAuthority::generate();
        let initial = AgentProvisionedMaterial {
            device_identity: device.clone(),
            trusted_hub: old_hub.verifier(),
            grant_verifier: old_grant.verifier(),
            additional_grant_verifiers: vec![],
            hub_rotation: None,
            tls_root_der: vec![1],
        };
        let first = AgentService::new(config.clone(), initial).unwrap();
        assert_eq!(first.trusted_hub.epoch(), 0);
        drop(first);

        let new_hub = HubIdentity::generate();
        let new_grant = GrantAuthority::generate();
        let rotation = build_hub_key_rotation(&old_hub, &new_hub, 1).unwrap();
        let rotated = AgentService::new(
            config.clone(),
            AgentProvisionedMaterial {
                device_identity: device.clone(),
                trusted_hub: new_hub.verifier(),
                grant_verifier: new_grant.verifier(),
                additional_grant_verifiers: vec![old_grant.verifier()],
                hub_rotation: Some(rotation),
                tls_root_der: vec![1],
            },
        )
        .unwrap();
        assert_eq!(rotated.trusted_hub.verifier(), new_hub.verifier());
        assert_eq!(rotated.trusted_hub.epoch(), 1);
        let overlap = rotated.grants.snapshot().verifier_keys;
        assert!(overlap.contains(&new_grant.verifier().to_bytes()));
        assert!(overlap.contains(&old_grant.verifier().to_bytes()));
        drop(rotated);

        let retired = AgentService::new(
            config,
            AgentProvisionedMaterial {
                device_identity: device,
                trusted_hub: new_hub.verifier(),
                grant_verifier: new_grant.verifier(),
                additional_grant_verifiers: vec![],
                hub_rotation: None,
                tls_root_der: vec![1],
            },
        )
        .unwrap();
        let keys = retired.grants.snapshot().verifier_keys;
        assert_eq!(keys, vec![new_grant.verifier().to_bytes()]);
        let _ = std::fs::remove_dir_all(state_dir);
    }

    #[test]
    fn terminal_proof_is_persisted_before_remote_result_delivery() {
        let source = include_str!("v2_m1_agent.rs").replace("\r\n", "\n");
        let start = source
            .find("record_terminal_evidence(\n                                &mut self.terminal_evidence")
            .unwrap();
        let end = source[start..]
            .find("AgentOperationOutcome::Indeterminate")
            .map(|offset| start + offset)
            .unwrap();
        let block = &source[start..end];
        let persist = block.find("self.persist_state()?").unwrap();
        let send = block
            .find("send_agent(&outbound_tx, AgentToHub::Result(signed)).await?")
            .unwrap();
        assert!(
            persist < send,
            "terminal proof must be durable before response delivery"
        );
    }

    #[test]
    fn terminal_journal_records_only_authoritative_payload_free_effectful_proof() {
        let active = ActiveOperation {
            operation_id: "op-journal".into(),
            device_generation: 4,
            capability_revision: 12,
            capability: DeviceCapability::Shell,
            dispatch_grant_id: "grant_journal_fence".into(),
            handoff_verification: None,
            cancellation: ActiveCancellation::None,
        };
        let result = DeviceResult::Shell {
            output: crate::v2_m0::ProcessOutput {
                exit_code: Some(0),
                stdout: "RAW_SECRET_RESULT_MARKER".into(),
                stderr: "RAW_SECRET_ERROR_MARKER".into(),
                stdout_truncated: false,
                stderr_truncated: false,
                timed_out: false,
                cancelled: false,
                duration_ms: 1,
            },
            output_refs: None,
            agent_output_locators: None,
        };
        let mut journal = VecDeque::new();
        record_terminal_evidence(&mut journal, "dev-a", &active, &result).unwrap();
        assert_eq!(journal.len(), 1);
        let encoded = serde_json::to_string(&journal).unwrap();
        assert!(!encoded.contains("RAW_SECRET_RESULT_MARKER"));
        assert!(!encoded.contains("RAW_SECRET_ERROR_MARKER"));
        assert_eq!(journal[0].operation.operation_id, "op-journal");
        assert_eq!(journal[0].capability, DeviceCapability::Shell);

        let mut indeterminate = journal.clone();
        let active = ActiveOperation {
            operation_id: "op-no-proof".into(),
            device_generation: 4,
            capability_revision: 12,
            capability: DeviceCapability::PointerClick,
            dispatch_grant_id: "grant_no_proof".into(),
            handoff_verification: None,
            cancellation: ActiveCancellation::None,
        };
        record_terminal_evidence(
            &mut indeterminate,
            "dev-a",
            &active,
            &DeviceResult::Error {
                code: DeviceErrorCode::BackendOutcomeIndeterminate,
            },
        )
        .unwrap();
        assert_eq!(indeterminate.len(), journal.len());
    }

    #[test]
    fn der_to_pem_is_bounded_ascii_certificate_encoding() {
        let pem = String::from_utf8(der_certificate_to_pem(&[1, 2, 3, 4])).unwrap();
        assert!(pem.starts_with("-----BEGIN CERTIFICATE-----\n"));
        assert!(pem.contains("AQIDBA=="));
        assert!(pem.ends_with("-----END CERTIFICATE-----\n"));
    }
}
