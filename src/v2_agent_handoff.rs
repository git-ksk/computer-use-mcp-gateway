//! Agent-owned Handoff coordination for the controlled device.
//!
//! The canonical `mcp-execution-handoff` runtime remains the only authority/FSM implementation.
//! This wrapper validates the signed Hub-provided session/surface fence, translates the bounded
//! Hub-Agent Handoff control contract, and provides the final local admission gate immediately
//! before a protected surface command can reach the local backend.

use crate::{
    v2_m0::DeviceCommand,
    v2_m0_transport::{
        RemoteHandoffAdmissionDecision, RemoteHandoffAuthority, RemoteHandoffErrorCode,
        RemoteHandoffExactWindow, RemoteHandoffOperatorCommand, RemoteHandoffRequestKind,
        RemoteHandoffResponseKind, RemoteHandoffStatus, RemoteHandoffSurfaceBinding,
    },
    v2_operator_handoff::{
        AgentAuthorityDecision, AgentAuthorityRequest, ExactWindowBinding, HandoffControlError,
        HandoffInterventionStatus, HandoffRuntimeControl, HandoffRuntimeStatus,
        ManagedOperatorHandoffAuthority, OperatorHandoffAuthority, OperatorHandoffError,
        VerificationReport, VerificationToken, device_binding, exact_window_binding,
        is_exact_verification_candidate,
    },
};
use std::sync::Arc;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentHandoffSessionFence {
    pub device_binding: String,
    pub generation: u64,
    pub capability_revision: u64,
}

impl AgentHandoffSessionFence {
    pub fn for_device(device_id: &str, generation: u64, capability_revision: u64) -> Self {
        Self {
            device_binding: device_binding(device_id),
            generation,
            capability_revision,
        }
    }
}

#[derive(Clone)]
pub struct AgentHandoffCoordinator {
    runtime: Arc<ManagedOperatorHandoffAuthority>,
}

impl AgentHandoffCoordinator {
    pub fn new(runtime: Arc<ManagedOperatorHandoffAuthority>) -> Self {
        Self { runtime }
    }

    pub async fn shutdown(&self) {
        self.runtime.shutdown().await;
    }

    pub async fn handle_remote(
        &self,
        request: RemoteHandoffRequestKind,
        session: AgentHandoffSessionFence,
    ) -> RemoteHandoffResponseKind {
        match self.handle_remote_inner(request, session).await {
            Ok(response) => response,
            Err(code) => RemoteHandoffResponseKind::Rejected { code },
        }
    }

    async fn handle_remote_inner(
        &self,
        request: RemoteHandoffRequestKind,
        session: AgentHandoffSessionFence,
    ) -> Result<RemoteHandoffResponseKind, RemoteHandoffErrorCode> {
        match request {
            RemoteHandoffRequestKind::Admission { authority } => {
                let authority = self.authority(authority, &session)?;
                let decision = self
                    .runtime
                    .admit_agent(authority)
                    .await
                    .map_err(map_authority_error)?;
                Ok(RemoteHandoffResponseKind::Admission {
                    decision: admission_decision(decision),
                })
            }
            RemoteHandoffRequestKind::Operator { command, authority } => {
                self.operator(command, authority, session).await
            }
        }
    }

