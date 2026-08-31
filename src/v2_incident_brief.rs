//! Read-only operator incident briefs above the authoritative V2 reconciliation audit.
//! Optional diagnostics are closed, observational-only inputs and never recovery authority.

use crate::mutation_authority::{MutationAuthorityRole, inspect_mutation_authority};
use crate::v2_maintenance::{
    MaintenanceError, QuarantineInspection, ReconciliationEvidenceSource,
    ReconciliationReadinessAudit, ReconciliationRecommendedAction, ReconciliationSupportedDecision,
    audit_reconciliation_read_only, inspect_quarantines_read_only,
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::path::Path;

pub const INCIDENT_BRIEF_SCHEMA_VERSION: u16 = 1;
pub const INCIDENT_DIAGNOSTICS_SCHEMA_VERSION: u16 = 1;
pub const MAX_INCIDENT_DIAGNOSTIC_OBSERVATIONS: usize = 16;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IncidentDiagnosticSource {
    MacosLaunchServices,
    MacosRunningBoard,
    Cua,
    CumgStructuredEvents,
}

impl IncidentDiagnosticSource {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::MacosLaunchServices => "macos_launchservices",
            Self::MacosRunningBoard => "macos_runningboard",
            Self::Cua => "cua",
            Self::CumgStructuredEvents => "cumg_structured_events",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IncidentDiagnosticRelation {
    BoundedAroundDispatch,
    AfterIndeterminate,
    CurrentState,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IncidentDiagnosticFinding {
    NoMatchingLaunchRecordObserved,
    MatchingLaunchRecordObserved,
    NoMatchingProcessStartObserved,
    MatchingProcessStartObserved,
    NoSuccessResponseObserved,
    SuccessResponseObserved,
    FailureResponseObserved,
    NoMatchingStructuredEventObserved,
    MatchingStructuredEventObserved,
    CollectorUnavailable,
}

impl IncidentDiagnosticFinding {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::NoMatchingLaunchRecordObserved => "no_matching_launch_record_observed",
            Self::MatchingLaunchRecordObserved => "matching_launch_record_observed",
            Self::NoMatchingProcessStartObserved => "no_matching_process_start_observed",
            Self::MatchingProcessStartObserved => "matching_process_start_observed",
            Self::NoSuccessResponseObserved => "no_success_response_observed",
            Self::SuccessResponseObserved => "success_response_observed",
            Self::FailureResponseObserved => "failure_response_observed",
            Self::NoMatchingStructuredEventObserved => "no_matching_structured_event_observed",
            Self::MatchingStructuredEventObserved => "matching_structured_event_observed",
            Self::CollectorUnavailable => "collector_unavailable",
        }
    }

    const fn absence_of_evidence(self) -> bool {
        matches!(
            self,
            Self::NoMatchingLaunchRecordObserved
                | Self::NoMatchingProcessStartObserved
                | Self::NoSuccessResponseObserved
                | Self::NoMatchingStructuredEventObserved
        )
    }

    const fn allowed_for(self, source: IncidentDiagnosticSource) -> bool {
        match source {
            IncidentDiagnosticSource::MacosLaunchServices => matches!(
                self,
                Self::NoMatchingLaunchRecordObserved
                    | Self::MatchingLaunchRecordObserved
                    | Self::CollectorUnavailable
            ),
            IncidentDiagnosticSource::MacosRunningBoard => matches!(
                self,
                Self::NoMatchingProcessStartObserved
                    | Self::MatchingProcessStartObserved
                    | Self::CollectorUnavailable
            ),
            IncidentDiagnosticSource::Cua => matches!(
                self,
                Self::NoSuccessResponseObserved
                    | Self::SuccessResponseObserved
                    | Self::FailureResponseObserved
                    | Self::CollectorUnavailable
            ),
            IncidentDiagnosticSource::CumgStructuredEvents => matches!(
                self,
                Self::NoMatchingStructuredEventObserved
                    | Self::MatchingStructuredEventObserved
                    | Self::CollectorUnavailable
            ),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IncidentDiagnosticObservationInput {
    pub source: IncidentDiagnosticSource,
    pub collected_at_ms: u64,
    pub relation: IncidentDiagnosticRelation,
    pub finding: IncidentDiagnosticFinding,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IncidentDiagnosticsDocument {
    pub schema_version: u16,
    pub observations: Vec<IncidentDiagnosticObservationInput>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum IncidentDiagnosticAuthority {
    ObservationalOnly,
    Unavailable,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct IncidentDiagnosticObservation {
    pub source: IncidentDiagnosticSource,
    pub collected_at_ms: u64,
    pub relation: IncidentDiagnosticRelation,
    pub authority: IncidentDiagnosticAuthority,
    pub finding: IncidentDiagnosticFinding,
    pub absence_of_evidence: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct IncidentOperationSummary {
    pub operation_id: String,
    pub capability: String,
    pub device_id: String,
    pub original_generation: u64,
    pub current_generation: Option<u64>,
    pub dispatch_recorded: bool,
    pub dispatched_at_ms: Option<u64>,
    pub indeterminate_at_ms: u64,
    pub indeterminate_reason: String,
    pub execution_outcome: String,
    pub retry_safe: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum IncidentContradictionKind {
    AuthoritativeVsObservational,
    ObservationalSourceConflict,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct IncidentContradiction {
    pub kind: IncidentContradictionKind,
    pub sources: Vec<IncidentDiagnosticSource>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub authoritative_terminal_state: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum IncidentKnownFact {
    DispatchRecorded,
    DispatchNotRecorded,
    AuthoritativeTerminalProofAvailable,
    NoAuthoritativeTerminalProof,
    OldOperationNotReplayed,
    AdditionalDiagnosticsAreNonAuthoritative,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum IncidentUnknownFact {
    WhetherSideEffectOccurred,
    AgentEvidenceUnavailable,
    MutationAuthorityUnavailable,
    DiagnosticConflictRequiresHumanReview,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum IncidentDecisionGuidance {
    KeepQuarantine,
    AwaitAuthoritativeSelfReconciliation,
    AuthenticatedHumanDecisionRequired,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct IncidentHumanSummary {
    pub known: Vec<IncidentKnownFact>,
    pub unknown: Vec<IncidentUnknownFact>,
    pub decision_guidance: IncidentDecisionGuidance,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum IncidentMutationAuthorityAvailability {
    Available,
    Unavailable,
    NotRequested,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct IncidentMutationAuthoritySummary {
    pub availability: IncidentMutationAuthorityAvailability,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub owner: Option<MutationAuthorityRole>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub epoch: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct IncidentBrief {
    pub schema_version: u16,
    pub operation: IncidentOperationSummary,
    /// Exact #133 audit: this remains the controlling CUMG authority view.
    pub cumg: ReconciliationReadinessAudit,
    pub mutation_authority: IncidentMutationAuthoritySummary,
    pub diagnostics: Vec<IncidentDiagnosticObservation>,
    pub contradictions: Vec<IncidentContradiction>,
    pub human_summary: IncidentHumanSummary,
}

#[derive(Debug)]
pub enum IncidentBriefError {
    Maintenance(MaintenanceError),
    InvalidDiagnostics,
    AuditInspectionMismatch,
}

impl fmt::Display for IncidentBriefError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Maintenance(error) => {
                write!(f, "incident brief maintenance read failed: {error}")
            }
            Self::InvalidDiagnostics => f.write_str(
                "incident diagnostics must match the bounded schema and source/finding allowlist",
            ),
            Self::AuditInspectionMismatch => {
                f.write_str("incident brief inputs disagree on the exact quarantined operation")
            }
        }
    }
}

impl std::error::Error for IncidentBriefError {}
impl From<MaintenanceError> for IncidentBriefError {
    fn from(value: MaintenanceError) -> Self {
        Self::Maintenance(value)
    }
}

pub fn parse_incident_diagnostics(
    json: &str,
) -> Result<Vec<IncidentDiagnosticObservation>, IncidentBriefError> {
    let document: IncidentDiagnosticsDocument =
        serde_json::from_str(json).map_err(|_| IncidentBriefError::InvalidDiagnostics)?;
    if document.schema_version != INCIDENT_DIAGNOSTICS_SCHEMA_VERSION
        || document.observations.len() > MAX_INCIDENT_DIAGNOSTIC_OBSERVATIONS
    {
        return Err(IncidentBriefError::InvalidDiagnostics);
    }
    document
        .observations
        .into_iter()
        .map(|input| {
            if input.collected_at_ms == 0 || !input.finding.allowed_for(input.source) {
                return Err(IncidentBriefError::InvalidDiagnostics);
            }
            let authority = if input.finding == IncidentDiagnosticFinding::CollectorUnavailable {
                IncidentDiagnosticAuthority::Unavailable
            } else {
                IncidentDiagnosticAuthority::ObservationalOnly
            };
            Ok(IncidentDiagnosticObservation {
                source: input.source,
                collected_at_ms: input.collected_at_ms,
                relation: input.relation,
                authority,
                finding: input.finding,
                absence_of_evidence: input.finding.absence_of_evidence(),
            })
        })
        .collect()
}

pub fn build_incident_brief_read_only(
    state_dir: &Path,
    agent_state_dir: &Path,
    operation_id: &str,
    mutation_authority_dir: Option<&Path>,
    diagnostics_json: Option<&str>,
) -> Result<IncidentBrief, IncidentBriefError> {
    let inspection_report = inspect_quarantines_read_only(state_dir, None)?;
    let inspection = inspection_report
        .quarantines
        .into_iter()
        .find(|candidate| candidate.blocking_operation_id == operation_id)
        .ok_or(MaintenanceError::AuditOperationNotQuarantined)?;
    let audit = audit_reconciliation_read_only(state_dir, agent_state_dir, operation_id)?;
    let diagnostics = diagnostics_json
        .map(parse_incident_diagnostics)
        .transpose()?
        .unwrap_or_default();
    let mut brief = compose_incident_brief(inspection, audit, diagnostics)?;
    brief.mutation_authority = mutation_authority_summary(mutation_authority_dir);
    if brief.mutation_authority.availability == IncidentMutationAuthorityAvailability::Unavailable {
        brief
            .human_summary
            .unknown
            .push(IncidentUnknownFact::MutationAuthorityUnavailable);
    }
    Ok(brief)
}

fn compose_incident_brief(
    inspection: QuarantineInspection,
    audit: ReconciliationReadinessAudit,
    diagnostics: Vec<IncidentDiagnosticObservation>,
) -> Result<IncidentBrief, IncidentBriefError> {
    if inspection.blocking_operation_id != audit.operation_id
        || inspection.device_id != audit.device_id
        || inspection.device_generation != audit.device_generation
        || inspection.capability != audit.capability
    {
        return Err(IncidentBriefError::AuditInspectionMismatch);
    }

    let contradictions = contradictions(&audit, &diagnostics);
    let human_summary = human_summary(&inspection, &audit, &diagnostics, &contradictions);
    let operation = IncidentOperationSummary {
        operation_id: inspection.blocking_operation_id.clone(),
        capability: inspection.capability.clone(),
        device_id: inspection.device_id.clone(),
        original_generation: inspection.device_generation,
        current_generation: inspection.current_device_generation,
        dispatch_recorded: inspection.dispatch_recorded,
        dispatched_at_ms: inspection.dispatched_at_ms,
        indeterminate_at_ms: inspection.indeterminate_at_ms,
        indeterminate_reason: inspection.indeterminate_reason.clone(),
        execution_outcome: inspection.execution_outcome.clone(),
        retry_safe: inspection.retry_safe,
    };

    Ok(IncidentBrief {
        schema_version: INCIDENT_BRIEF_SCHEMA_VERSION,
        operation,
        cumg: audit,
        mutation_authority: IncidentMutationAuthoritySummary {
            availability: IncidentMutationAuthorityAvailability::NotRequested,
            owner: None,
            epoch: None,
        },
        diagnostics,
        contradictions,
        human_summary,
    })
}

fn mutation_authority_summary(directory: Option<&Path>) -> IncidentMutationAuthoritySummary {
    let Some(directory) = directory else {
        return IncidentMutationAuthoritySummary {
            availability: IncidentMutationAuthorityAvailability::NotRequested,
            owner: None,
            epoch: None,
        };
    };
    match inspect_mutation_authority(directory) {
        Ok(status) => IncidentMutationAuthoritySummary {
            availability: IncidentMutationAuthorityAvailability::Available,
            owner: Some(status.owner),
            epoch: Some(status.epoch),
        },
        Err(_) => IncidentMutationAuthoritySummary {
            availability: IncidentMutationAuthorityAvailability::Unavailable,
            owner: None,
            epoch: None,
        },
    }
}

fn contradictions(
    audit: &ReconciliationReadinessAudit,
    diagnostics: &[IncidentDiagnosticObservation],
) -> Vec<IncidentContradiction> {
    let mut output = Vec::new();
    let cua_findings: BTreeSet<_> = diagnostics
        .iter()
        .filter(|item| item.source == IncidentDiagnosticSource::Cua)
        .filter(|item| item.authority == IncidentDiagnosticAuthority::ObservationalOnly)
        .map(|item| item.finding)
        .collect();
    if let Some(state) = audit.authoritative_terminal_state.as_deref() {
        let disagrees = if state == "completed" {
            cua_findings.contains(&IncidentDiagnosticFinding::FailureResponseObserved)
        } else {
            cua_findings.contains(&IncidentDiagnosticFinding::SuccessResponseObserved)
        };
        if disagrees {
            output.push(IncidentContradiction {
                kind: IncidentContradictionKind::AuthoritativeVsObservational,
                sources: vec![IncidentDiagnosticSource::Cua],
                authoritative_terminal_state: Some(state.to_owned()),
            });
        }
    }

    let mut by_source: BTreeMap<IncidentDiagnosticSource, BTreeSet<IncidentDiagnosticFinding>> =
        BTreeMap::new();
    for item in diagnostics
        .iter()
        .filter(|item| item.authority == IncidentDiagnosticAuthority::ObservationalOnly)
    {
        by_source
            .entry(item.source)
            .or_default()
            .insert(item.finding);
    }
    for (source, findings) in by_source {
        if source_has_conflict(source, &findings) {
            output.push(IncidentContradiction {
                kind: IncidentContradictionKind::ObservationalSourceConflict,
                sources: vec![source],
                authoritative_terminal_state: audit.authoritative_terminal_state.clone(),
            });
        }
    }
    output
}

fn source_has_conflict(
    source: IncidentDiagnosticSource,
    findings: &BTreeSet<IncidentDiagnosticFinding>,
) -> bool {
    let pair = |left, right| findings.contains(&left) && findings.contains(&right);
    match source {
        IncidentDiagnosticSource::MacosLaunchServices => pair(
            IncidentDiagnosticFinding::MatchingLaunchRecordObserved,
            IncidentDiagnosticFinding::NoMatchingLaunchRecordObserved,
        ),
        IncidentDiagnosticSource::MacosRunningBoard => pair(
            IncidentDiagnosticFinding::MatchingProcessStartObserved,
            IncidentDiagnosticFinding::NoMatchingProcessStartObserved,
        ),
        IncidentDiagnosticSource::Cua => {
            pair(
                IncidentDiagnosticFinding::SuccessResponseObserved,
                IncidentDiagnosticFinding::FailureResponseObserved,
            ) || pair(
                IncidentDiagnosticFinding::SuccessResponseObserved,
                IncidentDiagnosticFinding::NoSuccessResponseObserved,
            )
        }
        IncidentDiagnosticSource::CumgStructuredEvents => pair(
            IncidentDiagnosticFinding::MatchingStructuredEventObserved,
            IncidentDiagnosticFinding::NoMatchingStructuredEventObserved,
        ),
    }
}

fn human_summary(
    inspection: &QuarantineInspection,
    audit: &ReconciliationReadinessAudit,
    diagnostics: &[IncidentDiagnosticObservation],
    contradictions: &[IncidentContradiction],
) -> IncidentHumanSummary {
    let mut known = vec![if inspection.dispatch_recorded {
        IncidentKnownFact::DispatchRecorded
    } else {
        IncidentKnownFact::DispatchNotRecorded
    }];
    known.push(if audit.authoritative_terminal_state.is_some() {
        IncidentKnownFact::AuthoritativeTerminalProofAvailable
    } else {
        IncidentKnownFact::NoAuthoritativeTerminalProof
    });
    if !audit.replay_old_operation {
        known.push(IncidentKnownFact::OldOperationNotReplayed);
    }
    if diagnostics
        .iter()
        .any(|item| item.authority == IncidentDiagnosticAuthority::ObservationalOnly)
    {
        known.push(IncidentKnownFact::AdditionalDiagnosticsAreNonAuthoritative);
    }

    let mut unknown = Vec::new();
    if audit.authoritative_terminal_state.is_none() {
        unknown.push(IncidentUnknownFact::WhetherSideEffectOccurred);
    }
    if audit.agent_evidence_source == ReconciliationEvidenceSource::Unavailable {
        unknown.push(IncidentUnknownFact::AgentEvidenceUnavailable);
    }
    if !contradictions.is_empty() {
        unknown.push(IncidentUnknownFact::DiagnosticConflictRequiresHumanReview);
    }

    let decision_guidance = match audit.recommended_action {
        ReconciliationRecommendedAction::AuthorizedRecoverySupported
            if !audit.supported_decisions.is_empty() =>
        {
            IncidentDecisionGuidance::AuthenticatedHumanDecisionRequired
        }
        ReconciliationRecommendedAction::AwaitAuthoritativeSelfReconciliation => {
            IncidentDecisionGuidance::AwaitAuthoritativeSelfReconciliation
        }
        _ => IncidentDecisionGuidance::KeepQuarantine,
    };
    IncidentHumanSummary {
        known,
        unknown,
        decision_guidance,
    }
}

pub fn render_incident_brief_text(brief: &IncidentBrief) -> String {
    let mut output = format!(
        "Blocked operation\n  {} · {}\n\nCUMG verified\n",
        brief.operation.capability, brief.operation.operation_id
    );
    output.push_str(if brief.operation.dispatch_recorded {
        "  Dispatch recorded\n"
    } else {
        "  Dispatch not recorded\n"
    });
    if let Some(state) = brief.cumg.authoritative_terminal_state.as_deref() {
        output.push_str(&format!("  Authoritative terminal proof: {state}\n"));
    } else {
        output.push_str("  No authoritative terminal proof\n");
    }
    if !brief.cumg.replay_old_operation {
        output.push_str("  Old operation has not been replayed\n");
    }
    match brief.mutation_authority.availability {
        IncidentMutationAuthorityAvailability::Available => {
            let owner = brief
                .mutation_authority
                .owner
                .map(MutationAuthorityRole::as_str)
                .unwrap_or("unknown");
            let epoch = brief.mutation_authority.epoch.unwrap_or(0);
            output.push_str(&format!("  Mutation authority: {owner} epoch={epoch}\n"));
        }
        IncidentMutationAuthorityAvailability::Unavailable => {
            output.push_str("  Mutation authority: unavailable\n");
        }
        IncidentMutationAuthorityAvailability::NotRequested => {
            output.push_str("  Mutation authority: not_requested\n");
        }
    }

    output.push_str("\nAdditional audit observations\n");
    if brief.diagnostics.is_empty() {
        output.push_str("  None provided\n");
    } else {
        for item in &brief.diagnostics {
            let authority = match item.authority {
                IncidentDiagnosticAuthority::ObservationalOnly => "observational_only",
                IncidentDiagnosticAuthority::Unavailable => "unavailable",
            };
            output.push_str(&format!(
                "  {}: {} [{authority}]\n",
                item.source.as_str(),
                item.finding.as_str()
            ));
        }
    }
    if brief
        .diagnostics
        .iter()
        .any(|item| item.authority == IncidentDiagnosticAuthority::ObservationalOnly)
    {
        output.push_str("  Observational findings are not recovery authority\n");
    }

    output.push_str("\nUnknown\n");
    if brief.human_summary.unknown.is_empty() {
        output.push_str("  None identified by this bounded brief\n");
    } else {
        for item in &brief.human_summary.unknown {
            let line = match item {
                IncidentUnknownFact::WhetherSideEffectOccurred => {
                    "Whether the side effect occurred"
                }
                IncidentUnknownFact::AgentEvidenceUnavailable => {
                    "Agent durable evidence is unavailable"
                }
                IncidentUnknownFact::MutationAuthorityUnavailable => {
                    "Mutation authority inspection is unavailable"
                }
                IncidentUnknownFact::DiagnosticConflictRequiresHumanReview => {
                    "Diagnostic evidence conflicts and requires Human review"
                }
            };
            output.push_str(&format!("  {line}\n"));
        }
    }

    output.push_str("\nSafe actions\n");
    if brief.cumg.supported_decisions.is_empty() {
        match brief.cumg.recommended_action {
            ReconciliationRecommendedAction::AwaitAuthoritativeSelfReconciliation => {
                output.push_str("  Await authoritative self-reconciliation [supported]\n");
            }
            _ => output.push_str("  Keep quarantine [supported]\n"),
        }
    } else {
        for decision in &brief.cumg.supported_decisions {
            let label = match decision {
                ReconciliationSupportedDecision::ConfirmedCompleted => "Confirm completed",
                ReconciliationSupportedDecision::ConfirmedNotExecuted => "Confirm not executed",
            };
            output.push_str(&format!(
                "  {label} [CUMG-supported; Human/authenticated recovery required]\n"
            ));
        }
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::v2_maintenance::{
        AgentTerminalEvidenceStatus, AgentTerminalMarkerStatus, ReconciliationAuditReason,
        ReconciliationEvidenceAuthority, ReconciliationEvidenceStatus,
        ReconciliationResolutionReadiness,
    };

    fn inspection(capability: &str) -> QuarantineInspection {
        QuarantineInspection {
            blocking_operation_id: "op_incident".into(),
            device_id: "dev_incident".into(),
            device_generation: 7,
            current_device_generation: Some(9),
            capability: capability.into(),
            workflow_id: None,
            workflow_step_id: None,
            client_correlation_id: None,
            request_fingerprint_present: false,
            evidence_envelope: None,
            dispatch_binding_present: true,
            semantic_operation_class: capability.into(),
            effect_class: "effectful".into(),
            target_class: "application".into(),
            effect_kind: "application_launch".into(),
            verification_kind: "terminal_result".into(),
            dispatch_recorded: true,
            prepared_at_ms: 1_000,
            dispatched_at_ms: Some(1_010),
            indeterminate_at_ms: 1_020,
            indeterminate_reason: "backend_outcome_unproven".into(),
            evidence_class: None,
            evidence_status: "missing".into(),
            reconciliation_status: "operator_required".into(),
            recovery_disposition: "keep_quarantine".into(),
            manual_audit_required: true,
            retry_safe: false,
            execution_outcome: "indeterminate".into(),
            retirement_eligibility: "ineligible_policy".into(),
            retirement_policy: None,
            recommended_action: "keep_quarantine".into(),
        }
    }

    fn audit(capability: &str) -> ReconciliationReadinessAudit {
        ReconciliationReadinessAudit {
            operation_id: "op_incident".into(),
            hub_execution_schema_version: 8,
            device_id: "dev_incident".into(),
            device_generation: 7,
            capability: capability.into(),
            dispatch_recorded: true,
            dispatch_binding_present: true,
            hub_terminal_evidence: "none".into(),
            hub_reconciliation_status: "operator_required".into(),
            agent_evidence_source: ReconciliationEvidenceSource::Available,
            agent_device_match: Some(true),
            agent_replay_generation: Some(9),
            agent_terminal_marker: AgentTerminalMarkerStatus::Absent,
            agent_terminal_marker_authoritative: false,
            agent_terminal_evidence: AgentTerminalEvidenceStatus::Absent,
            authoritative_terminal_state: None,
            authoritative_evidence_class: None,
            evidence_authority: ReconciliationEvidenceAuthority::Missing,
            evidence_status: ReconciliationEvidenceStatus::Insufficient,
            resolution_readiness:
                ReconciliationResolutionReadiness::InsufficientEvidenceKeepQuarantine,
            supported_decisions: Vec::new(),
            manual_audit_required: true,
            recommended_action: ReconciliationRecommendedAction::KeepQuarantine,
            reasons: vec![ReconciliationAuditReason::AgentTerminalEvidenceMissing],
            replay_old_operation: false,
        }
    }

    fn diagnostic(
        source: IncidentDiagnosticSource,
        finding: IncidentDiagnosticFinding,
    ) -> IncidentDiagnosticObservation {
        IncidentDiagnosticObservation {
            source,
            collected_at_ms: 1_030,
            relation: IncidentDiagnosticRelation::BoundedAroundDispatch,
            authority: IncidentDiagnosticAuthority::ObservationalOnly,
            finding,
            absence_of_evidence: finding.absence_of_evidence(),
        }
    }

    #[test]
    fn legacy_launch_brief_keeps_observational_absence_non_authoritative() {
        let diagnostics = vec![
            diagnostic(
                IncidentDiagnosticSource::MacosLaunchServices,
                IncidentDiagnosticFinding::NoMatchingLaunchRecordObserved,
            ),
            diagnostic(
                IncidentDiagnosticSource::MacosRunningBoard,
                IncidentDiagnosticFinding::NoMatchingProcessStartObserved,
            ),
            diagnostic(
                IncidentDiagnosticSource::Cua,
                IncidentDiagnosticFinding::NoSuccessResponseObserved,
            ),
        ];
        let brief = compose_incident_brief(
            inspection("launch_application"),
            audit("launch_application"),
            diagnostics,
        )
        .unwrap();

        assert!(brief.cumg.supported_decisions.is_empty());
        assert_eq!(
            brief.human_summary.decision_guidance,
            IncidentDecisionGuidance::KeepQuarantine
        );
        assert!(brief.diagnostics.iter().all(|item| {
            item.authority == IncidentDiagnosticAuthority::ObservationalOnly
                && item.absence_of_evidence
        }));
        assert!(brief.contradictions.is_empty());
        assert!(
            brief
                .human_summary
                .unknown
                .contains(&IncidentUnknownFact::WhetherSideEffectOccurred)
        );
        let encoded = serde_json::to_string(&brief).unwrap();
        assert!(encoded.contains("observational_only"));
        assert!(!encoded.contains("confirmed_not_executed"));
    }

    #[test]
    fn authoritative_completion_controls_despite_contradictory_cua_observation() {
        let mut audit = audit("launch_application");
        audit.authoritative_terminal_state = Some("completed".into());
        audit.authoritative_evidence_class = Some("verified_agent_result".into());
        audit.evidence_authority = ReconciliationEvidenceAuthority::AuthoritativeTerminalEvidence;
        audit.evidence_status = ReconciliationEvidenceStatus::Sufficient;
        audit.resolution_readiness = ReconciliationResolutionReadiness::ConfirmedCompletedSupported;
        audit.supported_decisions = vec![ReconciliationSupportedDecision::ConfirmedCompleted];
        audit.manual_audit_required = false;
        audit.recommended_action = ReconciliationRecommendedAction::AuthorizedRecoverySupported;

        let brief = compose_incident_brief(
            inspection("launch_application"),
            audit,
            vec![diagnostic(
                IncidentDiagnosticSource::Cua,
                IncidentDiagnosticFinding::FailureResponseObserved,
            )],
        )
        .unwrap();

        assert_eq!(
            brief.cumg.evidence_authority,
            ReconciliationEvidenceAuthority::AuthoritativeTerminalEvidence
        );
        assert_eq!(
            brief.cumg.supported_decisions,
            vec![ReconciliationSupportedDecision::ConfirmedCompleted]
        );
        assert_eq!(brief.contradictions.len(), 1);
        assert_eq!(
            brief.contradictions[0].kind,
            IncidentContradictionKind::AuthoritativeVsObservational
        );
        assert_eq!(
            brief.human_summary.decision_guidance,
            IncidentDecisionGuidance::AuthenticatedHumanDecisionRequired
        );
    }

    #[test]
    fn diagnostics_input_cannot_claim_authority_or_use_arbitrary_fields() {
        let json = r#"{
            "schema_version": 1,
            "observations": [{
                "source": "cua",
                "collected_at_ms": 1234,
                "relation": "bounded_around_dispatch",
                "finding": "no_success_response_observed",
                "authority": "authoritative"
            }]
        }"#;
        assert!(matches!(
            parse_incident_diagnostics(json),
            Err(IncidentBriefError::InvalidDiagnostics)
        ));
    }

    #[test]
    fn diagnostics_source_finding_mismatch_fails_closed() {
        let json = r#"{
            "schema_version": 1,
            "observations": [{
                "source": "macos_launchservices",
                "collected_at_ms": 1234,
                "relation": "bounded_around_dispatch",
                "finding": "success_response_observed"
            }]
        }"#;
        assert!(matches!(
            parse_incident_diagnostics(json),
            Err(IncidentBriefError::InvalidDiagnostics)
        ));
    }

    #[test]
    fn same_source_positive_and_negative_observations_are_reported_as_conflict() {
        let brief = compose_incident_brief(
            inspection("launch_application"),
            audit("launch_application"),
            vec![
                diagnostic(
                    IncidentDiagnosticSource::MacosLaunchServices,
                    IncidentDiagnosticFinding::MatchingLaunchRecordObserved,
                ),
                diagnostic(
                    IncidentDiagnosticSource::MacosLaunchServices,
                    IncidentDiagnosticFinding::NoMatchingLaunchRecordObserved,
                ),
            ],
        )
        .unwrap();
        assert_eq!(brief.contradictions.len(), 1);
        assert_eq!(
            brief.contradictions[0].kind,
            IncidentContradictionKind::ObservationalSourceConflict
        );
    }

    #[test]
    fn text_rendering_marks_observations_as_non_authoritative() {
        let brief = compose_incident_brief(
            inspection("launch_application"),
            audit("launch_application"),
            vec![diagnostic(
                IncidentDiagnosticSource::Cua,
                IncidentDiagnosticFinding::NoSuccessResponseObserved,
            )],
        )
        .unwrap();
        let rendered = render_incident_brief_text(&brief);
        assert!(rendered.contains("Observational findings are not recovery authority"));
        assert!(rendered.contains("Keep quarantine [supported]"));
    }
}
