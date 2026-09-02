//! Pure planning and revalidation for the guided V2 quarantine-recovery UX.
//!
//! This module never signs, publishes, clears quarantine, replays work, or creates
//! recovery authority. `IncidentBrief.cumg.supported_decisions` remains the only
//! authoritative reconciliation decision source. When that set is empty, the plan
//! may expose a separately labelled interactive-Human historical assertion path;
//! that path never mutates or widens `supported_decisions`.

use crate::{
    v2_incident_brief::{IncidentBrief, IncidentHumanSummary},
    v2_maintenance::ReconciliationSupportedDecision,
    v2_online_recovery::RecoveryChallenge,
    v2_operator_status::OperatorOverallStatus,
};
use serde::Serialize;

pub const GUIDED_RECOVERY_SCHEMA_VERSION: u16 = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GuidedRecoveryDisposition {
    KeepQuarantine,
    HumanSelectionRequired,
    Reinspect,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GuidedRecoveryReason {
    NoSupportedDecision,
    SupportedDecisionAvailable,
    ExactBindingMismatch,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GuidedRecoveryOperation {
    pub operation_id: String,
    pub device_id: String,
    pub original_generation: u64,
    pub current_generation: Option<u64>,
    pub capability: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct GuidedRecoveryAuthorityBinding {
    operation_id: String,
    device_id: String,
    original_generation: u64,
    current_generation: u64,
    quarantine_fingerprint: [u8; 32],
    challenge_nonce: [u8; 32],
    challenge_expires_at_ms: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GuidedHistoricalAssertionAuthority {
    LocalHumanUserPresence,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GuidedHumanHistoricalAssertion {
    pub available: bool,
    pub authority: GuidedHistoricalAssertionAuthority,
    pub requires_interactive_human: bool,
    pub automatic_selection_allowed: bool,
    pub choices: Vec<ReconciliationSupportedDecision>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GuidedRecoveryPlan {
    pub schema_version: u16,
    pub disposition: GuidedRecoveryDisposition,
    pub reason: GuidedRecoveryReason,
    pub operation: GuidedRecoveryOperation,
    /// Exact closed set copied from the authoritative reconciliation audit.
    pub supported_decisions: Vec<ReconciliationSupportedDecision>,
    /// Separate Human-only historical assertion path. These choices are never
    /// copied into or treated as authoritative `supported_decisions`.
    pub human_historical_assertion: GuidedHumanHistoricalAssertion,
    pub incident: IncidentHumanSummary,
    pub observational_diagnostics_present: bool,
    pub old_operation_replayed: bool,
    #[serde(skip)]
    authority_binding: Option<GuidedRecoveryAuthorityBinding>,
    #[serde(skip)]
    review_snapshot: IncidentBrief,
}

impl GuidedRecoveryPlan {
    pub fn allows(&self, decision: ReconciliationSupportedDecision) -> bool {
        self.disposition == GuidedRecoveryDisposition::HumanSelectionRequired
            && self.supported_decisions.contains(&decision)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GuidedRecoveryRevalidationError {
    ReinspectRequired,
    DecisionNoLongerSupported,
}

/// Compose a privacy-bounded plan from an already verified Hub challenge and the
/// exact #233 incident brief. Observational diagnostics are copied only as a
/// presence bit; they never create or widen decisions.
pub fn compose_guided_recovery_plan(
    brief: &IncidentBrief,
    challenge: &RecoveryChallenge,
) -> GuidedRecoveryPlan {
    let binding_matches = brief.operation.operation_id == challenge.operation_id
        && brief.operation.device_id == challenge.device_id
        && brief.operation.original_generation == challenge.quarantine_generation
        && brief.operation.current_generation == Some(challenge.current_generation)
        && brief.cumg.operation_id == challenge.operation_id
        && brief.cumg.device_id == challenge.device_id
        && brief.cumg.device_generation == challenge.quarantine_generation;

    let authority_binding = binding_matches.then(|| GuidedRecoveryAuthorityBinding {
        operation_id: challenge.operation_id.clone(),
        device_id: challenge.device_id.clone(),
        original_generation: challenge.quarantine_generation,
        current_generation: challenge.current_generation,
        quarantine_fingerprint: challenge.quarantine_fingerprint,
        challenge_nonce: challenge.nonce,
        challenge_expires_at_ms: challenge.expires_at_ms,
    });

    let (disposition, reason) = if !binding_matches {
        (
            GuidedRecoveryDisposition::Reinspect,
            GuidedRecoveryReason::ExactBindingMismatch,
        )
    } else if brief.cumg.supported_decisions.is_empty() {
        (
            GuidedRecoveryDisposition::KeepQuarantine,
            GuidedRecoveryReason::NoSupportedDecision,
        )
    } else {
        (
            GuidedRecoveryDisposition::HumanSelectionRequired,
            GuidedRecoveryReason::SupportedDecisionAvailable,
        )
    };

    let human_historical_assertion_available =
        binding_matches && brief.cumg.supported_decisions.is_empty();
    let human_historical_assertion = GuidedHumanHistoricalAssertion {
        available: human_historical_assertion_available,
        authority: GuidedHistoricalAssertionAuthority::LocalHumanUserPresence,
        requires_interactive_human: true,
        automatic_selection_allowed: false,
        choices: if human_historical_assertion_available {
            vec![
                ReconciliationSupportedDecision::ConfirmedCompleted,
                ReconciliationSupportedDecision::ConfirmedNotExecuted,
            ]
        } else {
            Vec::new()
        },
    };

    GuidedRecoveryPlan {
        schema_version: GUIDED_RECOVERY_SCHEMA_VERSION,
        disposition,
        reason,
        operation: GuidedRecoveryOperation {
            operation_id: brief.operation.operation_id.clone(),
            device_id: brief.operation.device_id.clone(),
            original_generation: brief.operation.original_generation,
            current_generation: brief.operation.current_generation,
            capability: brief.operation.capability.clone(),
        },
        supported_decisions: brief.cumg.supported_decisions.clone(),
        human_historical_assertion,
        incident: brief.human_summary.clone(),
        observational_diagnostics_present: !brief.diagnostics.is_empty(),
        old_operation_replayed: brief.cumg.replay_old_operation,
        authority_binding,
        review_snapshot: brief.clone(),
    }
}

/// Require the Human-reviewed plan to still describe the exact same challenge,
/// incident brief, and authoritative decision set immediately before signing.
pub fn revalidate_guided_recovery_selection(
    reviewed: &GuidedRecoveryPlan,
    fresh: &GuidedRecoveryPlan,
    selected: ReconciliationSupportedDecision,
) -> Result<(), GuidedRecoveryRevalidationError> {
    if reviewed.disposition != GuidedRecoveryDisposition::HumanSelectionRequired
        || fresh.disposition != GuidedRecoveryDisposition::HumanSelectionRequired
        || reviewed.authority_binding.is_none()
        || reviewed.authority_binding != fresh.authority_binding
        || reviewed.review_snapshot != fresh.review_snapshot
        || reviewed.supported_decisions != fresh.supported_decisions
    {
        return Err(GuidedRecoveryRevalidationError::ReinspectRequired);
    }
    if !fresh.allows(selected) {
        return Err(GuidedRecoveryRevalidationError::DecisionNoLongerSupported);
    }
    Ok(())
}

/// Revalidate a separately labelled Human historical assertion immediately
/// before signing. The authoritative decision set must remain empty; a newly
/// available authoritative decision forces re-review instead of silently
/// converting the Human assertion into machine-supported evidence.
pub fn revalidate_guided_human_historical_selection(
    reviewed: &GuidedRecoveryPlan,
    fresh: &GuidedRecoveryPlan,
    selected: ReconciliationSupportedDecision,
) -> Result<(), GuidedRecoveryRevalidationError> {
    if reviewed.disposition != GuidedRecoveryDisposition::KeepQuarantine
        || fresh.disposition != GuidedRecoveryDisposition::KeepQuarantine
        || reviewed.authority_binding.is_none()
        || reviewed.authority_binding != fresh.authority_binding
        || reviewed.review_snapshot != fresh.review_snapshot
        || !reviewed.supported_decisions.is_empty()
        || !fresh.supported_decisions.is_empty()
        || !reviewed.human_historical_assertion.available
        || !fresh.human_historical_assertion.available
        || reviewed.human_historical_assertion != fresh.human_historical_assertion
    {
        return Err(GuidedRecoveryRevalidationError::ReinspectRequired);
    }
    if !fresh.human_historical_assertion.choices.contains(&selected) {
        return Err(GuidedRecoveryRevalidationError::DecisionNoLongerSupported);
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GuidedRecoveryPostDisposition {
    VerifiedHealthy,
    VerifiedWithUnrelatedStatusProblem,
    VerificationIncomplete,
}

pub const fn classify_guided_recovery_post_status(
    durable_completion_verified: bool,
    exact_quarantine_cleared: bool,
    recovery_mode_normal: bool,
    overall: OperatorOverallStatus,
) -> GuidedRecoveryPostDisposition {
    if !durable_completion_verified || !exact_quarantine_cleared {
        GuidedRecoveryPostDisposition::VerificationIncomplete
    } else if recovery_mode_normal && matches!(overall, OperatorOverallStatus::Healthy) {
        GuidedRecoveryPostDisposition::VerifiedHealthy
    } else {
        GuidedRecoveryPostDisposition::VerifiedWithUnrelatedStatusProblem
    }
}

pub const fn decision_name(decision: ReconciliationSupportedDecision) -> &'static str {
    match decision {
        ReconciliationSupportedDecision::ConfirmedCompleted => "confirmed_completed",
        ReconciliationSupportedDecision::ConfirmedNotExecuted => "confirmed_not_executed",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        v2_incident_brief::{
            IncidentDecisionGuidance, IncidentDiagnosticAuthority, IncidentDiagnosticFinding,
            IncidentDiagnosticObservation, IncidentDiagnosticRelation, IncidentDiagnosticSource,
            IncidentHumanSummary, IncidentMutationAuthorityAvailability,
            IncidentMutationAuthoritySummary, IncidentOperationSummary,
        },
        v2_maintenance::{
            AgentTerminalEvidenceStatus, AgentTerminalMarkerStatus, ReconciliationAuditReason,
            ReconciliationEvidenceAuthority, ReconciliationEvidenceSource,
            ReconciliationEvidenceStatus, ReconciliationReadinessAudit,
            ReconciliationRecommendedAction, ReconciliationResolutionReadiness,
        },
    };

    fn brief(decisions: Vec<ReconciliationSupportedDecision>) -> IncidentBrief {
        IncidentBrief {
            schema_version: 1,
            operation: IncidentOperationSummary {
                operation_id: "op_guided".into(),
                capability: "launch_application".into(),
                device_id: "dev_guided".into(),
                original_generation: 7,
                current_generation: Some(9),
                dispatch_recorded: true,
                dispatched_at_ms: Some(10),
                indeterminate_at_ms: 11,
                indeterminate_reason: "backend_timed_out".into(),
                execution_outcome: "indeterminate".into(),
                retry_safe: false,
            },
            cumg: ReconciliationReadinessAudit {
                operation_id: "op_guided".into(),
                hub_execution_schema_version: 8,
                device_id: "dev_guided".into(),
                device_generation: 7,
                capability: "launch_application".into(),
                dispatch_recorded: true,
                dispatch_binding_present: true,
                hub_terminal_evidence: "none".into(),
                hub_reconciliation_status: "auto_reconciling".into(),
                agent_evidence_source: ReconciliationEvidenceSource::Available,
                agent_device_match: Some(true),
                agent_replay_generation: Some(7),
                agent_terminal_marker: AgentTerminalMarkerStatus::Present,
                agent_terminal_marker_authoritative: false,
                agent_terminal_evidence: AgentTerminalEvidenceStatus::ExactAuthoritative,
                authoritative_terminal_state: Some("completed".into()),
                authoritative_evidence_class: Some("verified_agent_result".into()),
                evidence_authority: ReconciliationEvidenceAuthority::AuthoritativeTerminalEvidence,
                evidence_status: ReconciliationEvidenceStatus::Sufficient,
                resolution_readiness:
                    ReconciliationResolutionReadiness::ConfirmedCompletedSupported,
                supported_decisions: decisions,
                manual_audit_required: false,
                recommended_action: ReconciliationRecommendedAction::AuthorizedRecoverySupported,
                reasons: vec![ReconciliationAuditReason::AuthoritativeTerminalEvidenceAvailable],
                replay_old_operation: false,
            },
            mutation_authority: IncidentMutationAuthoritySummary {
                availability: IncidentMutationAuthorityAvailability::NotRequested,
                owner: None,
                epoch: None,
            },
            diagnostics: Vec::new(),
            contradictions: Vec::new(),
            human_summary: IncidentHumanSummary {
                known: Vec::new(),
                unknown: Vec::new(),
                decision_guidance: IncidentDecisionGuidance::AuthenticatedHumanDecisionRequired,
            },
        }
    }

    fn challenge() -> RecoveryChallenge {
        RecoveryChallenge {
            schema_version: 1,
            device_id: "dev_guided".into(),
            operation_id: "op_guided".into(),
            quarantine_generation: 7,
            current_generation: 9,
            quarantine_fingerprint: [3; 32],
            nonce: [4; 32],
            issued_at_ms: 20,
            expires_at_ms: 10_000,
            signature: vec![5; 64],
        }
    }

    #[test]
    fn empty_supported_decisions_keeps_quarantine() {
        let plan = compose_guided_recovery_plan(&brief(Vec::new()), &challenge());
        assert_eq!(plan.disposition, GuidedRecoveryDisposition::KeepQuarantine);
        assert!(plan.supported_decisions.is_empty());
        assert!(plan.human_historical_assertion.available);
        assert!(!plan.human_historical_assertion.automatic_selection_allowed);
        assert_eq!(
            plan.human_historical_assertion.choices,
            vec![
                ReconciliationSupportedDecision::ConfirmedCompleted,
                ReconciliationSupportedDecision::ConfirmedNotExecuted,
            ]
        );
        assert!(!plan.old_operation_replayed);
    }

    #[test]
    fn one_supported_decision_requires_explicit_human_selection() {
        let plan = compose_guided_recovery_plan(
            &brief(vec![ReconciliationSupportedDecision::ConfirmedCompleted]),
            &challenge(),
        );
        assert_eq!(
            plan.disposition,
            GuidedRecoveryDisposition::HumanSelectionRequired
        );
        assert!(plan.allows(ReconciliationSupportedDecision::ConfirmedCompleted));
        assert!(!plan.allows(ReconciliationSupportedDecision::ConfirmedNotExecuted));
    }

    #[test]
    fn two_supported_decisions_are_preserved_as_closed_set() {
        let decisions = vec![
            ReconciliationSupportedDecision::ConfirmedCompleted,
            ReconciliationSupportedDecision::ConfirmedNotExecuted,
        ];
        let plan = compose_guided_recovery_plan(&brief(decisions.clone()), &challenge());
        assert_eq!(plan.supported_decisions, decisions);
        assert_eq!(
            plan.disposition,
            GuidedRecoveryDisposition::HumanSelectionRequired
        );
    }

    #[test]
    fn observational_material_cannot_widen_supported_decisions() {
        let mut incident = brief(Vec::new());
        incident.diagnostics.push(IncidentDiagnosticObservation {
            source: IncidentDiagnosticSource::Cua,
            collected_at_ms: 42,
            relation: IncidentDiagnosticRelation::CurrentState,
            authority: IncidentDiagnosticAuthority::ObservationalOnly,
            finding: IncidentDiagnosticFinding::SuccessResponseObserved,
            absence_of_evidence: false,
        });
        incident.human_summary.unknown.clear();
        let plan = compose_guided_recovery_plan(&incident, &challenge());
        assert!(plan.observational_diagnostics_present);
        assert!(plan.supported_decisions.is_empty());
        assert!(plan.human_historical_assertion.available);
        assert!(!plan.human_historical_assertion.automatic_selection_allowed);
        assert_eq!(plan.disposition, GuidedRecoveryDisposition::KeepQuarantine);
    }

    #[test]
    fn stale_generation_forces_reinspection() {
        let mut stale = challenge();
        stale.current_generation += 1;
        let plan = compose_guided_recovery_plan(
            &brief(vec![ReconciliationSupportedDecision::ConfirmedCompleted]),
            &stale,
        );
        assert_eq!(plan.disposition, GuidedRecoveryDisposition::Reinspect);
    }

    #[test]
    fn challenge_change_after_review_fails_closed_before_signing() {
        let incident = brief(vec![ReconciliationSupportedDecision::ConfirmedCompleted]);
        let reviewed = compose_guided_recovery_plan(&incident, &challenge());
        let mut changed_challenge = challenge();
        changed_challenge.nonce = [8; 32];
        let fresh = compose_guided_recovery_plan(&incident, &changed_challenge);
        assert_eq!(
            revalidate_guided_recovery_selection(
                &reviewed,
                &fresh,
                ReconciliationSupportedDecision::ConfirmedCompleted,
            ),
            Err(GuidedRecoveryRevalidationError::ReinspectRequired)
        );
    }

    #[test]
    fn supported_decision_change_after_review_forces_reinspection() {
        let reviewed = compose_guided_recovery_plan(
            &brief(vec![ReconciliationSupportedDecision::ConfirmedCompleted]),
            &challenge(),
        );
        let fresh = compose_guided_recovery_plan(
            &brief(vec![ReconciliationSupportedDecision::ConfirmedNotExecuted]),
            &challenge(),
        );
        assert_eq!(
            revalidate_guided_recovery_selection(
                &reviewed,
                &fresh,
                ReconciliationSupportedDecision::ConfirmedCompleted,
            ),
            Err(GuidedRecoveryRevalidationError::ReinspectRequired)
        );
    }

    #[test]
    fn human_historical_assertion_revalidates_only_while_authoritative_set_stays_empty() {
        let reviewed = compose_guided_recovery_plan(&brief(Vec::new()), &challenge());
        let fresh = compose_guided_recovery_plan(&brief(Vec::new()), &challenge());
        assert_eq!(
            revalidate_guided_human_historical_selection(
                &reviewed,
                &fresh,
                ReconciliationSupportedDecision::ConfirmedCompleted,
            ),
            Ok(())
        );

        let authoritative_now = compose_guided_recovery_plan(
            &brief(vec![ReconciliationSupportedDecision::ConfirmedCompleted]),
            &challenge(),
        );
        assert_eq!(
            revalidate_guided_human_historical_selection(
                &reviewed,
                &authoritative_now,
                ReconciliationSupportedDecision::ConfirmedCompleted,
            ),
            Err(GuidedRecoveryRevalidationError::ReinspectRequired)
        );
    }

    #[test]
    fn stale_challenge_blocks_human_historical_assertion_before_signing() {
        let incident = brief(Vec::new());
        let reviewed = compose_guided_recovery_plan(&incident, &challenge());
        let mut changed = challenge();
        changed.nonce = [9; 32];
        let fresh = compose_guided_recovery_plan(&incident, &changed);
        assert_eq!(
            revalidate_guided_human_historical_selection(
                &reviewed,
                &fresh,
                ReconciliationSupportedDecision::ConfirmedNotExecuted,
            ),
            Err(GuidedRecoveryRevalidationError::ReinspectRequired)
        );
    }

    #[test]
    fn healthy_post_recovery_status_is_distinct_from_durable_completion_only() {
        assert_eq!(
            classify_guided_recovery_post_status(true, true, true, OperatorOverallStatus::Healthy),
            GuidedRecoveryPostDisposition::VerifiedHealthy
        );
        assert_eq!(
            classify_guided_recovery_post_status(false, true, true, OperatorOverallStatus::Healthy),
            GuidedRecoveryPostDisposition::VerificationIncomplete
        );
    }

    #[test]
    fn unrelated_post_recovery_degradation_does_not_erase_verified_recovery() {
        assert_eq!(
            classify_guided_recovery_post_status(true, true, true, OperatorOverallStatus::Degraded,),
            GuidedRecoveryPostDisposition::VerifiedWithUnrelatedStatusProblem
        );
        assert_eq!(
            classify_guided_recovery_post_status(
                true,
                false,
                true,
                OperatorOverallStatus::ActionRequired,
            ),
            GuidedRecoveryPostDisposition::VerificationIncomplete
        );
        assert_eq!(
            classify_guided_recovery_post_status(
                true,
                true,
                false,
                OperatorOverallStatus::ActionRequired,
            ),
            GuidedRecoveryPostDisposition::VerifiedWithUnrelatedStatusProblem
        );
    }

    #[test]
    fn json_contract_separates_authoritative_decisions_from_human_assertion() {
        let plan = compose_guided_recovery_plan(&brief(Vec::new()), &challenge());
        let value = serde_json::to_value(&plan).unwrap();
        assert_eq!(value["schema_version"], 2);
        assert_eq!(value["supported_decisions"], serde_json::json!([]));
        assert_eq!(value["human_historical_assertion"]["available"], true);
        assert_eq!(
            value["human_historical_assertion"]["authority"],
            "local_human_user_presence"
        );
        assert_eq!(
            value["human_historical_assertion"]["requires_interactive_human"],
            true
        );
        assert_eq!(
            value["human_historical_assertion"]["automatic_selection_allowed"],
            false
        );
        assert_eq!(
            value["human_historical_assertion"]["choices"],
            serde_json::json!(["confirmed_completed", "confirmed_not_executed"])
        );
    }

    #[test]
    fn json_contract_omits_private_challenge_binding_and_sensitive_field_names() {
        let plan = compose_guided_recovery_plan(
            &brief(vec![ReconciliationSupportedDecision::ConfirmedCompleted]),
            &challenge(),
        );
        let json = serde_json::to_string(&plan).unwrap();
        for forbidden in [
            "quarantine_fingerprint",
            "challenge_nonce",
            "locator",
            "credential",
            "key_file",
            "raw_command",
            "argv",
            "clipboard",
            "screenshot",
            "principal",
        ] {
            assert!(
                !json.contains(forbidden),
                "unexpected private field: {forbidden}"
            );
        }
    }
}
