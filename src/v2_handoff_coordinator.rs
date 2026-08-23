//! First-class CUMG coordination boundary for Human handoff.
//!
//! CUMG owns authenticated principal/device/capability/generation admission and consumer-specific
//! postcondition verification. `mcp-execution-handoff` remains the canonical owner of generic
//! Agent/Human authority, intervention/epoch transitions, `Done -> verifying`, and resume policy.
//! This module deliberately does not implement a second Handoff state machine.
//!
//! Normal runtime integration uses a Hub-owned managed Handoff process. The accepted Unix bridge is
//! retained only as a compatibility/regression backend. Both stay behind one surface-neutral seam
//! so Terminal/PTY #48 can bind a new surface without duplicating authority semantics.

use crate::{
    v2_m0::{DeviceCommand, DeviceResult, VerificationStatus},
    v2_m0_trust::AuthenticatedClientPrincipal,
    v2_operator_handoff::{
        AgentAuthorityDecision, AgentAuthorityRequest, ExactWindowBinding,
        ManagedOperatorHandoffAuthority, OperatorHandoffAuthority, OperatorHandoffError,
        VerificationReport, VerificationToken, device_binding, exact_window_binding,
        is_exact_verification_candidate, is_phase1_protected_command, principal_binding,
    },
};
use std::{fmt, sync::Arc};

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
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct HandoffAdmission {
    pub binding: HandoffAgentBinding,
    pub verification: Option<VerificationToken>,
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

/// CUMG-side coordinator. The backend supplies canonical Handoff semantic decisions; this type
/// supplies the consumer binding and validation around those decisions.
#[derive(Clone)]
pub struct HandoffCoordinator {
    backend: Arc<dyn OperatorHandoffAuthority>,
    managed_runtime: Option<Arc<ManagedOperatorHandoffAuthority>>,
}

impl HandoffCoordinator {
    /// Compatibility constructor for the acceptance-only Unix bridge backend.
    pub fn new(backend: Arc<dyn OperatorHandoffAuthority>) -> Self {
        Self {
            backend,
            managed_runtime: None,
        }
    }

    /// Normal CUMG runtime constructor. The Hub owns this process for the lifetime of the
    /// coordinator; runtime failure is fail-closed and never triggers authority-restoring restart.
    pub fn managed(runtime: Arc<ManagedOperatorHandoffAuthority>) -> Self {
        Self {
            backend: runtime.clone(),
            managed_runtime: Some(runtime),
        }
    }

    pub async fn shutdown(&self) {
        if let Some(runtime) = self.managed_runtime.as_ref() {
            runtime.shutdown().await;
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
    pub(crate) async fn admit_agent(
        &self,
        principal: &AuthenticatedClientPrincipal,
        device_id: &str,
        generation: u64,
        capability_revision: u64,
        command: &DeviceCommand,
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
        let decision = self.backend.admit_agent(request).await?;
        match decision {
            AgentAuthorityDecision::Allow => Ok(Some(HandoffAdmission {
                binding,
                verification: None,
            })),
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