    async fn operator(
        &self,
        command: RemoteHandoffOperatorCommand,
        authority: Option<RemoteHandoffAuthority>,
        session: AgentHandoffSessionFence,
    ) -> Result<RemoteHandoffResponseKind, RemoteHandoffErrorCode> {
        match command {
            RemoteHandoffOperatorCommand::Status => {
                if authority.is_some() {
                    return Err(RemoteHandoffErrorCode::InvalidRequest);
                }
                let status = self.runtime.status().await.map_err(map_control_error)?;
                Ok(RemoteHandoffResponseKind::Status {
                    status: remote_status(&status),
                })
            }
            RemoteHandoffOperatorCommand::Begin => {
                let authority = self.required_control_authority(authority, &session)?;
                let status = self
                    .runtime
                    .control(HandoffRuntimeControl::Begin { authority })
                    .await
                    .map_err(map_control_error)?;
                Ok(operator_status(status))
            }
            RemoteHandoffOperatorCommand::RecoverReissue => {
                let authority = self.required_control_authority(authority, &session)?;
                let status = self.runtime.status().await.map_err(map_control_error)?;
                if !status.recovery_required || status.recovery_expired {
                    return Err(RemoteHandoffErrorCode::Rejected);
                }
                let status = self
                    .runtime
                    .control(HandoffRuntimeControl::RecoverReissue { authority })
                    .await
                    .map_err(map_control_error)?;
                Ok(operator_status(status))
            }
            RemoteHandoffOperatorCommand::RecoverRebind {
                prior_context_id,
                prior_generation,
                prior_capability_revision,
            } => {
                let authority = self.required_control_authority(authority, &session)?;
                if !valid_context_id(&prior_context_id)
                    || prior_generation
                        .is_some_and(|value| value == 0 || value > session.generation)
                    || prior_capability_revision
                        .is_some_and(|value| value > session.capability_revision)
                {
                    return Err(RemoteHandoffErrorCode::InvalidRequest);
                }
                let status = self.runtime.status().await.map_err(map_control_error)?;
                if !status.recovery_required {
                    return Err(RemoteHandoffErrorCode::Rejected);
                }
                let status = self
                    .runtime
                    .control(HandoffRuntimeControl::RecoverRebind {
                        authority,
                        prior_context_id,
                        prior_generation,
                        prior_capability_revision,
                    })
                    .await
                    .map_err(map_control_error)?;
                Ok(operator_status(status))
            }
            RemoteHandoffOperatorCommand::RebindLive => {
                let authority = self.required_control_authority(authority, &session)?;
                let status = self.runtime.status().await.map_err(map_control_error)?;
                if status.recovery_required || status.active.is_none() || status.faulted {
                    return Err(RemoteHandoffErrorCode::Rejected);
                }
                let status = self
                    .runtime
                    .control(HandoffRuntimeControl::RebindLive { authority })
                    .await
                    .map_err(map_control_error)?;
                Ok(operator_status(status))
            }
            RemoteHandoffOperatorCommand::AbandonExpiredRecovery { expected_epoch } => {
                if authority.is_some() || expected_epoch == 0 {
                    return Err(RemoteHandoffErrorCode::InvalidRequest);
                }
                let status = self.runtime.status().await.map_err(map_control_error)?;
                if !status.recovery_required
                    || !status.recovery_expired
                    || status.recovery_epoch != Some(expected_epoch)
                    || status.active.is_some()
                    || status.faulted
                {
                    return Err(RemoteHandoffErrorCode::Rejected);
                }
                let status = self
                    .runtime
                    .control(HandoffRuntimeControl::AbandonExpiredRecovery { expected_epoch })
                    .await
                    .map_err(map_control_error)?;
                Ok(operator_status(status))
            }
            RemoteHandoffOperatorCommand::RequestResume => {
                if authority.is_some() {
                    return Err(RemoteHandoffErrorCode::InvalidRequest);
                }
                let status = self.runtime.status().await.map_err(map_control_error)?;
                let active = status
                    .active
                    .filter(|active| active.status == HandoffInterventionStatus::ReadyToResume)
                    .ok_or(RemoteHandoffErrorCode::Rejected)?;
                let status = self
                    .runtime
                    .control(HandoffRuntimeControl::RequestResume {
                        intervention_id: active.intervention_id,
                        epoch: active.epoch,
                    })
                    .await
                    .map_err(map_control_error)?;
                Ok(operator_status(status))
            }
            RemoteHandoffOperatorCommand::CancelBeforeHuman => {
                if authority.is_some() {
                    return Err(RemoteHandoffErrorCode::InvalidRequest);
                }
                let status = self.runtime.status().await.map_err(map_control_error)?;
                let active = status
                    .active
                    .filter(|active| active.status == HandoffInterventionStatus::AwaitingHuman)
                    .ok_or(RemoteHandoffErrorCode::Rejected)?;
                let status = self
                    .runtime
                    .control(HandoffRuntimeControl::CancelBeforeHuman {
                        intervention_id: active.intervention_id,
                        epoch: active.epoch,
                    })
                    .await
                    .map_err(map_control_error)?;
                Ok(operator_status(status))
            }
        }
    }

    /// Final local authority gate. A configured Agent calls this after consuming the
    /// one-shot CUMG grant and persisting its replay barrier, but immediately before
    /// any protected Window backend invocation.
    pub async fn final_admit(
        &self,
        authority: RemoteHandoffAuthority,
        command: &DeviceCommand,
        session: AgentHandoffSessionFence,
    ) -> Result<AgentAuthorityDecision, RemoteHandoffErrorCode> {
        if !authority_matches_command(&authority, command) {
            return Err(RemoteHandoffErrorCode::InvalidRequest);
        }
        let authority = self.authority(authority, &session)?;
        self.runtime
            .admit_agent(authority)
            .await
            .map_err(map_authority_error)
    }

