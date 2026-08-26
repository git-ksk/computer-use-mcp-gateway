//! First-class CUMG coordination boundary for Human handoff.
//!
//! CUMG owns authenticated principal/device/capability/generation admission and the semantic
//! postcondition contract. `mcp-execution-handoff` remains the canonical owner of generic
//! Agent/Human authority, intervention/epoch transitions, `Done -> verifying`, and resume policy.
//! The normal runtime/FSM is Agent-owned; this Hub-side coordinator keeps only CUMG selection,
//! conservative pre-dispatch admission, and a signed/session-fenced relay to that Agent runtime.
//! The compatibility Unix backend remains regression-only behind the same surface-neutral seam so
//! Terminal/PTY #48 can add a new Agent-local surface without duplicating authority semantics.

use crate::{
    v2_m0::{DeviceCommand, DeviceResult, VerificationStatus},
    v2_m0_transport::{
        RemoteHandoffAdmissionDecision, RemoteHandoffAuthority, RemoteHandoffErrorCode,
        RemoteHandoffExactWindow, RemoteHandoffOperatorCommand, RemoteHandoffRequestKind,
        RemoteHandoffResponseKind, RemoteHandoffStatus, RemoteHandoffSurfaceBinding,
    },
    v2_m0_trust::AuthenticatedClientPrincipal,
    v2_m1_hub::{HubCommandError, HubHandle},
    v2_operator_handoff::{
        AgentAuthorityDecision, AgentAuthorityRequest, ExactWindowBinding, HandoffControlError,
        HandoffInterventionStatus, HandoffRuntimeControl, HandoffRuntimeStatus,
        ManagedOperatorHandoffAuthority, OperatorHandoffAuthority, OperatorHandoffError,
        VerificationReport, VerificationToken, device_binding, exact_window_binding,
        interaction_context_binding, is_exact_verification_candidate, is_phase1_protected_command,
        principal_binding,
    },
};
use async_trait::async_trait;
use std::{
    fmt,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

/// A CUMG-owned binding to one Human-controllable execution surface.
///
/// Only OS Window is admitted by #152. Terminal is intentionally not represented until Handoff
/// #48 supplies the bounded PTY lifecycle; adding that binding must not duplicate authority logic.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum HandoffSurfaceBinding {
    OsWindow(ExactWindowBinding),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct HandoffAgentBinding {
    pub principal_binding: String,
    pub device_binding: String,
    pub generation: u64,
    pub capability_revision: u64,
    pub surface: Option<HandoffSurfaceBinding>,
    pub verification_candidate: bool,
}

impl HandoffAgentBinding {
    fn legacy_request(&self) -> AgentAuthorityRequest {
        AgentAuthorityRequest {
            principal_binding: self.principal_binding.clone(),
            device_binding: self.device_binding.clone(),
            generation: self.generation,
            capability_revision: self.capability_revision,
            exact_window: self.surface.as_ref().map(|surface| match surface {
                HandoffSurfaceBinding::OsWindow(window) => window.clone(),
            }),
            verification_candidate: self.verification_candidate,
        }
    }

    pub(crate) fn remote_authority(&self) -> RemoteHandoffAuthority {
        remote_authority(&self.legacy_request())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct HandoffAdmission {
    pub binding: HandoffAgentBinding,
    pub verification: Option<VerificationToken>,
    pub verification_local_to_agent: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HandoffCoordinatorError {
    AuthoritySuspended,
    InvalidSemanticDecision,
    Backend(OperatorHandoffError),
}

impl fmt::Display for HandoffCoordinatorError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AuthoritySuspended => {
                f.write_str("agent authority suspended by active Human handoff")
            }
            Self::InvalidSemanticDecision => {
                f.write_str("Handoff backend returned an invalid semantic decision")
            }
            Self::Backend(error) => write!(f, "Handoff backend unavailable: {error}"),
        }
    }
}

impl std::error::Error for HandoffCoordinatorError {}

impl From<OperatorHandoffError> for HandoffCoordinatorError {
    fn from(value: OperatorHandoffError) -> Self {
        Self::Backend(value)
    }
}

const HANDOFF_CONTROL_SELECTION_TTL: Duration = Duration::from_secs(60);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HandoffSessionFence {
    pub generation: u64,
    pub capability_revision: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HandoffOperatorCommand {
    Status,
    Begin,
    RecoverReissue,
    RecoverRebind {
        prior_context_id: String,
        prior_generation: Option<u64>,
        prior_capability_revision: Option<u64>,
    },
    RebindLive,
    AbandonExpiredRecovery {
        expected_epoch: u64,
    },
    RequestResume,
    CancelBeforeHuman,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HandoffOperatorControlError {
    Unsupported,
    NoFreshExactWindow,
    SessionFenceMismatch,
    InvalidRecoveryProof,
    AgentNotIdle,
    DeviceQuarantined,
    InvalidLifecycleState,
    Runtime(HandoffControlError),
}

impl HandoffOperatorControlError {
    pub const fn safe_code(self) -> &'static str {
        match self {
            Self::Unsupported => "handoff_control_unsupported",
            Self::NoFreshExactWindow => "handoff_no_fresh_exact_window",
            Self::SessionFenceMismatch => "handoff_session_fence_mismatch",
            Self::InvalidRecoveryProof => "handoff_invalid_recovery_proof",
            Self::AgentNotIdle => "handoff_agent_not_idle",
            Self::DeviceQuarantined => "handoff_device_quarantined",
            Self::InvalidLifecycleState => "handoff_invalid_lifecycle_state",
            Self::Runtime(HandoffControlError::Unavailable) => "handoff_runtime_unavailable",
            Self::Runtime(HandoffControlError::Rejected) => "handoff_control_rejected",
            Self::Runtime(HandoffControlError::Protocol) => "handoff_runtime_protocol_invalid",
        }
    }
}

impl fmt::Display for HandoffOperatorControlError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.safe_code())
    }
}

