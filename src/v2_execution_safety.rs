//! Authoritative V2 desktop execution-safety ledger.
//!
//! This module is the reviewed state-machine boundary for state-changing desktop
//! work. The older M0 admission controller still owns bounded queueing, while
//! this ledger adds the semantics that must survive transport/session churn:
//! exact principal ownership, generation fencing, durable pending-effect intent,
//! guarded finalization, desktop quarantine, explicit resolution, compact execution
//! receipts, and bounded recoverable process/shell results. Recovery state never
//! stores raw command, argv, cwd, or environment payloads.

use crate::v2_m0::{
    DeviceCapability, DeviceCommand, DeviceErrorCode, DeviceResult, InputDeliveryMode, InputTarget,
    ProcessOutput, ProcessRequest, ShellRequest,
};
use crate::v2_m0_execution::{
    AdmissionDecision, AdmissionLimits, CancellationDecision, CompletionDecision, ExecutionError,
    HubAdmissionController, HubAdmissionSnapshot, HubOperationState, IndeterminateResolution,
    OperationRef,
};
use crate::v2_m0_trust::AuthenticatedClientPrincipal;
use ring::hmac;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::HashMap;

pub const EXECUTION_SAFETY_SCHEMA_VERSION: u16 = 9;
const RECOVERY_EVIDENCE_READ_EXECUTION_SAFETY_SCHEMA_VERSION: u16 = 8;
const EVIDENCE_ENVELOPE_EXECUTION_SAFETY_SCHEMA_VERSION: u16 = 7;
const PARTIAL_INPUT_EXECUTION_SAFETY_SCHEMA_VERSION: u16 = 6;
const RETIREMENT_EXECUTION_SAFETY_SCHEMA_VERSION: u16 = 5;
const RECONCILIATION_EXECUTION_SAFETY_SCHEMA_VERSION: u16 = 4;
const AUDIT_EXECUTION_SAFETY_SCHEMA_VERSION: u16 = 3;
const RECOVERY_EXECUTION_SAFETY_SCHEMA_VERSION: u16 = 2;
const LEGACY_EXECUTION_SAFETY_SCHEMA_VERSION: u16 = 1;
pub const MAX_RESOLUTION_EVIDENCE_BYTES: usize = 1024;
pub const MAX_RECOVERY_ARCHIVE_ENTRIES: usize = 8;
pub const MAX_RECOVERY_ARCHIVE_BYTES: usize = 256 * 1024;
pub const MAX_AUDIT_CORRELATION_ID_BYTES: usize = 128;
pub const MAX_DISPATCH_FENCE_BYTES: usize = 128;
pub const MAX_RECONCILIATION_DEVICE_ID_BYTES: usize = 128;
pub const MAX_RECONCILIATION_OPERATION_ID_BYTES: usize = 128;
pub const MAX_AGENT_TERMINAL_EVIDENCE_ENTRIES: usize = 64;
pub const MAX_AUTO_RESOLUTION_RECORDS: usize = 64;
pub const MAX_RETIREMENT_REASON_BYTES: usize = 1024;
pub const MAX_RETIREMENT_RECORDS: usize = 64;
pub const REQUEST_FINGERPRINT_ALGORITHM: &str = "hmac-sha256:cumg-v2-shell-process-v1";
pub const TEXT_INPUT_FINGERPRINT_ALGORITHM: &str = "hmac-sha256:cumg-v2-text-input-v1";
pub const OPERATION_EVIDENCE_ENVELOPE_SCHEMA_VERSION: u16 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct OperationOwner {
    pub issuer: String,
    pub subject: String,
}

impl OperationOwner {
    pub fn new(
        issuer: impl Into<String>,
        subject: impl Into<String>,
    ) -> Result<Self, ExecutionError> {
        let owner = Self {
            issuer: issuer.into(),
            subject: subject.into(),
        };
        if owner.issuer.trim().is_empty() || owner.subject.trim().is_empty() {
            return Err(ExecutionError::InvalidOperation);
        }
        Ok(owner)
    }

    pub fn from_principal(principal: &AuthenticatedClientPrincipal) -> Self {
        Self {
            issuer: principal.issuer.clone(),
            subject: principal.subject.clone(),
        }
    }

