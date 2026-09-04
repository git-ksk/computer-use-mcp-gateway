//! Transport-neutral hosted operator-control seam for Agent-owned Human Handoff.
//!
//! This module deliberately does not expose an MCP tool or own a second Handoff FSM. A hosted
//! HTTP/OIDC/Trusted-Proxy adapter may authenticate a Human operator and call this service, but the
//! exact target surface remains selected by CUMG's [`HandoffCoordinator`] from a fresh observed or
//! admitted interaction context. The caller cannot manufacture PID/window authority.

use crate::{
    v2_handoff_coordinator::{
        HandoffCoordinator, HandoffOperatorCommand, HandoffOperatorControlError,
        HandoffSessionFence,
    },
    v2_m0_trust::AuthenticatedClientPrincipal,
    v2_m1_hub::HubHandle,
    v2_operator_handoff::HandoffRuntimeStatus,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, HashSet},
    fmt,
    sync::Arc,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HostedHandoffAction {
    Status,
    Begin,
    RecoverReissue,
    RecoverRebind,
    RebindLive,
    AbandonExpiredRecovery,
    RequestResume,
    CancelBeforeHuman,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostedHandoffControlRequest {
    pub action: HostedHandoffAction,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prior_context_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prior_generation: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prior_capability_revision: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_epoch: Option<u64>,
}

impl fmt::Debug for HostedHandoffControlRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HostedHandoffControlRequest")
            .field("action", &self.action)
            .field("has_prior_context", &self.prior_context_id.is_some())
            .field("has_prior_generation", &self.prior_generation.is_some())
            .field(
                "has_prior_capability_revision",
                &self.prior_capability_revision.is_some(),
            )
            .field("has_expected_epoch", &self.expected_epoch.is_some())
            .finish()
    }
}

impl HostedHandoffControlRequest {
    pub const fn action(&self) -> HostedHandoffAction {
        self.action
    }

