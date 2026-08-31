//! Stable, privacy-bounded operator status composed from existing V2 diagnostics.
//!
//! This module creates no recovery, replay, Handoff, mutation-authority, or
//! maintenance authority. It only maps already-existing read-only observations
//! into one schema suitable for CLI/Agent/UI consumers.

use crate::{
    v2_doctor::{CheckStatus, DoctorReport, LaneReadiness, ReadinessLanes},
    v2_operator_handoff::{HandoffInterventionStatus, HandoffRuntimeStatus},
    v2_upgrade_transaction::{
        UpgradeTransactionPhase, UpgradeTransactionRecord, UpgradeTransactionStatus,
    },
};
use serde::Serialize;

pub const OPERATOR_STATUS_SCHEMA_VERSION: u16 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OperatorOverallStatus {
    Healthy,
    Degraded,
    ActionRequired,
    Unavailable,
    Unknown,
}

impl OperatorOverallStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Healthy => "healthy",
            Self::Degraded => "degraded",
            Self::ActionRequired => "action_required",
            Self::Unavailable => "unavailable",
            Self::Unknown => "unknown",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OperatorReasonCode {
    None,
    PreviousOperationOutcomeUnknown,
    HandoffRecoveryRequired,
    HandoffFaulted,
    UpgradeInProgress,
    UpgradeActionRequired,
    MutationAuthorityMismatch,
    RuntimeUnverified,
    ControlPlaneUnavailable,
    BackendUnavailable,
    HandoffActive,
    ConfigurationOrTrustFailure,
    StatusIncomplete,
}

impl OperatorReasonCode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::PreviousOperationOutcomeUnknown => "previous_operation_outcome_unknown",
            Self::HandoffRecoveryRequired => "handoff_recovery_required",
            Self::HandoffFaulted => "handoff_faulted",
            Self::UpgradeInProgress => "upgrade_in_progress",
            Self::UpgradeActionRequired => "upgrade_action_required",
            Self::MutationAuthorityMismatch => "mutation_authority_mismatch",
            Self::RuntimeUnverified => "runtime_unverified",
            Self::ControlPlaneUnavailable => "control_plane_unavailable",
            Self::BackendUnavailable => "backend_unavailable",
            Self::HandoffActive => "handoff_active",
            Self::ConfigurationOrTrustFailure => "configuration_or_trust_failure",
            Self::StatusIncomplete => "status_incomplete",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OperatorNextAction {
    None,
    ReviewIncident,
    CompleteRecovery,
    InspectUpgrade,
    FixConfiguration,
    InspectDoctor,
    CheckBackend,
    FinishHandoff,
}