    /// Verification belongs to the controlled Agent. The Hub never reports the
    /// verification predicate result back into the Handoff FSM. Only the boolean
    /// postcondition and the already-returned intervention token cross this local seam.
    pub async fn report_verification_local(
        &self,
        authority: RemoteHandoffAuthority,
        token: VerificationToken,
        satisfied: bool,
        session: AgentHandoffSessionFence,
    ) -> Result<(), RemoteHandoffErrorCode> {
        let authority = self.authority(authority, &session)?;
        if !authority.verification_candidate
            || authority.exact_window.is_none()
            || token.intervention_id.is_empty()
            || token.intervention_id.len() > 160
            || token.epoch == 0
        {
            return Err(RemoteHandoffErrorCode::InvalidRequest);
        }
        self.runtime
            .report_verification(VerificationReport {
                authority,
                token,
                satisfied,
            })
            .await
            .map_err(map_authority_error)
    }

    fn required_control_authority(
        &self,
        authority: Option<RemoteHandoffAuthority>,
        session: &AgentHandoffSessionFence,
    ) -> Result<AgentAuthorityRequest, RemoteHandoffErrorCode> {
        let authority = authority.ok_or(RemoteHandoffErrorCode::InvalidRequest)?;
        let authority = self.authority(authority, session)?;
        if authority.verification_candidate || authority.exact_window.is_none() {
            return Err(RemoteHandoffErrorCode::InvalidRequest);
        }
        Ok(authority)
    }

    fn authority(
        &self,
        authority: RemoteHandoffAuthority,
        session: &AgentHandoffSessionFence,
    ) -> Result<AgentAuthorityRequest, RemoteHandoffErrorCode> {
        if !authority_matches_session(&authority, session) {
            return Err(RemoteHandoffErrorCode::SessionFenceMismatch);
        }
        let exact_window = authority
            .surface
            .map(|RemoteHandoffSurfaceBinding::OsWindow(window)| exact_window(window));
        if let Some(window) = exact_window.as_ref()
            && (!valid_binding(&window.context_binding)
                || window.process_id == 0
                || window.window_id == 0)
        {
            return Err(RemoteHandoffErrorCode::InvalidRequest);
        }
        Ok(AgentAuthorityRequest {
            principal_binding: authority.principal_binding,
            device_binding: authority.device_binding,
            generation: authority.generation,
            capability_revision: authority.capability_revision,
            exact_window,
            verification_candidate: authority.verification_candidate,
        })
    }
}

fn authority_matches_command(authority: &RemoteHandoffAuthority, command: &DeviceCommand) -> bool {
    if authority.verification_candidate != is_exact_verification_candidate(command) {
        return false;
    }
    let expected_window = exact_window_binding(command);
    matches!(
        (&authority.surface, expected_window.as_ref()),
        (Some(RemoteHandoffSurfaceBinding::OsWindow(remote)), Some(expected))
            if remote.context_binding == expected.context_binding
                && remote.process_id == expected.process_id
                && remote.window_id == expected.window_id
    ) || matches!((&authority.surface, expected_window.as_ref()), (None, None))
}

fn authority_matches_session(
    authority: &RemoteHandoffAuthority,
    session: &AgentHandoffSessionFence,
) -> bool {
    authority.generation == session.generation
        && authority.capability_revision == session.capability_revision
        && authority.device_binding == session.device_binding
        && valid_binding(&authority.principal_binding)
        && valid_binding(&authority.device_binding)
}

fn exact_window(window: RemoteHandoffExactWindow) -> ExactWindowBinding {
    ExactWindowBinding {
        context_binding: window.context_binding,
        process_id: window.process_id,
        window_id: window.window_id,
    }
}

fn admission_decision(decision: AgentAuthorityDecision) -> RemoteHandoffAdmissionDecision {
    match decision {
        AgentAuthorityDecision::Allow => RemoteHandoffAdmissionDecision::Allow,
        AgentAuthorityDecision::Deny => RemoteHandoffAdmissionDecision::Deny,
        AgentAuthorityDecision::Verification(token) => {
            RemoteHandoffAdmissionDecision::Verification {
                intervention_id: token.intervention_id,
                epoch: token.epoch,
            }
        }
    }
}