    /// Compatibility owner for internal callers that are not crossing the
    /// authenticated northbound principal boundary.
    pub fn local_hub() -> Self {
        Self {
            issuer: "cumg://local-hub".into(),
            subject: "local".into(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IndeterminateReason {
    CancellationUnproven,
    BackendTimedOut,
    BackendOutcomeUnproven,
    ConnectionLost,
    HubRestartAfterDispatch,
    AgentRestartAfterDispatch,
    ResultDeliveryLost,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionEvidence {
    VerifiedAgentResult,
    VerifiedRemoteError,
    ProvenProcessTermination,
    CancelledBeforeDispatch,
    OperatorResolution,
    RecoveryReadInterrupted,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecutionReceipt {
    pub schema_version: u16,
    pub operation: OperationRef,
    pub owner: OperationOwner,
    pub capability: DeviceCapability,
    pub terminal_state: HubOperationState,
    pub evidence: ExecutionEvidence,
    pub finalized_at_ms: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReconciliationStatus {
    AutoReconciling,
    AutoResolved,
    OperatorRequired,
    UnrecoverableEvidenceGap,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RetirementOutcome {
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RetirementPolicy {
    TransientUiInteractionV1,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RetirementAuthority {
    LocalMaintenanceOperator,
}

pub const fn retirement_policy_for_capability(
    capability: DeviceCapability,
) -> Option<RetirementPolicy> {
    match capability {
        DeviceCapability::Scroll | DeviceCapability::MovePointer => {
            Some(RetirementPolicy::TransientUiInteractionV1)
        }
        _ => None,
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OperationDispatchBinding {
    pub capability_revision: u64,
    pub grant_id: String,
}

impl OperationDispatchBinding {
    pub fn new(
        capability_revision: u64,
        grant_id: impl Into<String>,
    ) -> Result<Self, ExecutionError> {
        let binding = Self {
            capability_revision,
            grant_id: grant_id.into(),
        };
        binding.validate()?;
        Ok(binding)
    }

    fn validate(&self) -> Result<(), ExecutionError> {
        if self.capability_revision == 0
            || self.grant_id.is_empty()
            || self.grant_id.len() > MAX_DISPATCH_FENCE_BYTES
            || !self.grant_id.bytes().all(|byte| {
                byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.' | b':')
            })
        {
            return Err(ExecutionError::InvalidOperation);
        }
        Ok(())
    }
}

/// Payload-free proof that the normal Agent execution path already reached a
/// terminal result for one exact previously-dispatched operation. The proof is
/// useful only after the transport layer authenticates it to the enrolled device.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentTerminalEvidence {
    pub operation: OperationRef,
    pub capability_revision: u64,
    pub capability: DeviceCapability,
    pub dispatch_grant_id: String,
    pub terminal_state: HubOperationState,
    pub evidence: ExecutionEvidence,
}

impl AgentTerminalEvidence {
    pub fn validate(&self) -> Result<(), ExecutionError> {
        OperationDispatchBinding::new(self.capability_revision, self.dispatch_grant_id.clone())?;
        if self.operation.device_id.trim().is_empty()
            || self.operation.device_id.len() > MAX_RECONCILIATION_DEVICE_ID_BYTES
            || self.operation.device_generation == 0
            || self.operation.operation_id.trim().is_empty()
            || self.operation.operation_id.len() > MAX_RECONCILIATION_OPERATION_ID_BYTES
            || !authoritative_agent_terminal_pair(self.terminal_state, self.evidence)
        {
            return Err(ExecutionError::InvalidOperation);
        }
        Ok(())
    }

    pub fn dispatch_binding(&self) -> OperationDispatchBinding {
        OperationDispatchBinding {
            capability_revision: self.capability_revision,
            grant_id: self.dispatch_grant_id.clone(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AutoResolutionRecord {
    pub operation: OperationRef,
    pub capability: DeviceCapability,
    pub terminal_state: HubOperationState,
    pub evidence: ExecutionEvidence,
    pub dispatch_binding: OperationDispatchBinding,
    pub resolved_at_ms: u64,
}

impl AutoResolutionRecord {
    fn is_valid(&self) -> bool {
        !self.operation.device_id.trim().is_empty()
            && self.operation.device_id.len() <= MAX_RECONCILIATION_DEVICE_ID_BYTES
            && self.operation.device_generation > 0
            && !self.operation.operation_id.trim().is_empty()
            && self.operation.operation_id.len() <= MAX_RECONCILIATION_OPERATION_ID_BYTES
            && self.dispatch_binding.validate().is_ok()
            && authoritative_agent_terminal_pair(self.terminal_state, self.evidence)
    }
}

fn authoritative_agent_terminal_pair(
    terminal_state: HubOperationState,
    evidence: ExecutionEvidence,
) -> bool {
    matches!(
        (terminal_state, evidence),
        (
            HubOperationState::Completed,
            ExecutionEvidence::VerifiedAgentResult
        ) | (
            HubOperationState::Failed,
            ExecutionEvidence::VerifiedRemoteError
        ) | (
            HubOperationState::Failed | HubOperationState::Cancelled,
            ExecutionEvidence::ProvenProcessTermination
        )
    )
}

pub fn terminal_evidence_for_device_result(
    result: &DeviceResult,
) -> Option<(HubOperationState, ExecutionEvidence)> {
    match result {
        DeviceResult::Error {
            code: DeviceErrorCode::BackendOutcomeIndeterminate,
        } => None,
        DeviceResult::Error { .. } => Some((
            HubOperationState::Failed,
            ExecutionEvidence::VerifiedRemoteError,
        )),
        DeviceResult::Process { output } | DeviceResult::Shell { output } if output.cancelled => {
            Some((
                HubOperationState::Cancelled,
                ExecutionEvidence::ProvenProcessTermination,
            ))
        }
        DeviceResult::Process { output } | DeviceResult::Shell { output } if output.timed_out => {
            Some((
                HubOperationState::Failed,
                ExecutionEvidence::ProvenProcessTermination,
            ))
        }
        _ => Some((
            HubOperationState::Completed,
            ExecutionEvidence::VerifiedAgentResult,
        )),
    }
}

fn default_reconciliation_status(
    reason: Option<IndeterminateReason>,
    dispatch_binding: Option<&OperationDispatchBinding>,
) -> ReconciliationStatus {
    if dispatch_binding.is_some()
        && matches!(
            reason,
            Some(
                IndeterminateReason::ConnectionLost
                    | IndeterminateReason::HubRestartAfterDispatch
                    | IndeterminateReason::ResultDeliveryLost
            )
        )
    {
        ReconciliationStatus::AutoReconciling
    } else {
        ReconciliationStatus::OperatorRequired
    }
}

fn valid_reconciliation_state(
    state: HubOperationState,
    status: Option<ReconciliationStatus>,
    dispatch_binding: Option<&OperationDispatchBinding>,
) -> bool {
    match (state, status) {
        (HubOperationState::Indeterminate, None) => true,
        (HubOperationState::Indeterminate, Some(ReconciliationStatus::AutoReconciling)) => {
            dispatch_binding.is_some()
        }
        (
            HubOperationState::Indeterminate,
            Some(
                ReconciliationStatus::OperatorRequired
                | ReconciliationStatus::UnrecoverableEvidenceGap,
            ),
        ) => true,
        (
            HubOperationState::Completed | HubOperationState::Failed | HubOperationState::Cancelled,
            Some(ReconciliationStatus::AutoResolved),
        ) => dispatch_binding.is_some(),
        (
            HubOperationState::Completed | HubOperationState::Failed | HubOperationState::Cancelled,
            None,
        ) => true,
        _ => false,
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DesktopQuarantine {
    pub device_id: String,
    pub operation_id: String,
    pub device_generation: u64,
    pub owner: OperationOwner,
    pub reason: IndeterminateReason,
    pub since_ms: u64,
}

/// Untrusted caller-supplied workflow correlation metadata. These fields are
/// bounded opaque audit labels only: they never authorize admission, replay,
/// quarantine resolution, or proof of completion.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct OperationAuditMetadata {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workflow_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workflow_step_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_correlation_id: Option<String>,
}

impl OperationAuditMetadata {
    pub fn empty() -> Self {
        Self::default()
    }

    pub fn is_empty(&self) -> bool {
        self.workflow_id.is_none()
            && self.workflow_step_id.is_none()
            && self.client_correlation_id.is_none()
    }

    pub fn validate(&self) -> Result<(), ExecutionError> {
        for value in [
            self.workflow_id.as_deref(),
            self.workflow_step_id.as_deref(),
            self.client_correlation_id.as_deref(),
        ]
        .into_iter()
        .flatten()
        {
            validate_audit_correlation_id(value)?;
        }
        Ok(())
    }
}

/// Keyed, non-authoritative request comparison material. The value is never
/// exposed to operators or clients; inspection can only report same/different
/// against a candidate request when the same optional key is supplied.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OperationRequestFingerprint {
    pub algorithm: String,
    /// Non-secret key generation identifier derived with HMAC from the configured key.
    /// A key change therefore makes comparison unavailable instead of falsely reporting
    /// that the request itself changed.
    pub key_id: String,
    pub value: String,
}

impl OperationRequestFingerprint {
    pub fn validate(&self) -> Result<(), ExecutionError> {
        if !matches!(
            self.algorithm.as_str(),
            REQUEST_FINGERPRINT_ALGORITHM | TEXT_INPUT_FINGERPRINT_ALGORITHM
        ) || self.key_id.len() != 16
            || !self
                .key_id
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
            || self.value.len() != 64
            || !self
                .value
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(ExecutionError::InvalidOperation);
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TextInputTargetEvidence {
    Unspecified,
    Desktop,
    Window {
        process_id: u32,
        window_id: Option<u64>,
    },
    WindowPoint {
        process_id: u32,
        window_id: u64,
    },
    Element {
        process_id: u32,
        window_id: u64,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum OperationEvidenceEnvelope {
    TextInput {
        schema_version: u16,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        fingerprint: Option<OperationRequestFingerprint>,
        payload_bytes: u32,
        payload_chars: u32,
        line_count: u32,
        ends_with_newline: bool,
        separate_submit_requested: bool,
        target: TextInputTargetEvidence,
        delivery: Option<InputDeliveryMode>,
        delay_ms: Option<u16>,
    },
}

impl OperationEvidenceEnvelope {
    pub fn validate(&self, capability: DeviceCapability) -> Result<(), ExecutionError> {
        match self {
            Self::TextInput {
                schema_version,
                fingerprint,
                payload_bytes,
                payload_chars,
                line_count,
                separate_submit_requested,
                ..
            } => {
                if *schema_version != OPERATION_EVIDENCE_ENVELOPE_SCHEMA_VERSION
                    || capability != DeviceCapability::TypeText
                    || fingerprint.as_ref().is_some_and(|fingerprint| {
                        fingerprint.algorithm != TEXT_INPUT_FINGERPRINT_ALGORITHM
                            || fingerprint.validate().is_err()
                    })
                    || *payload_bytes == 0
                    || *payload_chars == 0
                    || *line_count == 0
                    || *separate_submit_requested
                {
                    return Err(ExecutionError::InvalidOperation);
                }
                Ok(())
            }
        }
    }

    pub fn fingerprint(&self) -> Option<&OperationRequestFingerprint> {
        match self {
            Self::TextInput { fingerprint, .. } => fingerprint.as_ref(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct OperationAdmissionMetadata {
    pub audit: OperationAuditMetadata,
    pub request_fingerprint: Option<OperationRequestFingerprint>,
    pub evidence_envelope: Option<OperationEvidenceEnvelope>,
}

impl OperationAdmissionMetadata {
    pub fn empty() -> Self {
        Self::default()
    }

    fn validate(&self, capability: DeviceCapability) -> Result<(), ExecutionError> {
        self.audit.validate()?;
        if let Some(fingerprint) = self.request_fingerprint.as_ref() {
            fingerprint.validate()?;
        }
        if let Some(envelope) = self.evidence_envelope.as_ref() {
            envelope.validate(capability)?;
        }
        Ok(())
    }
}

pub fn text_input_evidence_envelope(
    secret: Option<&[u8]>,
    command: &DeviceCommand,
) -> Result<Option<OperationEvidenceEnvelope>, ExecutionError> {
    let (text, target, delivery, delay_ms) = match command {
        DeviceCommand::TypeText { text } => (
            text.as_str(),
            TextInputTargetEvidence::Unspecified,
            None,
            None,
        ),
        DeviceCommand::TypeTextAdvanced {
            text,
            target,
            delivery,
            delay_ms,
            ..
        } => (
            text.as_str(),
            text_input_target_evidence(target),
            Some(*delivery),
            Some(*delay_ms),
        ),
        _ => return Ok(None),
    };
    let payload_bytes = u32::try_from(text.len()).map_err(|_| ExecutionError::InvalidOperation)?;
    let payload_chars =
        u32::try_from(text.chars().count()).map_err(|_| ExecutionError::InvalidOperation)?;
    let line_count =
        u32::try_from(text.split('\n').count()).map_err(|_| ExecutionError::InvalidOperation)?;
    if payload_bytes == 0 || payload_chars == 0 || line_count == 0 {
        return Err(ExecutionError::InvalidOperation);
    }
    let contract = json!({
        "contract": "type_text_payload",
        "text": text,
    });
    let fingerprint = secret
        .map(|secret| {
            fingerprint_canonical_contract(secret, TEXT_INPUT_FINGERPRINT_ALGORITHM, &contract)
        })
        .transpose()?;
    let envelope = OperationEvidenceEnvelope::TextInput {
        schema_version: OPERATION_EVIDENCE_ENVELOPE_SCHEMA_VERSION,
        fingerprint,
        payload_bytes,
        payload_chars,
        line_count,
        ends_with_newline: text.ends_with('\n'),
        // CUMG type_text has no implicit second Enter/submit operation. If a
        // future command adds one, it must get a new envelope contract instead
        // of inferring intent from the text bytes.
        separate_submit_requested: false,
        target,
        delivery,
        delay_ms,
    };
    envelope.validate(DeviceCapability::TypeText)?;
    Ok(Some(envelope))
}

fn text_input_target_evidence(target: &InputTarget) -> TextInputTargetEvidence {
    match target {
        InputTarget::Desktop => TextInputTargetEvidence::Desktop,
        InputTarget::Window {
            process_id,
            window_id,
        } => TextInputTargetEvidence::Window {
            process_id: *process_id,
            window_id: *window_id,
        },
        InputTarget::WindowPoint {
            process_id,
            window_id,
            ..
        } => TextInputTargetEvidence::WindowPoint {
            process_id: *process_id,
            window_id: *window_id,
        },
        InputTarget::Element {
            process_id,
            window_id,
            ..
        } => TextInputTargetEvidence::Element {
            process_id: *process_id,
            window_id: *window_id,
        },
    }
}

pub fn fingerprint_text_input_candidate(
    secret: &[u8],
    text: &str,
) -> Result<OperationRequestFingerprint, ExecutionError> {
    if text.is_empty() {
        return Err(ExecutionError::InvalidOperation);
    }
    let contract = json!({
        "contract": "type_text_payload",
        "text": text,
    });
    fingerprint_canonical_contract(secret, TEXT_INPUT_FINGERPRINT_ALGORITHM, &contract)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RequestFingerprintComparison {
    SameRequest,
    DifferentRequest,
    Unavailable,
}

pub fn fingerprint_process_request(
    secret: &[u8],
    request: &ProcessRequest,
) -> Result<OperationRequestFingerprint, ExecutionError> {
    let contract = json!({
        "contract": "execute_process",
        "program": request.program,
        "args": request.args,
        "cwd": request.cwd,
        "env": request.env,
        "timeout_ms": request.timeout_ms,
    });
    fingerprint_canonical_contract(secret, REQUEST_FINGERPRINT_ALGORITHM, &contract)
}

pub fn fingerprint_shell_request(
    secret: &[u8],
    request: &ShellRequest,
) -> Result<OperationRequestFingerprint, ExecutionError> {
    let contract = json!({
        "contract": "shell",
        "command": request.command,
        "cwd": request.cwd,
        "env": request.env,
        "timeout_ms": request.timeout_ms,
    });
    fingerprint_canonical_contract(secret, REQUEST_FINGERPRINT_ALGORITHM, &contract)
}

pub fn compare_request_fingerprint(
    stored: Option<&OperationRequestFingerprint>,
    candidate: Option<&OperationRequestFingerprint>,
) -> RequestFingerprintComparison {
    match (stored, candidate) {
        (Some(stored), Some(candidate))
            if stored.algorithm != candidate.algorithm || stored.key_id != candidate.key_id =>
        {
            RequestFingerprintComparison::Unavailable
        }
        (Some(stored), Some(candidate)) if stored.value == candidate.value => {
            RequestFingerprintComparison::SameRequest
        }
        (Some(_), Some(_)) => RequestFingerprintComparison::DifferentRequest,
        _ => RequestFingerprintComparison::Unavailable,
    }
}

fn fingerprint_canonical_contract(
    secret: &[u8],
    algorithm: &str,
    contract: &serde_json::Value,
) -> Result<OperationRequestFingerprint, ExecutionError> {
    if secret.is_empty() {
        return Err(ExecutionError::InvalidOperation);
    }
    let canonical = serde_json::to_vec(contract).map_err(|_| ExecutionError::InvalidOperation)?;
    let key = hmac::Key::new(hmac::HMAC_SHA256, secret);
    let tag = hmac::sign(&key, &canonical);
    let key_id_tag = hmac::sign(&key, b"cumg-v2-audit-fingerprint-key-id");
    let key_id_full = hex_lower(key_id_tag.as_ref());
    Ok(OperationRequestFingerprint {
        algorithm: algorithm.to_owned(),
        key_id: key_id_full[..16].to_owned(),
        value: hex_lower(tag.as_ref()),
    })
}

fn hex_lower(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(HEX[(byte >> 4) as usize] as char);
        output.push(HEX[(byte & 0x0f) as usize] as char);
    }
    output
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RetirementRecord {
    pub operation: OperationRef,
    pub capability: DeviceCapability,
    pub outcome: RetirementOutcome,
    pub indeterminate_reason: IndeterminateReason,
    pub prior_reconciliation_status: ReconciliationStatus,
    pub policy: RetirementPolicy,
    pub authority: RetirementAuthority,
    /// Bounded operator-supplied retirement rationale. Never include raw command,
    /// result, desktop content, credentials, URLs, or other sensitive payloads.
    pub reason: String,
    /// Current durable device generation observed by local maintenance when the
    /// older operation generation was permanently fenced from resumption.
    pub authorized_device_generation: u64,
    pub retired_at_ms: u64,
    pub replayed: bool,
}

impl RetirementRecord {
    fn is_valid(&self) -> bool {
        !self.operation.device_id.trim().is_empty()
            && self.operation.device_id.len() <= MAX_RECONCILIATION_DEVICE_ID_BYTES
            && self.operation.device_generation > 0
            && !self.operation.operation_id.trim().is_empty()
            && self.operation.operation_id.len() <= MAX_RECONCILIATION_OPERATION_ID_BYTES
            && self.outcome == RetirementOutcome::Unknown
            && matches!(
                self.prior_reconciliation_status,
                ReconciliationStatus::OperatorRequired
                    | ReconciliationStatus::UnrecoverableEvidenceGap
            )
            && retirement_policy_for_capability(self.capability) == Some(self.policy)
            && !self.reason.trim().is_empty()
            && self.reason.len() <= MAX_RETIREMENT_REASON_BYTES
            && self.authorized_device_generation > self.operation.device_generation
            && !self.replayed
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolutionRecord {
    pub operation_id: String,
    pub device_id: String,
    pub device_generation: u64,
    pub resolver: OperationOwner,
    pub decision: IndeterminateResolution,
    /// Bounded operator-supplied evidence metadata. This is intentionally not a
    /// screenshot, command payload, result payload, or raw provider exception.
    pub evidence: String,
    pub resolved_at_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RecoverableOperationResult {
    Process {
        output: ProcessOutput,
    },
    Shell {
        output: ProcessOutput,
    },
    Error {
        code: DeviceErrorCode,
    },
    /// Payload-free durable marker for a terminal effectful desktop/browser operation.
    /// The authoritative state/receipt carries the outcome; raw GUI/browser results are not stored.
    EffectfulStatus,
}

impl RecoverableOperationResult {
    fn is_valid_for(&self, capability: DeviceCapability, state: HubOperationState) -> bool {
        match self {
            Self::Process { output } if capability == DeviceCapability::ExecuteProcess => {
                process_output_matches_state(output, state)
            }
            Self::Shell { output } if capability == DeviceCapability::Shell => {
                process_output_matches_state(output, state)
            }
            Self::Error { .. }
                if matches!(
                    capability,
                    DeviceCapability::ExecuteProcess | DeviceCapability::Shell
                ) =>
            {
                state == HubOperationState::Failed
            }
            Self::EffectfulStatus
                if !matches!(
                    capability,
                    DeviceCapability::ExecuteProcess | DeviceCapability::Shell
                ) && !matches!(capability.class(), crate::v2_m0::CapabilityClass::Observe) =>
            {
                matches!(
                    state,
                    HubOperationState::Completed
                        | HubOperationState::Failed
                        | HubOperationState::Cancelled
                )
            }
            _ => false,
        }
    }
}

fn process_output_matches_state(output: &ProcessOutput, state: HubOperationState) -> bool {
    if output.cancelled && output.timed_out {
        false
    } else if output.cancelled {
        state == HubOperationState::Cancelled
    } else if output.timed_out {
        state == HubOperationState::Failed
    } else {
        state == HubOperationState::Completed
    }
}

fn validate_audit_correlation_id(value: &str) -> Result<(), ExecutionError> {
    if value.is_empty()
        || value.len() > MAX_AUDIT_CORRELATION_ID_BYTES
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.' | b':'))
    {
        return Err(ExecutionError::InvalidOperation);
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OperationRecoverySnapshot {
    pub operation: OperationRef,
    pub capability: DeviceCapability,
    pub state: HubOperationState,
    pub indeterminate_reason: Option<IndeterminateReason>,
    pub receipt: Option<ExecutionReceipt>,
    pub result: Option<RecoverableOperationResult>,
}

/// Privacy-preserving durable metadata for one unresolved desktop quarantine.
/// Raw command/browser/GUI payloads and authenticated owner identity are
/// intentionally absent from this inspection shape.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuarantineInspectionSnapshot {
    pub operation: OperationRef,
    pub capability: DeviceCapability,
    pub audit: OperationAuditMetadata,
    pub request_fingerprint: Option<OperationRequestFingerprint>,
    pub evidence_envelope: Option<OperationEvidenceEnvelope>,
    pub prepared_at_ms: u64,
    pub dispatched_at_ms: Option<u64>,
    pub indeterminate_at_ms: u64,
    pub indeterminate_reason: IndeterminateReason,
    pub evidence: Option<ExecutionEvidence>,
    pub reconciliation_status: ReconciliationStatus,
    pub dispatch_binding_present: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArchivedOperationRecovery {
    pub operation: OperationRef,
    pub owner: OperationOwner,
    pub capability: DeviceCapability,
    pub state: HubOperationState,
    pub receipt: ExecutionReceipt,
    pub result: RecoverableOperationResult,
}

impl ArchivedOperationRecovery {
    fn encoded_len(&self) -> usize {
        serde_json::to_vec(self).map_or(usize::MAX, |bytes| bytes.len())
    }

    fn is_valid(&self) -> bool {
        matches!(
            self.state,
            HubOperationState::Completed | HubOperationState::Failed | HubOperationState::Cancelled
        ) && self.result.is_valid_for(self.capability, self.state)
            && self.receipt.operation == self.operation
            && self.receipt.owner == self.owner
            && self.receipt.capability == self.capability
            && self.receipt.terminal_state == self.state
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OperationExecutionLane {
    #[default]
    Normal,
    RecoveryEvidenceRead,
}

impl OperationExecutionLane {
    fn is_normal(value: &Self) -> bool {
        *value == Self::Normal
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SafetyOperationSnapshot {
    pub operation: OperationRef,
    pub owner: OperationOwner,
    pub capability: DeviceCapability,
    #[serde(default, skip_serializing_if = "OperationExecutionLane::is_normal")]
    pub execution_lane: OperationExecutionLane,
    pub state: HubOperationState,
    pub prepared_at_ms: u64,
    pub dispatched_at_ms: Option<u64>,
    pub indeterminate_reason: Option<IndeterminateReason>,
    pub receipt: Option<ExecutionReceipt>,
    #[serde(default)]
    pub recoverable_result: Option<RecoverableOperationResult>,
    #[serde(default, skip_serializing_if = "OperationAuditMetadata::is_empty")]
    pub audit: OperationAuditMetadata,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_fingerprint: Option<OperationRequestFingerprint>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence_envelope: Option<OperationEvidenceEnvelope>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dispatch_binding: Option<OperationDispatchBinding>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reconciliation_status: Option<ReconciliationStatus>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuthoritativeSafetySnapshot {
    pub schema_version: u16,
    pub admission: HubAdmissionSnapshot,
    pub operations: Vec<SafetyOperationSnapshot>,
    #[serde(default)]
    pub recoveries: Vec<ArchivedOperationRecovery>,
    pub quarantines: Vec<DesktopQuarantine>,
    pub resolutions: Vec<ResolutionRecord>,
    #[serde(default)]
    pub auto_resolutions: Vec<AutoResolutionRecord>,
    #[serde(default)]
    pub retirements: Vec<RetirementRecord>,
}

#[derive(Debug, Clone)]
struct SafetyOperation {
    operation: OperationRef,
    owner: OperationOwner,
    capability: DeviceCapability,
    execution_lane: OperationExecutionLane,
    state: HubOperationState,
    prepared_at_ms: u64,
    dispatched_at_ms: Option<u64>,
    indeterminate_reason: Option<IndeterminateReason>,
    receipt: Option<ExecutionReceipt>,
    recoverable_result: Option<RecoverableOperationResult>,
    audit: OperationAuditMetadata,
    request_fingerprint: Option<OperationRequestFingerprint>,
    evidence_envelope: Option<OperationEvidenceEnvelope>,
    dispatch_binding: Option<OperationDispatchBinding>,
    reconciliation_status: Option<ReconciliationStatus>,
}

/// Single authoritative state machine for Hub-side desktop execution safety.
///
/// The invariant is intentionally stricter than transport liveness: once work
/// may have crossed the side-effect boundary, a missing proof converges to
/// durable `Indeterminate` + desktop quarantine. Reconnect/liveness alone never
/// resolves it; only exact authoritative terminal evidence for the same prior
/// dispatch may settle it, without replay.
#[derive(Debug, Clone)]
pub struct AuthoritativeOperationController {
    admission: HubAdmissionController,
    operations: HashMap<String, SafetyOperation>,
    recovery_archive: HashMap<String, ArchivedOperationRecovery>,
    quarantines: HashMap<String, DesktopQuarantine>,
    resolutions: Vec<ResolutionRecord>,
    auto_resolutions: Vec<AutoResolutionRecord>,
    retirements: Vec<RetirementRecord>,
}

impl AuthoritativeOperationController {
    pub fn new(limits: AdmissionLimits) -> Result<Self, ExecutionError> {
        Ok(Self {
            admission: HubAdmissionController::new(limits)?,
            operations: HashMap::new(),
            recovery_archive: HashMap::new(),
            quarantines: HashMap::new(),
            resolutions: Vec::new(),
            auto_resolutions: Vec::new(),
            retirements: Vec::new(),
        })
    }

    pub fn prepare(
        &mut self,
        operation: OperationRef,
        owner: OperationOwner,
        capability: DeviceCapability,
        now_ms: u64,
    ) -> Result<AdmissionDecision, ExecutionError> {
        self.prepare_with_metadata(
            operation,
            owner,
            capability,
            OperationAdmissionMetadata::empty(),
            now_ms,
        )
    }

    pub fn prepare_with_metadata(
        &mut self,
        operation: OperationRef,
        owner: OperationOwner,
        capability: DeviceCapability,
        metadata: OperationAdmissionMetadata,
        now_ms: u64,
    ) -> Result<AdmissionDecision, ExecutionError> {
        if self.operations.contains_key(&operation.operation_id)
            || self.recovery_archive.contains_key(&operation.operation_id)
            || self
                .retirements
                .iter()
                .any(|retirement| retirement.operation.operation_id == operation.operation_id)
        {
            return Err(ExecutionError::OperationReplay);
        }
        metadata.validate(capability)?;
        let OperationAdmissionMetadata {
            audit,
            request_fingerprint,
            evidence_envelope,
        } = metadata;
        let execution_lane = if self.quarantines.contains_key(&operation.device_id) {
            if !capability.is_recovery_evidence_read_only() {
                let blocking_operation_id = self
                    .quarantines
                    .get(&operation.device_id)
                    .expect("quarantine existence checked above")
                    .operation_id
                    .clone();
                return Err(ExecutionError::DeviceIndeterminate {
                    operation_id: blocking_operation_id,
                });
            }
            OperationExecutionLane::RecoveryEvidenceRead
        } else {
            OperationExecutionLane::Normal
        };
        let decision = match execution_lane {
            OperationExecutionLane::Normal => self.admission.admit(operation.clone())?,
            OperationExecutionLane::RecoveryEvidenceRead => {
                self.admission.admit_recovery_read_only(operation.clone())?
            }
        };
        let state = match decision {
            AdmissionDecision::StartNow(_) => HubOperationState::ActiveNotDispatched,
            AdmissionDecision::Queued { .. } => HubOperationState::Queued,
        };
        self.operations.insert(
            operation.operation_id.clone(),
            SafetyOperation {
                operation,
                owner,
                capability,
                execution_lane,
                state,
                prepared_at_ms: now_ms,
                dispatched_at_ms: None,
                indeterminate_reason: None,
                receipt: None,
                recoverable_result: None,
                audit,
                request_fingerprint,
                evidence_envelope,
                dispatch_binding: None,
                reconciliation_status: None,
            },
        );
        Ok(decision)
    }

    /// Transition the durable pending intent across the provider/transport
    /// effect boundary. Callers must persist this state before emitting bytes to
    /// the Agent.
    pub fn mark_dispatched(
        &mut self,
        operation_id: &str,
        owner: &OperationOwner,
        device_generation: u64,
        now_ms: u64,
    ) -> Result<(), ExecutionError> {
        self.mark_dispatched_with_binding(operation_id, owner, device_generation, None, now_ms)
    }

    pub fn mark_dispatched_with_binding(
        &mut self,
        operation_id: &str,
        owner: &OperationOwner,
        device_generation: u64,
        dispatch_binding: Option<OperationDispatchBinding>,
        now_ms: u64,
    ) -> Result<(), ExecutionError> {
        if let Some(binding) = dispatch_binding.as_ref() {
            binding.validate()?;
        }
        {
            let operation = self.checked(operation_id, owner, device_generation)?;
            if operation.state != HubOperationState::ActiveNotDispatched {
                return Err(ExecutionError::InvalidTransition);
            }
        }
        self.admission.mark_dispatched(operation_id)?;
        let operation = self
            .operations
            .get_mut(operation_id)
            .ok_or(ExecutionError::UnknownOperation)?;
        operation.state = HubOperationState::Dispatched;
        operation.dispatched_at_ms = Some(now_ms);
        operation.dispatch_binding = dispatch_binding;
        operation.reconciliation_status = None;
        Ok(())
    }

    pub fn request_cancel(
        &mut self,
        operation_id: &str,
        owner: &OperationOwner,
        device_generation: u64,
        now_ms: u64,
    ) -> Result<CancellationDecision, ExecutionError> {
        self.checked(operation_id, owner, device_generation)?;
        let decision = self.admission.cancel(operation_id)?;
        match &decision {
            CancellationDecision::CancelledBeforeDispatch { next } => {
                let record = self
                    .operations
                    .get_mut(operation_id)
                    .ok_or(ExecutionError::UnknownOperation)?;
                record.state = HubOperationState::Cancelled;
                record.receipt = Some(Self::receipt_for(
                    record,
                    HubOperationState::Cancelled,
                    ExecutionEvidence::CancelledBeforeDispatch,
                    now_ms,
                ));
                self.activate_next(next);
            }
            CancellationDecision::SendCancellation(_) => {
                self.operations
                    .get_mut(operation_id)
                    .ok_or(ExecutionError::UnknownOperation)?
                    .state = HubOperationState::CancelRequested;
            }
            CancellationDecision::AlreadyTerminal(_) => {}
        }
        Ok(decision)
    }

    pub fn finalize(
        &mut self,
        operation_id: &str,
        owner: &OperationOwner,
        device_generation: u64,
        terminal_state: HubOperationState,
        evidence: ExecutionEvidence,
        now_ms: u64,
    ) -> Result<(CompletionDecision, ExecutionReceipt), ExecutionError> {
        let record = self.checked(operation_id, owner, device_generation)?;
        if !matches!(
            record.state,
            HubOperationState::Dispatched | HubOperationState::CancelRequested
        ) || !matches!(
            (terminal_state, evidence),
            (
                HubOperationState::Completed,
                ExecutionEvidence::VerifiedAgentResult
            ) | (
                HubOperationState::Failed,
                ExecutionEvidence::VerifiedRemoteError
            ) | (
                HubOperationState::Failed | HubOperationState::Cancelled,
                ExecutionEvidence::ProvenProcessTermination
            )
        ) {
            return Err(ExecutionError::InvalidTransition);
        }
        let next = self
            .admission
            .finalize_terminal(operation_id, terminal_state)?;
        let record = self
            .operations
            .get_mut(operation_id)
            .ok_or(ExecutionError::UnknownOperation)?;
        record.state = terminal_state;
        record.indeterminate_reason = None;
        let receipt = Self::receipt_for(record, terminal_state, evidence, now_ms);
        record.receipt = Some(receipt.clone());
        self.activate_next(&next);
        Ok((next, receipt))
    }

    pub fn mark_indeterminate(
        &mut self,
        operation_id: &str,
        owner: &OperationOwner,
        device_generation: u64,
        reason: IndeterminateReason,
        now_ms: u64,
    ) -> Result<CompletionDecision, ExecutionError> {
        self.checked(operation_id, owner, device_generation)?;
        self.mark_indeterminate_internal(operation_id, reason, now_ms)
    }

    pub fn is_recovery_evidence_read(&self, operation_id: &str) -> bool {
        self.operations.get(operation_id).is_some_and(|record| {
            record.execution_lane == OperationExecutionLane::RecoveryEvidenceRead
        })
    }

    pub fn mark_recovery_read_interrupted(
        &mut self,
        operation_id: &str,
        now_ms: u64,
    ) -> Result<(CompletionDecision, ExecutionReceipt), ExecutionError> {
        let record = self
            .operations
            .get(operation_id)
            .ok_or(ExecutionError::UnknownOperation)?;
        if record.execution_lane != OperationExecutionLane::RecoveryEvidenceRead
            || !matches!(
                record.state,
                HubOperationState::Dispatched | HubOperationState::CancelRequested
            )
        {
            return Err(ExecutionError::InvalidTransition);
        }
        let next = self.admission.fail_recovery_read_only(operation_id)?;
        let record = self
            .operations
            .get_mut(operation_id)
            .ok_or(ExecutionError::UnknownOperation)?;
        record.state = HubOperationState::Failed;
        record.indeterminate_reason = None;
        record.recoverable_result = None;
        record.reconciliation_status = None;
        let receipt = Self::receipt_for(
            record,
            HubOperationState::Failed,
            ExecutionEvidence::RecoveryReadInterrupted,
            now_ms,
        );
        record.receipt = Some(receipt.clone());
        self.activate_next(&next);
        Ok((next, receipt))
    }

    /// Internal connection/restart path. Ownership is read from the durable
    /// record rather than inferred from a new connection/session.
    pub fn mark_connection_lost(
        &mut self,
        operation_id: &str,
        now_ms: u64,
    ) -> Result<CompletionDecision, ExecutionError> {
        if self.is_recovery_evidence_read(operation_id) {
            return self
                .mark_recovery_read_interrupted(operation_id, now_ms)
                .map(|(next, _)| next);
        }
        self.mark_indeterminate_internal(operation_id, IndeterminateReason::ConnectionLost, now_ms)
    }

    fn mark_indeterminate_internal(
        &mut self,
        operation_id: &str,
        reason: IndeterminateReason,
        now_ms: u64,
    ) -> Result<CompletionDecision, ExecutionError> {
        let target = self
            .operations
            .get(operation_id)
            .ok_or(ExecutionError::UnknownOperation)?;
        if target.execution_lane != OperationExecutionLane::Normal
            || !matches!(
                target.state,
                HubOperationState::Dispatched | HubOperationState::CancelRequested
            )
        {
            return Err(ExecutionError::InvalidTransition);
        }
        let device_id = target.operation.device_id.clone();
        if self
            .quarantines
            .get(&device_id)
            .is_some_and(|existing| existing.operation_id != operation_id)
        {
            return Err(ExecutionError::InvalidTransition);
        }
        let queued_ids: Vec<_> = self
            .operations
            .iter()
            .filter(|(id, record)| {
                id.as_str() != operation_id
                    && record.operation.device_id == device_id
                    && record.state == HubOperationState::Queued
            })
            .map(|(id, _)| id.clone())
            .collect();

        let next = self.admission.mark_indeterminate(operation_id)?;
        // Stronger recovery boundary: queued work for the quarantined desktop
        // is authority that was admitted before the ambiguity was known. Cancel
        // it before reuse instead of letting resolution implicitly resume it.
        for queued_id in queued_ids {
            let decision = self.admission.cancel(&queued_id)?;
            if !matches!(
                decision,
                CancellationDecision::CancelledBeforeDispatch { .. }
            ) {
                return Err(ExecutionError::InvalidTransition);
            }
            let queued = self
                .operations
                .get_mut(&queued_id)
                .ok_or(ExecutionError::UnknownOperation)?;
            queued.state = HubOperationState::Cancelled;
            queued.receipt = Some(Self::receipt_for(
                queued,
                HubOperationState::Cancelled,
                ExecutionEvidence::CancelledBeforeDispatch,
                now_ms,
            ));
        }

        let record = self
            .operations
            .get_mut(operation_id)
            .ok_or(ExecutionError::UnknownOperation)?;
        record.state = HubOperationState::Indeterminate;
        record.indeterminate_reason = Some(reason);
        record.receipt = None;
        record.recoverable_result = None;
        record.reconciliation_status = Some(default_reconciliation_status(
            Some(reason),
            record.dispatch_binding.as_ref(),
        ));
        let quarantine = DesktopQuarantine {
            device_id: record.operation.device_id.clone(),
            operation_id: record.operation.operation_id.clone(),
            device_generation: record.operation.device_generation,
            owner: record.owner.clone(),
            reason,
            since_ms: now_ms,
        };
        self.quarantines.insert(device_id, quarantine);
        Ok(next)
    }

    pub fn mark_reconciliation_evidence_gap(
        &mut self,
        operation_id: &str,
    ) -> Result<(), ExecutionError> {
        let record = self
            .operations
            .get_mut(operation_id)
            .ok_or(ExecutionError::UnknownOperation)?;
        if record.state != HubOperationState::Indeterminate
            || record.reconciliation_status != Some(ReconciliationStatus::AutoReconciling)
        {
            return Err(ExecutionError::InvalidTransition);
        }
        record.reconciliation_status = Some(ReconciliationStatus::UnrecoverableEvidenceGap);
        Ok(())
    }

    pub fn mark_reconciliation_operator_required(
        &mut self,
        operation_id: &str,
    ) -> Result<(), ExecutionError> {
        let record = self
            .operations
            .get_mut(operation_id)
            .ok_or(ExecutionError::UnknownOperation)?;
        if record.state != HubOperationState::Indeterminate {
            return Err(ExecutionError::InvalidTransition);
        }
        record.reconciliation_status = Some(ReconciliationStatus::OperatorRequired);
        Ok(())
    }

    pub fn reconcile_authoritative_terminal(
        &mut self,
        terminal: &AgentTerminalEvidence,
        now_ms: u64,
    ) -> Result<(CompletionDecision, ExecutionReceipt), ExecutionError> {
        terminal.validate()?;
        let record = self
            .operations
            .get(&terminal.operation.operation_id)
            .ok_or(ExecutionError::UnknownOperation)?;
        if record.state != HubOperationState::Indeterminate
            || record.reconciliation_status != Some(ReconciliationStatus::AutoReconciling)
            || record.operation != terminal.operation
            || record.capability != terminal.capability
            || record.dispatch_binding.as_ref() != Some(&terminal.dispatch_binding())
        {
            return Err(ExecutionError::OwnershipFenceMismatch);
        }
        let quarantine = self
            .quarantines
            .get(&record.operation.device_id)
            .ok_or(ExecutionError::InvalidTransition)?;
        if quarantine.operation_id != terminal.operation.operation_id
            || quarantine.device_generation != terminal.operation.device_generation
        {
            return Err(ExecutionError::OwnershipFenceMismatch);
        }

        let next = self.admission.reconcile_indeterminate_terminal(
            &terminal.operation.operation_id,
            terminal.terminal_state,
        )?;
        let record = self
            .operations
            .get_mut(&terminal.operation.operation_id)
            .ok_or(ExecutionError::UnknownOperation)?;
        record.state = terminal.terminal_state;
        record.indeterminate_reason = None;
        record.reconciliation_status = Some(ReconciliationStatus::AutoResolved);
        let receipt = Self::receipt_for(record, terminal.terminal_state, terminal.evidence, now_ms);
        record.receipt = Some(receipt.clone());
        record.recoverable_result = None;
        let dispatch_binding = record
            .dispatch_binding
            .clone()
            .ok_or(ExecutionError::InvalidTransition)?;
        self.quarantines.remove(&record.operation.device_id);
        self.auto_resolutions.push(AutoResolutionRecord {
            operation: record.operation.clone(),
            capability: record.capability,
            terminal_state: terminal.terminal_state,
            evidence: terminal.evidence,
            dispatch_binding,
            resolved_at_ms: now_ms,
        });
        if self.auto_resolutions.len() > MAX_AUTO_RESOLUTION_RECORDS {
            let excess = self.auto_resolutions.len() - MAX_AUTO_RESOLUTION_RECORDS;
            self.auto_resolutions.drain(..excess);
        }
        self.activate_next(&next);
        Ok((next, receipt))
    }

    pub fn resolve_indeterminate(
        &mut self,
        operation_id: &str,
        resolver: OperationOwner,
        decision: IndeterminateResolution,
        evidence: impl Into<String>,
        now_ms: u64,
    ) -> Result<(CompletionDecision, ExecutionReceipt), ExecutionError> {
        let evidence = evidence.into();
        if evidence.trim().is_empty() || evidence.len() > MAX_RESOLUTION_EVIDENCE_BYTES {
            return Err(ExecutionError::InvalidOperation);
        }
        let record = self
            .operations
            .get(operation_id)
            .ok_or(ExecutionError::UnknownOperation)?;
        if record.state != HubOperationState::Indeterminate {
            return Err(ExecutionError::InvalidTransition);
        }
        let quarantine = self
            .quarantines
            .get(&record.operation.device_id)
            .ok_or(ExecutionError::InvalidTransition)?;
        if quarantine.operation_id != operation_id
            || quarantine.device_generation != record.operation.device_generation
        {
            return Err(ExecutionError::InvalidTransition);
        }

        if decision == IndeterminateResolution::ConfirmedEffectAppliedUncommitted
            && !matches!(
                record.capability,
                DeviceCapability::TypeText | DeviceCapability::BrowserType
            )
        {
            return Err(ExecutionError::InvalidTransition);
        }

        let next = self
            .admission
            .resolve_indeterminate(operation_id, decision.clone())?;
        let terminal = match decision {
            IndeterminateResolution::ConfirmedCompleted
            | IndeterminateResolution::ConfirmedEffectAppliedUncommitted => {
                HubOperationState::Completed
            }
            IndeterminateResolution::ConfirmedNotExecuted => HubOperationState::Cancelled,
        };
        let record = self
            .operations
            .get_mut(operation_id)
            .ok_or(ExecutionError::UnknownOperation)?;
        record.state = terminal;
        record.indeterminate_reason = None;
        record.reconciliation_status = None;
        let receipt = Self::receipt_for(
            record,
            terminal,
            ExecutionEvidence::OperatorResolution,
            now_ms,
        );
        record.receipt = Some(receipt.clone());
        self.quarantines.remove(&record.operation.device_id);
        self.resolutions.push(ResolutionRecord {
            operation_id: operation_id.to_owned(),
            device_id: record.operation.device_id.clone(),
            device_generation: record.operation.device_generation,
            resolver,
            decision,
            evidence,
            resolved_at_ms: now_ms,
        });
        self.activate_next(&next);
        Ok((next, receipt))
    }

    /// Retire a permanently unknowable, policy-eligible indeterminate operation
    /// without asserting a synthetic execution outcome. Retirement is allowed
    /// only after a strictly newer durable device generation fences the original
    /// session and only for a reviewed capability allowlist.
    pub fn retire_indeterminate(
        &mut self,
        operation_id: &str,
        authority: RetirementAuthority,
        requested_policy: RetirementPolicy,
        reason: impl Into<String>,
        authorized_device_generation: u64,
        now_ms: u64,
    ) -> Result<(CompletionDecision, RetirementRecord), ExecutionError> {
        let reason = reason.into();
        if reason.trim().is_empty() || reason.len() > MAX_RETIREMENT_REASON_BYTES {
            return Err(ExecutionError::InvalidOperation);
        }
        if self.retirements.len() >= MAX_RETIREMENT_RECORDS
            || self
                .retirements
                .iter()
                .any(|retirement| retirement.operation.operation_id == operation_id)
        {
            return Err(ExecutionError::InvalidTransition);
        }
        let record = self
            .operations
            .get(operation_id)
            .ok_or(ExecutionError::UnknownOperation)?;
        if record.state != HubOperationState::Indeterminate
            || record.dispatched_at_ms.is_none()
            || record.receipt.is_some()
            || record.recoverable_result.is_some()
            || authorized_device_generation <= record.operation.device_generation
        {
            return Err(ExecutionError::InvalidTransition);
        }
        let prior_reconciliation_status = record
            .reconciliation_status
            .unwrap_or(ReconciliationStatus::OperatorRequired);
        if !matches!(
            prior_reconciliation_status,
            ReconciliationStatus::OperatorRequired | ReconciliationStatus::UnrecoverableEvidenceGap
        ) {
            return Err(ExecutionError::InvalidTransition);
        }
        let policy = retirement_policy_for_capability(record.capability)
            .ok_or(ExecutionError::InvalidTransition)?;
        if policy != requested_policy {
            return Err(ExecutionError::InvalidTransition);
        }
        let indeterminate_reason = record
            .indeterminate_reason
            .ok_or(ExecutionError::InvalidTransition)?;
        let quarantine = self
            .quarantines
            .get(&record.operation.device_id)
            .ok_or(ExecutionError::InvalidTransition)?;
        if quarantine.operation_id != operation_id
            || quarantine.device_generation != record.operation.device_generation
            || quarantine.reason != indeterminate_reason
        {
            return Err(ExecutionError::InvalidTransition);
        }
        let retirement = RetirementRecord {
            operation: record.operation.clone(),
            capability: record.capability,
            outcome: RetirementOutcome::Unknown,
            indeterminate_reason,
            prior_reconciliation_status,
            policy,
            authority,
            reason,
            authorized_device_generation,
            retired_at_ms: now_ms,
            replayed: false,
        };
        if !retirement.is_valid() {
            return Err(ExecutionError::InvalidTransition);
        }

        let next = self.admission.retire_indeterminate(operation_id)?;
        self.quarantines.remove(&retirement.operation.device_id);
        self.retirements.push(retirement.clone());
        self.activate_next(&next);
        Ok((next, retirement))
    }

    pub fn state(&self, operation_id: &str) -> Option<HubOperationState> {
        self.operations.get(operation_id).map(|record| record.state)
    }

    /// Returns true while this device still has work that has not reached a
    /// durable terminal or indeterminate state. Shutdown draining uses this to
    /// keep the Agent transport alive until already-admitted work either settles
    /// or the caller's bounded drain timeout expires.
    pub fn has_unsettled_work(&self, device_id: &str) -> bool {
        self.operations.values().any(|record| {
            record.operation.device_id == device_id
                && matches!(
                    record.state,
                    HubOperationState::Queued
                        | HubOperationState::ActiveNotDispatched
                        | HubOperationState::Dispatched
                        | HubOperationState::CancelRequested
                )
        })
    }

    pub fn owner(&self, operation_id: &str) -> Option<&OperationOwner> {
        self.operations
            .get(operation_id)
            .map(|record| &record.owner)
    }

    pub fn receipt(&self, operation_id: &str) -> Option<&ExecutionReceipt> {
        self.operations
            .get(operation_id)
            .and_then(|record| record.receipt.as_ref())
    }

    /// Attach bounded caller-recovery material to an already finalized operation.
    /// Process/shell may retain bounded output; other effectful capabilities retain only
    /// a payload-free terminal marker. Raw request/GUI/browser material is never accepted here.
    pub fn attach_recoverable_result(
        &mut self,
        operation_id: &str,
        owner: &OperationOwner,
        device_generation: u64,
        result: RecoverableOperationResult,
    ) -> Result<(), ExecutionError> {
        let record = self.checked(operation_id, owner, device_generation)?;
        if record.receipt.is_none()
            || !result.is_valid_for(record.capability, record.state)
            || serde_json::to_vec(&result)
                .map_or(true, |bytes| bytes.len() > MAX_RECOVERY_ARCHIVE_BYTES)
            || record.recoverable_result.is_some()
        {
            return Err(ExecutionError::InvalidTransition);
        }
        self.operations
            .get_mut(operation_id)
            .ok_or(ExecutionError::UnknownOperation)?
            .recoverable_result = Some(result);
        Ok(())
    }

    /// Exact-owner read-only recovery lookup. Wrong-owner and unknown IDs are
    /// intentionally indistinguishable.
    pub fn recovery_for_owner(
        &self,
        operation_id: &str,
        owner: &OperationOwner,
    ) -> Result<OperationRecoverySnapshot, ExecutionError> {
        if let Some(record) = self
            .operations
            .get(operation_id)
            .filter(|record| &record.owner == owner)
        {
            return Ok(OperationRecoverySnapshot {
                operation: record.operation.clone(),
                capability: record.capability,
                state: record.state,
                indeterminate_reason: record.indeterminate_reason,
                receipt: record.receipt.clone(),
                result: record.recoverable_result.clone(),
            });
        }
        let archived = self
            .recovery_archive
            .get(operation_id)
            .filter(|record| &record.owner == owner)
            .ok_or(ExecutionError::UnknownOperation)?;
        Ok(OperationRecoverySnapshot {
            operation: archived.operation.clone(),
            capability: archived.capability,
            state: archived.state,
            indeterminate_reason: None,
            receipt: Some(archived.receipt.clone()),
            result: Some(archived.result.clone()),
        })
    }

    pub fn quarantine(&self, device_id: &str) -> Option<&DesktopQuarantine> {
        self.quarantines.get(device_id)
    }

    /// Return bounded durable audit metadata for unresolved quarantines without
    /// exposing owner identity or any raw operation payload. The controller was
    /// already restored through invariant validation; this accessor still
    /// rechecks the quarantine/operation correlation and fails closed if it ever
    /// diverges.
    pub fn quarantine_inspections(
        &self,
    ) -> Result<Vec<QuarantineInspectionSnapshot>, ExecutionError> {
        let mut quarantines: Vec<_> = self.quarantines.values().collect();
        quarantines.sort_by(|left, right| left.device_id.cmp(&right.device_id));
        quarantines
            .into_iter()
            .map(|quarantine| {
                let record = self
                    .operations
                    .get(&quarantine.operation_id)
                    .ok_or(ExecutionError::InvalidSnapshot)?;
                if record.state != HubOperationState::Indeterminate
                    || record.operation.device_id != quarantine.device_id
                    || record.operation.device_generation != quarantine.device_generation
                    || record.owner != quarantine.owner
                    || record.indeterminate_reason != Some(quarantine.reason)
                {
                    return Err(ExecutionError::InvalidSnapshot);
                }
                Ok(QuarantineInspectionSnapshot {
                    operation: record.operation.clone(),
                    capability: record.capability,
                    audit: record.audit.clone(),
                    request_fingerprint: record.request_fingerprint.clone(),
                    evidence_envelope: record.evidence_envelope.clone(),
                    prepared_at_ms: record.prepared_at_ms,
                    dispatched_at_ms: record.dispatched_at_ms,
                    indeterminate_at_ms: quarantine.since_ms,
                    indeterminate_reason: quarantine.reason,
                    evidence: record.receipt.as_ref().map(|receipt| receipt.evidence),
                    reconciliation_status: record
                        .reconciliation_status
                        .unwrap_or(ReconciliationStatus::OperatorRequired),
                    dispatch_binding_present: record.dispatch_binding.is_some(),
                })
            })
            .collect()
    }

    pub fn resolutions(&self) -> &[ResolutionRecord] {
        &self.resolutions
    }

    pub fn auto_resolutions(&self) -> &[AutoResolutionRecord] {
        &self.auto_resolutions
    }

    pub fn retirements(&self) -> &[RetirementRecord] {
        &self.retirements
    }

    pub fn prune_terminal_before_generation(
        &mut self,
        device_id: &str,
        current_generation: u64,
    ) -> Result<usize, ExecutionError> {
        // Mirror the admission-controller precondition before mutating the
        // recovery archive, so the later admission prune cannot leave a
        // half-applied archive transition.
        if device_id.trim().is_empty() || current_generation == 0 {
            return Err(ExecutionError::InvalidOperation);
        }
        let to_archive: Vec<_> = self
            .operations
            .values()
            .filter(|record| {
                record.operation.device_id == device_id
                    && record.operation.device_generation < current_generation
                    && matches!(
                        record.state,
                        HubOperationState::Completed
                            | HubOperationState::Failed
                            | HubOperationState::Cancelled
                    )
                    && record.recoverable_result.is_some()
            })
            .map(|record| {
                let receipt = record
                    .receipt
                    .clone()
                    .ok_or(ExecutionError::InvalidTransition)?;
                let result = record
                    .recoverable_result
                    .clone()
                    .ok_or(ExecutionError::InvalidTransition)?;
                Ok(ArchivedOperationRecovery {
                    operation: record.operation.clone(),
                    owner: record.owner.clone(),
                    capability: record.capability,
                    state: record.state,
                    receipt,
                    result,
                })
            })
            .collect::<Result<_, ExecutionError>>()?;
        for archived in to_archive {
            if !archived.is_valid() {
                return Err(ExecutionError::InvalidTransition);
            }
            self.recovery_archive
                .insert(archived.operation.operation_id.clone(), archived);
        }
        self.bound_recovery_archive();

        let removed = self
            .admission
            .prune_terminal_before_generation(device_id, current_generation)?;
        self.operations.retain(|_, record| {
            !(record.operation.device_id == device_id
                && record.operation.device_generation < current_generation
                && matches!(
                    record.state,
                    HubOperationState::Completed
                        | HubOperationState::Failed
                        | HubOperationState::Cancelled
                ))
        });
        // Resolution audit is intentionally retained even after replay
        // tombstones are compacted; unresolved ambiguity is never pruned.
        Ok(removed)
    }

    fn recovery_archive_encoded_bytes(&self) -> usize {
        self.recovery_archive
            .values()
            .map(ArchivedOperationRecovery::encoded_len)
            .fold(0usize, usize::saturating_add)
    }

    fn bound_recovery_archive(&mut self) {
        while self.recovery_archive.len() > MAX_RECOVERY_ARCHIVE_ENTRIES
            || self.recovery_archive_encoded_bytes() > MAX_RECOVERY_ARCHIVE_BYTES
        {
            let oldest = self
                .recovery_archive
                .values()
                .min_by(|left, right| {
                    left.receipt
                        .finalized_at_ms
                        .cmp(&right.receipt.finalized_at_ms)
                        .then_with(|| {
                            left.operation
                                .operation_id
                                .cmp(&right.operation.operation_id)
                        })
                })
                .map(|record| record.operation.operation_id.clone());
            let Some(operation_id) = oldest else {
                break;
            };
            self.recovery_archive.remove(&operation_id);
        }
    }

    pub fn snapshot_for_restart(&self) -> AuthoritativeSafetySnapshot {
        let admission = self.admission.snapshot_for_restart();
        let mut operations = Vec::with_capacity(self.operations.len());
        let mut quarantines = self.quarantines.clone();
        for record in self.operations.values() {
            let mut state = record.state;
            let mut reason = record.indeterminate_reason;
            let mut receipt = record.receipt.clone();
            let mut recoverable_result = record.recoverable_result.clone();
            match record.state {
                HubOperationState::Queued | HubOperationState::ActiveNotDispatched => {
                    state = HubOperationState::Cancelled;
                    receipt = Some(Self::receipt_for(
                        record,
                        HubOperationState::Cancelled,
                        ExecutionEvidence::CancelledBeforeDispatch,
                        record.prepared_at_ms,
                    ));
                    recoverable_result = None;
                }
                HubOperationState::Dispatched | HubOperationState::CancelRequested
                    if record.execution_lane == OperationExecutionLane::RecoveryEvidenceRead =>
                {
                    state = HubOperationState::Failed;
                    reason = None;
                    receipt = Some(Self::receipt_for(
                        record,
                        HubOperationState::Failed,
                        ExecutionEvidence::RecoveryReadInterrupted,
                        record.dispatched_at_ms.unwrap_or(record.prepared_at_ms),
                    ));
                    recoverable_result = None;
                }
                HubOperationState::Dispatched | HubOperationState::CancelRequested => {
                    state = HubOperationState::Indeterminate;
                    reason = Some(IndeterminateReason::HubRestartAfterDispatch);
                    receipt = None;
                    recoverable_result = None;
                    quarantines.insert(
                        record.operation.device_id.clone(),
                        DesktopQuarantine {
                            device_id: record.operation.device_id.clone(),
                            operation_id: record.operation.operation_id.clone(),
                            device_generation: record.operation.device_generation,
                            owner: record.owner.clone(),
                            reason: IndeterminateReason::HubRestartAfterDispatch,
                            since_ms: record.dispatched_at_ms.unwrap_or(record.prepared_at_ms),
                        },
                    );
                }
                _ => {}
            }
            operations.push(SafetyOperationSnapshot {
                operation: record.operation.clone(),
                owner: record.owner.clone(),
                capability: record.capability,
                execution_lane: record.execution_lane,
                state,
                prepared_at_ms: record.prepared_at_ms,
                dispatched_at_ms: record.dispatched_at_ms,
                indeterminate_reason: reason,
                receipt,
                recoverable_result,
                audit: record.audit.clone(),
                request_fingerprint: record.request_fingerprint.clone(),
                evidence_envelope: record.evidence_envelope.clone(),
                dispatch_binding: record.dispatch_binding.clone(),
                reconciliation_status: if state == HubOperationState::Indeterminate {
                    Some(record.reconciliation_status.unwrap_or_else(|| {
                        default_reconciliation_status(reason, record.dispatch_binding.as_ref())
                    }))
                } else {
                    record.reconciliation_status
                },
            });
        }
        operations.sort_by(|a, b| a.operation.operation_id.cmp(&b.operation.operation_id));
        let mut recoveries: Vec<_> = self.recovery_archive.values().cloned().collect();
        recoveries.sort_by(|a, b| a.operation.operation_id.cmp(&b.operation.operation_id));
        let mut quarantines: Vec<_> = quarantines.into_values().collect();
        quarantines.sort_by(|a, b| a.device_id.cmp(&b.device_id));
        AuthoritativeSafetySnapshot {
            schema_version: EXECUTION_SAFETY_SCHEMA_VERSION,
            admission,
            operations,
            recoveries,
            quarantines,
            resolutions: self.resolutions.clone(),
            auto_resolutions: self.auto_resolutions.clone(),
            retirements: self.retirements.clone(),
        }
    }

    /// Capture restart state without silently upgrading the durable execution
    /// schema that the intended reader already proved it can consume.
    ///
    /// Offline maintenance uses this to preserve the writer contract of the
    /// authoritative input checkpoint. Schema v1 can represent the resolution
    /// ledger and terminal receipts, but it cannot represent v2 recoverable
    /// process/shell results or the recovery archive.
    pub(crate) fn snapshot_for_restart_compatible_with(
        &self,
        target_schema_version: u16,
    ) -> Result<AuthoritativeSafetySnapshot, ExecutionError> {
        let has_v4_state = !self.auto_resolutions.is_empty()
            || self.operations.values().any(|record| {
                record.dispatch_binding.is_some() || record.reconciliation_status.is_some()
            });
        let mut snapshot = self.snapshot_for_restart();
        let has_v5_state = !snapshot.retirements.is_empty()
            || !snapshot
                .admission
                .retired_indeterminate_operation_ids
                .is_empty();
        let has_v6_state = snapshot.resolutions.iter().any(|resolution| {
            resolution.decision == IndeterminateResolution::ConfirmedEffectAppliedUncommitted
        });
        let has_v7_state = snapshot
            .operations
            .iter()
            .any(|record| record.evidence_envelope.is_some());
        let has_v8_state = snapshot.operations.iter().any(|record| {
            record.execution_lane == OperationExecutionLane::RecoveryEvidenceRead
                || record.receipt.as_ref().is_some_and(|receipt| {
                    receipt.evidence == ExecutionEvidence::RecoveryReadInterrupted
                })
        });
        let has_v9_state = snapshot.operations.iter().any(|record| {
            matches!(
                record.recoverable_result,
                Some(RecoverableOperationResult::EffectfulStatus)
            )
        }) || snapshot
            .recoveries
            .iter()
            .any(|record| matches!(record.result, RecoverableOperationResult::EffectfulStatus));
        match target_schema_version {
            EXECUTION_SAFETY_SCHEMA_VERSION => Ok(snapshot),
            RECOVERY_EVIDENCE_READ_EXECUTION_SAFETY_SCHEMA_VERSION => {
                if has_v9_state {
                    return Err(ExecutionError::InvalidSnapshot);
                }
                for record in &mut snapshot.operations {
                    if let Some(receipt) = &mut record.receipt {
                        receipt.schema_version =
                            RECOVERY_EVIDENCE_READ_EXECUTION_SAFETY_SCHEMA_VERSION;
                    }
                }
                for archived in &mut snapshot.recoveries {
                    archived.receipt.schema_version =
                        RECOVERY_EVIDENCE_READ_EXECUTION_SAFETY_SCHEMA_VERSION;
                }
                snapshot.schema_version = RECOVERY_EVIDENCE_READ_EXECUTION_SAFETY_SCHEMA_VERSION;
                Ok(snapshot)
            }
            EVIDENCE_ENVELOPE_EXECUTION_SAFETY_SCHEMA_VERSION => {
                if has_v8_state || has_v9_state {
                    return Err(ExecutionError::InvalidSnapshot);
                }
                for record in &mut snapshot.operations {
                    if let Some(receipt) = &mut record.receipt {
                        receipt.schema_version = EVIDENCE_ENVELOPE_EXECUTION_SAFETY_SCHEMA_VERSION;
                    }
                }
                for archived in &mut snapshot.recoveries {
                    archived.receipt.schema_version =
                        EVIDENCE_ENVELOPE_EXECUTION_SAFETY_SCHEMA_VERSION;
                }
                snapshot.schema_version = EVIDENCE_ENVELOPE_EXECUTION_SAFETY_SCHEMA_VERSION;
                Ok(snapshot)
            }
            PARTIAL_INPUT_EXECUTION_SAFETY_SCHEMA_VERSION => {
                if has_v7_state || has_v8_state || has_v9_state {
                    return Err(ExecutionError::InvalidSnapshot);
                }
                for record in &mut snapshot.operations {
                    if let Some(receipt) = &mut record.receipt {
                        receipt.schema_version = PARTIAL_INPUT_EXECUTION_SAFETY_SCHEMA_VERSION;
                    }
                }
                for archived in &mut snapshot.recoveries {
                    archived.receipt.schema_version = PARTIAL_INPUT_EXECUTION_SAFETY_SCHEMA_VERSION;
                }
                snapshot.schema_version = PARTIAL_INPUT_EXECUTION_SAFETY_SCHEMA_VERSION;
                Ok(snapshot)
            }
            RETIREMENT_EXECUTION_SAFETY_SCHEMA_VERSION => {
                if has_v6_state || has_v7_state || has_v8_state || has_v9_state {
                    return Err(ExecutionError::InvalidSnapshot);
                }
                for record in &mut snapshot.operations {
                    if let Some(receipt) = &mut record.receipt {
                        receipt.schema_version = RETIREMENT_EXECUTION_SAFETY_SCHEMA_VERSION;
                    }
                }
                for archived in &mut snapshot.recoveries {
                    archived.receipt.schema_version = RETIREMENT_EXECUTION_SAFETY_SCHEMA_VERSION;
                }
                snapshot.schema_version = RETIREMENT_EXECUTION_SAFETY_SCHEMA_VERSION;
                Ok(snapshot)
            }
            RECONCILIATION_EXECUTION_SAFETY_SCHEMA_VERSION => {
                if has_v5_state || has_v6_state || has_v7_state || has_v8_state || has_v9_state {
                    return Err(ExecutionError::InvalidSnapshot);
                }
                snapshot.retirements.clear();
                snapshot
                    .admission
                    .retired_indeterminate_operation_ids
                    .clear();
                for record in &mut snapshot.operations {
                    if let Some(receipt) = &mut record.receipt {
                        receipt.schema_version = RECONCILIATION_EXECUTION_SAFETY_SCHEMA_VERSION;
                    }
                }
                for archived in &mut snapshot.recoveries {
                    archived.receipt.schema_version =
                        RECONCILIATION_EXECUTION_SAFETY_SCHEMA_VERSION;
                }
                snapshot.schema_version = RECONCILIATION_EXECUTION_SAFETY_SCHEMA_VERSION;
                Ok(snapshot)
            }
            AUDIT_EXECUTION_SAFETY_SCHEMA_VERSION => {
                if has_v4_state
                    || has_v5_state
                    || has_v6_state
                    || has_v7_state
                    || has_v8_state
                    || has_v9_state
                {
                    return Err(ExecutionError::InvalidSnapshot);
                }
                snapshot.auto_resolutions.clear();
                snapshot.retirements.clear();
                snapshot
                    .admission
                    .retired_indeterminate_operation_ids
                    .clear();
                for record in &mut snapshot.operations {
                    record.dispatch_binding = None;
                    record.reconciliation_status = None;
                    if let Some(receipt) = &mut record.receipt {
                        receipt.schema_version = AUDIT_EXECUTION_SAFETY_SCHEMA_VERSION;
                    }
                }
                for archived in &mut snapshot.recoveries {
                    archived.receipt.schema_version = AUDIT_EXECUTION_SAFETY_SCHEMA_VERSION;
                }
                snapshot.schema_version = AUDIT_EXECUTION_SAFETY_SCHEMA_VERSION;
                Ok(snapshot)
            }
            RECOVERY_EXECUTION_SAFETY_SCHEMA_VERSION => {
                if has_v4_state
                    || has_v5_state
                    || has_v6_state
                    || has_v7_state
                    || has_v8_state
                    || has_v9_state
                    || snapshot.operations.iter().any(|record| {
                        !record.audit.is_empty() || record.request_fingerprint.is_some()
                    })
                {
                    return Err(ExecutionError::InvalidSnapshot);
                }
                snapshot.auto_resolutions.clear();
                snapshot.retirements.clear();
                snapshot
                    .admission
                    .retired_indeterminate_operation_ids
                    .clear();
                for record in &mut snapshot.operations {
                    record.dispatch_binding = None;
                    record.reconciliation_status = None;
                    if let Some(receipt) = &mut record.receipt {
                        receipt.schema_version = RECOVERY_EXECUTION_SAFETY_SCHEMA_VERSION;
                    }
                }
                for archived in &mut snapshot.recoveries {
                    archived.receipt.schema_version = RECOVERY_EXECUTION_SAFETY_SCHEMA_VERSION;
                }
                snapshot.schema_version = RECOVERY_EXECUTION_SAFETY_SCHEMA_VERSION;
                Ok(snapshot)
            }
            LEGACY_EXECUTION_SAFETY_SCHEMA_VERSION => {
                if has_v4_state
                    || has_v5_state
                    || has_v6_state
                    || has_v7_state
                    || has_v8_state
                    || has_v9_state
                    || !snapshot.recoveries.is_empty()
                    || snapshot.operations.iter().any(|record| {
                        record.recoverable_result.is_some()
                            || !record.audit.is_empty()
                            || record.request_fingerprint.is_some()
                    })
                {
                    return Err(ExecutionError::InvalidSnapshot);
                }
                snapshot.auto_resolutions.clear();
                snapshot.retirements.clear();
                snapshot
                    .admission
                    .retired_indeterminate_operation_ids
                    .clear();
                for record in &mut snapshot.operations {
                    record.dispatch_binding = None;
                    record.reconciliation_status = None;
                    if let Some(receipt) = &mut record.receipt {
                        receipt.schema_version = LEGACY_EXECUTION_SAFETY_SCHEMA_VERSION;
                    }
                }
                snapshot.schema_version = LEGACY_EXECUTION_SAFETY_SCHEMA_VERSION;
                Ok(snapshot)
            }
            _ => Err(ExecutionError::InvalidSnapshot),
        }
    }

    pub fn restore_after_restart(
        limits: AdmissionLimits,
        snapshot: AuthoritativeSafetySnapshot,
    ) -> Result<Self, ExecutionError> {
        if !matches!(
            snapshot.schema_version,
            LEGACY_EXECUTION_SAFETY_SCHEMA_VERSION
                | RECOVERY_EXECUTION_SAFETY_SCHEMA_VERSION
                | AUDIT_EXECUTION_SAFETY_SCHEMA_VERSION
                | RECONCILIATION_EXECUTION_SAFETY_SCHEMA_VERSION
                | RETIREMENT_EXECUTION_SAFETY_SCHEMA_VERSION
                | PARTIAL_INPUT_EXECUTION_SAFETY_SCHEMA_VERSION
                | EVIDENCE_ENVELOPE_EXECUTION_SAFETY_SCHEMA_VERSION
                | RECOVERY_EVIDENCE_READ_EXECUTION_SAFETY_SCHEMA_VERSION
                | EXECUTION_SAFETY_SCHEMA_VERSION
        ) {
            return Err(ExecutionError::InvalidSnapshot);
        }
        if snapshot.schema_version < AUDIT_EXECUTION_SAFETY_SCHEMA_VERSION
            && snapshot
                .operations
                .iter()
                .any(|record| !record.audit.is_empty() || record.request_fingerprint.is_some())
        {
            return Err(ExecutionError::InvalidSnapshot);
        }
        if snapshot.schema_version < RECONCILIATION_EXECUTION_SAFETY_SCHEMA_VERSION
            && (snapshot.operations.iter().any(|record| {
                record.dispatch_binding.is_some() || record.reconciliation_status.is_some()
            }) || !snapshot.auto_resolutions.is_empty())
        {
            return Err(ExecutionError::InvalidSnapshot);
        }
        if snapshot.schema_version < RETIREMENT_EXECUTION_SAFETY_SCHEMA_VERSION
            && (!snapshot.retirements.is_empty()
                || !snapshot
                    .admission
                    .retired_indeterminate_operation_ids
                    .is_empty())
        {
            return Err(ExecutionError::InvalidSnapshot);
        }
        if snapshot.schema_version < PARTIAL_INPUT_EXECUTION_SAFETY_SCHEMA_VERSION
            && snapshot.resolutions.iter().any(|resolution| {
                resolution.decision == IndeterminateResolution::ConfirmedEffectAppliedUncommitted
            })
        {
            return Err(ExecutionError::InvalidSnapshot);
        }
        if snapshot.schema_version < EVIDENCE_ENVELOPE_EXECUTION_SAFETY_SCHEMA_VERSION
            && snapshot
                .operations
                .iter()
                .any(|record| record.evidence_envelope.is_some())
        {
            return Err(ExecutionError::InvalidSnapshot);
        }
        if snapshot.schema_version < RECOVERY_EVIDENCE_READ_EXECUTION_SAFETY_SCHEMA_VERSION
            && snapshot.operations.iter().any(|record| {
                record.execution_lane == OperationExecutionLane::RecoveryEvidenceRead
                    || record.receipt.as_ref().is_some_and(|receipt| {
                        receipt.evidence == ExecutionEvidence::RecoveryReadInterrupted
                    })
            })
        {
            return Err(ExecutionError::InvalidSnapshot);
        }
        if snapshot.schema_version < EXECUTION_SAFETY_SCHEMA_VERSION
            && (snapshot.operations.iter().any(|record| {
                matches!(
                    record.recoverable_result,
                    Some(RecoverableOperationResult::EffectfulStatus)
                )
            }) || snapshot
                .recoveries
                .iter()
                .any(|record| matches!(record.result, RecoverableOperationResult::EffectfulStatus)))
        {
            return Err(ExecutionError::InvalidSnapshot);
        }
        if snapshot.schema_version == LEGACY_EXECUTION_SAFETY_SCHEMA_VERSION
            && (!snapshot.recoveries.is_empty()
                || snapshot
                    .operations
                    .iter()
                    .any(|record| record.recoverable_result.is_some()))
        {
            return Err(ExecutionError::InvalidSnapshot);
        }
        let admission_retired_ids = snapshot
            .admission
            .retired_indeterminate_operation_ids
            .clone();
        let admission = HubAdmissionController::restore_after_restart(limits, snapshot.admission)?;
        let mut operations = HashMap::new();
        for record in snapshot.operations {
            if !matches!(
                record.state,
                HubOperationState::Completed
                    | HubOperationState::Failed
                    | HubOperationState::Cancelled
                    | HubOperationState::Indeterminate
            ) || operations.contains_key(&record.operation.operation_id)
                || record.recoverable_result.as_ref().is_some_and(|result| {
                    record.receipt.is_none()
                        || !result.is_valid_for(record.capability, record.state)
                })
                || (record.state == HubOperationState::Indeterminate
                    && record.recoverable_result.is_some())
                || record.audit.validate().is_err()
                || record
                    .request_fingerprint
                    .as_ref()
                    .is_some_and(|fingerprint| fingerprint.validate().is_err())
                || record
                    .evidence_envelope
                    .as_ref()
                    .is_some_and(|envelope| envelope.validate(record.capability).is_err())
                || record
                    .dispatch_binding
                    .as_ref()
                    .is_some_and(|binding| binding.validate().is_err())
                || !valid_reconciliation_state(
                    record.state,
                    record.reconciliation_status,
                    record.dispatch_binding.as_ref(),
                )
                || (record.execution_lane == OperationExecutionLane::RecoveryEvidenceRead
                    && (!record.capability.is_recovery_evidence_read_only()
                        || record.state == HubOperationState::Indeterminate
                        || record.dispatch_binding.is_some()
                        || record.reconciliation_status.is_some()))
                || record.receipt.as_ref().is_some_and(|receipt| {
                    receipt.evidence == ExecutionEvidence::RecoveryReadInterrupted
                        && (record.execution_lane != OperationExecutionLane::RecoveryEvidenceRead
                            || record.state != HubOperationState::Failed)
                })
            {
                return Err(ExecutionError::InvalidSnapshot);
            }
            operations.insert(
                record.operation.operation_id.clone(),
                SafetyOperation {
                    operation: record.operation,
                    owner: record.owner,
                    capability: record.capability,
                    execution_lane: record.execution_lane,
                    state: record.state,
                    prepared_at_ms: record.prepared_at_ms,
                    dispatched_at_ms: record.dispatched_at_ms,
                    indeterminate_reason: record.indeterminate_reason,
                    receipt: record.receipt,
                    recoverable_result: record.recoverable_result,
                    audit: record.audit,
                    request_fingerprint: record.request_fingerprint,
                    evidence_envelope: record.evidence_envelope,
                    dispatch_binding: record.dispatch_binding,
                    reconciliation_status: record.reconciliation_status,
                },
            );
        }
        let admission_snapshot = admission.snapshot_for_restart();
        if admission_snapshot.operations.len() != operations.len()
            || admission_snapshot.operations.iter().any(|admitted| {
                operations
                    .get(&admitted.operation.operation_id)
                    .is_none_or(|safe| {
                        safe.operation != admitted.operation || safe.state != admitted.state
                    })
            })
        {
            return Err(ExecutionError::InvalidSnapshot);
        }

        let mut recovery_archive = HashMap::new();
        for archived in snapshot.recoveries {
            if !archived.is_valid()
                || operations.contains_key(&archived.operation.operation_id)
                || recovery_archive
                    .insert(archived.operation.operation_id.clone(), archived)
                    .is_some()
            {
                return Err(ExecutionError::InvalidSnapshot);
            }
        }
        let recovery_archive_bytes = recovery_archive
            .values()
            .map(ArchivedOperationRecovery::encoded_len)
            .fold(0usize, usize::saturating_add);
        if recovery_archive.len() > MAX_RECOVERY_ARCHIVE_ENTRIES
            || recovery_archive_bytes > MAX_RECOVERY_ARCHIVE_BYTES
        {
            return Err(ExecutionError::InvalidSnapshot);
        }

        let mut quarantines = HashMap::new();
        for quarantine in snapshot.quarantines {
            let Some(record) = operations.get(&quarantine.operation_id) else {
                return Err(ExecutionError::InvalidSnapshot);
            };
            if record.state != HubOperationState::Indeterminate
                || record.operation.device_id != quarantine.device_id
                || record.operation.device_generation != quarantine.device_generation
                || record.owner != quarantine.owner
                || quarantines
                    .insert(quarantine.device_id.clone(), quarantine)
                    .is_some()
            {
                return Err(ExecutionError::InvalidSnapshot);
            }
        }
        let mut retirement_ids = std::collections::HashSet::new();
        for retirement in &snapshot.retirements {
            if !retirement.is_valid()
                || !retirement_ids.insert(retirement.operation.operation_id.clone())
                || snapshot
                    .resolutions
                    .iter()
                    .any(|resolution| resolution.operation_id == retirement.operation.operation_id)
                || snapshot.auto_resolutions.iter().any(|resolution| {
                    resolution.operation.operation_id == retirement.operation.operation_id
                })
            {
                return Err(ExecutionError::InvalidSnapshot);
            }
            let Some(record) = operations.get(&retirement.operation.operation_id) else {
                return Err(ExecutionError::InvalidSnapshot);
            };
            if record.state != HubOperationState::Indeterminate
                || record.operation != retirement.operation
                || record.capability != retirement.capability
                || record.indeterminate_reason != Some(retirement.indeterminate_reason)
                || record
                    .reconciliation_status
                    .unwrap_or(ReconciliationStatus::OperatorRequired)
                    != retirement.prior_reconciliation_status
                || record.receipt.is_some()
                || record.recoverable_result.is_some()
                || quarantines
                    .values()
                    .any(|quarantine| quarantine.operation_id == retirement.operation.operation_id)
            {
                return Err(ExecutionError::InvalidSnapshot);
            }
        }
        let admission_retired_ids: std::collections::HashSet<_> =
            admission_retired_ids.into_iter().collect();
        if admission_retired_ids != retirement_ids {
            return Err(ExecutionError::InvalidSnapshot);
        }
        for record in operations.values() {
            if record.state == HubOperationState::Indeterminate {
                let retired = retirement_ids.contains(&record.operation.operation_id);
                let matching_quarantine =
                    quarantines
                        .get(&record.operation.device_id)
                        .is_some_and(|quarantine| {
                            quarantine.operation_id == record.operation.operation_id
                                && quarantine.device_generation
                                    == record.operation.device_generation
                        });
                if retired == matching_quarantine {
                    return Err(ExecutionError::InvalidSnapshot);
                }
            }
        }
        if snapshot.retirements.len() > MAX_RETIREMENT_RECORDS {
            return Err(ExecutionError::InvalidSnapshot);
        }
        if snapshot.auto_resolutions.len() > MAX_AUTO_RESOLUTION_RECORDS
            || snapshot
                .auto_resolutions
                .iter()
                .any(|record| !record.is_valid())
        {
            return Err(ExecutionError::InvalidSnapshot);
        }
        let mut auto_resolution_ids = std::collections::HashSet::new();
        if snapshot
            .auto_resolutions
            .iter()
            .any(|record| !auto_resolution_ids.insert(record.operation.operation_id.as_str()))
        {
            return Err(ExecutionError::InvalidSnapshot);
        }
        for operation in operations.values().filter(|operation| {
            operation.reconciliation_status == Some(ReconciliationStatus::AutoResolved)
        }) {
            let Some(history) = snapshot
                .auto_resolutions
                .iter()
                .find(|history| history.operation.operation_id == operation.operation.operation_id)
            else {
                return Err(ExecutionError::InvalidSnapshot);
            };
            let Some(receipt) = operation.receipt.as_ref() else {
                return Err(ExecutionError::InvalidSnapshot);
            };
            if history.operation != operation.operation
                || history.capability != operation.capability
                || history.terminal_state != operation.state
                || history.evidence != receipt.evidence
                || operation.dispatch_binding.as_ref() != Some(&history.dispatch_binding)
            {
                return Err(ExecutionError::InvalidSnapshot);
            }
        }
        for history in &snapshot.auto_resolutions {
            if let Some(operation) = operations.get(&history.operation.operation_id)
                && (operation.reconciliation_status != Some(ReconciliationStatus::AutoResolved)
                    || operation.operation != history.operation
                    || operation.capability != history.capability
                    || operation.state != history.terminal_state
                    || operation.receipt.as_ref().map(|receipt| receipt.evidence)
                        != Some(history.evidence)
                    || operation.dispatch_binding.as_ref() != Some(&history.dispatch_binding))
            {
                return Err(ExecutionError::InvalidSnapshot);
            }
        }
        Ok(Self {
            admission,
            operations,
            recovery_archive,
            quarantines,
            resolutions: snapshot.resolutions,
            auto_resolutions: snapshot.auto_resolutions,
            retirements: snapshot.retirements,
        })
    }

    fn checked(
        &self,
        operation_id: &str,
        owner: &OperationOwner,
        device_generation: u64,
    ) -> Result<&SafetyOperation, ExecutionError> {
        let record = self
            .operations
            .get(operation_id)
            .ok_or(ExecutionError::UnknownOperation)?;
        if &record.owner != owner || record.operation.device_generation != device_generation {
            return Err(ExecutionError::OwnershipFenceMismatch);
        }
        Ok(record)
    }

    fn receipt_for(
        record: &SafetyOperation,
        terminal_state: HubOperationState,
        evidence: ExecutionEvidence,
        now_ms: u64,
    ) -> ExecutionReceipt {
        ExecutionReceipt {
            schema_version: EXECUTION_SAFETY_SCHEMA_VERSION,
            operation: record.operation.clone(),
            owner: record.owner.clone(),
            capability: record.capability,
            terminal_state,
            evidence,
            finalized_at_ms: now_ms,
        }
    }

    fn activate_next(&mut self, next: &CompletionDecision) {
        if let CompletionDecision::StartNext(operation) = next {
            if let Some(record) = self.operations.get_mut(&operation.operation_id) {
                record.state = HubOperationState::ActiveNotDispatched;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn op(id: &str, generation: u64) -> OperationRef {
        OperationRef {
            device_id: "desktop-a".into(),
            device_generation: generation,
            operation_id: id.into(),
        }
    }

    fn alice() -> OperationOwner {
        OperationOwner::new("https://issuer", "alice").unwrap()
    }

    fn bob() -> OperationOwner {
        OperationOwner::new("https://issuer", "bob").unwrap()
    }

    fn controller() -> AuthoritativeOperationController {
        AuthoritativeOperationController::new(AdmissionLimits {
            max_global_active: 1,
            max_queued_per_device: 8,
        })
        .unwrap()
    }

    #[test]
    fn owner_and_generation_fence_guard_dispatch_and_finalization() {
        let mut ledger = controller();
        ledger
            .prepare(op("op-1", 7), alice(), DeviceCapability::Shell, 10)
            .unwrap();
        assert_eq!(
            ledger.mark_dispatched("op-1", &bob(), 7, 11),
            Err(ExecutionError::OwnershipFenceMismatch)
        );
        ledger.mark_dispatched("op-1", &alice(), 7, 11).unwrap();
        assert_eq!(
            ledger.finalize(
                "op-1",
                &alice(),
                6,
                HubOperationState::Completed,
                ExecutionEvidence::VerifiedAgentResult,
                12,
            ),
            Err(ExecutionError::OwnershipFenceMismatch)
        );
        let (_, receipt) = ledger
            .finalize(
                "op-1",
                &alice(),
                7,
                HubOperationState::Completed,
                ExecutionEvidence::VerifiedAgentResult,
                12,
            )
            .unwrap();
        assert_eq!(receipt.owner, alice());
        assert_eq!(receipt.operation.operation_id, "op-1");
    }

    #[test]
    fn bounded_process_result_is_owner_scoped_and_survives_restart() {
        let mut ledger = controller();
        ledger
            .prepare(
                op("op-recover", 7),
                alice(),
                DeviceCapability::ExecuteProcess,
                10,
            )
            .unwrap();
        ledger
            .mark_dispatched("op-recover", &alice(), 7, 11)
            .unwrap();
        ledger
            .finalize(
                "op-recover",
                &alice(),
                7,
                HubOperationState::Completed,
                ExecutionEvidence::VerifiedAgentResult,
                12,
            )
            .unwrap();
        let output = ProcessOutput {
            exit_code: Some(0),
            stdout: "done\n".into(),
            stderr: String::new(),
            stdout_truncated: false,
            stderr_truncated: false,
            timed_out: false,
            cancelled: false,
            duration_ms: 2,
        };
        ledger
            .attach_recoverable_result(
                "op-recover",
                &alice(),
                7,
                RecoverableOperationResult::Process {
                    output: output.clone(),
                },
            )
            .unwrap();

        assert_eq!(
            ledger.recovery_for_owner("op-recover", &bob()),
            Err(ExecutionError::UnknownOperation)
        );
        let recovered = ledger.recovery_for_owner("op-recover", &alice()).unwrap();
        assert_eq!(recovered.state, HubOperationState::Completed);
        assert_eq!(
            recovered.result,
            Some(RecoverableOperationResult::Process {
                output: output.clone()
            })
        );

        let snapshot = ledger.snapshot_for_restart();
        assert_eq!(snapshot.schema_version, EXECUTION_SAFETY_SCHEMA_VERSION);
        let restored = AuthoritativeOperationController::restore_after_restart(
            AdmissionLimits {
                max_global_active: 1,
                max_queued_per_device: 8,
            },
            snapshot,
        )
        .unwrap();
        assert_eq!(
            restored
                .recovery_for_owner("op-recover", &alice())
                .unwrap()
                .result,
            Some(RecoverableOperationResult::Process { output })
        );
    }

    #[test]
    fn effectful_status_is_owner_scoped_payload_free_and_survives_pruning() {
        let mut ledger = controller();
        ledger
            .prepare(
                op("op-effectful", 1),
                alice(),
                DeviceCapability::PointerClick,
                10,
            )
            .unwrap();
        ledger
            .mark_dispatched("op-effectful", &alice(), 1, 11)
            .unwrap();
        ledger
            .finalize(
                "op-effectful",
                &alice(),
                1,
                HubOperationState::Completed,
                ExecutionEvidence::VerifiedAgentResult,
                12,
            )
            .unwrap();
        ledger
            .attach_recoverable_result(
                "op-effectful",
                &alice(),
                1,
                RecoverableOperationResult::EffectfulStatus,
            )
            .unwrap();

        assert_eq!(
            ledger.recovery_for_owner("op-effectful", &bob()),
            Err(ExecutionError::UnknownOperation)
        );
        let recovered = ledger.recovery_for_owner("op-effectful", &alice()).unwrap();
        assert_eq!(recovered.state, HubOperationState::Completed);
        assert_eq!(
            recovered.result,
            Some(RecoverableOperationResult::EffectfulStatus)
        );
        assert!(matches!(
            ledger.snapshot_for_restart_compatible_with(
                RECOVERY_EVIDENCE_READ_EXECUTION_SAFETY_SCHEMA_VERSION
            ),
            Err(ExecutionError::InvalidSnapshot)
        ));

        assert_eq!(
            ledger
                .prune_terminal_before_generation("desktop-a", 2)
                .unwrap(),
            1
        );
        assert_eq!(ledger.state("op-effectful"), None);
        assert_eq!(
            ledger
                .recovery_for_owner("op-effectful", &alice())
                .unwrap()
                .result,
            Some(RecoverableOperationResult::EffectfulStatus)
        );
        assert_eq!(
            ledger.prepare(
                op("op-effectful", 2),
                alice(),
                DeviceCapability::PointerClick,
                20,
            ),
            Err(ExecutionError::OperationReplay)
        );

        let snapshot = ledger.snapshot_for_restart();
        assert_eq!(snapshot.schema_version, EXECUTION_SAFETY_SCHEMA_VERSION);
        let serialized = serde_json::to_string(&snapshot).unwrap();
        assert!(serialized.contains("effectful_status"));
        let restored = AuthoritativeOperationController::restore_after_restart(
            AdmissionLimits {
                max_global_active: 1,
                max_queued_per_device: 8,
            },
            snapshot,
        )
        .unwrap();
        assert_eq!(
            restored
                .recovery_for_owner("op-effectful", &alice())
                .unwrap()
                .result,
            Some(RecoverableOperationResult::EffectfulStatus)
        );
    }

    #[test]
    fn recoverable_result_survives_generation_pruning_and_remains_a_replay_tombstone() {
        let mut ledger = controller();
        ledger
            .prepare(
                op("op-generation-recovery", 1),
                alice(),
                DeviceCapability::Shell,
                1,
            )
            .unwrap();
        ledger
            .mark_dispatched("op-generation-recovery", &alice(), 1, 2)
            .unwrap();
        ledger
            .finalize(
                "op-generation-recovery",
                &alice(),
                1,
                HubOperationState::Completed,
                ExecutionEvidence::VerifiedAgentResult,
                3,
            )
            .unwrap();
        let output = ProcessOutput {
            exit_code: Some(0),
            stdout: "survives generation rollover\n".into(),
            stderr: String::new(),
            stdout_truncated: false,
            stderr_truncated: false,
            timed_out: false,
            cancelled: false,
            duration_ms: 1,
        };
        ledger
            .attach_recoverable_result(
                "op-generation-recovery",
                &alice(),
                1,
                RecoverableOperationResult::Shell {
                    output: output.clone(),
                },
            )
            .unwrap();

        assert_eq!(
            ledger
                .prune_terminal_before_generation("desktop-a", 2)
                .unwrap(),
            1
        );
        assert_eq!(ledger.state("op-generation-recovery"), None);
        assert_eq!(
            ledger
                .recovery_for_owner("op-generation-recovery", &alice())
                .unwrap()
                .result,
            Some(RecoverableOperationResult::Shell {
                output: output.clone()
            })
        );
        assert_eq!(
            ledger.prepare(
                op("op-generation-recovery", 2),
                alice(),
                DeviceCapability::Shell,
                4,
            ),
            Err(ExecutionError::OperationReplay)
        );

        let snapshot = ledger.snapshot_for_restart();
        assert_eq!(snapshot.recoveries.len(), 1);
        assert!(snapshot.operations.is_empty());
        let restored = AuthoritativeOperationController::restore_after_restart(
            AdmissionLimits {
                max_global_active: 1,
                max_queued_per_device: 8,
            },
            snapshot,
        )
        .unwrap();
        assert_eq!(
            restored
                .recovery_for_owner("op-generation-recovery", &alice())
                .unwrap()
                .result,
            Some(RecoverableOperationResult::Shell { output })
        );
        assert_eq!(
            restored.recovery_for_owner("op-generation-recovery", &bob()),
            Err(ExecutionError::UnknownOperation)
        );
    }

    #[test]
    fn recovery_archive_is_bounded_by_count_and_encoded_bytes() {
        let mut ledger = controller();
        let stdout = "x".repeat(16 * 1024);
        let stderr = "y".repeat(16 * 1024);
        for index in 0..10_u64 {
            let operation_id = format!("op-bounded-{index:02}");
            ledger
                .prepare(
                    op(&operation_id, 1),
                    alice(),
                    DeviceCapability::ExecuteProcess,
                    index * 3 + 1,
                )
                .unwrap();
            ledger
                .mark_dispatched(&operation_id, &alice(), 1, index * 3 + 2)
                .unwrap();
            ledger
                .finalize(
                    &operation_id,
                    &alice(),
                    1,
                    HubOperationState::Completed,
                    ExecutionEvidence::VerifiedAgentResult,
                    index * 3 + 3,
                )
                .unwrap();
            ledger
                .attach_recoverable_result(
                    &operation_id,
                    &alice(),
                    1,
                    RecoverableOperationResult::Process {
                        output: ProcessOutput {
                            exit_code: Some(0),
                            stdout: stdout.clone(),
                            stderr: stderr.clone(),
                            stdout_truncated: true,
                            stderr_truncated: true,
                            timed_out: false,
                            cancelled: false,
                            duration_ms: 1,
                        },
                    },
                )
                .unwrap();
        }
        assert_eq!(
            ledger
                .prune_terminal_before_generation("desktop-a", 2)
                .unwrap(),
            10
        );
        assert!(ledger.recovery_archive.len() <= MAX_RECOVERY_ARCHIVE_ENTRIES);
        assert!(ledger.recovery_archive_encoded_bytes() <= MAX_RECOVERY_ARCHIVE_BYTES);
        assert!(ledger.recovery_for_owner("op-bounded-09", &alice()).is_ok());
        assert_eq!(
            ledger.recovery_for_owner("op-bounded-00", &alice()),
            Err(ExecutionError::UnknownOperation)
        );
        let snapshot = ledger.snapshot_for_restart();
        assert!(snapshot.recoveries.len() <= MAX_RECOVERY_ARCHIVE_ENTRIES);
        let restored = AuthoritativeOperationController::restore_after_restart(
            AdmissionLimits {
                max_global_active: 1,
                max_queued_per_device: 8,
            },
            snapshot,
        )
        .unwrap();
        assert!(restored.recovery_archive_encoded_bytes() <= MAX_RECOVERY_ARCHIVE_BYTES);
    }

    #[test]
    fn compatible_restart_snapshot_preserves_previous_schema_when_state_is_representable() {
        let mut ledger = controller();
        ledger
            .prepare(
                op("op-legacy-compatible", 7),
                alice(),
                DeviceCapability::Shell,
                10,
            )
            .unwrap();
        ledger
            .mark_dispatched("op-legacy-compatible", &alice(), 7, 11)
            .unwrap();
        ledger
            .finalize(
                "op-legacy-compatible",
                &alice(),
                7,
                HubOperationState::Completed,
                ExecutionEvidence::VerifiedAgentResult,
                12,
            )
            .unwrap();

        let snapshot = ledger
            .snapshot_for_restart_compatible_with(LEGACY_EXECUTION_SAFETY_SCHEMA_VERSION)
            .unwrap();
        assert_eq!(
            snapshot.schema_version,
            LEGACY_EXECUTION_SAFETY_SCHEMA_VERSION
        );
        assert!(snapshot.recoveries.is_empty());
        assert!(
            snapshot
                .operations
                .iter()
                .all(|record| record.recoverable_result.is_none())
        );
        assert!(
            AuthoritativeOperationController::restore_after_restart(
                AdmissionLimits {
                    max_global_active: 1,
                    max_queued_per_device: 8,
                },
                snapshot,
            )
            .is_ok()
        );
    }

    #[test]
    fn compatible_restart_snapshot_refuses_previous_schema_when_v2_recovery_data_exists() {
        let mut ledger = controller();
        ledger
            .prepare(op("op-v2-only", 7), alice(), DeviceCapability::Shell, 10)
            .unwrap();
        ledger
            .mark_dispatched("op-v2-only", &alice(), 7, 11)
            .unwrap();
        ledger
            .finalize(
                "op-v2-only",
                &alice(),
                7,
                HubOperationState::Completed,
                ExecutionEvidence::VerifiedAgentResult,
                12,
            )
            .unwrap();
        ledger
            .attach_recoverable_result(
                "op-v2-only",
                &alice(),
                7,
                RecoverableOperationResult::Shell {
                    output: ProcessOutput {
                        exit_code: Some(0),
                        stdout: "bounded result".into(),
                        stderr: String::new(),
                        stdout_truncated: false,
                        stderr_truncated: false,
                        timed_out: false,
                        cancelled: false,
                        duration_ms: 1,
                    },
                },
            )
            .unwrap();

        assert!(matches!(
            ledger.snapshot_for_restart_compatible_with(LEGACY_EXECUTION_SAFETY_SCHEMA_VERSION),
            Err(ExecutionError::InvalidSnapshot)
        ));
    }

    #[test]
    fn previous_safety_snapshot_without_result_remains_readable_but_cannot_smuggle_result() {
        let mut ledger = controller();
        ledger
            .prepare(op("op-old", 7), alice(), DeviceCapability::Shell, 10)
            .unwrap();
        ledger.mark_dispatched("op-old", &alice(), 7, 11).unwrap();
        ledger
            .finalize(
                "op-old",
                &alice(),
                7,
                HubOperationState::Completed,
                ExecutionEvidence::VerifiedAgentResult,
                12,
            )
            .unwrap();
        let mut legacy = ledger.snapshot_for_restart();
        legacy.schema_version = LEGACY_EXECUTION_SAFETY_SCHEMA_VERSION;
        assert!(
            AuthoritativeOperationController::restore_after_restart(
                AdmissionLimits {
                    max_global_active: 1,
                    max_queued_per_device: 8,
                },
                legacy.clone(),
            )
            .is_ok()
        );

        let mut legacy_with_archive = legacy.clone();
        let receipt = legacy_with_archive.operations[0].receipt.clone().unwrap();
        legacy_with_archive
            .recoveries
            .push(ArchivedOperationRecovery {
                operation: receipt.operation.clone(),
                owner: receipt.owner.clone(),
                capability: receipt.capability,
                state: receipt.terminal_state,
                receipt,
                result: RecoverableOperationResult::Shell {
                    output: ProcessOutput {
                        exit_code: Some(0),
                        stdout: "smuggled archive".into(),
                        stderr: String::new(),
                        stdout_truncated: false,
                        stderr_truncated: false,
                        timed_out: false,
                        cancelled: false,
                        duration_ms: 1,
                    },
                },
            });
        assert!(matches!(
            AuthoritativeOperationController::restore_after_restart(
                AdmissionLimits {
                    max_global_active: 1,
                    max_queued_per_device: 8,
                },
                legacy_with_archive,
            ),
            Err(ExecutionError::InvalidSnapshot)
        ));

        legacy.operations[0].recoverable_result = Some(RecoverableOperationResult::Shell {
            output: ProcessOutput {
                exit_code: Some(0),
                stdout: "smuggled".into(),
                stderr: String::new(),
                stdout_truncated: false,
                stderr_truncated: false,
                timed_out: false,
                cancelled: false,
                duration_ms: 1,
            },
        });
        assert!(matches!(
            AuthoritativeOperationController::restore_after_restart(
                AdmissionLimits {
                    max_global_active: 1,
                    max_queued_per_device: 8,
                },
                legacy,
            ),
            Err(ExecutionError::InvalidSnapshot)
        ));
    }

    #[test]
    fn indeterminate_quarantine_survives_restart_and_requires_exact_resolution() {
        let mut ledger = controller();
        ledger
            .prepare(
                op("op-ambiguous", 4),
                alice(),
                DeviceCapability::PointerClick,
                100,
            )
            .unwrap();
        ledger
            .mark_dispatched("op-ambiguous", &alice(), 4, 101)
            .unwrap();
        ledger
            .mark_indeterminate(
                "op-ambiguous",
                &alice(),
                4,
                IndeterminateReason::CancellationUnproven,
                102,
            )
            .unwrap();
        assert!(ledger.quarantine("desktop-a").is_some());
        assert!(matches!(
            ledger.prepare(op("op-bob", 5), bob(), DeviceCapability::Shell, 103),
            Err(ExecutionError::DeviceIndeterminate { .. })
        ));

        let snapshot = ledger.snapshot_for_restart();
        let mut restored = AuthoritativeOperationController::restore_after_restart(
            AdmissionLimits {
                max_global_active: 1,
                max_queued_per_device: 8,
            },
            snapshot,
        )
        .unwrap();
        assert!(restored.quarantine("desktop-a").is_some());
        let (_, receipt) = restored
            .resolve_indeterminate(
                "op-ambiguous",
                bob(),
                IndeterminateResolution::ConfirmedCompleted,
                "operator visually verified the resulting desktop state",
                200,
            )
            .unwrap();
        assert_eq!(receipt.evidence, ExecutionEvidence::OperatorResolution);
        assert!(restored.quarantine("desktop-a").is_none());
        assert!(
            restored
                .prepare(op("op-bob", 5), bob(), DeviceCapability::Shell, 201)
                .is_ok()
        );
    }

    #[test]
    fn quarantine_inspection_is_privacy_bounded_and_stable_across_restart() {
        let mut ledger = controller();
        ledger
            .prepare(op("op-audit", 4), alice(), DeviceCapability::Shell, 100)
            .unwrap();
        ledger
            .mark_dispatched("op-audit", &alice(), 4, 110)
            .unwrap();
        ledger
            .mark_indeterminate(
                "op-audit",
                &alice(),
                4,
                IndeterminateReason::ConnectionLost,
                120,
            )
            .unwrap();
        assert_eq!(
            ledger
                .prepare(
                    op("op-later-rejected", 5),
                    bob(),
                    DeviceCapability::Shell,
                    130,
                )
                .unwrap_err(),
            ExecutionError::DeviceIndeterminate {
                operation_id: "op-audit".into(),
            }
        );

        let before = ledger.quarantine_inspections().unwrap();
        assert_eq!(before.len(), 1);
        assert_eq!(before[0].operation.operation_id, "op-audit");
        assert_eq!(before[0].operation.device_id, "desktop-a");
        assert_eq!(before[0].operation.device_generation, 4);
        assert_eq!(before[0].capability, DeviceCapability::Shell);
        assert_eq!(before[0].prepared_at_ms, 100);
        assert_eq!(before[0].dispatched_at_ms, Some(110));
        assert_eq!(before[0].indeterminate_at_ms, 120);
        assert_eq!(
            before[0].indeterminate_reason,
            IndeterminateReason::ConnectionLost
        );
        assert_eq!(before[0].evidence, None);

        let restored = AuthoritativeOperationController::restore_after_restart(
            AdmissionLimits {
                max_global_active: 1,
                max_queued_per_device: 8,
            },
            ledger.snapshot_for_restart(),
        )
        .unwrap();
        assert_eq!(restored.quarantine_inspections().unwrap(), before);
    }

    #[test]
    fn crash_after_dispatch_persists_unknown_effect_and_never_runnable_work() {
        let mut ledger = controller();
        ledger
            .prepare(
                op("op-crash", 3),
                alice(),
                DeviceCapability::PointerDrag,
                10,
            )
            .unwrap();
        ledger.mark_dispatched("op-crash", &alice(), 3, 11).unwrap();
        let snapshot = ledger.snapshot_for_restart();
        let restored = AuthoritativeOperationController::restore_after_restart(
            AdmissionLimits {
                max_global_active: 1,
                max_queued_per_device: 8,
            },
            snapshot,
        )
        .unwrap();
        assert_eq!(
            restored.state("op-crash"),
            Some(HubOperationState::Indeterminate)
        );
        let quarantine = restored.quarantine("desktop-a").unwrap();
        assert_eq!(quarantine.operation_id, "op-crash");
        assert_eq!(
            quarantine.reason,
            IndeterminateReason::HubRestartAfterDispatch
        );
    }
    #[test]
    fn quarantine_cancels_preexisting_queue_and_resolution_never_resumes_it() {
        let mut ledger = controller();
        assert!(matches!(
            ledger
                .prepare(op("op-live", 1), alice(), DeviceCapability::PointerDrag, 1)
                .unwrap(),
            AdmissionDecision::StartNow(_)
        ));
        assert!(matches!(
            ledger
                .prepare(op("op-queued", 1), alice(), DeviceCapability::Shell, 2)
                .unwrap(),
            AdmissionDecision::Queued { .. }
        ));
        ledger.mark_dispatched("op-live", &alice(), 1, 3).unwrap();
        ledger
            .mark_indeterminate(
                "op-live",
                &alice(),
                1,
                IndeterminateReason::ConnectionLost,
                4,
            )
            .unwrap();
        assert_eq!(
            ledger.state("op-queued"),
            Some(HubOperationState::Cancelled)
        );
        assert_eq!(
            ledger.receipt("op-queued").unwrap().evidence,
            ExecutionEvidence::CancelledBeforeDispatch
        );

        let (next, _) = ledger
            .resolve_indeterminate(
                "op-live",
                alice(),
                IndeterminateResolution::ConfirmedCompleted,
                "desktop state manually reconciled",
                5,
            )
            .unwrap();
        assert_eq!(next, CompletionDecision::Idle);
        assert_eq!(
            ledger.state("op-queued"),
            Some(HubOperationState::Cancelled)
        );
    }

    #[test]
    fn duplicate_finalization_and_nonterminal_finalization_are_rejected() {
        let mut ledger = controller();
        ledger
            .prepare(op("op-finalize", 2), alice(), DeviceCapability::Shell, 1)
            .unwrap();
        assert_eq!(
            ledger.finalize(
                "op-finalize",
                &alice(),
                2,
                HubOperationState::Indeterminate,
                ExecutionEvidence::VerifiedAgentResult,
                2,
            ),
            Err(ExecutionError::InvalidTransition)
        );
        ledger
            .mark_dispatched("op-finalize", &alice(), 2, 3)
            .unwrap();
        assert_eq!(
            ledger.finalize(
                "op-finalize",
                &alice(),
                2,
                HubOperationState::Completed,
                ExecutionEvidence::VerifiedRemoteError,
                4,
            ),
            Err(ExecutionError::InvalidTransition)
        );
        assert_eq!(
            ledger.state("op-finalize"),
            Some(HubOperationState::Dispatched)
        );
        let (_, first) = ledger
            .finalize(
                "op-finalize",
                &alice(),
                2,
                HubOperationState::Completed,
                ExecutionEvidence::VerifiedAgentResult,
                4,
            )
            .unwrap();
        assert_eq!(
            ledger.finalize(
                "op-finalize",
                &alice(),
                2,
                HubOperationState::Completed,
                ExecutionEvidence::VerifiedAgentResult,
                5,
            ),
            Err(ExecutionError::InvalidTransition)
        );
        assert_eq!(ledger.receipt("op-finalize"), Some(&first));
    }

    #[test]
    fn duplicate_or_late_ambiguity_signal_cannot_clear_or_replace_quarantine() {
        let mut ledger = controller();
        ledger
            .prepare(
                op("op-ambiguous", 9),
                alice(),
                DeviceCapability::PointerClick,
                1,
            )
            .unwrap();
        ledger
            .mark_dispatched("op-ambiguous", &alice(), 9, 2)
            .unwrap();
        ledger
            .mark_indeterminate(
                "op-ambiguous",
                &alice(),
                9,
                IndeterminateReason::CancellationUnproven,
                3,
            )
            .unwrap();
        let before = ledger.quarantine("desktop-a").cloned().unwrap();
        assert_eq!(
            ledger.mark_indeterminate(
                "op-ambiguous",
                &alice(),
                9,
                IndeterminateReason::ConnectionLost,
                4,
            ),
            Err(ExecutionError::InvalidTransition)
        );
        assert_eq!(ledger.quarantine("desktop-a"), Some(&before));
    }

    #[test]
    fn crash_on_either_side_of_resolution_is_fail_closed_and_never_replays() {
        let limits = AdmissionLimits {
            max_global_active: 1,
            max_queued_per_device: 8,
        };
        let mut ledger = AuthoritativeOperationController::new(limits).unwrap();
        ledger
            .prepare(
                op("op-resolve", 4),
                alice(),
                DeviceCapability::PointerClick,
                1,
            )
            .unwrap();
        ledger
            .mark_dispatched("op-resolve", &alice(), 4, 2)
            .unwrap();
        ledger
            .mark_indeterminate(
                "op-resolve",
                &alice(),
                4,
                IndeterminateReason::ConnectionLost,
                3,
            )
            .unwrap();

        let before_resolution = ledger.snapshot_for_restart();
        let mut before =
            AuthoritativeOperationController::restore_after_restart(limits, before_resolution)
                .unwrap();
        assert_eq!(
            before.state("op-resolve"),
            Some(HubOperationState::Indeterminate)
        );
        assert!(matches!(
            before.prepare(op("op-new", 5), alice(), DeviceCapability::Shell, 4),
            Err(ExecutionError::DeviceIndeterminate { .. })
        ));

        ledger
            .resolve_indeterminate(
                "op-resolve",
                alice(),
                IndeterminateResolution::ConfirmedNotExecuted,
                "operator proved the action did not occur",
                5,
            )
            .unwrap();
        let after_resolution = ledger.snapshot_for_restart();
        let mut after =
            AuthoritativeOperationController::restore_after_restart(limits, after_resolution)
                .unwrap();
        assert_eq!(
            after.state("op-resolve"),
            Some(HubOperationState::Cancelled)
        );
        assert!(after.quarantine("desktop-a").is_none());
        assert_eq!(
            after.prepare(op("op-resolve", 5), alice(), DeviceCapability::Shell, 6),
            Err(ExecutionError::OperationReplay)
        );
        assert!(
            after
                .prepare(op("op-new", 5), alice(), DeviceCapability::Shell, 6)
                .is_ok()
        );
    }

    #[test]
    fn pruning_for_new_generation_never_forgets_unresolved_ambiguity_or_resolution_audit() {
        let mut ledger = controller();
        ledger
            .prepare(op("op-done", 1), alice(), DeviceCapability::Shell, 1)
            .unwrap();
        ledger.mark_dispatched("op-done", &alice(), 1, 2).unwrap();
        ledger
            .finalize(
                "op-done",
                &alice(),
                1,
                HubOperationState::Completed,
                ExecutionEvidence::VerifiedAgentResult,
                3,
            )
            .unwrap();
        ledger
            .prepare(
                op("op-ambiguous", 2),
                alice(),
                DeviceCapability::PointerClick,
                4,
            )
            .unwrap();
        ledger
            .mark_dispatched("op-ambiguous", &alice(), 2, 5)
            .unwrap();
        ledger
            .mark_indeterminate(
                "op-ambiguous",
                &alice(),
                2,
                IndeterminateReason::ConnectionLost,
                6,
            )
            .unwrap();
        assert_eq!(
            ledger
                .prune_terminal_before_generation("desktop-a", 3)
                .unwrap(),
            1
        );
        assert_eq!(ledger.state("op-done"), None);
        assert_eq!(
            ledger.state("op-ambiguous"),
            Some(HubOperationState::Indeterminate)
        );

        ledger
            .resolve_indeterminate(
                "op-ambiguous",
                bob(),
                IndeterminateResolution::ConfirmedCompleted,
                "operator reconciled external state",
                7,
            )
            .unwrap();
        assert_eq!(ledger.resolutions().len(), 1);
        assert_eq!(
            ledger
                .prune_terminal_before_generation("desktop-a", 4)
                .unwrap(),
            1
        );
        assert_eq!(ledger.state("op-ambiguous"), None);
        assert_eq!(ledger.resolutions().len(), 1);
    }

    #[test]
    fn restore_rejects_divergent_admission_and_safety_ledgers() {
        let mut ledger = controller();
        ledger
            .prepare(op("op-snapshot", 1), alice(), DeviceCapability::Shell, 1)
            .unwrap();
        ledger
            .mark_dispatched("op-snapshot", &alice(), 1, 2)
            .unwrap();
        let mut snapshot = ledger.snapshot_for_restart();
        snapshot.admission.operations.clear();
        assert!(matches!(
            AuthoritativeOperationController::restore_after_restart(
                AdmissionLimits {
                    max_global_active: 1,
                    max_queued_per_device: 8,
                },
                snapshot,
            ),
            Err(ExecutionError::InvalidSnapshot)
        ));
    }

    #[test]
    fn correlated_quarantine_survives_restart_without_persisting_raw_request() {
        use crate::v2_m0::ProcessEnvVar;

        let secret = b"0123456789abcdef0123456789abcdef";
        let rotated_secret = b"fedcba9876543210fedcba9876543210";
        let request = ShellRequest {
            command: "printf super-sensitive-command-marker".into(),
            cwd: "/private/sensitive-cwd".into(),
            env: vec![ProcessEnvVar {
                key: "SECRET_NAME".into(),
                value: "super-sensitive-env-marker".into(),
            }],
            timeout_ms: 1_000,
        };
        let fingerprint = fingerprint_shell_request(secret, &request).unwrap();
        let audit = OperationAuditMetadata {
            workflow_id: Some("wf_release_42".into()),
            workflow_step_id: Some("step_verify".into()),
            client_correlation_id: Some("client_abc123".into()),
        };
        let mut ledger = controller();
        ledger
            .prepare_with_metadata(
                op("op-correlated", 7),
                alice(),
                DeviceCapability::Shell,
                OperationAdmissionMetadata {
                    audit: audit.clone(),
                    request_fingerprint: Some(fingerprint.clone()),
                    evidence_envelope: None,
                },
                10,
            )
            .unwrap();
        ledger
            .mark_dispatched("op-correlated", &alice(), 7, 11)
            .unwrap();
        ledger.mark_connection_lost("op-correlated", 12).unwrap();

        let snapshot = ledger.snapshot_for_restart();
        assert_eq!(snapshot.schema_version, EXECUTION_SAFETY_SCHEMA_VERSION);
        let encoded = serde_json::to_string(&snapshot).unwrap();
        assert!(encoded.contains("wf_release_42"));
        assert!(!encoded.contains("super-sensitive-command-marker"));
        assert!(!encoded.contains("/private/sensitive-cwd"));
        assert!(!encoded.contains("super-sensitive-env-marker"));

        let restored = AuthoritativeOperationController::restore_after_restart(
            AdmissionLimits {
                max_global_active: 1,
                max_queued_per_device: 8,
            },
            snapshot,
        )
        .unwrap();
        let inspection = restored.quarantine_inspections().unwrap().remove(0);
        assert_eq!(inspection.audit, audit);
        assert_eq!(inspection.request_fingerprint.as_ref(), Some(&fingerprint));

        let same = fingerprint_shell_request(secret, &request).unwrap();
        assert_eq!(
            compare_request_fingerprint(Some(&fingerprint), Some(&same)),
            RequestFingerprintComparison::SameRequest
        );
        let mut changed_request = request.clone();
        changed_request.timeout_ms += 1;
        let changed = fingerprint_shell_request(secret, &changed_request).unwrap();
        assert_eq!(
            compare_request_fingerprint(Some(&fingerprint), Some(&changed)),
            RequestFingerprintComparison::DifferentRequest
        );
        let rotated = fingerprint_shell_request(rotated_secret, &request).unwrap();
        assert_eq!(
            compare_request_fingerprint(Some(&fingerprint), Some(&rotated)),
            RequestFingerprintComparison::Unavailable
        );
    }

    #[test]
    fn schema_v2_cannot_claim_v3_audit_or_fingerprint_state() {
        let secret = b"0123456789abcdef0123456789abcdef";
        let request = ShellRequest {
            command: "echo bounded".into(),
            cwd: "/tmp".into(),
            env: vec![],
            timeout_ms: 1_000,
        };
        let fingerprint = fingerprint_shell_request(secret, &request).unwrap();
        let mut ledger = controller();
        ledger
            .prepare_with_metadata(
                op("op-v3-audit", 7),
                alice(),
                DeviceCapability::Shell,
                OperationAdmissionMetadata {
                    audit: OperationAuditMetadata {
                        workflow_id: Some("wf_v3".into()),
                        workflow_step_id: None,
                        client_correlation_id: None,
                    },
                    request_fingerprint: Some(fingerprint),
                    evidence_envelope: None,
                },
                10,
            )
            .unwrap();
        ledger
            .mark_dispatched("op-v3-audit", &alice(), 7, 11)
            .unwrap();
        ledger.mark_connection_lost("op-v3-audit", 12).unwrap();

        assert!(matches!(
            ledger.snapshot_for_restart_compatible_with(RECOVERY_EXECUTION_SAFETY_SCHEMA_VERSION),
            Err(ExecutionError::InvalidSnapshot)
        ));
        let mut forged_v2 = ledger.snapshot_for_restart();
        forged_v2.schema_version = RECOVERY_EXECUTION_SAFETY_SCHEMA_VERSION;
        assert!(matches!(
            AuthoritativeOperationController::restore_after_restart(
                AdmissionLimits {
                    max_global_active: 1,
                    max_queued_per_device: 8,
                },
                forged_v2,
            ),
            Err(ExecutionError::InvalidSnapshot)
        ));
    }

    #[test]
    fn policy_eligible_unknown_scroll_can_retire_without_replay_or_synthetic_outcome() {
        let limits = AdmissionLimits {
            max_global_active: 1,
            max_queued_per_device: 8,
        };
        let mut ledger = AuthoritativeOperationController::new(limits).unwrap();
        ledger
            .prepare(
                op("op-retire-scroll", 4),
                alice(),
                DeviceCapability::Scroll,
                100,
            )
            .unwrap();
        ledger
            .mark_dispatched("op-retire-scroll", &alice(), 4, 110)
            .unwrap();
        ledger
            .mark_indeterminate(
                "op-retire-scroll",
                &alice(),
                4,
                IndeterminateReason::BackendOutcomeUnproven,
                120,
            )
            .unwrap();

        let (_next, retirement) = ledger
            .retire_indeterminate(
                "op-retire-scroll",
                RetirementAuthority::LocalMaintenanceOperator,
                RetirementPolicy::TransientUiInteractionV1,
                "legacy transient UI outcome permanently unknowable",
                5,
                130,
            )
            .unwrap();
        assert_eq!(retirement.outcome, RetirementOutcome::Unknown);
        assert_eq!(
            retirement.policy,
            RetirementPolicy::TransientUiInteractionV1
        );
        assert_eq!(retirement.authorized_device_generation, 5);
        assert!(!retirement.replayed);
        assert_eq!(
            ledger.state("op-retire-scroll"),
            Some(HubOperationState::Indeterminate)
        );
        assert!(ledger.receipt("op-retire-scroll").is_none());
        assert!(ledger.quarantine("desktop-a").is_none());
        assert_eq!(ledger.retirements(), &[retirement.clone()]);

        assert_eq!(
            ledger.resolve_indeterminate(
                "op-retire-scroll",
                bob(),
                IndeterminateResolution::ConfirmedCompleted,
                "must not synthesize completion after retirement",
                140,
            ),
            Err(ExecutionError::InvalidTransition)
        );
        assert_eq!(
            ledger.retire_indeterminate(
                "op-retire-scroll",
                RetirementAuthority::LocalMaintenanceOperator,
                RetirementPolicy::TransientUiInteractionV1,
                "duplicate",
                6,
                141,
            ),
            Err(ExecutionError::InvalidTransition)
        );
        assert_eq!(
            ledger.prepare(
                op("op-retire-scroll", 5),
                alice(),
                DeviceCapability::Scroll,
                150,
            ),
            Err(ExecutionError::OperationReplay)
        );
        assert!(
            ledger
                .prepare(
                    op("op-fresh-after-retirement", 5),
                    alice(),
                    DeviceCapability::Scroll,
                    150,
                )
                .is_ok()
        );

        let snapshot = ledger.snapshot_for_restart();
        let mut restored =
            AuthoritativeOperationController::restore_after_restart(limits, snapshot).unwrap();
        assert_eq!(
            restored.state("op-retire-scroll"),
            Some(HubOperationState::Indeterminate)
        );
        assert!(restored.quarantine("desktop-a").is_none());
        assert_eq!(restored.retirements().len(), 1);
        assert_eq!(
            restored
                .prune_terminal_before_generation("desktop-a", 100)
                .unwrap(),
            1
        );
        assert_eq!(
            restored.state("op-retire-scroll"),
            Some(HubOperationState::Indeterminate)
        );
    }

    #[test]
    fn retirement_requires_newer_generation_and_rejects_high_impact_capabilities() {
        let mut stale = controller();
        stale
            .prepare(
                op("op-stale-retire", 7),
                alice(),
                DeviceCapability::Scroll,
                1,
            )
            .unwrap();
        stale
            .mark_dispatched("op-stale-retire", &alice(), 7, 2)
            .unwrap();
        stale
            .mark_indeterminate(
                "op-stale-retire",
                &alice(),
                7,
                IndeterminateReason::BackendOutcomeUnproven,
                3,
            )
            .unwrap();
        assert_eq!(
            stale.retire_indeterminate(
                "op-stale-retire",
                RetirementAuthority::LocalMaintenanceOperator,
                RetirementPolicy::TransientUiInteractionV1,
                "not yet fenced",
                7,
                4,
            ),
            Err(ExecutionError::InvalidTransition)
        );
        assert!(stale.quarantine("desktop-a").is_some());

        let mut dangerous = controller();
        dangerous
            .prepare(
                op("op-shell-unknown", 3),
                alice(),
                DeviceCapability::Shell,
                10,
            )
            .unwrap();
        dangerous
            .mark_dispatched("op-shell-unknown", &alice(), 3, 11)
            .unwrap();
        dangerous
            .mark_indeterminate(
                "op-shell-unknown",
                &alice(),
                3,
                IndeterminateReason::BackendOutcomeUnproven,
                12,
            )
            .unwrap();
        assert_eq!(
            dangerous.retire_indeterminate(
                "op-shell-unknown",
                RetirementAuthority::LocalMaintenanceOperator,
                RetirementPolicy::TransientUiInteractionV1,
                "high impact must fail closed",
                4,
                13,
            ),
            Err(ExecutionError::InvalidTransition)
        );
        assert!(dangerous.quarantine("desktop-a").is_some());
        assert!(dangerous.retirements().is_empty());
    }

    #[test]
    fn retirement_history_is_bounded_and_capacity_exhaustion_fails_closed() {
        let mut ledger = controller();
        for index in 0..MAX_RETIREMENT_RECORDS {
            let generation = u64::try_from(index).unwrap() + 1;
            let operation_id = format!("op-retirement-bounded-{index}");
            ledger
                .prepare(
                    op(&operation_id, generation),
                    alice(),
                    DeviceCapability::Scroll,
                    generation * 10,
                )
                .unwrap();
            ledger
                .mark_dispatched(&operation_id, &alice(), generation, generation * 10 + 1)
                .unwrap();
            ledger
                .mark_indeterminate(
                    &operation_id,
                    &alice(),
                    generation,
                    IndeterminateReason::BackendOutcomeUnproven,
                    generation * 10 + 2,
                )
                .unwrap();
            ledger
                .retire_indeterminate(
                    &operation_id,
                    RetirementAuthority::LocalMaintenanceOperator,
                    RetirementPolicy::TransientUiInteractionV1,
                    "bounded retirement audit",
                    generation + 1,
                    generation * 10 + 3,
                )
                .unwrap();
        }
        assert_eq!(ledger.retirements().len(), MAX_RETIREMENT_RECORDS);

        let overflow_generation = u64::try_from(MAX_RETIREMENT_RECORDS).unwrap() + 1;
        ledger
            .prepare(
                op("op-retirement-overflow", overflow_generation),
                alice(),
                DeviceCapability::Scroll,
                10_000,
            )
            .unwrap();
        ledger
            .mark_dispatched(
                "op-retirement-overflow",
                &alice(),
                overflow_generation,
                10_001,
            )
            .unwrap();
        ledger
            .mark_indeterminate(
                "op-retirement-overflow",
                &alice(),
                overflow_generation,
                IndeterminateReason::BackendOutcomeUnproven,
                10_002,
            )
            .unwrap();
        assert_eq!(
            ledger.retire_indeterminate(
                "op-retirement-overflow",
                RetirementAuthority::LocalMaintenanceOperator,
                RetirementPolicy::TransientUiInteractionV1,
                "must fail closed instead of growing checkpoint state without bound",
                overflow_generation + 1,
                10_003,
            ),
            Err(ExecutionError::InvalidTransition)
        );
        assert!(ledger.quarantine("desktop-a").is_some());
        assert_eq!(ledger.retirements().len(), MAX_RETIREMENT_RECORDS);
    }

    #[test]
    fn schema_v4_remains_readable_but_cannot_claim_v5_retirement_state() {
        let mut source = controller();
        source
            .prepare(op("op-v4-scroll", 4), alice(), DeviceCapability::Scroll, 1)
            .unwrap();
        source
            .mark_dispatched("op-v4-scroll", &alice(), 4, 2)
            .unwrap();
        source
            .mark_indeterminate(
                "op-v4-scroll",
                &alice(),
                4,
                IndeterminateReason::BackendOutcomeUnproven,
                3,
            )
            .unwrap();
        let v4 = source
            .snapshot_for_restart_compatible_with(RECONCILIATION_EXECUTION_SAFETY_SCHEMA_VERSION)
            .unwrap();
        assert_eq!(
            v4.schema_version,
            RECONCILIATION_EXECUTION_SAFETY_SCHEMA_VERSION
        );
        let mut restored = AuthoritativeOperationController::restore_after_restart(
            AdmissionLimits {
                max_global_active: 1,
                max_queued_per_device: 8,
            },
            v4,
        )
        .unwrap();
        restored
            .retire_indeterminate(
                "op-v4-scroll",
                RetirementAuthority::LocalMaintenanceOperator,
                RetirementPolicy::TransientUiInteractionV1,
                "upgrade to v5 retirement",
                5,
                4,
            )
            .unwrap();
        assert!(matches!(
            restored.snapshot_for_restart_compatible_with(
                RECONCILIATION_EXECUTION_SAFETY_SCHEMA_VERSION
            ),
            Err(ExecutionError::InvalidSnapshot)
        ));

        let mut forged_v4 = restored.snapshot_for_restart();
        forged_v4.schema_version = RECONCILIATION_EXECUTION_SAFETY_SCHEMA_VERSION;
        assert!(matches!(
            AuthoritativeOperationController::restore_after_restart(
                AdmissionLimits {
                    max_global_active: 1,
                    max_queued_per_device: 8,
                },
                forged_v4,
            ),
            Err(ExecutionError::InvalidSnapshot)
        ));
    }

    #[test]
    fn authoritative_terminal_evidence_self_reconciles_exact_binding_without_replay() {
        let mut ledger = controller();
        let binding = OperationDispatchBinding::new(17, "grant_fence_exact").unwrap();
        ledger
            .prepare(
                op("op-auto-resolve", 4),
                alice(),
                DeviceCapability::PointerClick,
                100,
            )
            .unwrap();
        ledger
            .mark_dispatched_with_binding(
                "op-auto-resolve",
                &alice(),
                4,
                Some(binding.clone()),
                110,
            )
            .unwrap();
        ledger.mark_connection_lost("op-auto-resolve", 120).unwrap();

        let inspection = ledger.quarantine_inspections().unwrap().pop().unwrap();
        assert_eq!(
            inspection.reconciliation_status,
            ReconciliationStatus::AutoReconciling
        );
        assert!(inspection.dispatch_binding_present);

        let proof = AgentTerminalEvidence {
            operation: op("op-auto-resolve", 4),
            capability_revision: binding.capability_revision,
            capability: DeviceCapability::PointerClick,
            dispatch_grant_id: binding.grant_id.clone(),
            terminal_state: HubOperationState::Completed,
            evidence: ExecutionEvidence::VerifiedAgentResult,
        };
        let (next, receipt) = ledger
            .reconcile_authoritative_terminal(&proof, 130)
            .unwrap();
        assert_eq!(next, CompletionDecision::Idle);
        assert_eq!(receipt.terminal_state, HubOperationState::Completed);
        assert_eq!(receipt.evidence, ExecutionEvidence::VerifiedAgentResult);
        assert_eq!(
            ledger.state("op-auto-resolve"),
            Some(HubOperationState::Completed)
        );
        assert!(ledger.quarantine("desktop-a").is_none());
        assert_eq!(ledger.auto_resolutions().len(), 1);
        assert_eq!(
            ledger.auto_resolutions()[0].operation.operation_id,
            "op-auto-resolve"
        );

        // Duplicate evidence is not a new transition and can never trigger replay.
        assert!(matches!(
            ledger.reconcile_authoritative_terminal(&proof, 131),
            Err(ExecutionError::OwnershipFenceMismatch)
        ));
        assert_eq!(ledger.auto_resolutions().len(), 1);

        let snapshot = ledger.snapshot_for_restart();
        let record = snapshot
            .operations
            .iter()
            .find(|record| record.operation.operation_id == "op-auto-resolve")
            .unwrap();
        assert_eq!(
            record.reconciliation_status,
            Some(ReconciliationStatus::AutoResolved)
        );
        let restored = AuthoritativeOperationController::restore_after_restart(
            AdmissionLimits {
                max_global_active: 1,
                max_queued_per_device: 8,
            },
            snapshot,
        )
        .unwrap();
        assert_eq!(
            restored.state("op-auto-resolve"),
            Some(HubOperationState::Completed)
        );
        assert_eq!(restored.auto_resolutions().len(), 1);
    }

    #[test]
    fn hub_restart_and_result_delivery_loss_are_reconciled_only_from_bound_terminal_proof() {
        for (operation_id, reason) in [
            (
                "op-hub-restart",
                IndeterminateReason::HubRestartAfterDispatch,
            ),
            (
                "op-result-delivery-loss",
                IndeterminateReason::ResultDeliveryLost,
            ),
        ] {
            let binding =
                OperationDispatchBinding::new(31, format!("grant_{operation_id}")).unwrap();
            let mut ledger = controller();
            ledger
                .prepare(op(operation_id, 6), alice(), DeviceCapability::Shell, 1)
                .unwrap();
            ledger
                .mark_dispatched_with_binding(operation_id, &alice(), 6, Some(binding.clone()), 2)
                .unwrap();

            let mut ledger = if reason == IndeterminateReason::HubRestartAfterDispatch {
                AuthoritativeOperationController::restore_after_restart(
                    AdmissionLimits {
                        max_global_active: 1,
                        max_queued_per_device: 8,
                    },
                    ledger.snapshot_for_restart(),
                )
                .unwrap()
            } else {
                ledger
                    .mark_indeterminate(operation_id, &alice(), 6, reason, 3)
                    .unwrap();
                ledger
            };
            let inspection = ledger.quarantine_inspections().unwrap().pop().unwrap();
            assert_eq!(inspection.indeterminate_reason, reason);
            assert_eq!(
                inspection.reconciliation_status,
                ReconciliationStatus::AutoReconciling
            );

            let proof = AgentTerminalEvidence {
                operation: op(operation_id, 6),
                capability_revision: binding.capability_revision,
                capability: DeviceCapability::Shell,
                dispatch_grant_id: binding.grant_id,
                terminal_state: HubOperationState::Completed,
                evidence: ExecutionEvidence::VerifiedAgentResult,
            };
            ledger.reconcile_authoritative_terminal(&proof, 4).unwrap();
            assert_eq!(
                ledger.state(operation_id),
                Some(HubOperationState::Completed)
            );
            assert!(ledger.quarantine("desktop-a").is_none());
        }
    }

    #[test]
    fn reconciliation_requires_exact_operation_generation_capability_and_dispatch_fence() {
        let binding = OperationDispatchBinding::new(23, "grant_fence_original").unwrap();
        let mut base = controller();
        base.prepare(op("op-bound", 8), alice(), DeviceCapability::Shell, 1)
            .unwrap();
        base.mark_dispatched_with_binding("op-bound", &alice(), 8, Some(binding.clone()), 2)
            .unwrap();
        base.mark_connection_lost("op-bound", 3).unwrap();

        let exact = AgentTerminalEvidence {
            operation: op("op-bound", 8),
            capability_revision: 23,
            capability: DeviceCapability::Shell,
            dispatch_grant_id: "grant_fence_original".into(),
            terminal_state: HubOperationState::Completed,
            evidence: ExecutionEvidence::VerifiedAgentResult,
        };

        let mut wrong_generation = base.clone();
        let mut proof = exact.clone();
        proof.operation.device_generation = 7;
        assert_eq!(
            wrong_generation.reconcile_authoritative_terminal(&proof, 4),
            Err(ExecutionError::OwnershipFenceMismatch)
        );
        assert!(wrong_generation.quarantine("desktop-a").is_some());

        let mut wrong_device = base.clone();
        let mut proof = exact.clone();
        proof.operation.device_id = "desktop-b".into();
        assert_eq!(
            wrong_device.reconcile_authoritative_terminal(&proof, 4),
            Err(ExecutionError::OwnershipFenceMismatch)
        );
        assert!(wrong_device.quarantine("desktop-a").is_some());

        let mut wrong_capability = base.clone();
        let mut proof = exact.clone();
        proof.capability = DeviceCapability::ExecuteProcess;
        assert_eq!(
            wrong_capability.reconcile_authoritative_terminal(&proof, 4),
            Err(ExecutionError::OwnershipFenceMismatch)
        );
        assert!(wrong_capability.quarantine("desktop-a").is_some());

        let mut wrong_revision = base.clone();
        let mut proof = exact.clone();
        proof.capability_revision = 24;
        assert_eq!(
            wrong_revision.reconcile_authoritative_terminal(&proof, 4),
            Err(ExecutionError::OwnershipFenceMismatch)
        );
        assert!(wrong_revision.quarantine("desktop-a").is_some());

        let mut wrong_fence = base;
        let mut proof = exact;
        proof.dispatch_grant_id = "grant_fence_other".into();
        assert_eq!(
            wrong_fence.reconcile_authoritative_terminal(&proof, 4),
            Err(ExecutionError::OwnershipFenceMismatch)
        );
        assert!(wrong_fence.quarantine("desktop-a").is_some());
    }

    #[test]
    fn missing_or_non_authoritative_evidence_never_clears_quarantine() {
        let binding = OperationDispatchBinding::new(9, "grant_gap").unwrap();
        let mut ledger = controller();
        ledger
            .prepare(op("op-gap", 2), alice(), DeviceCapability::Shell, 1)
            .unwrap();
        ledger
            .mark_dispatched_with_binding("op-gap", &alice(), 2, Some(binding.clone()), 2)
            .unwrap();
        ledger.mark_connection_lost("op-gap", 3).unwrap();
        ledger.mark_reconciliation_evidence_gap("op-gap").unwrap();
        assert_eq!(
            ledger.quarantine_inspections().unwrap()[0].reconciliation_status,
            ReconciliationStatus::UnrecoverableEvidenceGap
        );
        let proof = AgentTerminalEvidence {
            operation: op("op-gap", 2),
            capability_revision: 9,
            capability: DeviceCapability::Shell,
            dispatch_grant_id: binding.grant_id,
            terminal_state: HubOperationState::Completed,
            evidence: ExecutionEvidence::VerifiedAgentResult,
        };
        assert!(matches!(
            ledger.reconcile_authoritative_terminal(&proof, 4),
            Err(ExecutionError::OwnershipFenceMismatch)
        ));
        assert!(ledger.quarantine("desktop-a").is_some());

        let mut operator = controller();
        operator
            .prepare(
                op("op-operator", 3),
                alice(),
                DeviceCapability::PointerClick,
                10,
            )
            .unwrap();
        operator
            .mark_dispatched_with_binding(
                "op-operator",
                &alice(),
                3,
                Some(OperationDispatchBinding::new(10, "grant_operator").unwrap()),
                11,
            )
            .unwrap();
        operator
            .mark_indeterminate(
                "op-operator",
                &alice(),
                3,
                IndeterminateReason::CancellationUnproven,
                12,
            )
            .unwrap();
        assert_eq!(
            operator.quarantine_inspections().unwrap()[0].reconciliation_status,
            ReconciliationStatus::OperatorRequired
        );
    }

    #[test]
    fn forged_auto_resolved_state_requires_matching_bounded_audit_history() {
        let mut ledger = controller();
        let binding = OperationDispatchBinding::new(13, "grant_history_exact").unwrap();
        ledger
            .prepare(
                op("op-history", 5),
                alice(),
                DeviceCapability::PointerClick,
                1,
            )
            .unwrap();
        ledger
            .mark_dispatched_with_binding("op-history", &alice(), 5, Some(binding.clone()), 2)
            .unwrap();
        ledger.mark_connection_lost("op-history", 3).unwrap();
        ledger
            .reconcile_authoritative_terminal(
                &AgentTerminalEvidence {
                    operation: op("op-history", 5),
                    capability_revision: binding.capability_revision,
                    capability: DeviceCapability::PointerClick,
                    dispatch_grant_id: binding.grant_id,
                    terminal_state: HubOperationState::Completed,
                    evidence: ExecutionEvidence::VerifiedAgentResult,
                },
                4,
            )
            .unwrap();

        let valid = ledger.snapshot_for_restart();
        assert!(
            AuthoritativeOperationController::restore_after_restart(
                AdmissionLimits {
                    max_global_active: 1,
                    max_queued_per_device: 8,
                },
                valid.clone(),
            )
            .is_ok()
        );

        let mut missing_history = valid.clone();
        missing_history.auto_resolutions.clear();
        assert!(matches!(
            AuthoritativeOperationController::restore_after_restart(
                AdmissionLimits {
                    max_global_active: 1,
                    max_queued_per_device: 8,
                },
                missing_history,
            ),
            Err(ExecutionError::InvalidSnapshot)
        ));

        let mut mismatched_history = valid;
        mismatched_history.auto_resolutions[0]
            .dispatch_binding
            .grant_id = "grant_history_other".into();
        assert!(matches!(
            AuthoritativeOperationController::restore_after_restart(
                AdmissionLimits {
                    max_global_active: 1,
                    max_queued_per_device: 8,
                },
                mismatched_history,
            ),
            Err(ExecutionError::InvalidSnapshot)
        ));
    }

    #[test]
    fn reconciliation_evidence_ids_are_explicitly_bounded_below_transport_limits() {
        let base = AgentTerminalEvidence {
            operation: op("op-bounded", 2),
            capability_revision: 1,
            capability: DeviceCapability::Shell,
            dispatch_grant_id: "grant_bounded".into(),
            terminal_state: HubOperationState::Completed,
            evidence: ExecutionEvidence::VerifiedAgentResult,
        };
        assert!(base.validate().is_ok());

        let mut oversized_operation = base.clone();
        oversized_operation.operation.operation_id =
            "x".repeat(MAX_RECONCILIATION_OPERATION_ID_BYTES + 1);
        assert_eq!(
            oversized_operation.validate(),
            Err(ExecutionError::InvalidOperation)
        );

        let mut oversized_device = base;
        oversized_device.operation.device_id = "d".repeat(MAX_RECONCILIATION_DEVICE_ID_BYTES + 1);
        assert_eq!(
            oversized_device.validate(),
            Err(ExecutionError::InvalidOperation)
        );
    }

    #[test]
    fn schema_v3_remains_readable_but_cannot_claim_v4_reconciliation_state() {
        let mut v3_source = controller();
        v3_source
            .prepare_with_metadata(
                op("op-v3-readable", 3),
                alice(),
                DeviceCapability::Shell,
                OperationAdmissionMetadata {
                    audit: OperationAuditMetadata {
                        workflow_id: Some("wf-v3".into()),
                        workflow_step_id: None,
                        client_correlation_id: None,
                    },
                    request_fingerprint: None,
                    evidence_envelope: None,
                },
                1,
            )
            .unwrap();
        v3_source
            .mark_dispatched("op-v3-readable", &alice(), 3, 2)
            .unwrap();
        v3_source
            .finalize(
                "op-v3-readable",
                &alice(),
                3,
                HubOperationState::Completed,
                ExecutionEvidence::VerifiedAgentResult,
                3,
            )
            .unwrap();
        let mut v3 = v3_source.snapshot_for_restart();
        v3.schema_version = AUDIT_EXECUTION_SAFETY_SCHEMA_VERSION;
        v3.operations[0].receipt.as_mut().unwrap().schema_version =
            AUDIT_EXECUTION_SAFETY_SCHEMA_VERSION;
        assert!(
            AuthoritativeOperationController::restore_after_restart(
                AdmissionLimits {
                    max_global_active: 1,
                    max_queued_per_device: 8,
                },
                v3,
            )
            .is_ok()
        );

        let mut v4 = controller();
        v4.prepare(op("op-v4-only", 4), alice(), DeviceCapability::Shell, 10)
            .unwrap();
        v4.mark_dispatched_with_binding(
            "op-v4-only",
            &alice(),
            4,
            Some(OperationDispatchBinding::new(11, "grant-v4-only").unwrap()),
            11,
        )
        .unwrap();
        v4.mark_connection_lost("op-v4-only", 12).unwrap();
        assert!(matches!(
            v4.snapshot_for_restart_compatible_with(AUDIT_EXECUTION_SAFETY_SCHEMA_VERSION),
            Err(ExecutionError::InvalidSnapshot)
        ));
        let mut forged_v3 = v4.snapshot_for_restart();
        forged_v3.schema_version = AUDIT_EXECUTION_SAFETY_SCHEMA_VERSION;
        assert!(matches!(
            AuthoritativeOperationController::restore_after_restart(
                AdmissionLimits {
                    max_global_active: 1,
                    max_queued_per_device: 8,
                },
                forged_v3,
            ),
            Err(ExecutionError::InvalidSnapshot)
        ));
    }

    #[test]
    fn recovery_evidence_read_preserves_original_quarantine_and_fails_safe_on_loss() {
        let mut ledger = controller();
        ledger
            .prepare(
                op("op-ambiguous", 7),
                alice(),
                DeviceCapability::TypeText,
                1,
            )
            .unwrap();
        ledger
            .mark_dispatched("op-ambiguous", &alice(), 7, 2)
            .unwrap();
        ledger
            .mark_indeterminate(
                "op-ambiguous",
                &alice(),
                7,
                IndeterminateReason::BackendOutcomeUnproven,
                3,
            )
            .unwrap();

        assert_eq!(
            ledger.prepare(op("op-mutation", 7), alice(), DeviceCapability::TypeText, 4,),
            Err(ExecutionError::DeviceIndeterminate {
                operation_id: "op-ambiguous".into()
            })
        );
        assert!(matches!(
            ledger
                .prepare(
                    op("op-evidence", 7),
                    alice(),
                    DeviceCapability::ListWindows,
                    4,
                )
                .unwrap(),
            AdmissionDecision::StartNow(_)
        ));
        assert!(ledger.is_recovery_evidence_read("op-evidence"));
        ledger
            .mark_dispatched("op-evidence", &alice(), 7, 5)
            .unwrap();
        ledger.mark_connection_lost("op-evidence", 6).unwrap();

        assert_eq!(ledger.state("op-evidence"), Some(HubOperationState::Failed));
        let evidence_receipt = ledger.receipt("op-evidence").unwrap();
        assert_eq!(
            evidence_receipt.evidence,
            ExecutionEvidence::RecoveryReadInterrupted
        );
        assert_eq!(
            ledger.quarantine("desktop-a").unwrap().operation_id,
            "op-ambiguous"
        );

        let snapshot = ledger.snapshot_for_restart();
        let encoded = serde_json::to_string(&snapshot).unwrap();
        assert!(encoded.contains("recovery_evidence_read"));
        let restored = AuthoritativeOperationController::restore_after_restart(
            AdmissionLimits {
                max_global_active: 1,
                max_queued_per_device: 8,
            },
            snapshot,
        )
        .unwrap();
        assert_eq!(
            restored.quarantine("desktop-a").unwrap().operation_id,
            "op-ambiguous"
        );
        assert_eq!(
            restored.state("op-evidence"),
            Some(HubOperationState::Failed)
        );
    }

    #[test]
    fn successful_recovery_evidence_read_never_settles_original_quarantine() {
        let mut ledger = controller();
        ledger
            .prepare(
                op("op-original-ambiguity", 9),
                alice(),
                DeviceCapability::TypeText,
                1,
            )
            .unwrap();
        ledger
            .mark_dispatched("op-original-ambiguity", &alice(), 9, 2)
            .unwrap();
        ledger
            .mark_connection_lost("op-original-ambiguity", 3)
            .unwrap();

        ledger
            .prepare(
                op("op-list-windows-evidence", 9),
                alice(),
                DeviceCapability::ListWindows,
                4,
            )
            .unwrap();
        ledger
            .mark_dispatched("op-list-windows-evidence", &alice(), 9, 5)
            .unwrap();
        let (_, receipt) = ledger
            .finalize(
                "op-list-windows-evidence",
                &alice(),
                9,
                HubOperationState::Completed,
                ExecutionEvidence::VerifiedAgentResult,
                6,
            )
            .unwrap();
        assert_eq!(receipt.terminal_state, HubOperationState::Completed);
        assert_eq!(
            ledger.quarantine("desktop-a").unwrap().operation_id,
            "op-original-ambiguity"
        );
        assert_eq!(
            ledger.prepare(
                op("op-mutation-still-blocked", 9),
                alice(),
                DeviceCapability::PointerClick,
                7,
            ),
            Err(ExecutionError::DeviceIndeterminate {
                operation_id: "op-original-ambiguity".into()
            })
        );
    }

    #[test]
    fn recovery_evidence_read_is_explicitly_allowlisted_and_v8_only() {
        for capability in [
            DeviceCapability::ListApplications,
            DeviceCapability::ScreenGeometry,
            DeviceCapability::Screenshot,
            DeviceCapability::ReadFile,
            DeviceCapability::ListDirectory,
            DeviceCapability::ListWindows,
            DeviceCapability::InspectWindow,
            DeviceCapability::VerifyUiState,
            DeviceCapability::ClipboardRead,
            DeviceCapability::PointerPosition,
            DeviceCapability::CaptureRegion,
            DeviceCapability::BrowserInspect,
        ] {
            assert!(capability.is_recovery_evidence_read_only());
        }
        for capability in [
            DeviceCapability::TypeText,
            DeviceCapability::PointerClick,
            DeviceCapability::ExecuteProcess,
            DeviceCapability::Shell,
            DeviceCapability::LaunchApplication,
            DeviceCapability::TerminateApplication,
            DeviceCapability::ActivateWindow,
            DeviceCapability::ClipboardWrite,
            DeviceCapability::BrowserNavigate,
            DeviceCapability::BrowserType,
            DeviceCapability::BrowserDownload,
        ] {
            assert!(!capability.is_recovery_evidence_read_only());
        }

        let mut ledger = controller();
        ledger
            .prepare(
                op("op-ambiguous-v8", 7),
                alice(),
                DeviceCapability::TypeText,
                1,
            )
            .unwrap();
        ledger
            .mark_dispatched("op-ambiguous-v8", &alice(), 7, 2)
            .unwrap();
        ledger.mark_connection_lost("op-ambiguous-v8", 3).unwrap();
        ledger
            .prepare(
                op("op-evidence-v8", 7),
                alice(),
                DeviceCapability::ListWindows,
                4,
            )
            .unwrap();
        assert!(matches!(
            ledger.snapshot_for_restart_compatible_with(
                EVIDENCE_ENVELOPE_EXECUTION_SAFETY_SCHEMA_VERSION
            ),
            Err(ExecutionError::InvalidSnapshot)
        ));
    }

    #[test]
    fn text_input_evidence_envelope_is_keyed_payload_free_and_restart_durable() {
        let secret = b"0123456789abcdef0123456789abcdef";
        let sensitive = "low-entropy-sensitive-value\n";
        let command = DeviceCommand::TypeTextAdvanced {
            context_id: None,
            text: sensitive.into(),
            target: InputTarget::Window {
                process_id: 42,
                window_id: Some(77),
            },
            delivery: InputDeliveryMode::Foreground,
            delay_ms: 5,
        };
        let envelope = text_input_evidence_envelope(Some(secret), &command)
            .unwrap()
            .unwrap();
        let candidate = fingerprint_text_input_candidate(secret, sensitive).unwrap();
        assert_eq!(
            compare_request_fingerprint(envelope.fingerprint(), Some(&candidate)),
            RequestFingerprintComparison::SameRequest
        );
        let changed =
            fingerprint_text_input_candidate(secret, "low-entropy-sensitive-value").unwrap();
        assert_eq!(
            compare_request_fingerprint(envelope.fingerprint(), Some(&changed)),
            RequestFingerprintComparison::DifferentRequest
        );
        let serialized_envelope = serde_json::to_string(&envelope).unwrap();
        assert!(!serialized_envelope.contains(sensitive));
        assert!(!serialized_envelope.contains("low-entropy-sensitive-value"));
        match &envelope {
            OperationEvidenceEnvelope::TextInput {
                payload_bytes,
                payload_chars,
                line_count,
                ends_with_newline,
                separate_submit_requested,
                target,
                delivery,
                delay_ms,
                ..
            } => {
                assert_eq!(*payload_bytes, u32::try_from(sensitive.len()).unwrap());
                assert_eq!(
                    *payload_chars,
                    u32::try_from(sensitive.chars().count()).unwrap()
                );
                assert_eq!(*line_count, 2);
                assert!(*ends_with_newline);
                assert!(!*separate_submit_requested);
                assert_eq!(
                    target,
                    &TextInputTargetEvidence::Window {
                        process_id: 42,
                        window_id: Some(77),
                    }
                );
                assert_eq!(*delivery, Some(InputDeliveryMode::Foreground));
                assert_eq!(*delay_ms, Some(5));
            }
        }

        let mut ledger = controller();
        ledger
            .prepare_with_metadata(
                op("op-evidence-envelope", 7),
                alice(),
                DeviceCapability::TypeText,
                OperationAdmissionMetadata {
                    audit: OperationAuditMetadata::empty(),
                    request_fingerprint: None,
                    evidence_envelope: Some(envelope.clone()),
                },
                10,
            )
            .unwrap();
        ledger
            .mark_dispatched("op-evidence-envelope", &alice(), 7, 11)
            .unwrap();
        ledger
            .mark_connection_lost("op-evidence-envelope", 12)
            .unwrap();
        let snapshot = ledger.snapshot_for_restart();
        let serialized_snapshot = serde_json::to_string(&snapshot).unwrap();
        assert!(!serialized_snapshot.contains(sensitive));
        assert!(!serialized_snapshot.contains("low-entropy-sensitive-value"));
        assert!(matches!(
            ledger.snapshot_for_restart_compatible_with(
                PARTIAL_INPUT_EXECUTION_SAFETY_SCHEMA_VERSION
            ),
            Err(ExecutionError::InvalidSnapshot)
        ));
        let restored = AuthoritativeOperationController::restore_after_restart(
            AdmissionLimits {
                max_global_active: 1,
                max_queued_per_device: 8,
            },
            snapshot,
        )
        .unwrap();
        let inspection = restored.quarantine_inspections().unwrap().remove(0);
        assert_eq!(inspection.evidence_envelope, Some(envelope));
    }

    #[test]
    fn text_input_shape_envelope_exists_without_fingerprint_secret() {
        let envelope = text_input_evidence_envelope(
            None,
            &DeviceCommand::TypeText {
                text: "shape-only".into(),
            },
        )
        .unwrap()
        .unwrap();
        assert!(envelope.fingerprint().is_none());
        assert!(envelope.validate(DeviceCapability::TypeText).is_ok());
    }

    #[test]
    fn type_text_reconciliation_distinguishes_no_effect_uncommitted_and_committed() {
        fn indeterminate_type_text(operation_id: &str) -> AuthoritativeOperationController {
            let mut ledger = controller();
            ledger
                .prepare(op(operation_id, 4), alice(), DeviceCapability::TypeText, 1)
                .unwrap();
            ledger
                .mark_dispatched(operation_id, &alice(), 4, 2)
                .unwrap();
            ledger
                .mark_indeterminate(
                    operation_id,
                    &alice(),
                    4,
                    IndeterminateReason::ConnectionLost,
                    3,
                )
                .unwrap();
            ledger
        }

        let mut before_delivery = indeterminate_type_text("op-before-delivery");
        let (_, no_effect_receipt) = before_delivery
            .resolve_indeterminate(
                "op-before-delivery",
                alice(),
                IndeterminateResolution::ConfirmedNotExecuted,
                "independent evidence proved input was not delivered",
                4,
            )
            .unwrap();
        assert_eq!(
            no_effect_receipt.terminal_state,
            HubOperationState::Cancelled
        );

        let mut after_delivery = indeterminate_type_text("op-after-delivery");
        let (_, partial_receipt) = after_delivery
            .resolve_indeterminate(
                "op-after-delivery",
                alice(),
                IndeterminateResolution::ConfirmedEffectAppliedUncommitted,
                "independent evidence proved text was present without submit",
                4,
            )
            .unwrap();
        assert_eq!(partial_receipt.terminal_state, HubOperationState::Completed);
        assert_eq!(
            after_delivery.resolutions()[0].decision,
            IndeterminateResolution::ConfirmedEffectAppliedUncommitted
        );
        assert!(matches!(
            after_delivery
                .snapshot_for_restart_compatible_with(RETIREMENT_EXECUTION_SAFETY_SCHEMA_VERSION),
            Err(ExecutionError::InvalidSnapshot)
        ));
        let snapshot = after_delivery.snapshot_for_restart();
        let mut restored = AuthoritativeOperationController::restore_after_restart(
            AdmissionLimits {
                max_global_active: 1,
                max_queued_per_device: 8,
            },
            snapshot,
        )
        .unwrap();
        assert_eq!(
            restored.prepare(
                op("op-after-delivery", 5),
                alice(),
                DeviceCapability::TypeText,
                5,
            ),
            Err(ExecutionError::OperationReplay)
        );

        let mut after_submit = indeterminate_type_text("op-after-submit");
        let (_, committed_receipt) = after_submit
            .resolve_indeterminate(
                "op-after-submit",
                alice(),
                IndeterminateResolution::ConfirmedCompleted,
                "independent evidence proved submit committed the intended effect",
                4,
            )
            .unwrap();
        assert_eq!(
            committed_receipt.terminal_state,
            HubOperationState::Completed
        );
    }

    #[test]
    fn partial_effect_resolution_is_restricted_to_text_input_capabilities() {
        let mut ledger = controller();
        ledger
            .prepare(
                op("op-click", 4),
                alice(),
                DeviceCapability::PointerClick,
                1,
            )
            .unwrap();
        ledger.mark_dispatched("op-click", &alice(), 4, 2).unwrap();
        ledger
            .mark_indeterminate(
                "op-click",
                &alice(),
                4,
                IndeterminateReason::ConnectionLost,
                3,
            )
            .unwrap();

        assert_eq!(
            ledger.resolve_indeterminate(
                "op-click",
                alice(),
                IndeterminateResolution::ConfirmedEffectAppliedUncommitted,
                "not a valid commit model for pointer click",
                4,
            ),
            Err(ExecutionError::InvalidTransition)
        );
        assert_eq!(
            ledger.state("op-click"),
            Some(HubOperationState::Indeterminate)
        );
        assert!(ledger.quarantine("desktop-a").is_some());
    }

    proptest! {
        #[test]
        fn stale_owner_or_generation_can_never_finalize(
            stale_generation in 1_u64..100_u64,
            use_competing_owner in any::<bool>(),
        ) {
            prop_assume!(stale_generation != 7 || use_competing_owner);
            let mut ledger = controller();
            ledger
                .prepare(op("op-prop-fence", 7), alice(), DeviceCapability::Shell, 1)
                .unwrap();
            ledger
                .mark_dispatched("op-prop-fence", &alice(), 7, 2)
                .unwrap();
            let owner = if use_competing_owner { bob() } else { alice() };
            let result = ledger.finalize(
                "op-prop-fence",
                &owner,
                stale_generation,
                HubOperationState::Completed,
                ExecutionEvidence::VerifiedAgentResult,
                3,
            );
            prop_assert_eq!(result, Err(ExecutionError::OwnershipFenceMismatch));
            prop_assert_eq!(ledger.state("op-prop-fence"), Some(HubOperationState::Dispatched));
        }

        #[test]
        fn quarantine_blocks_arbitrary_competing_principals_and_generations(
            generation in 2_u64..100_u64,
            subject in "[a-z]{1,12}",
        ) {
            let mut ledger = controller();
            ledger
                .prepare(op("op-prop-ambiguous", 1), alice(), DeviceCapability::PointerDrag, 1)
                .unwrap();
            ledger
                .mark_dispatched("op-prop-ambiguous", &alice(), 1, 2)
                .unwrap();
            ledger
                .mark_indeterminate(
                    "op-prop-ambiguous",
                    &alice(),
                    1,
                    IndeterminateReason::ConnectionLost,
                    3,
                )
                .unwrap();

            let principal = OperationOwner::new("https://issuer", subject).unwrap();
            let result = ledger.prepare(
                op("op-prop-next", generation),
                principal,
                DeviceCapability::Shell,
                4,
            );
            prop_assert_eq!(
                result.unwrap_err(),
                ExecutionError::DeviceIndeterminate {
                    operation_id: "op-prop-ambiguous".into(),
                }
            );
            prop_assert_eq!(ledger.state("op-prop-ambiguous"), Some(HubOperationState::Indeterminate));
        }
    }
}
