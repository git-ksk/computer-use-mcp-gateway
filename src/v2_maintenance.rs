//! Local operator maintenance for durable V2 Hub safety state.
//! Authority-bearing mutation shares the Hub state lock; read-only quarantine
//! inspection only reads atomically committed checkpoints and takes no lock.
//! No network entrypoint is provided.

use crate::v2_execution_safety::{
    AuthoritativeOperationController, EXECUTION_SAFETY_SCHEMA_VERSION, ExecutionEvidence,
    ExecutionReceipt, OperationEvidenceEnvelope, OperationOwner, ReconciliationStatus,
    RequestFingerprintComparison, ResolutionRecord, RetirementAuthority, RetirementDisposition,
    RetirementPolicy, RetirementRecord, TextInputTargetEvidence, compare_request_fingerprint,
    fingerprint_process_request, fingerprint_shell_request, fingerprint_text_input_candidate,
    retirement_policy_for_capability,
};
use crate::v2_m0::{
    CapabilityClass, DeviceCapability, DeviceRegistrySnapshot, ProcessEnvVar, ProcessRequest,
    ShellRequest,
};
use crate::v2_m0_execution::{
    AdmissionLimits, ExecutionError, HubOperationState, IndeterminateResolution,
};
use crate::v2_m1_persistence::{
    AgentPersistentState, CheckpointStore, HubPersistentState, M1_STATE_SCHEMA_VERSION,
    PersistenceError,
};
use crate::v2_state_lock::{StateDirectoryLock, StateDirectoryLockError};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