impl std::error::Error for HandoffOperatorControlError {}

impl From<HandoffControlError> for HandoffOperatorControlError {
    fn from(value: HandoffControlError) -> Self {
        Self::Runtime(value)
    }
}

#[derive(Clone)]
struct SelectedWindowAuthority {
    authority: AgentAuthorityRequest,
    expires_at: Instant,
}

#[derive(Default)]
struct HandoffSelectionState {
    last_observed: Option<SelectedWindowAuthority>,
    last_admitted: Option<SelectedWindowAuthority>,
}

#[derive(Clone)]
struct RemoteAgentHandoffAuthority {
    hub: HubHandle,
}

#[async_trait]
impl OperatorHandoffAuthority for RemoteAgentHandoffAuthority {
    async fn admit_agent(
        &self,
        request: AgentAuthorityRequest,
    ) -> Result<AgentAuthorityDecision, OperatorHandoffError> {
        let response = self
            .hub
            .handoff_request(RemoteHandoffRequestKind::Admission {
                authority: remote_authority(&request),
            })
            .await
            .map_err(map_remote_hub_error)?;
        match response {
            RemoteHandoffResponseKind::Admission { decision } => Ok(match decision {
                RemoteHandoffAdmissionDecision::Allow => AgentAuthorityDecision::Allow,
                RemoteHandoffAdmissionDecision::Deny => AgentAuthorityDecision::Deny,
                RemoteHandoffAdmissionDecision::Verification {
                    intervention_id,
                    epoch,
                } => AgentAuthorityDecision::Verification(VerificationToken {
                    intervention_id,
                    epoch,
                }),
            }),
            RemoteHandoffResponseKind::Rejected { code } => Err(map_remote_error(code)),
            _ => Err(OperatorHandoffError::Protocol),
        }
    }

    async fn report_verification(
        &self,
        _report: VerificationReport,
    ) -> Result<(), OperatorHandoffError> {
        // Agent-owned mode reports verification directly to its local canonical
        // runtime before the result returns to Hub.
        Err(OperatorHandoffError::Unsupported)
    }
}

fn remote_authority(request: &AgentAuthorityRequest) -> RemoteHandoffAuthority {
    RemoteHandoffAuthority {
        principal_binding: request.principal_binding.clone(),
        device_binding: request.device_binding.clone(),
        generation: request.generation,
        capability_revision: request.capability_revision,
        surface: request.exact_window.as_ref().map(|window| {
            RemoteHandoffSurfaceBinding::OsWindow(RemoteHandoffExactWindow {
                context_binding: window.context_binding.clone(),
                process_id: window.process_id,
                window_id: window.window_id,
            })
        }),
        verification_candidate: request.verification_candidate,
    }
}

fn map_remote_hub_error(error: HubCommandError) -> OperatorHandoffError {
    match error {
        HubCommandError::AgentOffline
        | HubCommandError::SessionClosed
        | HubCommandError::SessionSuperseded => OperatorHandoffError::Unavailable,
        _ => OperatorHandoffError::Protocol,
    }
}

