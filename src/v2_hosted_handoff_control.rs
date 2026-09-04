//! Transport-neutral hosted operator-control seam for Agent-owned Human Handoff.
//!
//! This module deliberately does not expose an MCP tool or own a second Handoff FSM. A hosted
//! HTTP/OIDC/Trusted-Proxy adapter may authenticate a Human operator and call this service, but the
//! exact target surface remains selected by CUMG's [`HandoffCoordinator`] from a fresh observed or
//! admitted interaction context. The caller cannot manufacture PID/window authority.

use crate::{
    v2_handoff_coordinator::{
        HandoffCoordinator, HandoffOperatorCommand, HandoffOperatorControlError,
        HandoffSessionFence, valid_operator_context_handle,
    },
    v2_m0_trust::AuthenticatedClientPrincipal,
    v2_m1_hub::HubHandle,
    v2_operator_handoff::HandoffRuntimeStatus,
};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, HashSet},
    fmt,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

const HOSTED_CONTEXT_BINDING_TTL: Duration = Duration::from_secs(60);

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

impl HostedHandoffAction {
    pub const fn may_return_locator(self) -> bool {
        matches!(
            self,
            Self::Begin | Self::RecoverReissue | Self::RecoverRebind | Self::RebindLive
        )
    }

    pub const fn requires_surface_context(self) -> bool {
        matches!(
            self,
            Self::Begin | Self::RecoverReissue | Self::RecoverRebind | Self::RebindLive
        )
    }