const RESOLVER_ISSUER: &str = "cumg://local-maintenance";
const RESOLVER_SUBJECT: &str = "operator";
const MIN_RETIREMENT_SOURCE_EXECUTION_SCHEMA_VERSION: u16 = 4;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OfflineResolutionResult {
    pub receipt: ExecutionReceipt,
    pub resolution: ResolutionRecord,
    pub checkpoint: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OfflineRetirementResult {
    pub retirement: RetirementRecord,
    pub checkpoint: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct OperationEvidenceInspection {
    pub schema_version: u16,
    pub kind: String,
    pub fingerprint_present: bool,
    pub payload_bytes: u32,
    pub payload_chars: u32,
    pub line_count: u32,
    pub ends_with_newline: bool,
    pub separate_submit_requested: bool,
    pub target_kind: String,
    pub target_process_id: Option<u32>,
    pub target_window_id: Option<u64>,
    pub delivery: Option<String>,
    pub delay_ms: Option<u16>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct QuarantineInspection {
    pub blocking_operation_id: String,
    pub device_id: String,
    pub device_generation: u64,
    pub current_device_generation: Option<u64>,
    pub capability: String,
    pub workflow_id: Option<String>,
    pub workflow_step_id: Option<String>,
    pub client_correlation_id: Option<String>,
    pub request_fingerprint_present: bool,
    pub evidence_envelope: Option<OperationEvidenceInspection>,
    pub dispatch_binding_present: bool,
    pub semantic_operation_class: String,
    pub effect_class: String,
    pub target_class: String,
    pub effect_kind: String,
    pub verification_kind: String,
    pub dispatch_recorded: bool,
    pub prepared_at_ms: u64,
    pub dispatched_at_ms: Option<u64>,
    pub indeterminate_at_ms: u64,
    pub indeterminate_reason: String,
    pub evidence_class: Option<String>,
    pub evidence_status: String,
    pub reconciliation_status: String,
    pub recovery_disposition: String,
    pub manual_audit_required: bool,
    pub retry_safe: bool,
    pub execution_outcome: String,
    pub retirement_eligibility: String,
    pub retirement_policy: Option<String>,
    pub recommended_action: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct QuarantineRecoveryGuidance {
    pub confirmed_not_executed: String,
    pub confirmed_effect_applied_uncommitted: String,
    pub confirmed_completed: String,
    pub otherwise: String,
    pub replay_old_operation: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct QuarantineInspectionReport {
    pub quarantines: Vec<QuarantineInspection>,
    pub recovery_guidance: QuarantineRecoveryGuidance,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AutoResolutionInspection {
    pub operation_id: String,
    pub device_id: String,
    pub device_generation: u64,
    pub capability: String,
    pub terminal_state: String,
    pub evidence_class: String,
    pub reconciliation_status: String,
    pub dispatch_binding_present: bool,
    pub resolved_at_ms: u64,
    pub replayed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AutoResolutionInspectionReport {
    pub auto_resolved: Vec<AutoResolutionInspection>,
    pub retired_indeterminate: Vec<RetirementInspection>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReconciliationEvidenceSource {
    Available,
    Unavailable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentTerminalMarkerStatus {
    Present,
    Absent,
    Unavailable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentTerminalEvidenceStatus {
    ExactAuthoritative,
    PresentUnverifiable,
    Absent,
    Unavailable,
    Mismatch,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReconciliationEvidenceAuthority {
    AuthoritativeTerminalEvidence,
    LegacyNonAuthoritativeMarker,
    ObservationalCorrelationOnly,
    Missing,
    StateMismatch,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReconciliationEvidenceStatus {
    Sufficient,
    Insufficient,
    Unrecoverable,
    Mismatch,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReconciliationResolutionReadiness {
    ConfirmedCompletedSupported,
    ConfirmedNotExecutedSupported,
    AuthoritativeTerminalSettlementSupported,
    InsufficientEvidenceKeepQuarantine,
    UnrecoverableEvidenceGap,
    StateMismatchFailClosed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReconciliationSupportedDecision {
    ConfirmedCompleted,
    ConfirmedNotExecuted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReconciliationRecommendedAction {
    AuthorizedRecoverySupported,
    AwaitAuthoritativeSelfReconciliation,
    KeepQuarantine,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReconciliationAuditReason {
    AuthoritativeTerminalEvidenceAvailable,
    LegacyAgentTerminalMarkerOnly,
    ObservationalCorrelationOnly,
    MissingExactDispatchBinding,
    NoTerminalHubReceipt,
    AgentEvidenceSourceUnavailable,
    AgentTerminalEvidenceMissing,
    DeviceMismatch,
    GenerationMismatch,
    CapabilityMismatch,
    DispatchBindingMismatch,
    DuplicateAgentTerminalEvidence,
    LegacyHubExecutionSchema,
    HubReconciliationStateMismatch,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ReconciliationReadinessAudit {
    pub operation_id: String,
    pub hub_execution_schema_version: u16,
    pub device_id: String,
    pub device_generation: u64,
    pub capability: String,
    pub dispatch_recorded: bool,
    pub dispatch_binding_present: bool,
    pub hub_terminal_evidence: String,
    pub hub_reconciliation_status: String,
    pub agent_evidence_source: ReconciliationEvidenceSource,
    pub agent_device_match: Option<bool>,
    pub agent_replay_generation: Option<u64>,
    pub agent_terminal_marker: AgentTerminalMarkerStatus,
    pub agent_terminal_marker_authoritative: bool,
    pub agent_terminal_evidence: AgentTerminalEvidenceStatus,
    pub authoritative_terminal_state: Option<String>,
    pub authoritative_evidence_class: Option<String>,
    pub evidence_authority: ReconciliationEvidenceAuthority,
    pub evidence_status: ReconciliationEvidenceStatus,
    pub resolution_readiness: ReconciliationResolutionReadiness,
    pub supported_decisions: Vec<ReconciliationSupportedDecision>,
    pub manual_audit_required: bool,
    pub recommended_action: ReconciliationRecommendedAction,
    pub reasons: Vec<ReconciliationAuditReason>,
    pub replay_old_operation: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RetirementInspection {
    pub operation_id: String,
    pub device_id: String,
    pub device_generation: u64,
    pub authorized_device_generation: u64,
    pub capability: String,
    pub execution_outcome: String,
    pub operational_disposition: String,
    pub indeterminate_reason: String,
    pub prior_reconciliation_status: String,
    pub retirement_policy: String,
    pub authority: String,
    pub reason_present: bool,
    pub retired_at_ms: u64,
    pub replayed: bool,
    pub quarantine_active: bool,
}

/// Read the latest committed Hub checkpoint without taking recovery authority.
/// Checkpoint publication is append-only/atomic, so this inspection may run while
/// the Hub owns the state lock. The result intentionally excludes owner identity
/// and every raw command/browser/GUI/result payload.
pub fn inspect_quarantines_read_only(
    state_dir: &Path,
    device_id: Option<&str>,
) -> Result<QuarantineInspectionReport, MaintenanceError> {
    let checkpoint = CheckpointStore::new(state_dir.to_path_buf(), "hub")
        .map_err(MaintenanceError::Persistence)?;
    let state = checkpoint
        .load_latest::<HubPersistentState>()
        .map_err(MaintenanceError::Persistence)?;
    let registry_generations: BTreeMap<_, _> = state
        .registry
        .devices
        .iter()
        .map(|device| (device.device_id.clone(), device.generation))
        .collect();
    let limits = AdmissionLimits {
        max_global_active: 1,
        max_queued_per_device: 1,
    };
    let (_registry, execution) = state
        .restore(limits)
        .map_err(MaintenanceError::Persistence)?;
    let inspections = execution
        .quarantine_inspections()
        .map_err(MaintenanceError::Execution)?
        .into_iter()
        .filter(|inspection| device_id.is_none_or(|id| inspection.operation.device_id == id))
        .map(|inspection| {
            let capability = crate::v2_observability::capability_name(inspection.capability);
            let current_device_generation = registry_generations
                .get(&inspection.operation.device_id)
                .copied();
            let retirement_policy = retirement_policy_for_capability(inspection.capability);
            let retirement_eligibility = if retirement_policy.is_none() {
                "ineligible_policy"
            } else if !matches!(
                inspection.reconciliation_status,
                ReconciliationStatus::OperatorRequired
                    | ReconciliationStatus::UnrecoverableEvidenceGap
            ) {
                "await_reconciliation"
            } else if inspection.dispatched_at_ms.is_none() {
                "ineligible_missing_dispatch_record"
            } else if current_device_generation
                .is_none_or(|generation| generation <= inspection.operation.device_generation)
            {
                "requires_newer_generation"
            } else {
                "eligible"
            };
            let recommended_action = if retirement_eligibility == "eligible" {
                "retire_with_local_maintenance_authorization"
            } else {
                "keep_quarantine"
            };
            let evidence_envelope = inspection
                .evidence_envelope
                .as_ref()
                .map(operation_evidence_inspection);
            let request_fingerprint_present = inspection.request_fingerprint.is_some()
                || inspection
                    .evidence_envelope
                    .as_ref()
                    .and_then(OperationEvidenceEnvelope::fingerprint)
                    .is_some();
            QuarantineInspection {
                blocking_operation_id: inspection.operation.operation_id,
                device_id: inspection.operation.device_id,
                device_generation: inspection.operation.device_generation,
                current_device_generation,
                capability: capability.to_owned(),
                workflow_id: inspection.audit.workflow_id,
                workflow_step_id: inspection.audit.workflow_step_id,
                client_correlation_id: inspection.audit.client_correlation_id,
                request_fingerprint_present,
                evidence_envelope,
                dispatch_binding_present: inspection.dispatch_binding_present,
                semantic_operation_class: capability.to_owned(),
                effect_class: capability_effect_class(inspection.capability).to_owned(),
                target_class: capability_target_class(inspection.capability).to_owned(),
                effect_kind: capability_effect_kind(inspection.capability).to_owned(),
                verification_kind: capability_verification_kind(inspection.capability).to_owned(),
                dispatch_recorded: inspection.dispatched_at_ms.is_some(),
                prepared_at_ms: inspection.prepared_at_ms,
                dispatched_at_ms: inspection.dispatched_at_ms,
                indeterminate_at_ms: inspection.indeterminate_at_ms,
                indeterminate_reason: crate::v2_observability::indeterminate_reason_name(
                    inspection.indeterminate_reason,
                )
                .to_owned(),
                evidence_class: inspection.evidence.map(evidence_name).map(str::to_owned),
                evidence_status: evidence_status(inspection.evidence).to_owned(),
                reconciliation_status: reconciliation_status_name(inspection.reconciliation_status)
                    .to_owned(),
                recovery_disposition: reconciliation_disposition(inspection.reconciliation_status)
                    .to_owned(),
                manual_audit_required: matches!(
                    inspection.reconciliation_status,
                    ReconciliationStatus::OperatorRequired
                        | ReconciliationStatus::UnrecoverableEvidenceGap
                ),
                retry_safe: false,
                execution_outcome: "indeterminate".to_owned(),
                retirement_eligibility: retirement_eligibility.to_owned(),
                retirement_policy: retirement_policy
                    .map(retirement_policy_name)
                    .map(str::to_owned),
                recommended_action: recommended_action.to_owned(),
            }
        })
        .collect();
    Ok(QuarantineInspectionReport {
        quarantines: inspections,
        recovery_guidance: QuarantineRecoveryGuidance {
            confirmed_not_executed:
                "requires independent evidence that the side effect did not occur".into(),
            confirmed_effect_applied_uncommitted:
                "input capabilities only: requires independent evidence that input was applied but a distinct submit/commit action did not occur".into(),
            confirmed_completed:
                "requires independent evidence that the intended side effect completed or committed".into(),
            otherwise: "keep quarantine intact unless an eligible unknown-outcome retirement is explicitly authorized".into(),
            replay_old_operation: false,
        },
    })
}

pub fn inspect_auto_resolutions_read_only(
    state_dir: &Path,
    device_id: Option<&str>,
) -> Result<AutoResolutionInspectionReport, MaintenanceError> {
    let checkpoint = CheckpointStore::new(state_dir.to_path_buf(), "hub")
        .map_err(MaintenanceError::Persistence)?;
    let state = checkpoint
        .load_latest::<HubPersistentState>()
        .map_err(MaintenanceError::Persistence)?;
    let limits = AdmissionLimits {
        max_global_active: 1,
        max_queued_per_device: 1,
    };
    let (_registry, execution) = state
        .restore(limits)
        .map_err(MaintenanceError::Persistence)?;
    let auto_resolved = execution
        .auto_resolutions()
        .iter()
        .filter(|resolution| device_id.is_none_or(|id| resolution.operation.device_id == id))
        .map(|resolution| AutoResolutionInspection {
            operation_id: resolution.operation.operation_id.clone(),
            device_id: resolution.operation.device_id.clone(),
            device_generation: resolution.operation.device_generation,
            capability: crate::v2_observability::capability_name(resolution.capability).to_owned(),
            terminal_state: hub_operation_state_name(resolution.terminal_state).to_owned(),
            evidence_class: evidence_name(resolution.evidence).to_owned(),
            reconciliation_status: "auto_resolved".to_owned(),
            dispatch_binding_present: true,
            resolved_at_ms: resolution.resolved_at_ms,
            replayed: false,
        })
        .collect();
    let retired_indeterminate = execution
        .retirements()
        .iter()
        .filter(|retirement| device_id.is_none_or(|id| retirement.operation.device_id == id))
        .map(|retirement| RetirementInspection {
            operation_id: retirement.operation.operation_id.clone(),
            device_id: retirement.operation.device_id.clone(),
            device_generation: retirement.operation.device_generation,
            authorized_device_generation: retirement.authorized_device_generation,
            capability: crate::v2_observability::capability_name(retirement.capability).to_owned(),
            execution_outcome: "indeterminate".to_owned(),
            operational_disposition: retirement_disposition_name(retirement.disposition).to_owned(),
            indeterminate_reason: crate::v2_observability::indeterminate_reason_name(
                retirement.indeterminate_reason,
            )
            .to_owned(),
            prior_reconciliation_status: reconciliation_status_name(
                retirement.prior_reconciliation_status,
            )
            .to_owned(),
            retirement_policy: retirement_policy_name(retirement.policy).to_owned(),
            authority: retirement_authority_name(retirement.authority).to_owned(),
            reason_present: !retirement.reason.is_empty(),
            retired_at_ms: retirement.retired_at_ms,
            replayed: retirement.replayed,
            quarantine_active: false,
        })
        .collect();
    Ok(AutoResolutionInspectionReport {
        auto_resolved,
        retired_indeterminate,
    })
}

/// Audit durable Hub and Agent reconciliation evidence without taking recovery
/// authority or mutating either checkpoint. Hidden dispatch-fence values are
/// compared in memory and are never included in the returned report.
pub fn audit_reconciliation_read_only(
    state_dir: &Path,
    agent_state_dir: &Path,
    operation_id: &str,
) -> Result<ReconciliationReadinessAudit, MaintenanceError> {
    let hub_store = CheckpointStore::new(state_dir.to_path_buf(), "hub")
        .map_err(MaintenanceError::Persistence)?;
    let hub_state = hub_store
        .load_latest::<HubPersistentState>()
        .map_err(MaintenanceError::Persistence)?;
    let limits = AdmissionLimits {
        max_global_active: 1,
        max_queued_per_device: 1,
    };
    let (_registry, execution) = hub_state
        .clone()
        .restore(limits)
        .map_err(MaintenanceError::Persistence)?;
    let inspection = execution
        .quarantine_inspections()
        .map_err(MaintenanceError::Execution)?
        .into_iter()
        .find(|inspection| inspection.operation.operation_id == operation_id)
        .ok_or(MaintenanceError::AuditOperationNotQuarantined)?;
    let hub_record = hub_state
        .execution
        .operations
        .iter()
        .find(|record| record.operation.operation_id == operation_id)
        .ok_or(MaintenanceError::Execution(ExecutionError::InvalidSnapshot))?;

    let mut reasons = vec![ReconciliationAuditReason::NoTerminalHubReceipt];
    if hub_state.execution.schema_version < 4 {
        reasons.push(ReconciliationAuditReason::LegacyHubExecutionSchema);
    }
    if hub_record.dispatch_binding.is_none() {
        reasons.push(ReconciliationAuditReason::MissingExactDispatchBinding);
    }

    let mut report = ReconciliationReadinessAudit {
        operation_id: inspection.operation.operation_id.clone(),
        hub_execution_schema_version: hub_state.execution.schema_version,
        device_id: inspection.operation.device_id.clone(),
        device_generation: inspection.operation.device_generation,
        capability: crate::v2_observability::capability_name(inspection.capability).to_owned(),
        dispatch_recorded: inspection.dispatched_at_ms.is_some(),
        dispatch_binding_present: hub_record.dispatch_binding.is_some(),
        hub_terminal_evidence: "none".to_owned(),
        hub_reconciliation_status: reconciliation_status_name(inspection.reconciliation_status)
            .to_owned(),
        agent_evidence_source: ReconciliationEvidenceSource::Unavailable,
        agent_device_match: None,
        agent_replay_generation: None,
        agent_terminal_marker: AgentTerminalMarkerStatus::Unavailable,
        agent_terminal_marker_authoritative: false,
        agent_terminal_evidence: AgentTerminalEvidenceStatus::Unavailable,
        authoritative_terminal_state: None,
        authoritative_evidence_class: None,
        evidence_authority: ReconciliationEvidenceAuthority::Missing,
        evidence_status: ReconciliationEvidenceStatus::Insufficient,
        resolution_readiness: ReconciliationResolutionReadiness::InsufficientEvidenceKeepQuarantine,
        supported_decisions: Vec::new(),
        manual_audit_required: true,
        recommended_action: ReconciliationRecommendedAction::KeepQuarantine,
        reasons,
        replay_old_operation: false,
    };

    let agent_store = CheckpointStore::new(agent_state_dir.to_path_buf(), "agent")
        .map_err(MaintenanceError::Persistence)?;
    let agent_state = match agent_store.load_latest::<AgentPersistentState>() {
        Ok(state) => state,
        Err(error) if reconciliation_evidence_source_unavailable(&error) => {
            report
                .reasons
                .push(ReconciliationAuditReason::AgentEvidenceSourceUnavailable);
            report.evidence_status = ReconciliationEvidenceStatus::Unrecoverable;
            report.resolution_readiness =
                ReconciliationResolutionReadiness::UnrecoverableEvidenceGap;
            return Ok(report);
        }
        Err(error) => return Err(MaintenanceError::Persistence(error)),
    };
    agent_state
        .clone()
        .restore_with_terminal_evidence()
        .map_err(MaintenanceError::Persistence)?;

    report.agent_evidence_source = ReconciliationEvidenceSource::Available;
    report.agent_device_match = Some(agent_state.device_id == inspection.operation.device_id);
    report.agent_replay_generation = agent_state.execution.replay_generation;
    if report.agent_device_match != Some(true) {
        report
            .reasons
            .push(ReconciliationAuditReason::DeviceMismatch);
        fail_reconciliation_audit_closed(&mut report);
        return Ok(report);
    }

    let marker_present = agent_state
        .execution
        .terminal_operation_ids
        .iter()
        .any(|candidate| candidate == operation_id);
    report.agent_terminal_marker = if marker_present {
        AgentTerminalMarkerStatus::Present
    } else {
        AgentTerminalMarkerStatus::Absent
    };
    if marker_present
        && agent_state.execution.replay_generation != Some(inspection.operation.device_generation)
    {
        report
            .reasons
            .push(ReconciliationAuditReason::GenerationMismatch);
        fail_reconciliation_audit_closed(&mut report);
        return Ok(report);
    }

    let matching_evidence: Vec<_> = agent_state
        .terminal_evidence
        .iter()
        .filter(|candidate| candidate.operation.operation_id == operation_id)
        .collect();
    if matching_evidence.len() > 1 {
        report
            .reasons
            .push(ReconciliationAuditReason::DuplicateAgentTerminalEvidence);
        report.agent_terminal_evidence = AgentTerminalEvidenceStatus::Mismatch;
        fail_reconciliation_audit_closed(&mut report);
        return Ok(report);
    }

    if let Some(terminal) = matching_evidence.first().copied() {
        if terminal.operation.device_id != agent_state.device_id
            || terminal.operation.device_id != inspection.operation.device_id
        {
            report
                .reasons
                .push(ReconciliationAuditReason::DeviceMismatch);
            report.agent_terminal_evidence = AgentTerminalEvidenceStatus::Mismatch;
            fail_reconciliation_audit_closed(&mut report);
            return Ok(report);
        }
        if terminal.operation.device_generation != inspection.operation.device_generation {
            report
                .reasons
                .push(ReconciliationAuditReason::GenerationMismatch);
            report.agent_terminal_evidence = AgentTerminalEvidenceStatus::Mismatch;
            fail_reconciliation_audit_closed(&mut report);
            return Ok(report);
        }
        if terminal.capability != inspection.capability {
            report
                .reasons
                .push(ReconciliationAuditReason::CapabilityMismatch);
            report.agent_terminal_evidence = AgentTerminalEvidenceStatus::Mismatch;
            fail_reconciliation_audit_closed(&mut report);
            return Ok(report);
        }
        let Some(binding) = hub_record.dispatch_binding.as_ref() else {
            report.agent_terminal_evidence = AgentTerminalEvidenceStatus::PresentUnverifiable;
            report.evidence_authority = if marker_present {
                ReconciliationEvidenceAuthority::LegacyNonAuthoritativeMarker
            } else {
                ReconciliationEvidenceAuthority::Missing
            };
            report.evidence_status = ReconciliationEvidenceStatus::Insufficient;
            report.resolution_readiness =
                ReconciliationResolutionReadiness::InsufficientEvidenceKeepQuarantine;
            report.recommended_action = ReconciliationRecommendedAction::KeepQuarantine;
            return Ok(report);
        };
        if terminal.dispatch_binding() != *binding {
            report
                .reasons
                .push(ReconciliationAuditReason::DispatchBindingMismatch);
            report.agent_terminal_evidence = AgentTerminalEvidenceStatus::Mismatch;
            fail_reconciliation_audit_closed(&mut report);
            return Ok(report);
        }
        if inspection.reconciliation_status != ReconciliationStatus::AutoReconciling {
            report
                .reasons
                .push(ReconciliationAuditReason::HubReconciliationStateMismatch);
            report.agent_terminal_evidence = AgentTerminalEvidenceStatus::PresentUnverifiable;
            fail_reconciliation_audit_closed(&mut report);
            return Ok(report);
        }

        report.agent_terminal_evidence = AgentTerminalEvidenceStatus::ExactAuthoritative;
        report.evidence_authority = ReconciliationEvidenceAuthority::AuthoritativeTerminalEvidence;
        report.evidence_status = ReconciliationEvidenceStatus::Sufficient;
        report.authoritative_terminal_state =
            Some(hub_operation_state_name(terminal.terminal_state).to_owned());
        report.authoritative_evidence_class = Some(evidence_name(terminal.evidence).to_owned());
        report.manual_audit_required = false;
        report
            .reasons
            .push(ReconciliationAuditReason::AuthoritativeTerminalEvidenceAvailable);
        match (terminal.terminal_state, terminal.evidence) {
            (HubOperationState::Completed, ExecutionEvidence::VerifiedAgentResult) => {
                report.resolution_readiness =
                    ReconciliationResolutionReadiness::ConfirmedCompletedSupported;
                report
                    .supported_decisions
                    .push(ReconciliationSupportedDecision::ConfirmedCompleted);
                report.recommended_action =
                    ReconciliationRecommendedAction::AuthorizedRecoverySupported;
            }
            _ => {
                report.resolution_readiness =
                    ReconciliationResolutionReadiness::AuthoritativeTerminalSettlementSupported;
                report.recommended_action =
                    ReconciliationRecommendedAction::AwaitAuthoritativeSelfReconciliation;
            }
        }
        return Ok(report);
    }

    report.agent_terminal_evidence = AgentTerminalEvidenceStatus::Absent;
    report
        .reasons
        .push(ReconciliationAuditReason::AgentTerminalEvidenceMissing);
    if marker_present {
        report.evidence_authority = ReconciliationEvidenceAuthority::LegacyNonAuthoritativeMarker;
        report
            .reasons
            .push(ReconciliationAuditReason::LegacyAgentTerminalMarkerOnly);
    } else if inspection.request_fingerprint.is_some() || inspection.evidence_envelope.is_some() {
        report.evidence_authority = ReconciliationEvidenceAuthority::ObservationalCorrelationOnly;
        report
            .reasons
            .push(ReconciliationAuditReason::ObservationalCorrelationOnly);
    }
    if inspection.reconciliation_status == ReconciliationStatus::UnrecoverableEvidenceGap {
        report.evidence_status = ReconciliationEvidenceStatus::Unrecoverable;
        report.resolution_readiness = ReconciliationResolutionReadiness::UnrecoverableEvidenceGap;
    }
    Ok(report)
}

fn reconciliation_evidence_source_unavailable(error: &PersistenceError) -> bool {
    matches!(error, PersistenceError::NoCheckpoint)
        || matches!(
            error,
            PersistenceError::Io(io_error) if io_error.kind() == std::io::ErrorKind::NotFound
        )
}

fn fail_reconciliation_audit_closed(report: &mut ReconciliationReadinessAudit) {
    report.evidence_authority = ReconciliationEvidenceAuthority::StateMismatch;
    report.evidence_status = ReconciliationEvidenceStatus::Mismatch;
    report.resolution_readiness = ReconciliationResolutionReadiness::StateMismatchFailClosed;
    report.supported_decisions.clear();
    report.manual_audit_required = true;
    report.recommended_action = ReconciliationRecommendedAction::KeepQuarantine;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RequestComparisonReport {
    SameRequest,
    DifferentRequest,
    Unavailable,
}

pub fn compare_quarantined_request_read_only(
    state_dir: &Path,
    operation_id: &str,
    tool_name: &str,
    candidate_request: Value,
    fingerprint_secret: &[u8],
) -> Result<RequestComparisonReport, MaintenanceError> {
    let checkpoint = CheckpointStore::new(state_dir.to_path_buf(), "hub")
        .map_err(MaintenanceError::Persistence)?;
    let state = checkpoint
        .load_latest::<HubPersistentState>()
        .map_err(MaintenanceError::Persistence)?;
    let limits = AdmissionLimits {
        max_global_active: 1,
        max_queued_per_device: 1,
    };
    let (_registry, execution) = state
        .restore(limits)
        .map_err(MaintenanceError::Persistence)?;
    let Some(inspection) = execution
        .quarantine_inspections()
        .map_err(MaintenanceError::Execution)?
        .into_iter()
        .find(|inspection| inspection.operation.operation_id == operation_id)
    else {
        return Ok(RequestComparisonReport::Unavailable);
    };
    let candidate = match tool_name {
        "execute_process" => {
            let request = candidate_process_request(candidate_request)?;
            Some(
                fingerprint_process_request(fingerprint_secret, &request)
                    .map_err(MaintenanceError::Execution)?,
            )
        }
        "shell" => {
            let request = candidate_shell_request(candidate_request)?;
            Some(
                fingerprint_shell_request(fingerprint_secret, &request)
                    .map_err(MaintenanceError::Execution)?,
            )
        }
        "type_text" => {
            let text = candidate_type_text(candidate_request)?;
            Some(
                fingerprint_text_input_candidate(fingerprint_secret, &text)
                    .map_err(MaintenanceError::Execution)?,
            )
        }
        _ => return Ok(RequestComparisonReport::Unavailable),
    };
    let stored = inspection.request_fingerprint.as_ref().or_else(|| {
        inspection
            .evidence_envelope
            .as_ref()
            .and_then(OperationEvidenceEnvelope::fingerprint)
    });
    Ok(
        match compare_request_fingerprint(stored, candidate.as_ref()) {
            RequestFingerprintComparison::SameRequest => RequestComparisonReport::SameRequest,
            RequestFingerprintComparison::DifferentRequest => {
                RequestComparisonReport::DifferentRequest
            }
            RequestFingerprintComparison::Unavailable => RequestComparisonReport::Unavailable,
        },
    )
}

fn operation_evidence_inspection(
    envelope: &OperationEvidenceEnvelope,
) -> OperationEvidenceInspection {
    match envelope {
        OperationEvidenceEnvelope::TextInput {
            schema_version,
            fingerprint,
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
            let (target_kind, target_process_id, target_window_id) = match target {
                TextInputTargetEvidence::Unspecified => ("unspecified", None, None),
                TextInputTargetEvidence::Desktop => ("desktop", None, None),
                TextInputTargetEvidence::Window {
                    process_id,
                    window_id,
                } => ("window", Some(*process_id), *window_id),
                TextInputTargetEvidence::WindowPoint {
                    process_id,
                    window_id,
                } => ("window_point", Some(*process_id), Some(*window_id)),
                TextInputTargetEvidence::Element {
                    process_id,
                    window_id,
                } => ("element", Some(*process_id), Some(*window_id)),
            };
            OperationEvidenceInspection {
                schema_version: *schema_version,
                kind: "text_input".to_owned(),
                fingerprint_present: fingerprint.is_some(),
                payload_bytes: *payload_bytes,
                payload_chars: *payload_chars,
                line_count: *line_count,
                ends_with_newline: *ends_with_newline,
                separate_submit_requested: *separate_submit_requested,
                target_kind: target_kind.to_owned(),
                target_process_id,
                target_window_id,
                delivery: delivery.map(|value| match value {
                    crate::v2_m0::InputDeliveryMode::Background => "background".to_owned(),
                    crate::v2_m0::InputDeliveryMode::Foreground => "foreground".to_owned(),
                }),
                delay_ms: *delay_ms,
            }
        }
    }
}

fn capability_effect_class(capability: DeviceCapability) -> &'static str {
    if matches!(capability.class(), CapabilityClass::Observe) {
        "read_only"
    } else {
        "effectful"
    }
}

fn capability_target_class(capability: DeviceCapability) -> &'static str {
    match capability {
        DeviceCapability::ExecuteProcess | DeviceCapability::Shell => "process",
        DeviceCapability::ReadFile | DeviceCapability::ListDirectory => "filesystem",
        DeviceCapability::BrowserInspect
        | DeviceCapability::BrowserPrepare
        | DeviceCapability::BrowserNavigate
        | DeviceCapability::BrowserClick
        | DeviceCapability::BrowserType
        | DeviceCapability::BrowserDialog
        | DeviceCapability::BrowserPointer
        | DeviceCapability::BrowserUploadFile
        | DeviceCapability::BrowserDownload => "browser",
        DeviceCapability::Screenshot
        | DeviceCapability::PointerClick
        | DeviceCapability::PointerDrag
        | DeviceCapability::TypeText
        | DeviceCapability::ListApplications
        | DeviceCapability::ScreenGeometry
        | DeviceCapability::ListWindows
        | DeviceCapability::LaunchApplication
        | DeviceCapability::InspectWindow
        | DeviceCapability::VerifyUiState
        | DeviceCapability::TerminateApplication
        | DeviceCapability::ActivateWindow
        | DeviceCapability::SetWindowFrame
        | DeviceCapability::InvokeMenu
        | DeviceCapability::KeyboardInput
        | DeviceCapability::Scroll
        | DeviceCapability::ClipboardRead
        | DeviceCapability::ClipboardWrite
        | DeviceCapability::PointerPosition
        | DeviceCapability::MovePointer
        | DeviceCapability::SetUiValue
        | DeviceCapability::CaptureRegion
        | DeviceCapability::DesktopScope => "desktop",
    }
}

fn capability_effect_kind(capability: DeviceCapability) -> &'static str {
    if matches!(capability.class(), CapabilityClass::Observe) {
        return "observation";
    }
    match capability {
        DeviceCapability::ExecuteProcess | DeviceCapability::Shell => "execute",
        DeviceCapability::LaunchApplication => "launch",
        DeviceCapability::TerminateApplication => "terminate",
        DeviceCapability::BrowserNavigate => "navigate",
        DeviceCapability::BrowserUploadFile => "upload",
        DeviceCapability::BrowserDownload => "create",
        DeviceCapability::DesktopScope => "scope_expand",
        DeviceCapability::PointerClick
        | DeviceCapability::PointerDrag
        | DeviceCapability::TypeText
        | DeviceCapability::ActivateWindow
        | DeviceCapability::SetWindowFrame
        | DeviceCapability::InvokeMenu
        | DeviceCapability::KeyboardInput
        | DeviceCapability::Scroll
        | DeviceCapability::ClipboardWrite
        | DeviceCapability::MovePointer
        | DeviceCapability::SetUiValue
        | DeviceCapability::BrowserPrepare
        | DeviceCapability::BrowserClick
        | DeviceCapability::BrowserType
        | DeviceCapability::BrowserDialog
        | DeviceCapability::BrowserPointer => "modify",
        _ => "interact",
    }
}

fn capability_verification_kind(capability: DeviceCapability) -> &'static str {
    match capability {
        DeviceCapability::LaunchApplication
        | DeviceCapability::TerminateApplication
        | DeviceCapability::ActivateWindow
        | DeviceCapability::SetWindowFrame
        | DeviceCapability::InvokeMenu
        | DeviceCapability::KeyboardInput
        | DeviceCapability::Scroll
        | DeviceCapability::ClipboardWrite
        | DeviceCapability::MovePointer
        | DeviceCapability::SetUiValue
        | DeviceCapability::PointerClick
        | DeviceCapability::PointerDrag
        | DeviceCapability::TypeText => "application_state",
        DeviceCapability::BrowserPrepare
        | DeviceCapability::BrowserNavigate
        | DeviceCapability::BrowserClick
        | DeviceCapability::BrowserType
        | DeviceCapability::BrowserDialog
        | DeviceCapability::BrowserPointer
        | DeviceCapability::BrowserUploadFile => "browser_state",
        DeviceCapability::BrowserDownload => "filesystem_postcondition",
        // Arbitrary shell/process semantics are intentionally not inferred from text/argv.
        DeviceCapability::ExecuteProcess | DeviceCapability::Shell => "none",
        _ => "none",
    }
}

const fn reconciliation_status_name(status: ReconciliationStatus) -> &'static str {
    match status {
        ReconciliationStatus::AutoReconciling => "auto_reconciling",
        ReconciliationStatus::AutoResolved => "auto_resolved",
        ReconciliationStatus::OperatorRequired => "operator_required",
        ReconciliationStatus::UnrecoverableEvidenceGap => "unrecoverable_evidence_gap",
    }
}

const fn reconciliation_disposition(status: ReconciliationStatus) -> &'static str {
    match status {
        ReconciliationStatus::AutoReconciling => "await_authoritative_evidence",
        ReconciliationStatus::AutoResolved => "resolved_without_replay",
        ReconciliationStatus::OperatorRequired => "needs_reconciliation",
        ReconciliationStatus::UnrecoverableEvidenceGap => "needs_reconciliation",
    }
}

const fn retirement_policy_name(policy: RetirementPolicy) -> &'static str {
    match policy {
        RetirementPolicy::TransientUiInteractionV1 => "transient_ui_interaction_v1",
    }
}

const fn retirement_disposition_name(disposition: RetirementDisposition) -> &'static str {
    match disposition {
        RetirementDisposition::Retired => "retired",
        RetirementDisposition::CurrentStateAccepted => "current_state_accepted",
    }
}

const fn retirement_authority_name(authority: RetirementAuthority) -> &'static str {
    match authority {
        RetirementAuthority::LocalMaintenanceOperator => "local_maintenance_operator",
        RetirementAuthority::LocalUserPresence => "local_user_presence",
    }
}

const fn hub_operation_state_name(
    state: crate::v2_m0_execution::HubOperationState,
) -> &'static str {
    match state {
        crate::v2_m0_execution::HubOperationState::Queued => "queued",
        crate::v2_m0_execution::HubOperationState::ActiveNotDispatched => "active_not_dispatched",
        crate::v2_m0_execution::HubOperationState::Dispatched => "dispatched",
        crate::v2_m0_execution::HubOperationState::CancelRequested => "cancel_requested",
        crate::v2_m0_execution::HubOperationState::Completed => "completed",
        crate::v2_m0_execution::HubOperationState::Failed => "failed",
        crate::v2_m0_execution::HubOperationState::Cancelled => "cancelled",
        crate::v2_m0_execution::HubOperationState::Indeterminate => "indeterminate",
    }
}

fn evidence_status(evidence: Option<ExecutionEvidence>) -> &'static str {
    if evidence.is_some() {
        "available"
    } else {
        "insufficient"
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CandidateProcessArgs {
    program: String,
    #[serde(default)]
    args: Vec<String>,
    cwd: String,
    #[serde(default)]
    env: BTreeMap<String, String>,
    timeout_ms: u64,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CandidateTypeTextArgs {
    text: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CandidateShellArgs {
    command: String,
    cwd: String,
    #[serde(default)]
    env: BTreeMap<String, String>,
    timeout_ms: u64,
}

fn candidate_process_request(value: Value) -> Result<ProcessRequest, MaintenanceError> {
    let args: CandidateProcessArgs = serde_json::from_value(strip_audit_and_operation_id(value)?)
        .map_err(|_| MaintenanceError::InvalidCandidateRequest)?;
    Ok(ProcessRequest {
        program: args.program,
        args: args.args,
        cwd: args.cwd,
        env: env_map(args.env),
        timeout_ms: args.timeout_ms,
    })
}

fn candidate_type_text(value: Value) -> Result<String, MaintenanceError> {
    let args: CandidateTypeTextArgs = serde_json::from_value(strip_audit_and_operation_id(value)?)
        .map_err(|_| MaintenanceError::InvalidCandidateRequest)?;
    if args.text.is_empty() {
        return Err(MaintenanceError::InvalidCandidateRequest);
    }
    Ok(args.text)
}

fn candidate_shell_request(value: Value) -> Result<ShellRequest, MaintenanceError> {
    let args: CandidateShellArgs = serde_json::from_value(strip_audit_and_operation_id(value)?)
        .map_err(|_| MaintenanceError::InvalidCandidateRequest)?;
    Ok(ShellRequest {
        command: args.command,
        cwd: args.cwd,
        env: env_map(args.env),
        timeout_ms: args.timeout_ms,
    })
}

fn strip_audit_and_operation_id(mut value: Value) -> Result<Value, MaintenanceError> {
    let Value::Object(object) = &mut value else {
        return Err(MaintenanceError::InvalidCandidateRequest);
    };
    for key in [
        "operation_id",
        "workflow_id",
        "workflow_step_id",
        "client_correlation_id",
    ] {
        object.remove(key);
    }
    Ok(value)
}

fn env_map(env: BTreeMap<String, String>) -> Vec<ProcessEnvVar> {
    env.into_iter()
        .map(|(key, value)| ProcessEnvVar { key, value })
        .collect()
}

const fn evidence_name(evidence: ExecutionEvidence) -> &'static str {
    match evidence {
        ExecutionEvidence::VerifiedAgentResult => "verified_agent_result",
        ExecutionEvidence::VerifiedRemoteError => "verified_remote_error",
        ExecutionEvidence::ProvenProcessTermination => "proven_process_termination",
        ExecutionEvidence::CancelledBeforeDispatch => "cancelled_before_dispatch",
        ExecutionEvidence::OperatorResolution => "operator_resolution",
        ExecutionEvidence::RecoveryReadInterrupted => "recovery_read_interrupted",
    }
}

pub fn resolve_indeterminate_offline(
    state_dir: &Path,
    operation_id: &str,
    decision: IndeterminateResolution,
    evidence: impl Into<String>,
) -> Result<OfflineResolutionResult, MaintenanceError> {
    let now_ms = u64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| MaintenanceError::SystemClockBeforeEpoch)?
            .as_millis(),
    )
    .unwrap_or(u64::MAX);
    resolve_indeterminate_offline_at(state_dir, operation_id, decision, evidence.into(), now_ms)
}

fn resolve_indeterminate_offline_at(
    state_dir: &Path,
    operation_id: &str,
    decision: IndeterminateResolution,
    evidence: String,
    now_ms: u64,
) -> Result<OfflineResolutionResult, MaintenanceError> {
    let _state_lock =
        StateDirectoryLock::acquire(state_dir).map_err(MaintenanceError::StateLock)?;
    let checkpoint = CheckpointStore::new(state_dir.to_path_buf(), "hub")
        .map_err(MaintenanceError::Persistence)?;
    let state = checkpoint
        .load_latest::<HubPersistentState>()
        .map_err(MaintenanceError::Persistence)?;
    let source_state_schema = state.schema_version;
    let source_registry = state.registry.clone();
    let source_execution_schema = state.execution.schema_version;
    // Checkpoints are restart-normalized before serialization, so queue capacity
    // is irrelevant to this single offline transition. V2 remains single-active.
    let limits = AdmissionLimits {
        max_global_active: 1,
        max_queued_per_device: 1,
    };
    let (_registry, mut execution) = state
        .restore(limits)
        .map_err(MaintenanceError::Persistence)?;
    // Prove the restored state can still be represented by the source writer
    // contract before applying even the in-memory authority-bearing transition.
    // A second check below validates the post-resolution candidate before bytes
    // are published, protecting this invariant if resolution gains new fields.
    execution
        .snapshot_for_restart_compatible_with(source_execution_schema)
        .map_err(|_| MaintenanceError::PersistenceCompatibility {
            checkpoint_execution_schema: source_execution_schema,
            maintenance_execution_schema: EXECUTION_SAFETY_SCHEMA_VERSION,
        })?;
    let resolver = OperationOwner::new(RESOLVER_ISSUER, RESOLVER_SUBJECT)
        .map_err(MaintenanceError::Execution)?;
    let (_next, mut receipt) = execution
        .resolve_indeterminate(operation_id, resolver, decision, evidence, now_ms)
        .map_err(MaintenanceError::Execution)?;
    let resolution = execution
        .resolutions()
        .last()
        .cloned()
        .ok_or(MaintenanceError::MissingResolutionRecord)?;
    let candidate = compatible_checkpoint(
        source_state_schema,
        source_registry,
        source_execution_schema,
        &execution,
    )?;
    // Return the same receipt schema that is actually persisted. The CLI does
    // not expose the schema today, but keeping the in-memory result aligned with
    // durable evidence avoids a split audit contract for future callers.
    receipt.schema_version = source_execution_schema;
    // Validate the complete candidate through the current restore path before
    // publishing any bytes. This is deliberately non-destructive: a failed
    // compatibility/preflight check leaves the authoritative checkpoint intact.
    candidate
        .clone()
        .restore(limits)
        .map_err(MaintenanceError::Persistence)?;
    let checkpoint_path = checkpoint
        .save(&candidate)
        .map_err(MaintenanceError::Persistence)?;
    Ok(OfflineResolutionResult {
        receipt,
        resolution,
        checkpoint: checkpoint_path,
    })
}

pub fn retire_indeterminate_offline(
    state_dir: &Path,
    operation_id: &str,
    policy: RetirementPolicy,
    reason: impl Into<String>,
) -> Result<OfflineRetirementResult, MaintenanceError> {
    let now_ms = u64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| MaintenanceError::SystemClockBeforeEpoch)?
            .as_millis(),
    )
    .unwrap_or(u64::MAX);
    retire_indeterminate_offline_at(state_dir, operation_id, policy, reason.into(), now_ms)
}

fn retire_indeterminate_offline_at(
    state_dir: &Path,
    operation_id: &str,
    policy: RetirementPolicy,
    reason: String,
    now_ms: u64,
) -> Result<OfflineRetirementResult, MaintenanceError> {
    retire_indeterminate_offline_at_with_commit(
        state_dir,
        operation_id,
        policy,
        reason,
        now_ms,
        |checkpoint, candidate| checkpoint.save(candidate),
    )
}

fn retire_indeterminate_offline_at_with_commit<F>(
    state_dir: &Path,
    operation_id: &str,
    policy: RetirementPolicy,
    reason: String,
    now_ms: u64,
    commit: F,
) -> Result<OfflineRetirementResult, MaintenanceError>
where
    F: FnOnce(&CheckpointStore, &HubPersistentState) -> Result<PathBuf, PersistenceError>,
{
    let _state_lock =
        StateDirectoryLock::acquire(state_dir).map_err(MaintenanceError::StateLock)?;
    let checkpoint = CheckpointStore::new(state_dir.to_path_buf(), "hub")
        .map_err(MaintenanceError::Persistence)?;
    let state = checkpoint
        .load_latest::<HubPersistentState>()
        .map_err(MaintenanceError::Persistence)?;
    let source_state_schema = state.schema_version;
    let source_registry = state.registry.clone();
    let source_execution_schema = state.execution.schema_version;
    if source_state_schema != M1_STATE_SCHEMA_VERSION {
        return Err(MaintenanceError::RetirementRequiresCurrentStateSchema {
            checkpoint_state_schema: source_state_schema,
            maintenance_state_schema: M1_STATE_SCHEMA_VERSION,
        });
    }
    if source_execution_schema < MIN_RETIREMENT_SOURCE_EXECUTION_SCHEMA_VERSION {
        return Err(MaintenanceError::RetirementSchemaTooOld {
            checkpoint_execution_schema: source_execution_schema,
            minimum_execution_schema: MIN_RETIREMENT_SOURCE_EXECUTION_SCHEMA_VERSION,
        });
    }
    let limits = AdmissionLimits {
        max_global_active: 1,
        max_queued_per_device: 1,
    };
    let (_registry, mut execution) = state
        .restore(limits)
        .map_err(MaintenanceError::Persistence)?;
    // Preflight the input writer contract before performing the in-memory
    // authority-bearing transition. Retirement intentionally upgrades only the
    // nested execution-safety schema to v5 because older schemas cannot represent
    // an unknown-outcome tombstone truthfully.
    execution
        .snapshot_for_restart_compatible_with(source_execution_schema)
        .map_err(|_| MaintenanceError::PersistenceCompatibility {
            checkpoint_execution_schema: source_execution_schema,
            maintenance_execution_schema: EXECUTION_SAFETY_SCHEMA_VERSION,
        })?;
    let inspection = execution
        .quarantine_inspections()
        .map_err(MaintenanceError::Execution)?
        .into_iter()
        .find(|inspection| inspection.operation.operation_id == operation_id)
        .ok_or(MaintenanceError::Execution(
            ExecutionError::UnknownOperation,
        ))?;
    let current_device_generation = source_registry
        .devices
        .iter()
        .find(|device| device.device_id == inspection.operation.device_id)
        .map(|device| device.generation)
        .ok_or(MaintenanceError::RetirementDeviceMissing)?;
    let (_next, retirement) = execution
        .retire_indeterminate(
            operation_id,
            RetirementAuthority::LocalMaintenanceOperator,
            policy,
            reason,
            current_device_generation,
            now_ms,
        )
        .map_err(MaintenanceError::Execution)?;
    if execution
        .retirements()
        .last()
        .is_none_or(|record| record != &retirement)
    {
        return Err(MaintenanceError::MissingRetirementRecord);
    }
    let candidate = HubPersistentState {
        schema_version: source_state_schema,
        registry: source_registry,
        execution: execution.snapshot_for_restart(),
    };
    candidate
        .clone()
        .restore(limits)
        .map_err(MaintenanceError::Persistence)?;
    let checkpoint_path = commit(&checkpoint, &candidate).map_err(MaintenanceError::Persistence)?;
    Ok(OfflineRetirementResult {
        retirement,
        checkpoint: checkpoint_path,
    })
}

fn compatible_checkpoint(
    source_state_schema: u16,
    source_registry: DeviceRegistrySnapshot,
    source_execution_schema: u16,
    execution: &AuthoritativeOperationController,
) -> Result<HubPersistentState, MaintenanceError> {
    let execution = execution
        .snapshot_for_restart_compatible_with(source_execution_schema)
        .map_err(|_| MaintenanceError::PersistenceCompatibility {
            checkpoint_execution_schema: source_execution_schema,
            maintenance_execution_schema: EXECUTION_SAFETY_SCHEMA_VERSION,
        })?;
    Ok(HubPersistentState {
        // Maintenance changes only the execution-safety transition. Preserve the
        // outer/registry writer contract instead of performing an incidental
        // persisted-state migration while the intended Hub is offline.
        schema_version: source_state_schema,
        registry: source_registry,
        execution,
    })
}

#[derive(Debug)]
pub enum MaintenanceError {
    StateLock(StateDirectoryLockError),
    Persistence(PersistenceError),
    Execution(ExecutionError),
    MissingResolutionRecord,
    MissingRetirementRecord,
    RetirementDeviceMissing,
    RetirementSchemaTooOld {
        checkpoint_execution_schema: u16,
        minimum_execution_schema: u16,
    },
    RetirementRequiresCurrentStateSchema {
        checkpoint_state_schema: u16,
        maintenance_state_schema: u16,
    },
    InvalidCandidateRequest,
    AuditOperationNotQuarantined,
    PersistenceCompatibility {
        checkpoint_execution_schema: u16,
        maintenance_execution_schema: u16,
    },
    SystemClockBeforeEpoch,
}
impl fmt::Display for MaintenanceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::StateLock(error) => write!(f, "{error}"),
            Self::Persistence(error) => write!(f, "checkpoint maintenance failed: {error}"),
            Self::Execution(error) => write!(f, "quarantine transition rejected: {error}"),
            Self::MissingResolutionRecord => {
                f.write_str("quarantine resolution produced no audit record")
            }
            Self::MissingRetirementRecord => {
                f.write_str("quarantine retirement produced no durable audit record")
            }
            Self::RetirementDeviceMissing => {
                f.write_str("quarantine retirement device is missing from the durable registry")
            }
            Self::RetirementSchemaTooOld {
                checkpoint_execution_schema,
                minimum_execution_schema,
            } => write!(
                f,
                "quarantine retirement requires execution-safety schema >= {minimum_execution_schema}; checkpoint has schema {checkpoint_execution_schema}; upgrade and normalize the Hub checkpoint before retirement"
            ),
            Self::RetirementRequiresCurrentStateSchema {
                checkpoint_state_schema,
                maintenance_state_schema,
            } => write!(
                f,
                "quarantine retirement requires current Hub state schema {maintenance_state_schema}; checkpoint has schema {checkpoint_state_schema}; upgrade the Hub before retirement"
            ),
            Self::InvalidCandidateRequest => f.write_str(
                "candidate request does not match the supported shell/process comparison contract",
            ),
            Self::AuditOperationNotQuarantined => {
                f.write_str("reconciliation audit requires one exact quarantined operation")
            }
            Self::PersistenceCompatibility {
                checkpoint_execution_schema,
                maintenance_execution_schema,
            } => write!(
                f,
                "offline recovery persistence compatibility check failed: checkpoint execution schema {checkpoint_execution_schema} cannot represent maintenance schema {maintenance_execution_schema}; no checkpoint was written; use a version-paired Hub/v2_maint or upgrade the Hub before recovery"
            ),
            Self::SystemClockBeforeEpoch => f.write_str("system clock is before the Unix epoch"),
        }
    }
}
impl std::error::Error for MaintenanceError {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::v2_execution_safety::{
        AgentTerminalEvidence, AuthoritativeOperationController, ExecutionEvidence,
        OperationAdmissionMetadata, OperationAuditMetadata, OperationDispatchBinding,
        OperationOwner,
    };
    use crate::v2_m0::{
        DeviceCapability, DeviceIdentity, DeviceRegistry, GrantAuthority, GrantLedger,
    };
    use crate::v2_m0_execution::{
        AdmissionDecision, AgentExecutionGate, AgentExecutionSnapshot, HubOperationState,
        OperationRef,
    };
    use crate::v2_m0_transport::HubIdentity;
    use crate::v2_m0_trust::TrustedHubIdentity;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    fn test_dir(name: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "cumg-v2-maint-{name}-{}-{}",
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

    fn checkpoint_count(state_dir: &Path, prefix: &str) -> usize {
        std::fs::read_dir(state_dir)
            .into_iter()
            .flatten()
            .filter_map(Result::ok)
            .filter(|entry| {
                entry.file_name().to_str().is_some_and(|name| {
                    name.starts_with(&format!("{prefix}-")) && name.ends_with(".json")
                })
            })
            .count()
    }

    fn seed_reconciliation_quarantine(
        state_dir: &Path,
        dispatch_binding: Option<OperationDispatchBinding>,
    ) -> (String, String) {
        let identity = DeviceIdentity::generate();
        let mut registry = DeviceRegistry::default();
        let device_id = registry.provision_trusted_device(identity.verifying_key());
        let owner = OperationOwner::new("https://issuer.example", "alice").unwrap();
        let operation_id = "op_reconciliation_audit".to_owned();
        let operation = OperationRef {
            device_id: device_id.clone(),
            device_generation: 1,
            operation_id: operation_id.clone(),
        };
        let mut execution = AuthoritativeOperationController::new(AdmissionLimits {
            max_global_active: 1,
            max_queued_per_device: 2,
        })
        .unwrap();
        execution
            .prepare(operation, owner.clone(), DeviceCapability::Scroll, 100)
            .unwrap();
        execution
            .mark_dispatched_with_binding(&operation_id, &owner, 1, dispatch_binding, 110)
            .unwrap();
        execution.mark_connection_lost(&operation_id, 120).unwrap();
        CheckpointStore::new(state_dir.to_path_buf(), "hub")
            .unwrap()
            .save(&HubPersistentState::capture(&registry, &execution))
            .unwrap();
        (device_id, operation_id)
    }

    fn seed_agent_reconciliation_state(
        state_dir: &Path,
        device_id: &str,
        replay_generation: u64,
        terminal_operation_ids: Vec<String>,
        terminal_evidence: Vec<AgentTerminalEvidence>,
    ) {
        let hub = HubIdentity::generate();
        let trusted_hub = TrustedHubIdentity::new(hub.verifier());
        let authority = GrantAuthority::generate();
        let grants = GrantLedger::new(authority.verifier());
        let execution = AgentExecutionGate::restore_after_restart(AgentExecutionSnapshot {
            replay_generation: Some(replay_generation),
            terminal_operation_ids,
        })
        .unwrap();
        let state = AgentPersistentState::capture_with_terminal_evidence(
            device_id,
            &trusted_hub,
            &grants,
            &execution,
            &terminal_evidence,
        )
        .unwrap();
        CheckpointStore::new(state_dir.to_path_buf(), "agent")
            .unwrap()
            .save(&state)
            .unwrap();
    }

    fn seed_quarantine(state_dir: &Path) -> (String, String) {
        let identity = DeviceIdentity::generate();
        let mut registry = DeviceRegistry::default();
        let device_id = registry.provision_trusted_device(identity.verifying_key());
        let owner = OperationOwner::new("https://issuer.example", "alice").unwrap();
        let operation_id = "op_offline_resolution".to_owned();
        let operation = OperationRef {
            device_id: device_id.clone(),
            device_generation: 1,
            operation_id: operation_id.clone(),
        };
        let limits = AdmissionLimits {
            max_global_active: 1,
            max_queued_per_device: 2,
        };
        let mut execution = AuthoritativeOperationController::new(limits).unwrap();
        assert!(matches!(
            execution
                .prepare(operation, owner.clone(), DeviceCapability::Shell, 100)
                .unwrap(),
            AdmissionDecision::StartNow(_)
        ));
        execution
            .mark_dispatched(&operation_id, &owner, 1, 110)
            .unwrap();
        execution.mark_connection_lost(&operation_id, 120).unwrap();
        CheckpointStore::new(state_dir.to_path_buf(), "hub")
            .unwrap()
            .save(&HubPersistentState::capture(&registry, &execution))
            .unwrap();
        (device_id, operation_id)
    }

    fn seed_retirable_quarantine(
        state_dir: &Path,
        capability: DeviceCapability,
        current_device_generation: u64,
    ) -> (String, String) {
        let identity = DeviceIdentity::generate();
        let mut registry = DeviceRegistry::default();
        let device_id = registry.provision_trusted_device(identity.verifying_key());
        let owner = OperationOwner::new("https://issuer.example", "alice").unwrap();
        let operation_id = "op_retirable_legacy".to_owned();
        let operation = OperationRef {
            device_id: device_id.clone(),
            device_generation: 1,
            operation_id: operation_id.clone(),
        };
        let mut execution = AuthoritativeOperationController::new(AdmissionLimits {
            max_global_active: 1,
            max_queued_per_device: 2,
        })
        .unwrap();
        execution
            .prepare(operation, owner.clone(), capability, 100)
            .unwrap();
        execution
            .mark_dispatched(&operation_id, &owner, 1, 110)
            .unwrap();
        execution
            .mark_indeterminate(
                &operation_id,
                &owner,
                1,
                crate::v2_execution_safety::IndeterminateReason::BackendOutcomeUnproven,
                120,
            )
            .unwrap();
        let mut registry_snapshot = registry.snapshot();
        registry_snapshot
            .devices
            .iter_mut()
            .find(|device| device.device_id == device_id)
            .unwrap()
            .generation = current_device_generation;
        let state = HubPersistentState {
            schema_version: M1_STATE_SCHEMA_VERSION,
            registry: registry_snapshot,
            execution: execution
                .snapshot_for_restart_compatible_with(
                    MIN_RETIREMENT_SOURCE_EXECUTION_SCHEMA_VERSION,
                )
                .unwrap(),
        };
        CheckpointStore::new(state_dir.to_path_buf(), "hub")
            .unwrap()
            .save(&state)
            .unwrap();
        (device_id, operation_id)
    }

    fn seed_text_input_quarantine(state_dir: &Path) -> (String, String, Vec<u8>, String) {
        let identity = DeviceIdentity::generate();
        let mut registry = DeviceRegistry::default();
        let device_id = registry.provision_trusted_device(identity.verifying_key());
        let owner = OperationOwner::new("https://issuer.example", "alice").unwrap();
        let operation_id = "op_text_evidence".to_owned();
        let operation = OperationRef {
            device_id: device_id.clone(),
            device_generation: 1,
            operation_id: operation_id.clone(),
        };
        let secret = b"0123456789abcdef0123456789abcdef".to_vec();
        let sensitive = "sensitive-short-secret\n".to_owned();
        let command = crate::v2_m0::DeviceCommand::TypeTextAdvanced {
            context_id: None,
            text: sensitive.clone(),
            target: crate::v2_m0::InputTarget::Window {
                process_id: 81,
                window_id: Some(91),
            },
            delivery: crate::v2_m0::InputDeliveryMode::Foreground,
            delay_ms: 7,
        };
        let envelope =
            crate::v2_execution_safety::text_input_evidence_envelope(Some(&secret), &command)
                .unwrap()
                .unwrap();
        let mut execution = AuthoritativeOperationController::new(AdmissionLimits {
            max_global_active: 1,
            max_queued_per_device: 2,
        })
        .unwrap();
        execution
            .prepare_with_metadata(
                operation,
                owner.clone(),
                DeviceCapability::TypeText,
                OperationAdmissionMetadata {
                    audit: OperationAuditMetadata::empty(),
                    request_fingerprint: None,
                    evidence_envelope: Some(envelope),
                },
                100,
            )
            .unwrap();
        execution
            .mark_dispatched(&operation_id, &owner, 1, 110)
            .unwrap();
        execution.mark_connection_lost(&operation_id, 120).unwrap();
        CheckpointStore::new(state_dir.to_path_buf(), "hub")
            .unwrap()
            .save(&HubPersistentState::capture(&registry, &execution))
            .unwrap();
        (device_id, operation_id, secret, sensitive)
    }

    fn seed_shape_only_text_input_quarantine(state_dir: &Path) -> String {
        let identity = DeviceIdentity::generate();
        let mut registry = DeviceRegistry::default();
        let device_id = registry.provision_trusted_device(identity.verifying_key());
        let owner = OperationOwner::new("https://issuer.example", "alice").unwrap();
        let operation_id = "op_text_shape_only".to_owned();
        let operation = OperationRef {
            device_id: device_id.clone(),
            device_generation: 1,
            operation_id: operation_id.clone(),
        };
        let command = crate::v2_m0::DeviceCommand::TypeText {
            text: "shape-only-sensitive".to_owned(),
        };
        let envelope = crate::v2_execution_safety::text_input_evidence_envelope(None, &command)
            .unwrap()
            .unwrap();
        let mut execution = AuthoritativeOperationController::new(AdmissionLimits {
            max_global_active: 1,
            max_queued_per_device: 2,
        })
        .unwrap();
        execution
            .prepare_with_metadata(
                operation,
                owner.clone(),
                DeviceCapability::TypeText,
                OperationAdmissionMetadata {
                    audit: OperationAuditMetadata::empty(),
                    request_fingerprint: None,
                    evidence_envelope: Some(envelope),
                },
                100,
            )
            .unwrap();
        execution
            .mark_dispatched(&operation_id, &owner, 1, 110)
            .unwrap();
        execution.mark_connection_lost(&operation_id, 120).unwrap();
        CheckpointStore::new(state_dir.to_path_buf(), "hub")
            .unwrap()
            .save(&HubPersistentState::capture(&registry, &execution))
            .unwrap();
        device_id
    }

    fn seed_correlated_shell_quarantine(
        state_dir: &Path,
    ) -> (
        String,
        String,
        Vec<u8>,
        crate::v2_execution_safety::OperationRequestFingerprint,
    ) {
        use crate::v2_execution_safety::{OperationAuditMetadata, fingerprint_shell_request};
        use crate::v2_m0::{ProcessEnvVar, ShellRequest};

        let identity = DeviceIdentity::generate();
        let mut registry = DeviceRegistry::default();
        let device_id = registry.provision_trusted_device(identity.verifying_key());
        let owner = OperationOwner::new("https://issuer.example", "alice").unwrap();
        let operation_id = "op_correlated_maintenance".to_owned();
        let secret = b"0123456789abcdef0123456789abcdef".to_vec();
        let request = ShellRequest {
            command: "printf raw-command-must-not-escape".into(),
            cwd: "/private/raw-cwd-must-not-escape".into(),
            env: vec![ProcessEnvVar {
                key: "RAW_SECRET".into(),
                value: "raw-env-must-not-escape".into(),
            }],
            timeout_ms: 2_000,
        };
        let fingerprint = fingerprint_shell_request(&secret, &request).unwrap();
        let operation = OperationRef {
            device_id: device_id.clone(),
            device_generation: 1,
            operation_id: operation_id.clone(),
        };
        let mut execution = AuthoritativeOperationController::new(AdmissionLimits {
            max_global_active: 1,
            max_queued_per_device: 2,
        })
        .unwrap();
        execution
            .prepare_with_metadata(
                operation,
                owner.clone(),
                DeviceCapability::Shell,
                OperationAdmissionMetadata {
                    audit: OperationAuditMetadata {
                        workflow_id: Some("wf_release_42".into()),
                        workflow_step_id: Some("step_package".into()),
                        client_correlation_id: Some("client_corr_7".into()),
                    },
                    request_fingerprint: Some(fingerprint.clone()),
                    evidence_envelope: None,
                },
                100,
            )
            .unwrap();
        execution
            .mark_dispatched(&operation_id, &owner, 1, 110)
            .unwrap();
        execution.mark_connection_lost(&operation_id, 120).unwrap();
        CheckpointStore::new(state_dir.to_path_buf(), "hub")
            .unwrap()
            .save(&HubPersistentState::capture(&registry, &execution))
            .unwrap();
        (device_id, operation_id, secret, fingerprint)
    }

    #[test]
    fn reconciliation_audit_schema_v3_hub_state_never_promotes_legacy_agent_marker() {
        let root = test_dir("reconciliation-schema-v3");
        let hub_dir = root.join("hub");
        let agent_dir = root.join("agent");
        let identity = DeviceIdentity::generate();
        let mut registry = DeviceRegistry::default();
        let device_id = registry.provision_trusted_device(identity.verifying_key());
        let owner = OperationOwner::new("https://issuer.example", "alice").unwrap();
        let operation_id = "op_schema_v3_legacy_audit".to_owned();
        let operation = OperationRef {
            device_id: device_id.clone(),
            device_generation: 1,
            operation_id: operation_id.clone(),
        };
        let mut execution = AuthoritativeOperationController::new(AdmissionLimits {
            max_global_active: 1,
            max_queued_per_device: 1,
        })
        .unwrap();
        execution
            .prepare(operation, owner.clone(), DeviceCapability::Scroll, 100)
            .unwrap();
        execution
            .mark_dispatched(&operation_id, &owner, 1, 110)
            .unwrap();
        execution.mark_connection_lost(&operation_id, 120).unwrap();
        let mut hub_state = HubPersistentState::capture(&registry, &execution);
        hub_state.execution.schema_version = 3;
        hub_state.execution.auto_resolutions.clear();
        hub_state.execution.retirements.clear();
        for record in &mut hub_state.execution.operations {
            record.dispatch_binding = None;
            record.reconciliation_status = None;
        }
        CheckpointStore::new(hub_dir.clone(), "hub")
            .unwrap()
            .save(&hub_state)
            .unwrap();
        seed_agent_reconciliation_state(
            &agent_dir,
            &device_id,
            1,
            vec![operation_id.clone()],
            Vec::new(),
        );

        let report = audit_reconciliation_read_only(&hub_dir, &agent_dir, &operation_id).unwrap();

        assert_eq!(
            report.resolution_readiness,
            ReconciliationResolutionReadiness::InsufficientEvidenceKeepQuarantine
        );
        assert_eq!(
            report.evidence_authority,
            ReconciliationEvidenceAuthority::LegacyNonAuthoritativeMarker
        );
        assert_eq!(report.hub_execution_schema_version, 3);
        assert!(!report.dispatch_binding_present);
        assert_eq!(report.hub_reconciliation_status, "operator_required");
        assert!(
            report
                .reasons
                .contains(&ReconciliationAuditReason::LegacyHubExecutionSchema)
        );
        assert!(report.supported_decisions.is_empty());
        assert!(
            report
                .reasons
                .contains(&ReconciliationAuditReason::MissingExactDispatchBinding)
        );
        assert!(
            report
                .reasons
                .contains(&ReconciliationAuditReason::LegacyAgentTerminalMarkerOnly)
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn reconciliation_audit_legacy_marker_only_keeps_quarantine() {
        let root = test_dir("reconciliation-legacy-marker");
        let hub_dir = root.join("hub");
        let agent_dir = root.join("agent");
        let (device_id, operation_id) = seed_reconciliation_quarantine(&hub_dir, None);
        seed_agent_reconciliation_state(
            &agent_dir,
            &device_id,
            1,
            vec![operation_id.clone()],
            Vec::new(),
        );
        let hub_before = checkpoint_count(&hub_dir, "hub");
        let agent_before = checkpoint_count(&agent_dir, "agent");

        let report = audit_reconciliation_read_only(&hub_dir, &agent_dir, &operation_id).unwrap();

        assert_eq!(
            report.resolution_readiness,
            ReconciliationResolutionReadiness::InsufficientEvidenceKeepQuarantine
        );
        assert_eq!(
            report.evidence_authority,
            ReconciliationEvidenceAuthority::LegacyNonAuthoritativeMarker
        );
        assert_eq!(
            report.agent_terminal_marker,
            AgentTerminalMarkerStatus::Present
        );
        assert!(!report.agent_terminal_marker_authoritative);
        assert_eq!(
            report.agent_terminal_evidence,
            AgentTerminalEvidenceStatus::Absent
        );
        assert_eq!(report.hub_reconciliation_status, "operator_required");
        assert!(!report.dispatch_binding_present);
        assert!(report.supported_decisions.is_empty());
        assert!(report.manual_audit_required);
        assert_eq!(
            report.recommended_action,
            ReconciliationRecommendedAction::KeepQuarantine
        );
        assert!(!report.replay_old_operation);
        assert!(
            report
                .reasons
                .contains(&ReconciliationAuditReason::MissingExactDispatchBinding)
        );
        assert!(
            report
                .reasons
                .contains(&ReconciliationAuditReason::LegacyAgentTerminalMarkerOnly)
        );
        assert_eq!(checkpoint_count(&hub_dir, "hub"), hub_before);
        assert_eq!(checkpoint_count(&agent_dir, "agent"), agent_before);

        let encoded = serde_json::to_string(&report).unwrap();
        assert!(!encoded.contains("https://issuer.example"));
        assert!(!encoded.contains("alice"));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn reconciliation_audit_exact_authoritative_evidence_supports_completed_without_mutation() {
        let root = test_dir("reconciliation-authoritative");
        let hub_dir = root.join("hub");
        let agent_dir = root.join("agent");
        let hidden_fence = "grant_private_reconciliation_fence";
        let binding = OperationDispatchBinding::new(9, hidden_fence).unwrap();
        let (device_id, operation_id) =
            seed_reconciliation_quarantine(&hub_dir, Some(binding.clone()));
        let evidence = AgentTerminalEvidence {
            operation: OperationRef {
                device_id: device_id.clone(),
                device_generation: 1,
                operation_id: operation_id.clone(),
            },
            capability_revision: binding.capability_revision,
            capability: DeviceCapability::Scroll,
            dispatch_grant_id: binding.grant_id.clone(),
            terminal_state: HubOperationState::Completed,
            evidence: ExecutionEvidence::VerifiedAgentResult,
        };
        seed_agent_reconciliation_state(
            &agent_dir,
            &device_id,
            1,
            vec![operation_id.clone()],
            vec![evidence],
        );
        let hub_before = checkpoint_count(&hub_dir, "hub");
        let agent_before = checkpoint_count(&agent_dir, "agent");

        let report = audit_reconciliation_read_only(&hub_dir, &agent_dir, &operation_id).unwrap();

        assert_eq!(
            report.resolution_readiness,
            ReconciliationResolutionReadiness::ConfirmedCompletedSupported
        );
        assert_eq!(
            report.evidence_authority,
            ReconciliationEvidenceAuthority::AuthoritativeTerminalEvidence
        );
        assert_eq!(
            report.evidence_status,
            ReconciliationEvidenceStatus::Sufficient
        );
        assert_eq!(
            report.agent_terminal_evidence,
            AgentTerminalEvidenceStatus::ExactAuthoritative
        );
        assert_eq!(
            report.authoritative_terminal_state.as_deref(),
            Some("completed")
        );
        assert_eq!(
            report.authoritative_evidence_class.as_deref(),
            Some("verified_agent_result")
        );
        assert_eq!(
            report.supported_decisions,
            vec![ReconciliationSupportedDecision::ConfirmedCompleted]
        );
        assert!(!report.manual_audit_required);
        assert_eq!(
            report.recommended_action,
            ReconciliationRecommendedAction::AuthorizedRecoverySupported
        );
        assert!(!report.replay_old_operation);
        assert_eq!(checkpoint_count(&hub_dir, "hub"), hub_before);
        assert_eq!(checkpoint_count(&agent_dir, "agent"), agent_before);

        let encoded = serde_json::to_string(&report).unwrap();
        assert!(!encoded.contains(hidden_fence));
        assert!(!encoded.contains("https://issuer.example"));
        assert!(!encoded.contains("alice"));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn reconciliation_audit_cross_state_mismatches_fail_closed() {
        let root = test_dir("reconciliation-mismatch");
        let hub_dir = root.join("hub");
        let hidden_fence = "grant_hidden_hub_fence";
        let binding = OperationDispatchBinding::new(11, hidden_fence).unwrap();
        let (device_id, operation_id) =
            seed_reconciliation_quarantine(&hub_dir, Some(binding.clone()));

        for case in ["device", "generation", "capability", "binding"] {
            let agent_dir = root.join(format!("agent-{case}"));
            let mut agent_device_id = device_id.clone();
            let mut replay_generation = 1;
            let mut evidence = AgentTerminalEvidence {
                operation: OperationRef {
                    device_id: device_id.clone(),
                    device_generation: 1,
                    operation_id: operation_id.clone(),
                },
                capability_revision: binding.capability_revision,
                capability: DeviceCapability::Scroll,
                dispatch_grant_id: binding.grant_id.clone(),
                terminal_state: HubOperationState::Completed,
                evidence: ExecutionEvidence::VerifiedAgentResult,
            };
            let expected_reason = match case {
                "device" => {
                    agent_device_id = "different-stable-device".to_owned();
                    evidence.operation.device_id = agent_device_id.clone();
                    ReconciliationAuditReason::DeviceMismatch
                }
                "generation" => {
                    replay_generation = 2;
                    evidence.operation.device_generation = 2;
                    ReconciliationAuditReason::GenerationMismatch
                }
                "capability" => {
                    evidence.capability = DeviceCapability::PointerClick;
                    ReconciliationAuditReason::CapabilityMismatch
                }
                "binding" => {
                    evidence.dispatch_grant_id = "grant_hidden_agent_mismatch".to_owned();
                    ReconciliationAuditReason::DispatchBindingMismatch
                }
                _ => unreachable!(),
            };
            seed_agent_reconciliation_state(
                &agent_dir,
                &agent_device_id,
                replay_generation,
                Vec::new(),
                vec![evidence],
            );

            let report =
                audit_reconciliation_read_only(&hub_dir, &agent_dir, &operation_id).unwrap();
            assert_eq!(
                report.resolution_readiness,
                ReconciliationResolutionReadiness::StateMismatchFailClosed,
                "case={case}"
            );
            assert_eq!(
                report.evidence_authority,
                ReconciliationEvidenceAuthority::StateMismatch,
                "case={case}"
            );
            assert_eq!(
                report.evidence_status,
                ReconciliationEvidenceStatus::Mismatch
            );
            assert!(report.supported_decisions.is_empty());
            assert_eq!(
                report.recommended_action,
                ReconciliationRecommendedAction::KeepQuarantine
            );
            assert!(report.reasons.contains(&expected_reason), "case={case}");
            let encoded = serde_json::to_string(&report).unwrap();
            assert!(!encoded.contains(hidden_fence));
            assert!(!encoded.contains("grant_hidden_agent_mismatch"));
        }
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn reconciliation_audit_structural_proof_is_not_authoritative_when_hub_state_rejects_it() {
        let root = test_dir("reconciliation-hub-status-mismatch");
        let hub_dir = root.join("hub");
        let agent_dir = root.join("agent");
        let binding = OperationDispatchBinding::new(13, "grant_hidden_status_fence").unwrap();
        let (device_id, operation_id) =
            seed_reconciliation_quarantine(&hub_dir, Some(binding.clone()));
        let hub_store = CheckpointStore::new(hub_dir.clone(), "hub").unwrap();
        let mut hub_state: HubPersistentState = hub_store.load_latest().unwrap();
        let record = hub_state
            .execution
            .operations
            .iter_mut()
            .find(|record| record.operation.operation_id == operation_id)
            .unwrap();
        record.reconciliation_status = Some(ReconciliationStatus::OperatorRequired);
        hub_store.save(&hub_state).unwrap();
        seed_agent_reconciliation_state(
            &agent_dir,
            &device_id,
            1,
            Vec::new(),
            vec![AgentTerminalEvidence {
                operation: OperationRef {
                    device_id: device_id.clone(),
                    device_generation: 1,
                    operation_id: operation_id.clone(),
                },
                capability_revision: binding.capability_revision,
                capability: DeviceCapability::Scroll,
                dispatch_grant_id: binding.grant_id.clone(),
                terminal_state: HubOperationState::Completed,
                evidence: ExecutionEvidence::VerifiedAgentResult,
            }],
        );

        let report = audit_reconciliation_read_only(&hub_dir, &agent_dir, &operation_id).unwrap();

        assert_eq!(
            report.resolution_readiness,
            ReconciliationResolutionReadiness::StateMismatchFailClosed
        );
        assert_eq!(
            report.evidence_authority,
            ReconciliationEvidenceAuthority::StateMismatch
        );
        assert_eq!(
            report.agent_terminal_evidence,
            AgentTerminalEvidenceStatus::PresentUnverifiable
        );
        assert!(report.supported_decisions.is_empty());
        assert!(
            report
                .reasons
                .contains(&ReconciliationAuditReason::HubReconciliationStateMismatch)
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn reconciliation_audit_missing_agent_checkpoint_is_unrecoverable_and_read_only() {
        let root = test_dir("reconciliation-agent-unavailable");
        let hub_dir = root.join("hub");
        let agent_dir = root.join("agent");
        std::fs::create_dir_all(&agent_dir).unwrap();
        #[cfg(unix)]
        std::fs::set_permissions(&agent_dir, std::fs::Permissions::from_mode(0o700)).unwrap();
        let binding = OperationDispatchBinding::new(3, "grant_unavailable_agent").unwrap();
        let (_device_id, operation_id) = seed_reconciliation_quarantine(&hub_dir, Some(binding));
        let hub_before = checkpoint_count(&hub_dir, "hub");

        let report = audit_reconciliation_read_only(&hub_dir, &agent_dir, &operation_id).unwrap();

        assert_eq!(
            report.agent_evidence_source,
            ReconciliationEvidenceSource::Unavailable
        );
        assert_eq!(
            report.resolution_readiness,
            ReconciliationResolutionReadiness::UnrecoverableEvidenceGap
        );
        assert_eq!(
            report.recommended_action,
            ReconciliationRecommendedAction::KeepQuarantine
        );
        assert!(
            report
                .reasons
                .contains(&ReconciliationAuditReason::AgentEvidenceSourceUnavailable)
        );
        assert_eq!(checkpoint_count(&hub_dir, "hub"), hub_before);
        assert_eq!(checkpoint_count(&agent_dir, "agent"), 0);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn read_only_quarantine_inspection_requires_no_hub_stop_and_mutates_nothing() {
        let dir = test_dir("inspect-live");
        let (device_id, operation_id) = seed_quarantine(&dir);
        let checkpoint_count = || {
            std::fs::read_dir(&dir)
                .unwrap()
                .filter_map(Result::ok)
                .filter(|entry| {
                    entry
                        .file_name()
                        .to_str()
                        .is_some_and(|name| name.starts_with("hub-") && name.ends_with(".json"))
                })
                .count()
        };
        let before_count = checkpoint_count();
        let _hub_lock = StateDirectoryLock::acquire(&dir).unwrap();

        let report = inspect_quarantines_read_only(&dir, None).unwrap();
        assert_eq!(report.quarantines.len(), 1);
        let inspection = &report.quarantines[0];
        assert_eq!(inspection.blocking_operation_id, operation_id);
        assert_eq!(inspection.device_id, device_id);
        assert_eq!(inspection.device_generation, 1);
        assert_eq!(inspection.capability, "shell");
        assert_eq!(inspection.semantic_operation_class, "shell");
        assert_eq!(inspection.effect_class, "effectful");
        assert_eq!(inspection.workflow_id, None);
        assert_eq!(inspection.workflow_step_id, None);
        assert_eq!(inspection.client_correlation_id, None);
        assert!(!inspection.request_fingerprint_present);
        assert_eq!(inspection.target_class, "process");
        assert_eq!(inspection.effect_kind, "execute");
        assert_eq!(inspection.verification_kind, "none");
        assert!(inspection.dispatch_recorded);
        assert_eq!(inspection.prepared_at_ms, 100);
        assert_eq!(inspection.dispatched_at_ms, Some(110));
        assert_eq!(inspection.indeterminate_at_ms, 120);
        assert_eq!(inspection.indeterminate_reason, "connection_lost");
        assert_eq!(inspection.evidence_class, None);
        assert_eq!(inspection.evidence_status, "insufficient");
        assert_eq!(inspection.reconciliation_status, "operator_required");
        assert_eq!(inspection.recovery_disposition, "needs_reconciliation");
        assert!(!inspection.retry_safe);
        assert!(!report.recovery_guidance.replay_old_operation);
        assert_eq!(checkpoint_count(), before_count);

        let filtered = inspect_quarantines_read_only(&dir, Some(&device_id)).unwrap();
        assert_eq!(filtered.quarantines, report.quarantines);
        let missing = inspect_quarantines_read_only(&dir, Some("device-not-present")).unwrap();
        assert!(missing.quarantines.is_empty());

        let serialized = serde_json::to_string(&report).unwrap();
        assert!(!serialized.contains("https://issuer.example"));
        assert!(!serialized.contains("alice"));
        assert!(!serialized.contains("owner"));
        assert_eq!(checkpoint_count(), before_count);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn text_input_evidence_is_private_inspectable_and_candidate_matchable() {
        let dir = test_dir("text-input-evidence");
        let (device_id, operation_id, secret, sensitive) = seed_text_input_quarantine(&dir);
        let report = inspect_quarantines_read_only(&dir, Some(&device_id)).unwrap();
        let inspection = &report.quarantines[0];
        assert!(inspection.request_fingerprint_present);
        let evidence = inspection.evidence_envelope.as_ref().unwrap();
        assert_eq!(evidence.kind, "text_input");
        assert_eq!(evidence.schema_version, 1);
        assert!(evidence.fingerprint_present);
        assert_eq!(evidence.line_count, 2);
        assert!(evidence.ends_with_newline);
        assert!(!evidence.separate_submit_requested);
        assert_eq!(evidence.target_kind, "window");
        assert_eq!(evidence.target_process_id, Some(81));
        assert_eq!(evidence.target_window_id, Some(91));
        assert_eq!(evidence.delivery.as_deref(), Some("foreground"));
        assert_eq!(evidence.delay_ms, Some(7));
        let serialized_report = serde_json::to_string(&report).unwrap();
        assert!(!serialized_report.contains(&sensitive));
        assert!(!serialized_report.contains("sensitive-short-secret"));

        let candidate = serde_json::json!({"text": sensitive});
        assert_eq!(
            compare_quarantined_request_read_only(
                &dir,
                &operation_id,
                "type_text",
                candidate.clone(),
                &secret,
            )
            .unwrap(),
            RequestComparisonReport::SameRequest
        );
        assert_eq!(
            compare_quarantined_request_read_only(
                &dir,
                &operation_id,
                "type_text",
                serde_json::json!({"text": "sensitive-short-secret"}),
                &secret,
            )
            .unwrap(),
            RequestComparisonReport::DifferentRequest
        );
        assert_eq!(
            compare_quarantined_request_read_only(
                &dir,
                &operation_id,
                "type_text",
                candidate,
                b"fedcba9876543210fedcba9876543210",
            )
            .unwrap(),
            RequestComparisonReport::Unavailable
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn text_input_shape_only_inspection_does_not_claim_fingerprint_presence() {
        let dir = test_dir("text-input-shape-only");
        let device_id = seed_shape_only_text_input_quarantine(&dir);
        let report = inspect_quarantines_read_only(&dir, Some(&device_id)).unwrap();
        let inspection = &report.quarantines[0];
        assert!(!inspection.request_fingerprint_present);
        let evidence = inspection.evidence_envelope.as_ref().unwrap();
        assert!(!evidence.fingerprint_present);
        assert_eq!(evidence.kind, "text_input");
        let serialized = serde_json::to_string(&report).unwrap();
        assert!(!serialized.contains("shape-only-sensitive"));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn correlated_inspection_and_request_comparison_are_private_read_only_evidence() {
        let dir = test_dir("correlated-inspection");
        let (device_id, operation_id, secret, fingerprint) = seed_correlated_shell_quarantine(&dir);
        let checkpoint_count = || {
            std::fs::read_dir(&dir)
                .unwrap()
                .filter_map(Result::ok)
                .filter(|entry| {
                    entry
                        .file_name()
                        .to_str()
                        .is_some_and(|name| name.starts_with("hub-") && name.ends_with(".json"))
                })
                .count()
        };
        let before = checkpoint_count();

        let report = inspect_quarantines_read_only(&dir, Some(&device_id)).unwrap();
        let inspection = &report.quarantines[0];
        assert_eq!(inspection.blocking_operation_id, operation_id);
        assert_eq!(inspection.workflow_id.as_deref(), Some("wf_release_42"));
        assert_eq!(inspection.workflow_step_id.as_deref(), Some("step_package"));
        assert_eq!(
            inspection.client_correlation_id.as_deref(),
            Some("client_corr_7")
        );
        assert!(inspection.request_fingerprint_present);
        assert_eq!(inspection.target_class, "process");
        assert_eq!(inspection.effect_kind, "execute");
        assert_eq!(inspection.verification_kind, "none");
        assert_eq!(inspection.reconciliation_status, "operator_required");
        assert!(!inspection.retry_safe);

        let serialized = serde_json::to_string(&report).unwrap();
        assert!(!serialized.contains("raw-command-must-not-escape"));
        assert!(!serialized.contains("raw-cwd-must-not-escape"));
        assert!(!serialized.contains("raw-env-must-not-escape"));
        assert!(!serialized.contains(&fingerprint.value));
        assert!(!serialized.contains(&fingerprint.key_id));
        assert!(!serialized.contains("https://issuer.example"));
        assert!(!serialized.contains("alice"));

        let candidate = serde_json::json!({
            "operation_id": "op_0123456789abcdef0123456789abcdef",
            "workflow_id": "wf_release_42",
            "workflow_step_id": "step_package",
            "client_correlation_id": "client_corr_7",
            "command": "printf raw-command-must-not-escape",
            "cwd": "/private/raw-cwd-must-not-escape",
            "env": {"RAW_SECRET": "raw-env-must-not-escape"},
            "timeout_ms": 2000
        });
        assert_eq!(
            compare_quarantined_request_read_only(
                &dir,
                &operation_id,
                "shell",
                candidate.clone(),
                &secret,
            )
            .unwrap(),
            RequestComparisonReport::SameRequest
        );
        let mut different = candidate.clone();
        different["timeout_ms"] = serde_json::json!(2001);
        assert_eq!(
            compare_quarantined_request_read_only(
                &dir,
                &operation_id,
                "shell",
                different,
                &secret,
            )
            .unwrap(),
            RequestComparisonReport::DifferentRequest
        );
        assert_eq!(
            compare_quarantined_request_read_only(
                &dir,
                &operation_id,
                "shell",
                candidate,
                b"fedcba9876543210fedcba9876543210",
            )
            .unwrap(),
            RequestComparisonReport::Unavailable
        );
        assert_eq!(checkpoint_count(), before);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn auto_resolution_history_is_private_bounded_and_read_only() {
        let dir = test_dir("auto-resolution-history");
        let identity = DeviceIdentity::generate();
        let mut registry = DeviceRegistry::default();
        let device_id = registry.provision_trusted_device(identity.verifying_key());
        let owner = OperationOwner::new("https://issuer.example", "alice").unwrap();
        let operation_id = "op_auto_resolution_history".to_owned();
        let operation = OperationRef {
            device_id: device_id.clone(),
            device_generation: 3,
            operation_id: operation_id.clone(),
        };
        let binding = OperationDispatchBinding::new(21, "grant_private_dispatch_fence").unwrap();
        let mut execution = AuthoritativeOperationController::new(AdmissionLimits {
            max_global_active: 1,
            max_queued_per_device: 2,
        })
        .unwrap();
        execution
            .prepare(operation.clone(), owner, DeviceCapability::Shell, 100)
            .unwrap();
        execution
            .mark_dispatched_with_binding(
                &operation_id,
                &OperationOwner::new("https://issuer.example", "alice").unwrap(),
                3,
                Some(binding.clone()),
                110,
            )
            .unwrap();
        execution.mark_connection_lost(&operation_id, 120).unwrap();
        execution
            .reconcile_authoritative_terminal(
                &AgentTerminalEvidence {
                    operation,
                    capability_revision: binding.capability_revision,
                    capability: DeviceCapability::Shell,
                    dispatch_grant_id: binding.grant_id.clone(),
                    terminal_state: HubOperationState::Completed,
                    evidence: ExecutionEvidence::VerifiedAgentResult,
                },
                130,
            )
            .unwrap();
        CheckpointStore::new(dir.clone(), "hub")
            .unwrap()
            .save(&HubPersistentState::capture(&registry, &execution))
            .unwrap();
        let checkpoint_count = std::fs::read_dir(&dir).unwrap().count();

        let report = inspect_auto_resolutions_read_only(&dir, Some(&device_id)).unwrap();
        assert_eq!(report.auto_resolved.len(), 1);
        let entry = &report.auto_resolved[0];
        assert_eq!(entry.operation_id, operation_id);
        assert_eq!(entry.reconciliation_status, "auto_resolved");
        assert!(entry.dispatch_binding_present);
        assert!(!entry.replayed);
        let serialized = serde_json::to_string(&report).unwrap();
        assert!(!serialized.contains("grant_private_dispatch_fence"));
        assert!(!serialized.contains("https://issuer.example"));
        assert!(!serialized.contains("alice"));
        assert!(!serialized.contains("owner"));
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), checkpoint_count);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn retirement_inspection_and_offline_transition_preserve_unknown_outcome_without_replay() {
        let dir = test_dir("retire-unknown");
        let (device_id, operation_id) =
            seed_retirable_quarantine(&dir, DeviceCapability::Scroll, 2);

        let before = inspect_quarantines_read_only(&dir, Some(&device_id)).unwrap();
        assert_eq!(before.quarantines.len(), 1);
        let inspection = &before.quarantines[0];
        assert_eq!(inspection.blocking_operation_id, operation_id);
        assert_eq!(inspection.device_generation, 1);
        assert_eq!(inspection.current_device_generation, Some(2));
        assert_eq!(inspection.capability, "scroll");
        assert_eq!(inspection.execution_outcome, "indeterminate");
        assert_eq!(inspection.retirement_eligibility, "eligible");
        assert_eq!(
            inspection.retirement_policy.as_deref(),
            Some("transient_ui_interaction_v1")
        );
        assert_eq!(
            inspection.recommended_action,
            "retire_with_local_maintenance_authorization"
        );

        let audit_reason =
            "legacy transient UI outcome permanently unknowable; retired without replay";
        let result = retire_indeterminate_offline_at(
            &dir,
            &operation_id,
            RetirementPolicy::TransientUiInteractionV1,
            audit_reason.into(),
            200,
        )
        .unwrap();
        assert_eq!(result.retirement.operation.operation_id, operation_id);
        assert_eq!(result.retirement.authorized_device_generation, 2);
        assert_eq!(
            result.retirement.outcome,
            crate::v2_execution_safety::RetirementOutcome::Unknown
        );
        assert!(!result.retirement.replayed);

        let latest = CheckpointStore::new(dir.clone(), "hub")
            .unwrap()
            .load_latest::<HubPersistentState>()
            .unwrap();
        assert_eq!(
            latest.execution.schema_version,
            EXECUTION_SAFETY_SCHEMA_VERSION
        );
        let (_registry, mut execution) = latest
            .restore(AdmissionLimits {
                max_global_active: 1,
                max_queued_per_device: 2,
            })
            .unwrap();
        assert!(execution.quarantine(&device_id).is_none());
        assert_eq!(
            execution.state(&operation_id),
            Some(HubOperationState::Indeterminate)
        );
        assert!(execution.receipt(&operation_id).is_none());
        assert_eq!(execution.retirements().len(), 1);
        assert_eq!(
            execution.prepare(
                OperationRef {
                    device_id: device_id.clone(),
                    device_generation: 2,
                    operation_id: operation_id.clone(),
                },
                OperationOwner::new("https://issuer.example", "alice").unwrap(),
                DeviceCapability::Scroll,
                210,
            ),
            Err(ExecutionError::OperationReplay)
        );
        assert!(
            execution
                .prepare(
                    OperationRef {
                        device_id: device_id.clone(),
                        device_generation: 2,
                        operation_id: "op_fresh_after_retirement".into(),
                    },
                    OperationOwner::new("https://issuer.example", "alice").unwrap(),
                    DeviceCapability::Scroll,
                    210,
                )
                .is_ok()
        );

        let after = inspect_quarantines_read_only(&dir, Some(&device_id)).unwrap();
        assert!(after.quarantines.is_empty());
        let history = inspect_auto_resolutions_read_only(&dir, Some(&device_id)).unwrap();
        assert_eq!(history.retired_indeterminate.len(), 1);
        let retired = &history.retired_indeterminate[0];
        assert_eq!(retired.operation_id, operation_id);
        assert_eq!(retired.execution_outcome, "indeterminate");
        assert_eq!(retired.operational_disposition, "retired");
        assert_eq!(retired.retirement_policy, "transient_ui_interaction_v1");
        assert_eq!(retired.authority, "local_maintenance_operator");
        assert!(retired.reason_present);
        assert!(!retired.replayed);
        assert!(!retired.quarantine_active);
        let serialized_history = serde_json::to_string(&history).unwrap();
        assert!(!serialized_history.contains(audit_reason));
        assert!(!serialized_history.contains("https://issuer.example"));
        assert!(!serialized_history.contains("alice"));

        let before_duplicate_count = std::fs::read_dir(&dir).unwrap().count();
        assert!(
            retire_indeterminate_offline_at(
                &dir,
                &operation_id,
                RetirementPolicy::TransientUiInteractionV1,
                "duplicate retirement".into(),
                220,
            )
            .is_err()
        );
        assert_eq!(
            std::fs::read_dir(&dir).unwrap().count(),
            before_duplicate_count
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn offline_retirement_requires_newer_generation_and_policy_eligible_capability() {
        let stale_dir = test_dir("retire-stale-generation");
        let (stale_device, stale_operation) =
            seed_retirable_quarantine(&stale_dir, DeviceCapability::Scroll, 1);
        let stale_inspection =
            inspect_quarantines_read_only(&stale_dir, Some(&stale_device)).unwrap();
        assert_eq!(
            stale_inspection.quarantines[0].retirement_eligibility,
            "requires_newer_generation"
        );
        let checkpoint_count = |dir: &Path| {
            std::fs::read_dir(dir)
                .unwrap()
                .filter_map(Result::ok)
                .filter(|entry| {
                    entry
                        .file_name()
                        .to_str()
                        .is_some_and(|name| name.starts_with("hub-") && name.ends_with(".json"))
                })
                .count()
        };
        let stale_count = checkpoint_count(&stale_dir);
        assert!(
            retire_indeterminate_offline_at(
                &stale_dir,
                &stale_operation,
                RetirementPolicy::TransientUiInteractionV1,
                "stale generation must fail closed".into(),
                200,
            )
            .is_err()
        );
        assert_eq!(checkpoint_count(&stale_dir), stale_count);
        assert_eq!(
            inspect_quarantines_read_only(&stale_dir, Some(&stale_device))
                .unwrap()
                .quarantines
                .len(),
            1
        );

        let shell_dir = test_dir("retire-shell-ineligible");
        let (shell_device, shell_operation) =
            seed_retirable_quarantine(&shell_dir, DeviceCapability::Shell, 2);
        let shell_inspection =
            inspect_quarantines_read_only(&shell_dir, Some(&shell_device)).unwrap();
        assert_eq!(
            shell_inspection.quarantines[0].retirement_eligibility,
            "ineligible_policy"
        );
        let shell_count = checkpoint_count(&shell_dir);
        assert!(
            retire_indeterminate_offline_at(
                &shell_dir,
                &shell_operation,
                RetirementPolicy::TransientUiInteractionV1,
                "dangerous operation must remain quarantined".into(),
                200,
            )
            .is_err()
        );
        assert_eq!(checkpoint_count(&shell_dir), shell_count);
        assert_eq!(
            inspect_quarantines_read_only(&shell_dir, Some(&shell_device))
                .unwrap()
                .quarantines
                .len(),
            1
        );
        let _ = std::fs::remove_dir_all(stale_dir);
        let _ = std::fs::remove_dir_all(shell_dir);
    }

    #[test]
    fn retirement_persistence_failure_leaves_last_committed_quarantine_authoritative() {
        let dir = test_dir("retire-persistence-failure");
        let (device_id, operation_id) =
            seed_retirable_quarantine(&dir, DeviceCapability::Scroll, 2);
        let checkpoint_count = || {
            std::fs::read_dir(&dir)
                .unwrap()
                .filter_map(Result::ok)
                .filter(|entry| {
                    entry
                        .file_name()
                        .to_str()
                        .is_some_and(|name| name.starts_with("hub-") && name.ends_with(".json"))
                })
                .count()
        };
        let before = checkpoint_count();
        let error = retire_indeterminate_offline_at_with_commit(
            &dir,
            &operation_id,
            RetirementPolicy::TransientUiInteractionV1,
            "candidate must not become authoritative when commit fails".into(),
            200,
            |_checkpoint, _candidate| Err(PersistenceError::CheckpointTooLarge),
        )
        .unwrap_err();
        assert!(matches!(
            error,
            MaintenanceError::Persistence(PersistenceError::CheckpointTooLarge)
        ));
        assert_eq!(checkpoint_count(), before);
        let inspection = inspect_quarantines_read_only(&dir, Some(&device_id)).unwrap();
        assert_eq!(inspection.quarantines.len(), 1);
        assert_eq!(
            inspection.quarantines[0].blocking_operation_id,
            operation_id
        );
        let history = inspect_auto_resolutions_read_only(&dir, Some(&device_id)).unwrap();
        assert!(history.retired_indeterminate.is_empty());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn offline_retirement_refuses_when_state_directory_is_owned() {
        let dir = test_dir("retire-busy");
        let (_, operation_id) = seed_retirable_quarantine(&dir, DeviceCapability::Scroll, 2);
        let _hub_lock = StateDirectoryLock::acquire(&dir).unwrap();
        assert!(matches!(
            retire_indeterminate_offline_at(
                &dir,
                &operation_id,
                RetirementPolicy::TransientUiInteractionV1,
                "must require exclusive maintenance authority".into(),
                200,
            ),
            Err(MaintenanceError::StateLock(StateDirectoryLockError::Busy))
        ));
    }

    #[test]
    fn offline_resolution_is_durable_and_audit_survives_pruning() {
        let dir = test_dir("durable");
        let (device_id, operation_id) = seed_quarantine(&dir);
        let result = resolve_indeterminate_offline_at(
            &dir,
            &operation_id,
            IndeterminateResolution::ConfirmedNotExecuted,
            "operator verified no side effect".into(),
            200,
        )
        .unwrap();
        assert_eq!(result.receipt.terminal_state, HubOperationState::Cancelled);
        assert_eq!(result.resolution.operation_id, operation_id);

        let latest = CheckpointStore::new(dir.clone(), "hub")
            .unwrap()
            .load_latest::<HubPersistentState>()
            .unwrap();
        let (registry, mut execution) = latest
            .restore(AdmissionLimits {
                max_global_active: 1,
                max_queued_per_device: 2,
            })
            .unwrap();
        assert!(execution.quarantine(&device_id).is_none());
        assert_eq!(
            execution.receipt(&operation_id).unwrap().terminal_state,
            HubOperationState::Cancelled
        );
        assert_eq!(execution.resolutions().len(), 1);
        execution
            .prune_terminal_before_generation(&device_id, 2)
            .unwrap();
        assert!(execution.receipt(&operation_id).is_none());
        assert_eq!(execution.resolutions().len(), 1);

        CheckpointStore::new(dir.clone(), "hub")
            .unwrap()
            .save(&HubPersistentState::capture(&registry, &execution))
            .unwrap();
        let reloaded = CheckpointStore::new(dir.clone(), "hub")
            .unwrap()
            .load_latest::<HubPersistentState>()
            .unwrap();
        let (_, execution) = reloaded
            .restore(AdmissionLimits {
                max_global_active: 1,
                max_queued_per_device: 2,
            })
            .unwrap();
        assert_eq!(execution.resolutions().len(), 1);
        assert_eq!(execution.resolutions()[0].operation_id, operation_id);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn offline_resolution_preserves_legacy_writer_contract_instead_of_upgrading_it() {
        let dir = test_dir("legacy-writer-contract");
        let (_, operation_id) = seed_quarantine(&dir);
        let store = CheckpointStore::new(dir.clone(), "hub").unwrap();
        let mut legacy = store.load_latest::<HubPersistentState>().unwrap();
        legacy.execution.schema_version = 1;
        for operation in &mut legacy.execution.operations {
            operation.audit = Default::default();
            operation.request_fingerprint = None;
            operation.dispatch_binding = None;
            operation.reconciliation_status = None;
            operation.recoverable_result = None;
        }
        legacy.execution.recoveries.clear();
        legacy.execution.auto_resolutions.clear();
        // Exercise preservation of the registry writer contract too. Maintenance
        // does not own a registry transition and must not incidentally migrate it.
        legacy.registry.schema_version = 5;
        let legacy_registry = legacy.registry.clone();
        store.save(&legacy).unwrap();
        let checkpoint_count = || {
            std::fs::read_dir(&dir)
                .unwrap()
                .filter_map(Result::ok)
                .filter(|entry| {
                    entry
                        .file_name()
                        .to_str()
                        .is_some_and(|name| name.starts_with("hub-") && name.ends_with(".json"))
                })
                .count()
        };
        let before_count = checkpoint_count();

        let result = resolve_indeterminate_offline_at(
            &dir,
            &operation_id,
            IndeterminateResolution::ConfirmedNotExecuted,
            "operator verified no side effect".into(),
            200,
        )
        .unwrap();
        assert_eq!(result.receipt.schema_version, 1);

        let after_count = checkpoint_count();
        assert_eq!(after_count, before_count + 1);
        let resolved = store.load_latest::<HubPersistentState>().unwrap();
        assert_eq!(resolved.schema_version, legacy.schema_version);
        assert_eq!(resolved.registry, legacy_registry);
        assert_eq!(resolved.execution.schema_version, 1);
        assert_eq!(
            resolved
                .execution
                .operations
                .iter()
                .find(|record| record.operation.operation_id == operation_id)
                .and_then(|record| record.receipt.as_ref())
                .unwrap()
                .schema_version,
            1
        );
        let (registry, execution) = resolved
            .restore(AdmissionLimits {
                max_global_active: 1,
                max_queued_per_device: 2,
            })
            .unwrap();
        let device_id = registry.snapshot().devices[0].device_id.clone();
        assert!(execution.quarantine(&device_id).is_none());
        assert_eq!(
            execution.receipt(&operation_id).unwrap().terminal_state,
            HubOperationState::Cancelled
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn incompatible_legacy_writer_contract_fails_before_checkpoint_publication() {
        use crate::v2_execution_safety::{
            EXECUTION_SAFETY_SCHEMA_VERSION, ExecutionEvidence, RecoverableOperationResult,
        };
        use crate::v2_m0::ProcessOutput;

        let dir = test_dir("legacy-writer-refusal");
        let identity = DeviceIdentity::generate();
        let mut registry = DeviceRegistry::default();
        let device_id = registry.provision_trusted_device(identity.verifying_key());
        let owner = OperationOwner::new("https://issuer.example", "alice").unwrap();
        let operation = OperationRef {
            device_id,
            device_generation: 1,
            operation_id: "op-v2-recovery-only".into(),
        };
        let mut execution = AuthoritativeOperationController::new(AdmissionLimits {
            max_global_active: 1,
            max_queued_per_device: 2,
        })
        .unwrap();
        execution
            .prepare(
                operation.clone(),
                owner.clone(),
                DeviceCapability::Shell,
                100,
            )
            .unwrap();
        execution
            .mark_dispatched(&operation.operation_id, &owner, 1, 110)
            .unwrap();
        execution
            .finalize(
                &operation.operation_id,
                &owner,
                1,
                HubOperationState::Completed,
                ExecutionEvidence::VerifiedAgentResult,
                120,
            )
            .unwrap();
        execution
            .attach_recoverable_result(
                &operation.operation_id,
                &owner,
                1,
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
        let store = CheckpointStore::new(dir.clone(), "hub").unwrap();
        store
            .save(&HubPersistentState::capture(&registry, &execution))
            .unwrap();
        let before_count = std::fs::read_dir(&dir).unwrap().count();

        let error = compatible_checkpoint(
            crate::v2_m1_persistence::M1_STATE_SCHEMA_VERSION,
            registry.snapshot(),
            1,
            &execution,
        )
        .unwrap_err();
        assert!(matches!(
            error,
            MaintenanceError::PersistenceCompatibility {
                checkpoint_execution_schema: 1,
                maintenance_execution_schema: EXECUTION_SAFETY_SCHEMA_VERSION,
            }
        ));
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), before_count);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn offline_resolution_refuses_when_state_directory_is_owned() {
        let dir = test_dir("busy");
        let (_, operation_id) = seed_quarantine(&dir);
        let _hub_lock = StateDirectoryLock::acquire(&dir).unwrap();
        assert!(matches!(
            resolve_indeterminate_offline_at(
                &dir,
                &operation_id,
                IndeterminateResolution::ConfirmedCompleted,
                "operator verified completion".into(),
                200,
            ),
            Err(MaintenanceError::StateLock(StateDirectoryLockError::Busy))
        ));
    }
}