fn map_remote_error(code: RemoteHandoffErrorCode) -> OperatorHandoffError {
    match code {
        RemoteHandoffErrorCode::Unsupported => OperatorHandoffError::Unsupported,
        RemoteHandoffErrorCode::Unavailable => OperatorHandoffError::Unavailable,
        RemoteHandoffErrorCode::Rejected
        | RemoteHandoffErrorCode::Protocol
        | RemoteHandoffErrorCode::SessionFenceMismatch
        | RemoteHandoffErrorCode::InvalidRequest => OperatorHandoffError::Protocol,
    }
}

fn local_status_from_remote(
    status: RemoteHandoffStatus,
    locator: Option<String>,
) -> HandoffRuntimeStatus {
    HandoffRuntimeStatus {
        active: status.active,
        recovery_required: status.recovery_required,
        recovery_status: status.recovery_status,
        recovery_epoch: status.recovery_epoch,
        recovery_expired: status.recovery_expired,
        resume_requested: status.resume_requested,
        faulted: status.faulted,
        human_surface: status.human_surface,
        locator,
    }
}

/// CUMG-side coordinator. The backend supplies canonical Handoff semantic decisions; this type
/// supplies the consumer binding and validation around those decisions.
#[derive(Clone)]
pub struct HandoffCoordinator {
    backend: Arc<dyn OperatorHandoffAuthority>,
    managed_runtime: Option<Arc<ManagedOperatorHandoffAuthority>>,
    remote_hub: Option<HubHandle>,
    selections: Arc<Mutex<HandoffSelectionState>>,
}

impl HandoffCoordinator {
    /// Compatibility constructor for the acceptance-only Unix bridge backend.
    pub fn new(backend: Arc<dyn OperatorHandoffAuthority>) -> Self {
        Self {
            backend,
            managed_runtime: None,
            remote_hub: None,
            selections: Arc::new(Mutex::new(HandoffSelectionState::default())),
        }
    }

    /// Normal CUMG runtime constructor. The Hub owns this process for the lifetime of the
    /// coordinator; runtime failure is fail-closed and never triggers authority-restoring restart.
    pub fn managed(runtime: Arc<ManagedOperatorHandoffAuthority>) -> Self {
        Self {
            backend: runtime.clone(),
            managed_runtime: Some(runtime),
            remote_hub: None,
            selections: Arc::new(Mutex::new(HandoffSelectionState::default())),
        }
    }

    /// Normal distributed CUMG runtime: the controlled Agent owns the canonical
    /// Handoff process/FSM and this Hub-side coordinator is only a signed remote
    /// admission/control adapter plus fresh CUMG surface selection.
    pub fn agent_owned(hub: HubHandle) -> Self {
        let backend = Arc::new(RemoteAgentHandoffAuthority { hub: hub.clone() });
        Self {
            backend,
            managed_runtime: None,
            remote_hub: Some(hub),
            selections: Arc::new(Mutex::new(HandoffSelectionState::default())),
        }
    }

    pub fn verification_local_to_agent(&self) -> bool {
        self.remote_hub.is_some()
    }

    pub async fn shutdown(&self) {
        if let Some(runtime) = self.managed_runtime.as_ref() {
            runtime.shutdown().await;
        }
    }

    pub fn invalidate_context(&self, context_id: &str) {
        let context_binding = interaction_context_binding(context_id);
        let mut selections = self
            .selections
            .lock()
            .expect("handoff selection lock poisoned");
        if selections
            .last_observed
            .as_ref()
            .and_then(|selected| selected.authority.exact_window.as_ref())
            .is_some_and(|window| window.context_binding == context_binding)
        {
            selections.last_observed = None;
        }
        if selections
            .last_admitted
            .as_ref()
            .and_then(|selected| selected.authority.exact_window.as_ref())
            .is_some_and(|window| window.context_binding == context_binding)
        {
            selections.last_admitted = None;
        }
    }

    fn remember_observed_window(
        &self,
        authority: &AgentAuthorityRequest,
        context_valid_for: Option<Duration>,
    ) {
        if authority.exact_window.is_none() {
            return;
        }
        self.selections
            .lock()
            .expect("handoff selection lock poisoned")
            .last_observed = Some(SelectedWindowAuthority {
            authority: control_authority(authority),
            expires_at: selection_expiry(context_valid_for),
        });
    }

    fn remember_admitted_window(
        &self,
        authority: &AgentAuthorityRequest,
        context_valid_for: Option<Duration>,
    ) {
        if authority.exact_window.is_none() {
            return;
        }
        self.selections
            .lock()
            .expect("handoff selection lock poisoned")
            .last_admitted = Some(SelectedWindowAuthority {
            authority: control_authority(authority),
            expires_at: selection_expiry(context_valid_for),
        });
    }