impl OperatorNextAction {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::ReviewIncident => "review_incident",
            Self::CompleteRecovery => "complete_recovery",
            Self::InspectUpgrade => "inspect_upgrade",
            Self::FixConfiguration => "fix_configuration",
            Self::InspectDoctor => "inspect_doctor",
            Self::CheckBackend => "check_backend",
            Self::FinishHandoff => "finish_handoff",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ControlPlaneConnectivity {
    Connected,
    Unavailable,
    Unknown,
}

impl ControlPlaneConnectivity {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Connected => "connected",
            Self::Unavailable => "unavailable",
            Self::Unknown => "unknown",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BackendStatus {
    Ready,
    Unavailable,
    Unknown,
    NotConfigured,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ControlPlaneSummary {
    pub connectivity: ControlPlaneConnectivity,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_connected: Option<bool>,
    pub backend: BackendStatus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum QuarantineStatus {
    Clear,
    Present,
    Unknown,
}

impl QuarantineStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Clear => "clear",
            Self::Present => "present",
            Self::Unknown => "unknown",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum IncidentReviewAvailability {
    NotRequired,
    Available,
    Unavailable,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RecoverySummary {
    pub quarantine: QuarantineStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub live_quarantine_count: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub replay_safe: Option<bool>,
    pub recovery_mode: String,
    pub incident_review: IncidentReviewAvailability,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HandoffOperatorStatus {
    Idle,
    AwaitingHuman,
    HumanActive,
    Verifying,
    ReadyToResume,
    ResumeRequested,
    RecoveryRequired,
    RecoveryExpired,
    Faulted,
    Unavailable,
    NotConfigured,
}

impl HandoffOperatorStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::AwaitingHuman => "awaiting_human",
            Self::HumanActive => "human_active",
            Self::Verifying => "verifying",
            Self::ReadyToResume => "ready_to_resume",
            Self::ResumeRequested => "resume_requested",
            Self::RecoveryRequired => "recovery_required",
            Self::RecoveryExpired => "recovery_expired",
            Self::Faulted => "faulted",
            Self::Unavailable => "unavailable",
            Self::NotConfigured => "not_configured",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HandoffSummary {
    pub status: HandoffOperatorStatus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MutationAuthorityOperatorStatus {
    VerifiedV2,
    OtherOwner,
    Unavailable,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MutationAuthorityOperatorSummary {
    pub status: MutationAuthorityOperatorStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub owner: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub epoch: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeVerificationStatus {
    Verified,
    Unverified,
}

impl RuntimeVerificationStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Verified => "verified",
            Self::Unverified => "unverified",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct OperatorRuntimeSummary {
    pub package_version: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_commit: Option<String>,
    pub verification: RuntimeVerificationStatus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MaintenanceOperatorStatus {
    None,
    InProgress,
    Completed,
    FailedBeforeInstall,
    FailedClosedAfterStop,
    OperatorActionRequired,
    Unavailable,
}

impl MaintenanceOperatorStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::InProgress => "in_progress",
            Self::Completed => "completed",
            Self::FailedBeforeInstall => "failed_before_install",
            Self::FailedClosedAfterStop => "failed_closed_after_stop",
            Self::OperatorActionRequired => "operator_action_required",
            Self::Unavailable => "unavailable",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MaintenanceSummary {
    pub status: MaintenanceOperatorStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub phase: Option<UpgradeTransactionPhase>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct OperatorStatusReport {
    pub schema_version: u16,
    pub overall: OperatorOverallStatus,
    pub control_plane: ControlPlaneSummary,
    pub lanes: ReadinessLanes,
    pub recovery: RecoverySummary,
    pub handoff: HandoffSummary,
    pub mutation_authority: MutationAuthorityOperatorSummary,
    pub runtime: OperatorRuntimeSummary,
    pub maintenance: MaintenanceSummary,
    pub primary_reason: OperatorReasonCode,
    pub next_action: OperatorNextAction,
}

impl OperatorStatusReport {
    pub const fn exit_code(&self) -> u8 {
        match self.overall {
            OperatorOverallStatus::Healthy => 0,
            OperatorOverallStatus::Degraded => 1,
            OperatorOverallStatus::ActionRequired
            | OperatorOverallStatus::Unavailable
            | OperatorOverallStatus::Unknown => 2,
        }
    }
}

pub enum HandoffStatusInput<'a> {
    NotConfigured,
    Unavailable,
    Available(&'a HandoffRuntimeStatus),
}

pub enum UpgradeStatusInput<'a> {
    None,
    Unavailable,
    Available(&'a UpgradeTransactionRecord),
}

pub fn build_operator_status(
    doctor: &DoctorReport,
    handoff_input: HandoffStatusInput<'_>,
    upgrade_input: UpgradeStatusInput<'_>,
) -> OperatorStatusReport {
    let control_plane = summarize_control_plane(doctor);
    let recovery = summarize_recovery(doctor);
    let handoff = HandoffSummary {
        status: summarize_handoff(handoff_input),
    };
    let mutation_authority = doctor
        .mutation_authority
        .as_ref()
        .map(|authority| MutationAuthorityOperatorSummary {
            status: if authority.owner == "v2" {
                MutationAuthorityOperatorStatus::VerifiedV2
            } else {
                MutationAuthorityOperatorStatus::OtherOwner
            },
            owner: Some(authority.owner.clone()),
            epoch: Some(authority.epoch),
        })
        .unwrap_or(MutationAuthorityOperatorSummary {
            status: MutationAuthorityOperatorStatus::Unavailable,
            owner: None,
            epoch: None,
        });
    let runtime = OperatorRuntimeSummary {
        package_version: doctor.runtime.package_version.clone(),
        source_commit: doctor.runtime.source_commit.clone(),
        verification: if doctor.runtime.manifest_verified {
            RuntimeVerificationStatus::Verified
        } else {
            RuntimeVerificationStatus::Unverified
        },
    };
    let maintenance = summarize_maintenance(upgrade_input);

    let (overall, primary_reason, next_action) = primary_operator_state(
        doctor,
        &control_plane,
        &recovery,
        &handoff,
        &mutation_authority,
        &runtime,
        &maintenance,
    );

    OperatorStatusReport {
        schema_version: OPERATOR_STATUS_SCHEMA_VERSION,
        overall,
        control_plane,
        lanes: doctor.readiness.lanes.clone(),
        recovery,
        handoff,
        mutation_authority,
        runtime,
        maintenance,
        primary_reason,
        next_action,
    }
}

fn summarize_control_plane(doctor: &DoctorReport) -> ControlPlaneSummary {
    let connectivity = match doctor.readiness.lanes.control_plane {
        LaneReadiness::Ready => ControlPlaneConnectivity::Connected,
        LaneReadiness::Unavailable => ControlPlaneConnectivity::Unavailable,
        LaneReadiness::Unknown
        | LaneReadiness::Unsupported
        | LaneReadiness::IndeterminateFenced => ControlPlaneConnectivity::Unknown,
    };
    let agent_connected = match check_status(doctor, "agent_hub_transport") {
        Some(CheckStatus::Ok) => Some(true),
        Some(CheckStatus::Error) => Some(false),
        Some(CheckStatus::Warning) | None => None,
    };
    let backend = match check_status(doctor, "cua_version") {
        Some(CheckStatus::Ok) => BackendStatus::Ready,
        Some(CheckStatus::Error) => BackendStatus::Unavailable,
        Some(CheckStatus::Warning) => BackendStatus::Unknown,
        None => BackendStatus::NotConfigured,
    };
    ControlPlaneSummary {
        connectivity,
        agent_connected,
        backend,
    }
}

fn summarize_recovery(doctor: &DoctorReport) -> RecoverySummary {
    let quarantine = match doctor.hub.live_quarantine_count {
        Some(0) => QuarantineStatus::Clear,
        Some(_) => QuarantineStatus::Present,
        None => QuarantineStatus::Unknown,
    };
    let hub_state_readable = !matches!(
        check_status(doctor, "hub_state"),
        Some(CheckStatus::Error) | None
    );
    let incident_review = match quarantine {
        QuarantineStatus::Clear => IncidentReviewAvailability::NotRequired,
        QuarantineStatus::Present if hub_state_readable => IncidentReviewAvailability::Available,
        QuarantineStatus::Present | QuarantineStatus::Unknown => {
            IncidentReviewAvailability::Unavailable
        }
    };
    RecoverySummary {
        quarantine,
        live_quarantine_count: doctor.hub.live_quarantine_count,
        replay_safe: (quarantine == QuarantineStatus::Present).then_some(false),
        recovery_mode: doctor.hub.recovery_mode.clone(),
        incident_review,
    }
}

fn summarize_handoff(input: HandoffStatusInput<'_>) -> HandoffOperatorStatus {
    let status = match input {
        HandoffStatusInput::NotConfigured => return HandoffOperatorStatus::NotConfigured,
        HandoffStatusInput::Unavailable => return HandoffOperatorStatus::Unavailable,
        HandoffStatusInput::Available(status) => status,
    };
    if status.faulted {
        return HandoffOperatorStatus::Faulted;
    }
    if status.recovery_required {
        return if status.recovery_expired {
            HandoffOperatorStatus::RecoveryExpired
        } else {
            HandoffOperatorStatus::RecoveryRequired
        };
    }
    if let Some(active) = status.active.as_ref() {
        return match active.status {
            HandoffInterventionStatus::AwaitingHuman => HandoffOperatorStatus::AwaitingHuman,
            HandoffInterventionStatus::HumanActive => HandoffOperatorStatus::HumanActive,
            HandoffInterventionStatus::Verifying => HandoffOperatorStatus::Verifying,
            HandoffInterventionStatus::ReadyToResume => HandoffOperatorStatus::ReadyToResume,
        };
    }
    if status.resume_requested {
        HandoffOperatorStatus::ResumeRequested
    } else {
        HandoffOperatorStatus::Idle
    }
}

fn summarize_maintenance(input: UpgradeStatusInput<'_>) -> MaintenanceSummary {
    match input {
        UpgradeStatusInput::None => MaintenanceSummary {
            status: MaintenanceOperatorStatus::None,
            phase: None,
        },
        UpgradeStatusInput::Unavailable => MaintenanceSummary {
            status: MaintenanceOperatorStatus::Unavailable,
            phase: None,
        },
        UpgradeStatusInput::Available(record) => MaintenanceSummary {
            status: match record.status {
                UpgradeTransactionStatus::InProgress => MaintenanceOperatorStatus::InProgress,
                UpgradeTransactionStatus::Completed => MaintenanceOperatorStatus::Completed,
                UpgradeTransactionStatus::FailedBeforeInstall => {
                    MaintenanceOperatorStatus::FailedBeforeInstall
                }
                UpgradeTransactionStatus::FailedClosedAfterStop => {
                    MaintenanceOperatorStatus::FailedClosedAfterStop
                }
                UpgradeTransactionStatus::OperatorActionRequired => {
                    MaintenanceOperatorStatus::OperatorActionRequired
                }
            },
            phase: Some(record.phase),
        },
    }
}

fn primary_operator_state(
    doctor: &DoctorReport,
    control: &ControlPlaneSummary,
    recovery: &RecoverySummary,
    handoff: &HandoffSummary,
    mutation: &MutationAuthorityOperatorSummary,
    runtime: &OperatorRuntimeSummary,
    maintenance: &MaintenanceSummary,
) -> (
    OperatorOverallStatus,
    OperatorReasonCode,
    OperatorNextAction,
) {
    if recovery.quarantine == QuarantineStatus::Present {
        return (
            OperatorOverallStatus::ActionRequired,
            OperatorReasonCode::PreviousOperationOutcomeUnknown,
            OperatorNextAction::ReviewIncident,
        );
    }
    if matches!(
        handoff.status,
        HandoffOperatorStatus::RecoveryRequired | HandoffOperatorStatus::RecoveryExpired
    ) {
        return (
            OperatorOverallStatus::ActionRequired,
            OperatorReasonCode::HandoffRecoveryRequired,
            OperatorNextAction::CompleteRecovery,
        );
    }
    if handoff.status == HandoffOperatorStatus::Faulted {
        return (
            OperatorOverallStatus::ActionRequired,
            OperatorReasonCode::HandoffFaulted,
            OperatorNextAction::InspectDoctor,
        );
    }
    match maintenance.status {
        MaintenanceOperatorStatus::InProgress => {
            return (
                OperatorOverallStatus::ActionRequired,
                OperatorReasonCode::UpgradeInProgress,
                OperatorNextAction::InspectUpgrade,
            );
        }
        MaintenanceOperatorStatus::FailedBeforeInstall
        | MaintenanceOperatorStatus::FailedClosedAfterStop
        | MaintenanceOperatorStatus::OperatorActionRequired => {
            return (
                OperatorOverallStatus::ActionRequired,
                OperatorReasonCode::UpgradeActionRequired,
                OperatorNextAction::InspectUpgrade,
            );
        }
        MaintenanceOperatorStatus::None
        | MaintenanceOperatorStatus::Completed
        | MaintenanceOperatorStatus::Unavailable => {}
    }
    if control.connectivity == ControlPlaneConnectivity::Unavailable {
        return (
            OperatorOverallStatus::Unavailable,
            OperatorReasonCode::ControlPlaneUnavailable,
            OperatorNextAction::InspectDoctor,
        );
    }
    if control.backend != BackendStatus::NotConfigured
        && mutation.status != MutationAuthorityOperatorStatus::VerifiedV2
    {
        return (
            OperatorOverallStatus::ActionRequired,
            OperatorReasonCode::MutationAuthorityMismatch,
            OperatorNextAction::FixConfiguration,
        );
    }
    if runtime.verification != RuntimeVerificationStatus::Verified {
        return (
            OperatorOverallStatus::ActionRequired,
            OperatorReasonCode::RuntimeUnverified,
            OperatorNextAction::FixConfiguration,
        );
    }
    if control.backend == BackendStatus::Unavailable {
        return (
            OperatorOverallStatus::Degraded,
            OperatorReasonCode::BackendUnavailable,
            OperatorNextAction::CheckBackend,
        );
    }
    if matches!(
        handoff.status,
        HandoffOperatorStatus::AwaitingHuman
            | HandoffOperatorStatus::HumanActive
            | HandoffOperatorStatus::Verifying
            | HandoffOperatorStatus::ReadyToResume
            | HandoffOperatorStatus::ResumeRequested
    ) {
        return (
            OperatorOverallStatus::Degraded,
            OperatorReasonCode::HandoffActive,
            OperatorNextAction::FinishHandoff,
        );
    }
    if handoff.status == HandoffOperatorStatus::Unavailable {
        return (
            OperatorOverallStatus::ActionRequired,
            OperatorReasonCode::StatusIncomplete,
            OperatorNextAction::InspectDoctor,
        );
    }
    if maintenance.status == MaintenanceOperatorStatus::Unavailable {
        return (
            OperatorOverallStatus::Unknown,
            OperatorReasonCode::StatusIncomplete,
            OperatorNextAction::InspectUpgrade,
        );
    }
    let readiness_unknown = [
        doctor.readiness.lanes.control_plane,
        doctor.readiness.lanes.computer_use_observation,
        doctor.readiness.lanes.filesystem_observation,
        doctor.readiness.lanes.effectful_execution,
        doctor.readiness.lanes.browser_effectful_execution,
    ]
    .into_iter()
    .any(|lane| lane == LaneReadiness::Unknown);
    if recovery.quarantine == QuarantineStatus::Unknown
        || control.connectivity == ControlPlaneConnectivity::Unknown
        || readiness_unknown
    {
        return (
            OperatorOverallStatus::Unknown,
            OperatorReasonCode::StatusIncomplete,
            OperatorNextAction::InspectDoctor,
        );
    }
    if doctor
        .checks
        .iter()
        .any(|check| check.status == CheckStatus::Error)
    {
        return (
            OperatorOverallStatus::ActionRequired,
            OperatorReasonCode::ConfigurationOrTrustFailure,
            OperatorNextAction::FixConfiguration,
        );
    }
    if doctor
        .checks
        .iter()
        .any(|check| check.status == CheckStatus::Warning)
        || matches!(handoff.status, HandoffOperatorStatus::Unavailable)
    {
        return (
            OperatorOverallStatus::Degraded,
            OperatorReasonCode::StatusIncomplete,
            OperatorNextAction::InspectDoctor,
        );
    }
    (
        OperatorOverallStatus::Healthy,
        OperatorReasonCode::None,
        OperatorNextAction::None,
    )
}

fn check_status(doctor: &DoctorReport, name: &str) -> Option<CheckStatus> {
    doctor
        .checks
        .iter()
        .rev()
        .find(|check| check.name == name)
        .map(|check| check.status)
}

pub fn render_operator_status_text(report: &OperatorStatusReport) -> String {
    let agent = match report.control_plane.agent_connected {
        Some(true) => "connected",
        Some(false) => "disconnected",
        None => "unknown",
    };
    let replay_safe = match report.recovery.replay_safe {
        Some(true) => "yes",
        Some(false) => "no",
        None => "not_applicable",
    };
    format!(
        concat!(
            "CUMG: {}\n",
            "Agent: {}\n",
            "Control plane: {}\n",
            "Computer Use observation: {}\n",
            "Filesystem observation: {}\n",
            "Effectful execution: {}\n",
            "Browser effectful execution: {}\n",
            "Human Handoff: {}\n",
            "Recovery: {}\n",
            "Replay safe: {}\n",
            "Runtime: {} {}\n",
            "Maintenance: {}\n",
            "Reason: {}\n",
            "Next action: {}"
        ),
        report.overall.as_str(),
        agent,
        report.control_plane.connectivity.as_str(),
        report.lanes.computer_use_observation.as_str(),
        report.lanes.filesystem_observation.as_str(),
        report.lanes.effectful_execution.as_str(),
        report.lanes.browser_effectful_execution.as_str(),
        report.handoff.status.as_str(),
        report.recovery.quarantine.as_str(),
        replay_safe,
        report.runtime.verification.as_str(),
        report.runtime.package_version,
        report.maintenance.status.as_str(),
        report.primary_reason.as_str(),
        report.next_action.as_str(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        v2_doctor::{
            AgentSummary, DoctorCheck, HubSummary, MutationAuthoritySummary, ReadinessSummary,
            RuntimeSummary,
        },
        v2_operator_handoff::{HandoffActiveStatus, HandoffExecutionAuthority, HandoffSurfaceKind},
        v2_upgrade_transaction::{UpgradeCompletionContract, UpgradeMutationAuthorityStatus},
    };

    fn check(name: &str, status: CheckStatus, detail: &str) -> DoctorCheck {
        DoctorCheck {
            name: name.into(),
            status,
            detail: detail.into(),
        }
    }

    fn healthy_doctor() -> DoctorReport {
        DoctorReport {
            schema_version: 1,
            overall: "healthy".into(),
            readiness: ReadinessSummary {
                device: "healthy".into(),
                lanes: ReadinessLanes {
                    control_plane: LaneReadiness::Ready,
                    computer_use_observation: LaneReadiness::Ready,
                    filesystem_observation: LaneReadiness::Ready,
                    effectful_execution: LaneReadiness::Ready,
                    browser_effectful_execution: LaneReadiness::Ready,
                },
                blocking_operation_present: Some(false),
                blocking_operation_retry_safe: None,
                operator_action: None,
            },
            runtime: RuntimeSummary {
                package_version: "0.3.0".into(),
                source_commit: Some("a".repeat(40)),
                manifest_verified: true,
            },
            hub: HubSummary {
                state_schema: Some(8),
                registry_schema: Some(1),
                device_count: 1,
                generation: Some(7),
                capability_schema: Some(5),
                capability_revision: Some(9),
                live_quarantine_count: Some(0),
                recovery_mode: "normal".into(),
            },
            agent: AgentSummary {
                state_schema: Some(1),
                replay_generation: Some(7),
            },
            mutation_authority: Some(MutationAuthoritySummary {
                owner: "v2".into(),
                epoch: 4,
            }),
            checks: vec![
                check("runtime_manifest", CheckStatus::Ok, "verified"),
                check("hub_state", CheckStatus::Ok, "restored_and_compatible"),
                check("agent_state", CheckStatus::Ok, "restored_and_compatible"),
                check("hub_service", CheckStatus::Ok, "running"),
                check("agent_service", CheckStatus::Ok, "running"),
                check("agent_hub_transport", CheckStatus::Ok, "established"),
                check("live_quarantine", CheckStatus::Ok, "none"),
                check("recovery_mode", CheckStatus::Ok, "normal"),
                check("cua_version", CheckStatus::Ok, "expected_version_present"),
                check("mutation_authority", CheckStatus::Ok, "v2_owner"),
            ],
        }
    }

    fn idle_handoff() -> HandoffRuntimeStatus {
        HandoffRuntimeStatus {
            active: None,
            recovery_required: false,
            recovery_status: None,
            recovery_epoch: None,
            recovery_expired: false,
            resume_requested: false,
            faulted: false,
            human_surface: None,
            locator: None,
        }
    }

    fn upgrade(
        status: UpgradeTransactionStatus,
        phase: UpgradeTransactionPhase,
    ) -> UpgradeTransactionRecord {
        UpgradeTransactionRecord {
            schema_version: 1,
            transaction_id: "tx-test".into(),
            status,
            phase,
            started_at_ms: 1,
            updated_at_ms: 2,
            cumg_source_commit: "a".repeat(40),
            handoff_source_commit: "b".repeat(40),
            runtime_generation: Some("runtime-a-b".into()),
            rollback_asset: Some("runtime-upgrade-test".into()),
            mutation_authority: UpgradeMutationAuthorityStatus {
                owner: Some("v2".into()),
                epoch: Some(4),
            },
            completion: UpgradeCompletionContract {
                runtime_manifest_verified: false,
                launchd_topology_safe: false,
                mutation_authority_verified: false,
                quarantine_clear: false,
                handoff_runtime_paired: false,
                services_restarted: false,
                doctor_healthy: false,
                cleanup_completed: false,
                rollback_asset_created: true,
            },
            failure_reason: None,
            operator_action: None,
        }
    }

    #[test]
    fn healthy_status_composes_existing_green_signals() {
        let doctor = healthy_doctor();
        let handoff = idle_handoff();
        let report = build_operator_status(
            &doctor,
            HandoffStatusInput::Available(&handoff),
            UpgradeStatusInput::None,
        );
        assert_eq!(report.overall, OperatorOverallStatus::Healthy);
        assert_eq!(report.primary_reason, OperatorReasonCode::None);
        assert_eq!(report.next_action, OperatorNextAction::None);
        assert_eq!(report.control_plane.agent_connected, Some(true));
        assert_eq!(report.recovery.quarantine, QuarantineStatus::Clear);
        assert_eq!(report.handoff.status, HandoffOperatorStatus::Idle);
    }

    #[test]
    fn json_contract_has_exact_top_level_shape_and_omits_raw_doctor_checks() {
        let doctor = healthy_doctor();
        let report = build_operator_status(
            &doctor,
            HandoffStatusInput::NotConfigured,
            UpgradeStatusInput::None,
        );
        let value = serde_json::to_value(&report).unwrap();
        let object = value.as_object().unwrap();
        let keys = object
            .keys()
            .map(String::as_str)
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(
            keys,
            std::collections::BTreeSet::from([
                "schema_version",
                "overall",
                "control_plane",
                "lanes",
                "recovery",
                "handoff",
                "mutation_authority",
                "runtime",
                "maintenance",
                "primary_reason",
                "next_action",
            ])
        );
        let json = serde_json::to_string(&report).unwrap();
        assert!(!json.contains("checks"));
        assert!(!json.contains("detail"));
        assert!(!json.contains("operation_id"));
    }

    #[test]
    fn quarantine_preserves_read_only_lanes_and_routes_to_incident_review() {
        let mut doctor = healthy_doctor();
        doctor.hub.live_quarantine_count = Some(1);
        doctor.hub.recovery_mode = "restricted_read_only".into();
        doctor.readiness.lanes.effectful_execution = LaneReadiness::IndeterminateFenced;
        doctor.readiness.lanes.browser_effectful_execution = LaneReadiness::IndeterminateFenced;
        doctor
            .checks
            .push(check("live_quarantine", CheckStatus::Error, "present"));
        let report = build_operator_status(
            &doctor,
            HandoffStatusInput::NotConfigured,
            UpgradeStatusInput::None,
        );
        assert_eq!(report.overall, OperatorOverallStatus::ActionRequired);
        assert_eq!(
            report.primary_reason,
            OperatorReasonCode::PreviousOperationOutcomeUnknown
        );
        assert_eq!(report.next_action, OperatorNextAction::ReviewIncident);
        assert_eq!(report.recovery.replay_safe, Some(false));
        assert_eq!(report.lanes.computer_use_observation, LaneReadiness::Ready);
        assert_eq!(
            report.lanes.effectful_execution,
            LaneReadiness::IndeterminateFenced
        );
    }

    #[test]
    fn backend_unavailable_is_distinct_from_quarantine_fencing() {
        let mut doctor = healthy_doctor();
        doctor.readiness.lanes.computer_use_observation = LaneReadiness::Unavailable;
        doctor.readiness.lanes.effectful_execution = LaneReadiness::Unavailable;
        doctor.readiness.lanes.browser_effectful_execution = LaneReadiness::Unavailable;
        doctor.checks.retain(|item| item.name != "cua_version");
        doctor.checks.push(check(
            "cua_version",
            CheckStatus::Error,
            "command_unavailable",
        ));
        let report = build_operator_status(
            &doctor,
            HandoffStatusInput::NotConfigured,
            UpgradeStatusInput::None,
        );
        assert_eq!(report.recovery.quarantine, QuarantineStatus::Clear);
        assert_eq!(report.overall, OperatorOverallStatus::Degraded);
        assert_eq!(
            report.primary_reason,
            OperatorReasonCode::BackendUnavailable
        );
        assert_eq!(report.next_action, OperatorNextAction::CheckBackend);
    }

    #[test]
    fn handoff_status_never_serializes_locator_or_intervention_identity() {
        let doctor = healthy_doctor();
        let mut handoff = idle_handoff();
        handoff.active = Some(HandoffActiveStatus {
            intervention_id: "private-intervention-id".into(),
            status: HandoffInterventionStatus::HumanActive,
            epoch: 44,
            authority: HandoffExecutionAuthority::Human,
        });
        handoff.human_surface = Some(HandoffSurfaceKind::Webrtc);
        handoff.locator = Some("sensitive-takeover-locator".into());
        let report = build_operator_status(
            &doctor,
            HandoffStatusInput::Available(&handoff),
            UpgradeStatusInput::None,
        );
        assert_eq!(report.primary_reason, OperatorReasonCode::HandoffActive);
        assert_eq!(report.next_action, OperatorNextAction::FinishHandoff);
        let json = serde_json::to_string(&report).unwrap();
        assert!(!json.contains("private-intervention-id"));
        assert!(!json.contains("sensitive-takeover-locator"));
        assert!(!json.contains("44"));
    }

    #[test]
    fn handoff_recovery_routes_to_explicit_recovery_without_exposing_epoch() {
        let doctor = healthy_doctor();
        let mut handoff = idle_handoff();
        handoff.recovery_required = true;
        handoff.recovery_expired = true;
        handoff.recovery_epoch = Some(1234567);
        let report = build_operator_status(
            &doctor,
            HandoffStatusInput::Available(&handoff),
            UpgradeStatusInput::None,
        );
        assert_eq!(report.overall, OperatorOverallStatus::ActionRequired);
        assert_eq!(
            report.primary_reason,
            OperatorReasonCode::HandoffRecoveryRequired
        );
        assert_eq!(report.next_action, OperatorNextAction::CompleteRecovery);
        assert_eq!(
            report.handoff.status,
            HandoffOperatorStatus::RecoveryExpired
        );
        assert!(!serde_json::to_string(&report).unwrap().contains("1234567"));
    }

    #[test]
    fn active_upgrade_routes_to_read_only_upgrade_inspection() {
        let doctor = healthy_doctor();
        let record = upgrade(
            UpgradeTransactionStatus::InProgress,
            UpgradeTransactionPhase::ServiceDrain,
        );
        let report = build_operator_status(
            &doctor,
            HandoffStatusInput::NotConfigured,
            UpgradeStatusInput::Available(&record),
        );
        assert_eq!(report.overall, OperatorOverallStatus::ActionRequired);
        assert_eq!(report.primary_reason, OperatorReasonCode::UpgradeInProgress);
        assert_eq!(report.next_action, OperatorNextAction::InspectUpgrade);
        assert_eq!(
            report.maintenance.phase,
            Some(UpgradeTransactionPhase::ServiceDrain)
        );
    }

    #[test]
    fn runtime_and_mutation_mismatch_fail_closed() {
        let mut doctor = healthy_doctor();
        doctor.mutation_authority = Some(MutationAuthoritySummary {
            owner: "v1".into(),
            epoch: 5,
        });
        let report = build_operator_status(
            &doctor,
            HandoffStatusInput::NotConfigured,
            UpgradeStatusInput::None,
        );
        assert_eq!(
            report.primary_reason,
            OperatorReasonCode::MutationAuthorityMismatch
        );
        assert_eq!(report.next_action, OperatorNextAction::FixConfiguration);

        doctor.mutation_authority = Some(MutationAuthoritySummary {
            owner: "v2".into(),
            epoch: 6,
        });
        doctor.runtime.manifest_verified = false;
        let report = build_operator_status(
            &doctor,
            HandoffStatusInput::NotConfigured,
            UpgradeStatusInput::None,
        );
        assert_eq!(report.primary_reason, OperatorReasonCode::RuntimeUnverified);
        assert_eq!(report.next_action, OperatorNextAction::FixConfiguration);
    }
}