fn operator_status(status: HandoffRuntimeStatus) -> RemoteHandoffResponseKind {
    let locator = status.locator.clone();
    RemoteHandoffResponseKind::Operator {
        status: remote_status(&status),
        locator,
    }
}

fn remote_status(status: &HandoffRuntimeStatus) -> RemoteHandoffStatus {
    RemoteHandoffStatus {
        active: status.active.clone(),
        recovery_required: status.recovery_required,
        recovery_status: status.recovery_status,
        recovery_epoch: status.recovery_epoch,
        recovery_expired: status.recovery_expired,
        resume_requested: status.resume_requested,
        faulted: status.faulted,
        human_surface: status.human_surface,
    }
}

fn map_authority_error(error: OperatorHandoffError) -> RemoteHandoffErrorCode {
    match error {
        OperatorHandoffError::Unavailable => RemoteHandoffErrorCode::Unavailable,
        OperatorHandoffError::Protocol => RemoteHandoffErrorCode::Protocol,
        OperatorHandoffError::Unsupported => RemoteHandoffErrorCode::Unsupported,
    }
}

fn map_control_error(error: HandoffControlError) -> RemoteHandoffErrorCode {
    match error {
        HandoffControlError::Unavailable => RemoteHandoffErrorCode::Unavailable,
        HandoffControlError::Rejected => RemoteHandoffErrorCode::Rejected,
        HandoffControlError::Protocol => RemoteHandoffErrorCode::Protocol,
    }
}

fn valid_binding(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
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
    use crate::v2_m0::UiPredicate;

    fn inspect_command(window_id: u64) -> DeviceCommand {
        DeviceCommand::InspectWindowContextual {
            context_id: "ctx_0123456789abcdef0123456789abcdef".into(),
            process_id: 12,
            window_id,
            query: None,
            max_elements: 16,
            max_depth: 4,
            include_screenshot: false,
        }
    }

    fn verification_command(window_id: u64) -> DeviceCommand {
        DeviceCommand::VerifyUiStateContextual {
            context_id: "ctx_0123456789abcdef0123456789abcdef".into(),
            process_id: 12,
            window_id,
            predicates: Vec::<UiPredicate>::new(),
            timeout_ms: 500,
            stable_samples: 2,
            include_screenshot: false,
        }
    }

    fn authority_for(
        command: &DeviceCommand,
        device_id: &str,
        verification_candidate: bool,
    ) -> RemoteHandoffAuthority {
        let exact = exact_window_binding(command).expect("test command is exact-window scoped");
        RemoteHandoffAuthority {
            principal_binding: "a".repeat(64),
            device_binding: device_binding(device_id),
            generation: 7,
            capability_revision: 3,
            surface: Some(RemoteHandoffSurfaceBinding::OsWindow(
                RemoteHandoffExactWindow {
                    context_binding: exact.context_binding,
                    process_id: exact.process_id,
                    window_id: exact.window_id,
                },
            )),
            verification_candidate,
        }
    }

    #[test]
    fn final_gate_authority_is_bound_to_the_actual_exact_window_and_verification_shape() {
        let inspect = inspect_command(34);
        let authority = authority_for(&inspect, "device-a", false);
        assert!(authority_matches_command(&authority, &inspect));
        assert!(!authority_matches_command(&authority, &inspect_command(35)));

        let mut wrong_verification_flag = authority.clone();
        wrong_verification_flag.verification_candidate = true;
        assert!(!authority_matches_command(
            &wrong_verification_flag,
            &inspect
        ));

        let verify = verification_command(34);
        let verify_authority = authority_for(&verify, "device-a", true);
        assert!(authority_matches_command(&verify_authority, &verify));
    }

    #[test]
    fn final_gate_recomputes_device_binding_and_rejects_stale_session_fences() {
        let command = inspect_command(34);
        let authority = authority_for(&command, "device-a", false);
        let current = AgentHandoffSessionFence::for_device("device-a", 7, 3);
        assert!(authority_matches_session(&authority, &current));

        assert!(!authority_matches_session(
            &authority,
            &AgentHandoffSessionFence::for_device("device-b", 7, 3),
        ));
        assert!(!authority_matches_session(
            &authority,
            &AgentHandoffSessionFence::for_device("device-a", 8, 3),
        ));
        assert!(!authority_matches_session(
            &authority,
            &AgentHandoffSessionFence::for_device("device-a", 7, 4),
        ));
    }
}