    const fn uses_admitted_surface(self) -> bool {
        matches!(self, Self::Begin)
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostedHandoffControlRequest {
    pub action: HostedHandoffAction,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_handle: Option<String>,
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
            .field("has_context_handle", &self.context_handle.is_some())
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

    fn into_operator_command(
        self,
    ) -> Result<(HandoffOperatorCommand, Option<String>), HostedHandoffControlError> {
        let has_prior = self.prior_context_id.is_some()
            || self.prior_generation.is_some()
            || self.prior_capability_revision.is_some();
        let has_expected_epoch = self.expected_epoch.is_some();
        let context_handle = match (self.action.requires_surface_context(), self.context_handle) {
            (true, Some(handle)) if valid_operator_context_handle(&handle) => Some(handle),
            (true, _) => return Err(HostedHandoffControlError::InvalidRequest),
            (false, None) => None,
            (false, Some(_)) => return Err(HostedHandoffControlError::InvalidRequest),
        };
        let command = match self.action {
            HostedHandoffAction::Status
            | HostedHandoffAction::Begin
            | HostedHandoffAction::RecoverReissue
            | HostedHandoffAction::RebindLive
            | HostedHandoffAction::RequestResume
            | HostedHandoffAction::CancelBeforeHuman
                if !has_prior && !has_expected_epoch =>
            {
                match self.action {
                    HostedHandoffAction::Status => HandoffOperatorCommand::Status,
                    HostedHandoffAction::Begin => HandoffOperatorCommand::Begin,
                    HostedHandoffAction::RecoverReissue => HandoffOperatorCommand::RecoverReissue,
                    HostedHandoffAction::RebindLive => HandoffOperatorCommand::RebindLive,
                    HostedHandoffAction::RequestResume => HandoffOperatorCommand::RequestResume,
                    HostedHandoffAction::CancelBeforeHuman => {
                        HandoffOperatorCommand::CancelBeforeHuman
                    }
                    _ => unreachable!(),
                }
            }
            HostedHandoffAction::RecoverRebind if !has_expected_epoch => {
                let prior_context_id = self
                    .prior_context_id
                    .ok_or(HostedHandoffControlError::InvalidRequest)?;
                HandoffOperatorCommand::RecoverRebind {
                    prior_context_id,
                    prior_generation: self.prior_generation,
                    prior_capability_revision: self.prior_capability_revision,
                }
            }
            HostedHandoffAction::AbandonExpiredRecovery if !has_prior => {
                let expected_epoch = self
                    .expected_epoch
                    .ok_or(HostedHandoffControlError::InvalidRequest)?;
                HandoffOperatorCommand::AbandonExpiredRecovery { expected_epoch }
            }
            _ => return Err(HostedHandoffControlError::InvalidRequest),
        };
        Ok((command, context_handle))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostedHandoffContextRequest {
    pub action: HostedHandoffAction,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostedHandoffContextResponse {
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_handle: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_code: Option<String>,
}

impl fmt::Debug for HostedHandoffContextResponse {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HostedHandoffContextResponse")
            .field("ok", &self.ok)
            .field("context_handle_present", &self.context_handle.is_some())
            .field("error_code", &self.error_code)
            .finish()
    }
}

impl HostedHandoffContextResponse {
    fn success(context_handle: String) -> Self {
        Self {
            ok: true,
            context_handle: Some(context_handle),
            error_code: None,
        }
    }

    fn error(error: HostedHandoffControlError) -> Self {
        Self {
            ok: false,
            context_handle: None,
            error_code: Some(error.safe_code().to_owned()),
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
struct IssuedHostedContext {
    issuer: String,
    subject: String,
    action: HostedHandoffAction,
    expires_at: Instant,
}

#[derive(Default)]
struct IssuedHostedContexts {
    bindings: Mutex<HashMap<String, IssuedHostedContext>>,
}

impl IssuedHostedContexts {
    fn remember(
        &self,
        principal: &AuthenticatedClientPrincipal,
        action: HostedHandoffAction,
        handle: &str,
    ) {
        let now = Instant::now();
        let mut issued = self
            .bindings
            .lock()
            .expect("hosted Handoff context lock poisoned");
        issued.retain(|_, binding| binding.expires_at > now);
        issued.insert(
            handle.to_owned(),
            IssuedHostedContext {
                issuer: principal.issuer.clone(),
                subject: principal.subject.clone(),
                action,
                expires_at: now + HOSTED_CONTEXT_BINDING_TTL,
            },
        );
    }

    fn validate(
        &self,
        principal: &AuthenticatedClientPrincipal,
        action: HostedHandoffAction,
        handle: &str,
    ) -> Result<(), HostedHandoffControlError> {
        self.validate_at(principal, action, handle, Instant::now())
    }

    fn validate_at(
        &self,
        principal: &AuthenticatedClientPrincipal,
        action: HostedHandoffAction,
        handle: &str,
        now: Instant,
    ) -> Result<(), HostedHandoffControlError> {
        let mut issued = self
            .bindings
            .lock()
            .expect("hosted Handoff context lock poisoned");
        issued.retain(|_, binding| binding.expires_at > now);
        let binding = issued
            .get(handle)
            .ok_or(HostedHandoffControlError::ContextInvalid)?;
        if binding.issuer != principal.issuer || binding.subject != principal.subject {
            return Err(HostedHandoffControlError::Unauthorized);
        }
        if binding.action != action {
            return Err(HostedHandoffControlError::ContextInvalid);
        }
        Ok(())
    }
}

#[async_trait]
pub trait HostedHandoffControlApi: Send + Sync {
    async fn issue_context(
        &self,
        principal: &AuthenticatedClientPrincipal,
        request: HostedHandoffContextRequest,
    ) -> HostedHandoffContextResponse;

    async fn execute(
        &self,
        principal: &AuthenticatedClientPrincipal,
        request: HostedHandoffControlRequest,
    ) -> HostedHandoffControlResponse;
}

#[derive(Clone)]
pub struct HostedHandoffControlService {
    device_id: Arc<str>,
    policy: Arc<HostedHandoffAuthorizationPolicy>,
    coordinator: Arc<HandoffCoordinator>,
    hub: HubHandle,
    issued_contexts: Arc<IssuedHostedContexts>,
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
            issued_contexts: Arc::new(IssuedHostedContexts::default()),
        })
    }

    pub async fn issue_context(
        &self,
        principal: &AuthenticatedClientPrincipal,
        request: HostedHandoffContextRequest,
    ) -> HostedHandoffContextResponse {
        if let Err(error) = self
            .policy
            .authorize(principal, &self.device_id, request.action)
        {
            return HostedHandoffContextResponse::error(error);
        }
        if !request.action.requires_surface_context() {
            return HostedHandoffContextResponse::error(HostedHandoffControlError::InvalidRequest);
        }
        let session = self
            .hub
            .current_session_binding()
            .await
            .map(|(generation, capabilities)| HandoffSessionFence {
                generation,
                capability_revision: capabilities.revision,
            });
        match self
            .coordinator
            .issue_operator_context_handle(request.action.uses_admitted_surface(), session)
        {
            Ok(handle) => {
                self.issued_contexts
                    .remember(principal, request.action, &handle);
                HostedHandoffContextResponse::success(handle)
            }
            Err(error) => HostedHandoffContextResponse::error(error.into()),
        }
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
        let action = request.action();
        let session = self
            .hub
            .current_session_binding()
            .await
            .map(|(generation, capabilities)| HandoffSessionFence {
                generation,
                capability_revision: capabilities.revision,
            });
        let (command, context_handle) = match request.into_operator_command() {
            Ok(command) => command,
            Err(error) => return HostedHandoffControlResponse::error(error),
        };
        let result = if let Some(context_handle) = context_handle.as_deref() {
            if let Err(error) = self
                .issued_contexts
                .validate(principal, action, context_handle)
            {
                return HostedHandoffControlResponse::error(error);
            }
            self.coordinator
                .operator_control_for_context(command, session, context_handle)
                .await
        } else {
            self.coordinator.operator_control(command, session).await
        };
        match result {
            Ok(mut status) => {
                if !action.may_return_locator() {
                    status.locator = None;
                }
                HostedHandoffControlResponse::success(status)
            }
            Err(error) => HostedHandoffControlResponse::error(error.into()),
        }
    }
}

#[async_trait]
impl HostedHandoffControlApi for HostedHandoffControlService {
    async fn issue_context(
        &self,
        principal: &AuthenticatedClientPrincipal,
        request: HostedHandoffContextRequest,
    ) -> HostedHandoffContextResponse {
        HostedHandoffControlService::issue_context(self, principal, request).await
    }

    async fn execute(
        &self,
        principal: &AuthenticatedClientPrincipal,
        request: HostedHandoffControlRequest,
    ) -> HostedHandoffControlResponse {
        HostedHandoffControlService::execute(self, principal, request).await
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
    ContextInvalid,
    Control(HandoffOperatorControlError),
}

impl HostedHandoffControlError {
    pub const fn safe_code(self) -> &'static str {
        match self {
            Self::InvalidConfiguration => "hosted_handoff_invalid_configuration",
            Self::Unauthorized => "hosted_handoff_unauthorized",
            Self::InvalidRequest => "hosted_handoff_request_invalid",
            Self::ContextInvalid => "hosted_handoff_context_invalid",
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
        let raw = r#"{"action":"recover_rebind","context_handle":"hctx_0123456789abcdef0123456789abcdef","prior_context_id":"ctx_0123456789abcdef0123456789abcdef","prior_generation":7,"prior_capability_revision":8}"#;
        let parsed: HostedHandoffControlRequest = serde_json::from_str(raw).unwrap();
        assert_eq!(parsed.action(), HostedHandoffAction::RecoverRebind);
        assert!(parsed.clone().into_operator_command().is_ok());
        assert!(!format!("{parsed:?}").contains("ctx_0123456789abcdef"));
    }

    #[test]
    fn hosted_request_rejects_action_field_mismatch() {
        let raw = r#"{"action":"begin","context_handle":"hctx_0123456789abcdef0123456789abcdef","expected_epoch":7}"#;
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
    fn target_actions_require_valid_opaque_context_handle() {
        let raw = r#"{"action":"begin"}"#;
        let parsed: HostedHandoffControlRequest = serde_json::from_str(raw).unwrap();
        assert_eq!(
            parsed.into_operator_command(),
            Err(HostedHandoffControlError::InvalidRequest)
        );

        let raw = r#"{"action":"begin","context_handle":"hctx_NOT_SECRET"}"#;
        let parsed: HostedHandoffControlRequest = serde_json::from_str(raw).unwrap();
        assert_eq!(
            parsed.into_operator_command(),
            Err(HostedHandoffControlError::InvalidRequest)
        );

        let raw = r#"{"action":"status","context_handle":"hctx_0123456789abcdef0123456789abcdef"}"#;
        let parsed: HostedHandoffControlRequest = serde_json::from_str(raw).unwrap();
        assert_eq!(
            parsed.into_operator_command(),
            Err(HostedHandoffControlError::InvalidRequest)
        );
    }

    #[test]
    fn issued_context_is_bound_to_exact_principal_action_and_expiry() {
        let alpha = principal("alpha");
        let beta = principal("beta");
        let registry = IssuedHostedContexts::default();
        let handle = "hctx_0123456789abcdef0123456789abcdef";
        registry.remember(&alpha, HostedHandoffAction::Begin, handle);
        assert!(
            registry
                .validate(&alpha, HostedHandoffAction::Begin, handle)
                .is_ok()
        );
        assert_eq!(
            registry.validate(&beta, HostedHandoffAction::Begin, handle),
            Err(HostedHandoffControlError::Unauthorized)
        );
        assert_eq!(
            registry.validate(&alpha, HostedHandoffAction::RebindLive, handle),
            Err(HostedHandoffControlError::ContextInvalid)
        );
        assert_eq!(
            registry.validate_at(
                &alpha,
                HostedHandoffAction::Begin,
                handle,
                Instant::now() + HOSTED_CONTEXT_BINDING_TTL + Duration::from_secs(1),
            ),
            Err(HostedHandoffControlError::ContextInvalid)
        );
    }

    #[test]
    fn hosted_context_response_debug_redacts_handle() {
        let response = HostedHandoffContextResponse::success(
            "hctx_0123456789abcdef0123456789abcdef".to_owned(),
        );
        let debug = format!("{response:?}");
        assert!(debug.contains("context_handle_present: true"));
        assert!(!debug.contains("0123456789abcdef"));
    }

    #[test]
    fn status_and_non_issuance_actions_never_return_locator_capability() {
        assert!(!HostedHandoffAction::Status.may_return_locator());
        assert!(!HostedHandoffAction::RequestResume.may_return_locator());
        assert!(!HostedHandoffAction::CancelBeforeHuman.may_return_locator());
        assert!(HostedHandoffAction::Begin.may_return_locator());
        assert!(HostedHandoffAction::RecoverRebind.may_return_locator());
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