    fn selected_authority(
        &self,
        admitted: bool,
        session: Option<HandoffSessionFence>,
    ) -> Result<AgentAuthorityRequest, HandoffOperatorControlError> {
        let session = session.ok_or(HandoffOperatorControlError::SessionFenceMismatch)?;
        let selections = self
            .selections
            .lock()
            .expect("handoff selection lock poisoned");
        let selected = if admitted {
            selections.last_admitted.as_ref()
        } else {
            selections.last_observed.as_ref()
        }
        .ok_or(HandoffOperatorControlError::NoFreshExactWindow)?;
        if Instant::now() >= selected.expires_at {
            return Err(HandoffOperatorControlError::NoFreshExactWindow);
        }
        if selected.authority.generation != session.generation
            || selected.authority.capability_revision != session.capability_revision
        {
            return Err(HandoffOperatorControlError::SessionFenceMismatch);
        }
        Ok(selected.authority.clone())
    }

    async fn remote_operator_control(
        &self,
        command: HandoffOperatorCommand,
        session: Option<HandoffSessionFence>,
    ) -> Result<HandoffRuntimeStatus, HandoffOperatorControlError> {
        let hub = self
            .remote_hub
            .as_ref()
            .ok_or(HandoffOperatorControlError::Unsupported)?;
        let (command, authority) = match command {
            HandoffOperatorCommand::Status => (RemoteHandoffOperatorCommand::Status, None),
            HandoffOperatorCommand::Begin => (
                RemoteHandoffOperatorCommand::Begin,
                Some(remote_authority(&self.selected_authority(true, session)?)),
            ),
            HandoffOperatorCommand::RecoverReissue => (
                RemoteHandoffOperatorCommand::RecoverReissue,
                Some(remote_authority(&self.selected_authority(false, session)?)),
            ),
            HandoffOperatorCommand::RecoverRebind {
                prior_context_id,
                prior_generation,
                prior_capability_revision,
            } => {
                if !valid_context_id(&prior_context_id) {
                    return Err(HandoffOperatorControlError::InvalidRecoveryProof);
                }
                (
                    RemoteHandoffOperatorCommand::RecoverRebind {
                        prior_context_id,
                        prior_generation,
                        prior_capability_revision,
                    },
                    Some(remote_authority(&self.selected_authority(false, session)?)),
                )
            }
            HandoffOperatorCommand::RebindLive => (
                RemoteHandoffOperatorCommand::RebindLive,
                Some(remote_authority(&self.selected_authority(false, session)?)),
            ),
            HandoffOperatorCommand::AbandonExpiredRecovery { expected_epoch } => (
                RemoteHandoffOperatorCommand::AbandonExpiredRecovery { expected_epoch },
                None,
            ),
            HandoffOperatorCommand::RequestResume => {
                (RemoteHandoffOperatorCommand::RequestResume, None)
            }
            HandoffOperatorCommand::CancelBeforeHuman => {
                (RemoteHandoffOperatorCommand::CancelBeforeHuman, None)
            }
        };
        let response = hub
            .handoff_request(RemoteHandoffRequestKind::Operator { command, authority })
            .await
            .map_err(|error| match error {
                HubCommandError::AgentOffline
                | HubCommandError::SessionClosed
                | HubCommandError::SessionSuperseded => {
                    HandoffOperatorControlError::Runtime(HandoffControlError::Unavailable)
                }
                HubCommandError::Busy => HandoffOperatorControlError::AgentNotIdle,
                HubCommandError::DeviceIndeterminate { .. } => {
                    HandoffOperatorControlError::DeviceQuarantined
                }
                _ => HandoffOperatorControlError::Runtime(HandoffControlError::Protocol),
            })?;
        match response {
            RemoteHandoffResponseKind::Status { status } => {
                Ok(local_status_from_remote(status, None))
            }
            RemoteHandoffResponseKind::Operator { status, locator } => {
                Ok(local_status_from_remote(status, locator))
            }
            RemoteHandoffResponseKind::Rejected { code } => Err(match code {
                RemoteHandoffErrorCode::SessionFenceMismatch => {
                    HandoffOperatorControlError::SessionFenceMismatch
                }
                RemoteHandoffErrorCode::InvalidRequest => {
                    HandoffOperatorControlError::InvalidRecoveryProof
                }
                RemoteHandoffErrorCode::Unsupported => HandoffOperatorControlError::Unsupported,
                RemoteHandoffErrorCode::Unavailable => {
                    HandoffOperatorControlError::Runtime(HandoffControlError::Unavailable)
                }
                RemoteHandoffErrorCode::Rejected => {
                    HandoffOperatorControlError::InvalidLifecycleState
                }
                RemoteHandoffErrorCode::Protocol => {
                    HandoffOperatorControlError::Runtime(HandoffControlError::Protocol)
                }
            }),
            RemoteHandoffResponseKind::Admission { .. } => Err(
                HandoffOperatorControlError::Runtime(HandoffControlError::Protocol),
            ),
        }
    }