    fn into_operator_command(self) -> Result<HandoffOperatorCommand, HostedHandoffControlError> {
        let has_prior = self.prior_context_id.is_some()
            || self.prior_generation.is_some()
            || self.prior_capability_revision.is_some();
        let has_expected_epoch = self.expected_epoch.is_some();
        match self.action {
            HostedHandoffAction::Status
            | HostedHandoffAction::Begin
            | HostedHandoffAction::RecoverReissue
            | HostedHandoffAction::RebindLive
            | HostedHandoffAction::RequestResume
            | HostedHandoffAction::CancelBeforeHuman
                if !has_prior && !has_expected_epoch =>
            {
                Ok(match self.action {
                    HostedHandoffAction::Status => HandoffOperatorCommand::Status,
                    HostedHandoffAction::Begin => HandoffOperatorCommand::Begin,
                    HostedHandoffAction::RecoverReissue => HandoffOperatorCommand::RecoverReissue,
                    HostedHandoffAction::RebindLive => HandoffOperatorCommand::RebindLive,
                    HostedHandoffAction::RequestResume => HandoffOperatorCommand::RequestResume,
                    HostedHandoffAction::CancelBeforeHuman => {
                        HandoffOperatorCommand::CancelBeforeHuman
                    }
                    _ => unreachable!(),
                })
            }
            HostedHandoffAction::RecoverRebind if !has_expected_epoch => {
                let prior_context_id = self
                    .prior_context_id
                    .ok_or(HostedHandoffControlError::InvalidRequest)?;
                Ok(HandoffOperatorCommand::RecoverRebind {
                    prior_context_id,
                    prior_generation: self.prior_generation,
                    prior_capability_revision: self.prior_capability_revision,
                })
            }
            HostedHandoffAction::AbandonExpiredRecovery if !has_prior => {
                let expected_epoch = self
                    .expected_epoch
                    .ok_or(HostedHandoffControlError::InvalidRequest)?;
                Ok(HandoffOperatorCommand::AbandonExpiredRecovery { expected_epoch })
            }
            _ => Err(HostedHandoffControlError::InvalidRequest),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct HostedHandoffPrincipalDeviceKey {
    issuer: String,
    subject: String,
    device_id: String,
}

#[derive(Debug, Clone, Default)]
pub struct HostedHandoffAuthorizationPolicy {
    allowed: HashMap<HostedHandoffPrincipalDeviceKey, HashSet<HostedHandoffAction>>,
}

impl HostedHandoffAuthorizationPolicy {
    pub fn allow(
        &mut self,
        principal: &AuthenticatedClientPrincipal,
        device_id: &str,
        action: HostedHandoffAction,
    ) {
        self.allowed
            .entry(HostedHandoffPrincipalDeviceKey {
                issuer: principal.issuer.clone(),
                subject: principal.subject.clone(),
                device_id: device_id.to_owned(),
            })
            .or_default()
            .insert(action);
    }

    pub fn authorize(
        &self,
        principal: &AuthenticatedClientPrincipal,
        device_id: &str,
        action: HostedHandoffAction,
    ) -> Result<(), HostedHandoffControlError> {
        let key = HostedHandoffPrincipalDeviceKey {
            issuer: principal.issuer.clone(),
            subject: principal.subject.clone(),
            device_id: device_id.to_owned(),
        };
        if self
            .allowed
            .get(&key)
            .is_some_and(|actions| actions.contains(&action))
        {
            Ok(())
        } else {
            Err(HostedHandoffControlError::Unauthorized)
        }
    }
}

#[derive(Clone)]
pub struct HostedHandoffControlService {
    device_id: Arc<str>,
    policy: Arc<HostedHandoffAuthorizationPolicy>,
    coordinator: Arc<HandoffCoordinator>,
    hub: HubHandle,
}

impl HostedHandoffControlService {
    pub fn new(
        device_id: impl Into<String>,
        policy: Arc<HostedHandoffAuthorizationPolicy>,
        coordinator: Arc<HandoffCoordinator>,
        hub: HubHandle,
    ) -> Result<Self, HostedHandoffControlError> {
        let device_id = device_id.into();
        if device_id.trim().is_empty() || device_id.len() > 256 {
            return Err(HostedHandoffControlError::InvalidConfiguration);
        }
        Ok(Self {
            device_id: Arc::from(device_id),
            policy,
            coordinator,
            hub,
        })
    }

    /// Execute one already-authenticated operator request.
    ///
    /// The hosted adapter supplies only the authenticated principal and the closed lifecycle
    /// request. Exact target authority is never accepted from the caller: `operator_control()`
    /// selects only CUMG's current fresh observed/admitted target and validates the current Agent
    /// generation + capability revision before relaying to the Agent-owned Handoff runtime.
    pub async fn execute(
        &self,
        principal: &AuthenticatedClientPrincipal,
        request: HostedHandoffControlRequest,
    ) -> HostedHandoffControlResponse {
        if let Err(error) = self
            .policy
            .authorize(principal, &self.device_id, request.action())
        {
            return HostedHandoffControlResponse::error(error);
        }
        let session = self
            .hub
            .current_session_binding()
            .await
            .map(|(generation, capabilities)| HandoffSessionFence {
                generation,
                capability_revision: capabilities.revision,
            });
        let command = match request.into_operator_command() {
            Ok(command) => command,
            Err(error) => return HostedHandoffControlResponse::error(error),
        };
        match self.coordinator.operator_control(command, session).await {
            Ok(status) => HostedHandoffControlResponse::success(status),
            Err(error) => HostedHandoffControlResponse::error(error.into()),
        }
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostedHandoffControlResponse {
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<HandoffRuntimeStatus>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_code: Option<String>,
}

impl fmt::Debug for HostedHandoffControlResponse {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HostedHandoffControlResponse")
            .field("ok", &self.ok)
            .field("status_present", &self.status.is_some())
            .field(
                "locator_present",
                &self.status.as_ref().is_some_and(|s| s.locator.is_some()),
            )
            .field("error_code", &self.error_code)
            .finish()
    }
}

impl HostedHandoffControlResponse {
    fn success(status: HandoffRuntimeStatus) -> Self {
        Self {
            ok: true,
            status: Some(status),
            error_code: None,
        }
    }

    fn error(error: HostedHandoffControlError) -> Self {
        Self {
            ok: false,
            status: None,
            error_code: Some(error.safe_code().to_owned()),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostedHandoffControlError {
    InvalidConfiguration,
    Unauthorized,
    InvalidRequest,
    Control(HandoffOperatorControlError),
}

impl HostedHandoffControlError {
    pub const fn safe_code(self) -> &'static str {
        match self {
            Self::InvalidConfiguration => "hosted_handoff_invalid_configuration",
            Self::Unauthorized => "hosted_handoff_unauthorized",
            Self::InvalidRequest => "hosted_handoff_request_invalid",
            Self::Control(error) => error.safe_code(),
        }
    }
}

impl fmt::Display for HostedHandoffControlError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.safe_code())
    }
}

impl std::error::Error for HostedHandoffControlError {}

impl From<HandoffOperatorControlError> for HostedHandoffControlError {
    fn from(value: HandoffOperatorControlError) -> Self {
        Self::Control(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn principal(subject: &str) -> AuthenticatedClientPrincipal {
        AuthenticatedClientPrincipal::new("https://operator.example", subject).unwrap()
    }

    #[test]
    fn hosted_request_is_closed_and_rejects_raw_target_authority() {
        let raw = r#"{"action":"begin","process_id":12,"window_id":34}"#;
        assert!(serde_json::from_str::<HostedHandoffControlRequest>(raw).is_err());

        let raw = r#"{"action":"recover_rebind","prior_context_id":"ctx_0123456789abcdef0123456789abcdef","prior_generation":7,"prior_capability_revision":8,"window_id":34}"#;
        assert!(serde_json::from_str::<HostedHandoffControlRequest>(raw).is_err());
    }

    #[test]
    fn hosted_recovery_request_carries_only_bounded_prior_proof_fields() {
        let raw = r#"{"action":"recover_rebind","prior_context_id":"ctx_0123456789abcdef0123456789abcdef","prior_generation":7,"prior_capability_revision":8}"#;
        let parsed: HostedHandoffControlRequest = serde_json::from_str(raw).unwrap();
        assert_eq!(parsed.action(), HostedHandoffAction::RecoverRebind);
        assert!(parsed.clone().into_operator_command().is_ok());
        assert!(!format!("{parsed:?}").contains("ctx_0123456789abcdef"));
    }

    #[test]
    fn hosted_request_rejects_action_field_mismatch() {
        let raw = r#"{"action":"begin","expected_epoch":7}"#;
        let parsed: HostedHandoffControlRequest = serde_json::from_str(raw).unwrap();
        assert_eq!(
            parsed.into_operator_command(),
            Err(HostedHandoffControlError::InvalidRequest)
        );

        let raw = r#"{"action":"abandon_expired_recovery"}"#;
        let parsed: HostedHandoffControlRequest = serde_json::from_str(raw).unwrap();
        assert_eq!(
            parsed.into_operator_command(),
            Err(HostedHandoffControlError::InvalidRequest)
        );
    }

    #[test]
    fn hosted_policy_binds_exact_principal_device_and_action() {
        let alpha = principal("alpha");
        let beta = principal("beta");
        let mut policy = HostedHandoffAuthorizationPolicy::default();
        policy.allow(&alpha, "device-a", HostedHandoffAction::Begin);

        assert!(
            policy
                .authorize(&alpha, "device-a", HostedHandoffAction::Begin)
                .is_ok()
        );
        assert_eq!(
            policy.authorize(&alpha, "device-a", HostedHandoffAction::RequestResume),
            Err(HostedHandoffControlError::Unauthorized)
        );
        assert_eq!(
            policy.authorize(&alpha, "device-b", HostedHandoffAction::Begin),
            Err(HostedHandoffControlError::Unauthorized)
        );
        assert_eq!(
            policy.authorize(&beta, "device-a", HostedHandoffAction::Begin),
            Err(HostedHandoffControlError::Unauthorized)
        );
    }

    #[test]
    fn hosted_response_debug_never_emits_locator_capability() {
        let response = HostedHandoffControlResponse::success(HandoffRuntimeStatus {
            active: None,
            recovery_required: false,
            recovery_status: None,
            recovery_epoch: None,
            recovery_expired: false,
            resume_requested: false,
            faulted: false,
            human_surface: None,
            locator: Some("https://handoff.example/secret-capability".to_owned()),
        });
        let debug = format!("{response:?}");
        assert!(debug.contains("locator_present: true"));
        assert!(!debug.contains("secret-capability"));
    }
}
