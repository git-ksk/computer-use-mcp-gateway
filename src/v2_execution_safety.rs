//! Authoritative V2 desktop execution-safety ledger.
//!
//! This module is the reviewed state-machine boundary for state-changing desktop
//! work. The older M0 admission controller still owns bounded queueing, while
//! this ledger adds the semantics that must survive transport/session churn:
//! exact principal ownership, generation fencing, durable pending-effect intent,
//! guarded finalization, desktop quarantine, explicit resolution, compact execution
//! receipts, and bounded recoverable process/shell results. Recovery state never
//! stores raw command, argv, cwd, or environment payloads.

use crate::v2_m0::{DeviceCapability, DeviceErrorCode, ProcessOutput};
use crate::v2_m0_execution::{
    AdmissionDecision, AdmissionLimits, CancellationDecision, CompletionDecision, ExecutionError,
    HubAdmissionController, HubAdmissionSnapshot, HubOperationState, IndeterminateResolution,
    OperationRef,
};
use crate::v2_m0_trust::AuthenticatedClientPrincipal;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

pub const EXECUTION_SAFETY_SCHEMA_VERSION: u16 = 2;
const PREVIOUS_EXECUTION_SAFETY_SCHEMA_VERSION: u16 = 1;
pub const MAX_RESOLUTION_EVIDENCE_BYTES: usize = 1024;
pub const MAX_RECOVERY_ARCHIVE_ENTRIES: usize = 8;
pub const MAX_RECOVERY_ARCHIVE_BYTES: usize = 256 * 1024;

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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DesktopQuarantine {
    pub device_id: String,
    pub operation_id: String,
    pub device_generation: u64,
    pub owner: OperationOwner,
    pub reason: IndeterminateReason,
    pub since_ms: u64,
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
    Process { output: ProcessOutput },
    Shell { output: ProcessOutput },
    Error { code: DeviceErrorCode },
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OperationRecoverySnapshot {
    pub operation: OperationRef,
    pub capability: DeviceCapability,
    pub state: HubOperationState,
    pub indeterminate_reason: Option<IndeterminateReason>,
    pub receipt: Option<ExecutionReceipt>,
    pub result: Option<RecoverableOperationResult>,
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SafetyOperationSnapshot {
    pub operation: OperationRef,
    pub owner: OperationOwner,
    pub capability: DeviceCapability,
    pub state: HubOperationState,
    pub prepared_at_ms: u64,
    pub dispatched_at_ms: Option<u64>,
    pub indeterminate_reason: Option<IndeterminateReason>,
    pub receipt: Option<ExecutionReceipt>,
    #[serde(default)]
    pub recoverable_result: Option<RecoverableOperationResult>,
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
}

#[derive(Debug, Clone)]
struct SafetyOperation {
    operation: OperationRef,
    owner: OperationOwner,
    capability: DeviceCapability,
    state: HubOperationState,
    prepared_at_ms: u64,
    dispatched_at_ms: Option<u64>,
    indeterminate_reason: Option<IndeterminateReason>,
    receipt: Option<ExecutionReceipt>,
    recoverable_result: Option<RecoverableOperationResult>,
}

/// Single authoritative state machine for Hub-side desktop execution safety.
///
/// The invariant is intentionally stricter than transport liveness: once work
/// may have crossed the side-effect boundary, a missing proof converges to
/// durable `Indeterminate` + desktop quarantine. Reconnect never resolves it.
#[derive(Debug)]
pub struct AuthoritativeOperationController {
    admission: HubAdmissionController,
    operations: HashMap<String, SafetyOperation>,
    recovery_archive: HashMap<String, ArchivedOperationRecovery>,
    quarantines: HashMap<String, DesktopQuarantine>,
    resolutions: Vec<ResolutionRecord>,
}

impl AuthoritativeOperationController {
    pub fn new(limits: AdmissionLimits) -> Result<Self, ExecutionError> {
        Ok(Self {
            admission: HubAdmissionController::new(limits)?,
            operations: HashMap::new(),
            recovery_archive: HashMap::new(),
            quarantines: HashMap::new(),
            resolutions: Vec::new(),
        })
    }

    pub fn prepare(
        &mut self,
        operation: OperationRef,
        owner: OperationOwner,
        capability: DeviceCapability,
        now_ms: u64,
    ) -> Result<AdmissionDecision, ExecutionError> {
        if self.operations.contains_key(&operation.operation_id)
            || self.recovery_archive.contains_key(&operation.operation_id)
        {
            return Err(ExecutionError::OperationReplay);
        }
        let decision = self.admission.admit(operation.clone())?;
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
                state,
                prepared_at_ms: now_ms,
                dispatched_at_ms: None,
                indeterminate_reason: None,
                receipt: None,
                recoverable_result: None,
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

    /// Internal connection/restart path. Ownership is read from the durable
    /// record rather than inferred from a new connection/session.
    pub fn mark_connection_lost(
        &mut self,
        operation_id: &str,
        now_ms: u64,
    ) -> Result<CompletionDecision, ExecutionError> {
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
        if !matches!(
            target.state,
            HubOperationState::Dispatched | HubOperationState::CancelRequested
        ) {
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

        let next = self
            .admission
            .resolve_indeterminate(operation_id, decision.clone())?;
        let terminal = match decision {
            IndeterminateResolution::ConfirmedCompleted => HubOperationState::Completed,
            IndeterminateResolution::ConfirmedNotExecuted => HubOperationState::Cancelled,
        };
        let record = self
            .operations
            .get_mut(operation_id)
            .ok_or(ExecutionError::UnknownOperation)?;
        record.state = terminal;
        record.indeterminate_reason = None;
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

    /// Attach bounded caller-visible output to an already finalized process/shell operation.
    /// Raw request material is never accepted here.
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

    pub fn resolutions(&self) -> &[ResolutionRecord] {
        &self.resolutions
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
                state,
                prepared_at_ms: record.prepared_at_ms,
                dispatched_at_ms: record.dispatched_at_ms,
                indeterminate_reason: reason,
                receipt,
                recoverable_result,
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
        }
    }

    pub fn restore_after_restart(
        limits: AdmissionLimits,
        snapshot: AuthoritativeSafetySnapshot,
    ) -> Result<Self, ExecutionError> {
        if !matches!(
            snapshot.schema_version,
            PREVIOUS_EXECUTION_SAFETY_SCHEMA_VERSION | EXECUTION_SAFETY_SCHEMA_VERSION
        ) {
            return Err(ExecutionError::InvalidSnapshot);
        }
        if snapshot.schema_version == PREVIOUS_EXECUTION_SAFETY_SCHEMA_VERSION
            && (!snapshot.recoveries.is_empty()
                || snapshot
                    .operations
                    .iter()
                    .any(|record| record.recoverable_result.is_some()))
        {
            return Err(ExecutionError::InvalidSnapshot);
        }
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
            {
                return Err(ExecutionError::InvalidSnapshot);
            }
            operations.insert(
                record.operation.operation_id.clone(),
                SafetyOperation {
                    operation: record.operation,
                    owner: record.owner,
                    capability: record.capability,
                    state: record.state,
                    prepared_at_ms: record.prepared_at_ms,
                    dispatched_at_ms: record.dispatched_at_ms,
                    indeterminate_reason: record.indeterminate_reason,
                    receipt: record.receipt,
                    recoverable_result: record.recoverable_result,
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
        for record in operations.values() {
            if record.state == HubOperationState::Indeterminate
                && !quarantines.contains_key(&record.operation.device_id)
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
        legacy.schema_version = PREVIOUS_EXECUTION_SAFETY_SCHEMA_VERSION;
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