    pub async fn operator_control(
        &self,
        command: HandoffOperatorCommand,
        session: Option<HandoffSessionFence>,
    ) -> Result<HandoffRuntimeStatus, HandoffOperatorControlError> {
        if self.remote_hub.is_some() {
            return self.remote_operator_control(command, session).await;
        }
        let runtime = self
            .managed_runtime
            .as_ref()
            .ok_or(HandoffOperatorControlError::Unsupported)?;
        match command {
            HandoffOperatorCommand::Status => runtime.status().await.map_err(Into::into),
            HandoffOperatorCommand::Begin => {
                let authority = self.selected_authority(true, session)?;
                runtime
                    .control(HandoffRuntimeControl::Begin { authority })
                    .await
                    .map_err(Into::into)
            }
            HandoffOperatorCommand::RecoverReissue => {
                let status = runtime.status().await?;
                if !status.recovery_required || status.recovery_expired {
                    return Err(HandoffOperatorControlError::InvalidLifecycleState);
                }
                let authority = self.selected_authority(false, session)?;
                runtime
                    .control(HandoffRuntimeControl::RecoverReissue { authority })
                    .await
                    .map_err(Into::into)
            }
            HandoffOperatorCommand::RecoverRebind {
                prior_context_id,
                prior_generation,
                prior_capability_revision,
            } => {
                if !valid_context_id(&prior_context_id) {
                    return Err(HandoffOperatorControlError::InvalidRecoveryProof);
                }
                let status = runtime.status().await?;
                if !status.recovery_required {
                    return Err(HandoffOperatorControlError::InvalidLifecycleState);
                }
                let authority = self.selected_authority(false, session)?;
                if prior_generation.is_some_and(|value| value == 0 || value > authority.generation)
                    || prior_capability_revision
                        .is_some_and(|value| value > authority.capability_revision)
                {
                    return Err(HandoffOperatorControlError::InvalidRecoveryProof);
                }
                runtime
                    .control(HandoffRuntimeControl::RecoverRebind {
                        authority,
                        prior_context_id,
                        prior_generation,
                        prior_capability_revision,
                    })
                    .await
                    .map_err(Into::into)
            }
            HandoffOperatorCommand::RebindLive => {
                let authority = self.selected_authority(false, session)?;
                runtime
                    .control(HandoffRuntimeControl::RebindLive { authority })
                    .await
                    .map_err(Into::into)
            }
            HandoffOperatorCommand::AbandonExpiredRecovery { expected_epoch } => {
                let status = runtime.status().await?;
                if !status.recovery_required
                    || !status.recovery_expired
                    || status.recovery_epoch != Some(expected_epoch)
                    || status.active.is_some()
                    || status.faulted
                {
                    return Err(HandoffOperatorControlError::InvalidLifecycleState);
                }
                runtime
                    .control(HandoffRuntimeControl::AbandonExpiredRecovery { expected_epoch })
                    .await
                    .map_err(Into::into)
            }
            HandoffOperatorCommand::RequestResume => {
                let status = runtime.status().await?;
                let active = status
                    .active
                    .filter(|active| active.status == HandoffInterventionStatus::ReadyToResume)
                    .ok_or(HandoffOperatorControlError::InvalidLifecycleState)?;
                runtime
                    .control(HandoffRuntimeControl::RequestResume {
                        intervention_id: active.intervention_id,
                        epoch: active.epoch,
                    })
                    .await
                    .map_err(Into::into)
            }
            HandoffOperatorCommand::CancelBeforeHuman => {
                let status = runtime.status().await?;
                let active = status
                    .active
                    .filter(|active| active.status == HandoffInterventionStatus::AwaitingHuman)
                    .ok_or(HandoffOperatorControlError::InvalidLifecycleState)?;
                runtime
                    .control(HandoffRuntimeControl::CancelBeforeHuman {
                        intervention_id: active.intervention_id,
                        epoch: active.epoch,
                    })
                    .await
                    .map_err(Into::into)
            }
        }
    }

    pub(crate) fn protects_agent_command(&self, command: &DeviceCommand) -> bool {
        is_phase1_protected_command(command)
    }

    /// Gate one Agent command before it can enter the Hub/Agent execution path.
    ///
    /// `Ok(None)` means the command is outside the current Window handoff surface (process/shell and
    /// bounded filesystem observation). A configured coordinator is fail-closed for protected
    /// commands when its canonical Handoff backend is unavailable or returns an invalid decision.
    #[cfg(test)]
    pub(crate) async fn admit_agent(
        &self,
        principal: &AuthenticatedClientPrincipal,
        device_id: &str,
        generation: u64,
        capability_revision: u64,
        command: &DeviceCommand,
    ) -> Result<Option<HandoffAdmission>, HandoffCoordinatorError> {
        self.admit_agent_with_context_valid_for(
            principal,
            device_id,
            generation,
            capability_revision,
            command,
            None,
        )
        .await
    }

    pub(crate) async fn admit_agent_with_context_valid_for(
        &self,
        principal: &AuthenticatedClientPrincipal,
        device_id: &str,
        generation: u64,
        capability_revision: u64,
        command: &DeviceCommand,
        context_valid_for: Option<Duration>,
    ) -> Result<Option<HandoffAdmission>, HandoffCoordinatorError> {
        if !is_phase1_protected_command(command) {
            return Ok(None);
        }

        let exact_window = exact_window_binding(command);
        let binding = HandoffAgentBinding {
            principal_binding: principal_binding(principal),
            device_binding: device_binding(device_id),
            generation,
            capability_revision,
            surface: exact_window.map(HandoffSurfaceBinding::OsWindow),
            verification_candidate: is_exact_verification_candidate(command),
        };
        let request = binding.legacy_request();
        self.remember_observed_window(&request, context_valid_for);
        let decision = self.backend.admit_agent(request.clone()).await?;
        match decision {
            AgentAuthorityDecision::Allow => {
                self.remember_admitted_window(&request, context_valid_for);
                Ok(Some(HandoffAdmission {
                    binding,
                    verification: None,
                    verification_local_to_agent: self.remote_hub.is_some(),
                }))
            }
            AgentAuthorityDecision::Deny => Err(HandoffCoordinatorError::AuthoritySuspended),
            AgentAuthorityDecision::Verification(token) => {
                if !binding.verification_candidate
                    || !matches!(binding.surface, Some(HandoffSurfaceBinding::OsWindow(_)))
                {
                    return Err(HandoffCoordinatorError::InvalidSemanticDecision);
                }
                Ok(Some(HandoffAdmission {
                    binding,
                    verification: Some(token),
                    verification_local_to_agent: self.remote_hub.is_some(),
                }))
            }
        }
    }

    /// Report only the CUMG-observed postcondition result. No screenshot, predicate payload, raw
    /// backend result, command text, clipboard value, or Human input crosses the Handoff boundary.
    pub(crate) async fn report_verification(
        &self,
        admission: HandoffAdmission,
        result: &DeviceResult,
    ) -> Result<(), HandoffCoordinatorError> {
        let Some(token) = admission.verification else {
            return Ok(());
        };
        let satisfied = matches!(
            result,
            DeviceResult::UiStateVerification {
                status: VerificationStatus::Satisfied,
                ..
            }
        );
        self.backend
            .report_verification(VerificationReport {
                authority: admission.binding.legacy_request(),
                token,
                satisfied,
            })
            .await
            .map_err(HandoffCoordinatorError::Backend)
    }
}

fn selection_expiry(context_valid_for: Option<Duration>) -> Instant {
    let valid_for = context_valid_for
        .unwrap_or(HANDOFF_CONTROL_SELECTION_TTL)
        .min(HANDOFF_CONTROL_SELECTION_TTL);
    Instant::now() + valid_for
}

fn control_authority(authority: &AgentAuthorityRequest) -> AgentAuthorityRequest {
    let mut authority = authority.clone();
    authority.verification_candidate = false;
    authority
}

fn valid_context_id(value: &str) -> bool {
    value.len() == 36
        && value.starts_with("ctx_")
        && value[4..]
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use std::sync::Mutex;

    #[derive(Clone)]
    struct FakeAuthority {
        decision: Result<AgentAuthorityDecision, OperatorHandoffError>,
        admissions: Arc<Mutex<Vec<AgentAuthorityRequest>>>,
        reports: Arc<Mutex<Vec<VerificationReport>>>,
    }

    impl FakeAuthority {
        fn new(decision: Result<AgentAuthorityDecision, OperatorHandoffError>) -> Self {
            Self {
                decision,
                admissions: Arc::new(Mutex::new(Vec::new())),
                reports: Arc::new(Mutex::new(Vec::new())),
            }
        }
    }

    #[async_trait]
    impl OperatorHandoffAuthority for FakeAuthority {
        async fn admit_agent(
            &self,
            request: AgentAuthorityRequest,
        ) -> Result<AgentAuthorityDecision, OperatorHandoffError> {
            self.admissions.lock().unwrap().push(request);
            self.decision.clone()
        }

        async fn report_verification(
            &self,
            report: VerificationReport,
        ) -> Result<(), OperatorHandoffError> {
            self.reports.lock().unwrap().push(report);
            Ok(())
        }
    }

    fn principal() -> AuthenticatedClientPrincipal {
        AuthenticatedClientPrincipal::new("https://issuer.example", "subject-1").unwrap()
    }

    fn verification_command() -> DeviceCommand {
        DeviceCommand::VerifyUiStateContextual {
            context_id: "ctx_0123456789abcdef0123456789abcdef".into(),
            process_id: 12,
            window_id: 34,
            predicates: Vec::new(),
            timeout_ms: 500,
            stable_samples: 2,
            include_screenshot: false,
        }
    }

    #[test]
    fn operator_preflight_has_distinct_privacy_safe_codes() {
        assert_eq!(
            HandoffOperatorControlError::AgentNotIdle.safe_code(),
            "handoff_agent_not_idle"
        );
        assert_eq!(
            HandoffOperatorControlError::DeviceQuarantined.safe_code(),
            "handoff_device_quarantined"
        );
    }

    #[tokio::test]
    async fn coordinator_maps_exact_window_without_exposing_command_payload() {
        let backend = Arc::new(FakeAuthority::new(Ok(AgentAuthorityDecision::Allow)));
        let coordinator = HandoffCoordinator::new(backend.clone());
        let admission = coordinator
            .admit_agent(&principal(), "device-1", 7, 9, &verification_command())
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(
            admission.binding.surface,
            Some(HandoffSurfaceBinding::OsWindow(ExactWindowBinding {
                process_id: 12,
                window_id: 34,
                ..
            }))
        ));
        let requests = backend.admissions.lock().unwrap();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].generation, 7);
        assert_eq!(requests[0].capability_revision, 9);
        assert_eq!(requests[0].exact_window.as_ref().unwrap().process_id, 12);
        assert_eq!(requests[0].exact_window.as_ref().unwrap().window_id, 34);
    }

    #[tokio::test]
    async fn coordinator_control_selection_is_exact_session_bound_and_context_invalidated() {
        let backend = Arc::new(FakeAuthority::new(Ok(AgentAuthorityDecision::Allow)));
        let coordinator = HandoffCoordinator::new(backend);
        coordinator
            .admit_agent_with_context_valid_for(
                &principal(),
                "device-1",
                7,
                9,
                &verification_command(),
                Some(Duration::from_secs(5)),
            )
            .await
            .unwrap();
        let selected = coordinator
            .selected_authority(
                true,
                Some(HandoffSessionFence {
                    generation: 7,
                    capability_revision: 9,
                }),
            )
            .unwrap();
        assert!(!selected.verification_candidate);
        assert_eq!(selected.exact_window.as_ref().unwrap().process_id, 12);
        assert_eq!(selected.exact_window.as_ref().unwrap().window_id, 34);
        assert_eq!(
            coordinator.selected_authority(
                true,
                Some(HandoffSessionFence {
                    generation: 8,
                    capability_revision: 9,
                }),
            ),
            Err(HandoffOperatorControlError::SessionFenceMismatch)
        );
        coordinator.invalidate_context("ctx_0123456789abcdef0123456789abcdef");
        assert_eq!(
            coordinator.selected_authority(
                true,
                Some(HandoffSessionFence {
                    generation: 7,
                    capability_revision: 9,
                }),
            ),
            Err(HandoffOperatorControlError::NoFreshExactWindow)
        );
    }

    #[tokio::test]
    async fn recovery_selection_uses_fresh_denied_observation_not_prior_admitted_target() {
        let backend = Arc::new(FakeAuthority::new(Ok(AgentAuthorityDecision::Deny)));
        let coordinator = HandoffCoordinator::new(backend);
        assert_eq!(
            coordinator
                .admit_agent_with_context_valid_for(
                    &principal(),
                    "device-1",
                    11,
                    12,
                    &verification_command(),
                    Some(Duration::from_secs(5)),
                )
                .await,
            Err(HandoffCoordinatorError::AuthoritySuspended)
        );
        let session = Some(HandoffSessionFence {
            generation: 11,
            capability_revision: 12,
        });
        assert!(coordinator.selected_authority(false, session).is_ok());
        assert_eq!(
            coordinator.selected_authority(true, session),
            Err(HandoffOperatorControlError::NoFreshExactWindow)
        );
    }

    #[tokio::test]
    async fn expired_context_lifetime_never_remains_a_begin_candidate() {
        let backend = Arc::new(FakeAuthority::new(Ok(AgentAuthorityDecision::Allow)));
        let coordinator = HandoffCoordinator::new(backend);
        coordinator
            .admit_agent_with_context_valid_for(
                &principal(),
                "device-1",
                7,
                9,
                &verification_command(),
                Some(Duration::ZERO),
            )
            .await
            .unwrap();
        assert_eq!(
            coordinator.selected_authority(
                true,
                Some(HandoffSessionFence {
                    generation: 7,
                    capability_revision: 9,
                }),
            ),
            Err(HandoffOperatorControlError::NoFreshExactWindow)
        );
    }

    #[tokio::test]
    async fn coordinator_leaves_future_terminal_commands_outside_window_handoff() {
        let backend = Arc::new(FakeAuthority::new(Ok(AgentAuthorityDecision::Allow)));
        let coordinator = HandoffCoordinator::new(backend.clone());
        let command = DeviceCommand::Shell {
            request: crate::v2_m0::ShellRequest {
                command: "printf should-not-cross-handoff".into(),
                cwd: ".".into(),
                env: Vec::new(),
                timeout_ms: 1_000,
            },
        };
        assert!(
            coordinator
                .admit_agent(&principal(), "device-1", 7, 9, &command)
                .await
                .unwrap()
                .is_none()
        );
        assert!(backend.admissions.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn coordinator_rejects_verification_decision_for_non_verification_action() {
        let backend = Arc::new(FakeAuthority::new(Ok(
            AgentAuthorityDecision::Verification(VerificationToken {
                intervention_id: "intervention_1".into(),
                epoch: 2,
            }),
        )));
        let coordinator = HandoffCoordinator::new(backend);
        let command = DeviceCommand::InspectWindowContextual {
            context_id: "ctx_0123456789abcdef0123456789abcdef".into(),
            process_id: 12,
            window_id: 34,
            query: None,
            max_elements: 8,
            max_depth: 4,
            include_screenshot: false,
        };
        assert_eq!(
            coordinator
                .admit_agent(&principal(), "device-1", 7, 9, &command)
                .await,
            Err(HandoffCoordinatorError::InvalidSemanticDecision)
        );
    }

    #[tokio::test]
    async fn coordinator_maps_canonical_deny_to_suspended_authority() {
        let backend = Arc::new(FakeAuthority::new(Ok(AgentAuthorityDecision::Deny)));
        let coordinator = HandoffCoordinator::new(backend);
        assert_eq!(
            coordinator
                .admit_agent(&principal(), "device-1", 7, 9, &verification_command())
                .await,
            Err(HandoffCoordinatorError::AuthoritySuspended)
        );
    }

    #[tokio::test]
    async fn coordinator_backend_unavailable_fails_closed() {
        let backend = Arc::new(FakeAuthority::new(Err(OperatorHandoffError::Unavailable)));
        let coordinator = HandoffCoordinator::new(backend);
        assert_eq!(
            coordinator
                .admit_agent(&principal(), "device-1", 7, 9, &verification_command())
                .await,
            Err(HandoffCoordinatorError::Backend(
                OperatorHandoffError::Unavailable
            ))
        );
    }

    #[tokio::test]
    async fn coordinator_reports_only_satisfied_boolean_after_verification() {
        let backend = Arc::new(FakeAuthority::new(Ok(
            AgentAuthorityDecision::Verification(VerificationToken {
                intervention_id: "intervention_1".into(),
                epoch: 2,
            }),
        )));
        let coordinator = HandoffCoordinator::new(backend.clone());
        let admission = coordinator
            .admit_agent(&principal(), "device-1", 7, 9, &verification_command())
            .await
            .unwrap()
            .unwrap();
        let result = DeviceResult::UiStateVerification {
            status: VerificationStatus::Satisfied,
            stable: true,
            samples: 2,
            predicates: Vec::new(),
            screenshot: None,
        };
        coordinator
            .report_verification(admission, &result)
            .await
            .unwrap();
        let reports = backend.reports.lock().unwrap();
        assert_eq!(reports.len(), 1);
        assert!(reports[0].satisfied);
        assert_eq!(reports[0].token.epoch, 2);
        assert_eq!(
            reports[0]
                .authority
                .exact_window
                .as_ref()
                .unwrap()
                .window_id,
            34
        );
    }
}
