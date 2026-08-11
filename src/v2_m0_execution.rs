//! V2-M0 bounded admission, single-device execution, and cancellation semantics.
//!
//! The Hub owns bounded admission. The Agent independently enforces one active
//! operation and remembers terminal operation IDs so a reconnect or retry cannot
//! silently replay an action whose outcome may already be externally visible.

use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet, VecDeque};
use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AdmissionLimits {
    pub max_global_active: usize,
    pub max_queued_per_device: usize,
}

impl AdmissionLimits {
    pub fn validate(self) -> Result<Self, ExecutionError> {
        if self.max_global_active == 0 {
            return Err(ExecutionError::InvalidGlobalLimit);
        }
        Ok(self)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OperationRef {
    pub device_id: String,
    pub device_generation: u64,
    pub operation_id: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum HubOperationState {
    Queued,
    ActiveNotDispatched,
    Dispatched,
    CancelRequested,
    Completed,
    Cancelled,
    Indeterminate,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AdmissionDecision {
    StartNow(OperationRef),
    Queued { position: usize },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CompletionDecision {
    Idle,
    StartNext(OperationRef),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IndeterminateResolution {
    ConfirmedCompleted,
    ConfirmedNotExecuted,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CancellationDecision {
    CancelledBeforeDispatch { next: CompletionDecision },
    SendCancellation(OperationRef),
    AlreadyTerminal(HubOperationState),
}

#[derive(Debug, Clone)]
struct HubOperation {
    operation: OperationRef,
    state: HubOperationState,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HubOperationSnapshot {
    pub operation: OperationRef,
    pub state: HubOperationState,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HubAdmissionSnapshot {
    pub operations: Vec<HubOperationSnapshot>,
}

#[derive(Debug)]
pub struct HubAdmissionController {
    limits: AdmissionLimits,
    active_by_device: HashMap<String, String>,
    operations: HashMap<String, HubOperation>,
    queued_by_device: HashMap<String, VecDeque<OperationRef>>,
    blocked_by_indeterminate: HashMap<String, String>,
}

impl HubAdmissionController {
    pub fn new(limits: AdmissionLimits) -> Result<Self, ExecutionError> {
        Ok(Self {
            limits: limits.validate()?,
            active_by_device: HashMap::new(),
            operations: HashMap::new(),
            queued_by_device: HashMap::new(),
            blocked_by_indeterminate: HashMap::new(),
        })
    }

    pub fn snapshot_for_restart(&self) -> HubAdmissionSnapshot {
        let mut operations: Vec<_> = self
            .operations
            .values()
            .map(|operation| HubOperationSnapshot {
                operation: operation.operation.clone(),
                state: match operation.state {
                    HubOperationState::Queued | HubOperationState::ActiveNotDispatched => {
                        HubOperationState::Cancelled
                    }
                    HubOperationState::Dispatched | HubOperationState::CancelRequested => {
                        HubOperationState::Indeterminate
                    }
                    terminal => terminal,
                },
            })
            .collect();
        operations.sort_by(|left, right| {
            left.operation
                .operation_id
                .cmp(&right.operation.operation_id)
        });
        HubAdmissionSnapshot { operations }
    }

    pub fn restore_after_restart(
        limits: AdmissionLimits,
        snapshot: HubAdmissionSnapshot,
    ) -> Result<Self, ExecutionError> {
        let mut controller = Self::new(limits)?;
        for persisted in snapshot.operations {
            if !matches!(
                persisted.state,
                HubOperationState::Completed
                    | HubOperationState::Cancelled
                    | HubOperationState::Indeterminate
            ) {
                return Err(ExecutionError::InvalidSnapshot);
            }
            if controller
                .operations
                .contains_key(&persisted.operation.operation_id)
            {
                return Err(ExecutionError::InvalidSnapshot);
            }
            if persisted.state == HubOperationState::Indeterminate {
                if controller
                    .blocked_by_indeterminate
                    .insert(
                        persisted.operation.device_id.clone(),
                        persisted.operation.operation_id.clone(),
                    )
                    .is_some()
                {
                    return Err(ExecutionError::InvalidSnapshot);
                }
            }
            controller.operations.insert(
                persisted.operation.operation_id.clone(),
                HubOperation {
                    operation: persisted.operation,
                    state: persisted.state,
                },
            );
        }
        Ok(controller)
    }

    pub fn admit(&mut self, operation: OperationRef) -> Result<AdmissionDecision, ExecutionError> {
        if operation.operation_id.trim().is_empty() || operation.device_id.trim().is_empty() {
            return Err(ExecutionError::InvalidOperation);
        }
        if self.operations.contains_key(&operation.operation_id) {
            return Err(ExecutionError::OperationReplay);
        }
        if let Some(operation_id) = self.blocked_by_indeterminate.get(&operation.device_id) {
            return Err(ExecutionError::DeviceIndeterminate {
                operation_id: operation_id.clone(),
            });
        }

        let device_busy = self.active_by_device.contains_key(&operation.device_id);
        let global_busy = self.active_by_device.len() >= self.limits.max_global_active;
        if !device_busy && !global_busy {
            self.start(operation.clone());
            return Ok(AdmissionDecision::StartNow(operation));
        }

        let queue = self
            .queued_by_device
            .entry(operation.device_id.clone())
            .or_default();
        if queue.len() >= self.limits.max_queued_per_device {
            return Err(ExecutionError::BackpressureRejected);
        }
        queue.push_back(operation.clone());
        let position = queue.len();
        self.operations.insert(
            operation.operation_id.clone(),
            HubOperation {
                operation,
                state: HubOperationState::Queued,
            },
        );
        Ok(AdmissionDecision::Queued { position })
    }

    pub fn mark_dispatched(&mut self, operation_id: &str) -> Result<(), ExecutionError> {
        let operation = self
            .operations
            .get_mut(operation_id)
            .ok_or(ExecutionError::UnknownOperation)?;
        if operation.state != HubOperationState::ActiveNotDispatched {
            return Err(ExecutionError::InvalidTransition);
        }
        operation.state = HubOperationState::Dispatched;
        Ok(())
    }

    pub fn cancel(&mut self, operation_id: &str) -> Result<CancellationDecision, ExecutionError> {
        let state = self
            .operations
            .get(operation_id)
            .map(|operation| operation.state)
            .ok_or(ExecutionError::UnknownOperation)?;

        match state {
            HubOperationState::Queued => {
                let device_id = self.operations[operation_id].operation.device_id.clone();
                if let Some(queue) = self.queued_by_device.get_mut(&device_id) {
                    if let Some(index) = queue
                        .iter()
                        .position(|operation| operation.operation_id == operation_id)
                    {
                        queue.remove(index);
                    }
                    if queue.is_empty() {
                        self.queued_by_device.remove(&device_id);
                    }
                }
                self.operations
                    .get_mut(operation_id)
                    .expect("operation was resolved above")
                    .state = HubOperationState::Cancelled;
                Ok(CancellationDecision::CancelledBeforeDispatch {
                    next: CompletionDecision::Idle,
                })
            }
            HubOperationState::ActiveNotDispatched => {
                let device_id = self.operations[operation_id].operation.device_id.clone();
                self.operations
                    .get_mut(operation_id)
                    .expect("operation was resolved above")
                    .state = HubOperationState::Cancelled;
                self.active_by_device.remove(&device_id);
                let next = self
                    .start_next_for_available_capacity(Some(&device_id))
                    .map_or(CompletionDecision::Idle, CompletionDecision::StartNext);
                Ok(CancellationDecision::CancelledBeforeDispatch { next })
            }
            HubOperationState::Dispatched => {
                let operation = self
                    .operations
                    .get_mut(operation_id)
                    .expect("operation was resolved above");
                operation.state = HubOperationState::CancelRequested;
                Ok(CancellationDecision::SendCancellation(
                    operation.operation.clone(),
                ))
            }
            HubOperationState::CancelRequested => Ok(CancellationDecision::SendCancellation(
                self.operations[operation_id].operation.clone(),
            )),
            terminal => Ok(CancellationDecision::AlreadyTerminal(terminal)),
        }
    }

    pub fn complete(
        &mut self,
        operation_id: &str,
        cancelled: bool,
    ) -> Result<CompletionDecision, ExecutionError> {
        let operation = self
            .operations
            .get_mut(operation_id)
            .ok_or(ExecutionError::UnknownOperation)?;
        if !matches!(
            operation.state,
            HubOperationState::Dispatched | HubOperationState::CancelRequested
        ) {
            return Err(ExecutionError::InvalidTransition);
        }
        operation.state = if cancelled {
            HubOperationState::Cancelled
        } else {
            HubOperationState::Completed
        };
        let device_id = operation.operation.device_id.clone();
        self.active_by_device.remove(&device_id);
        Ok(self
            .start_next_for_available_capacity(Some(&device_id))
            .map_or(CompletionDecision::Idle, CompletionDecision::StartNext))
    }

    pub fn mark_connection_lost(
        &mut self,
        operation_id: &str,
    ) -> Result<CompletionDecision, ExecutionError> {
        self.mark_indeterminate(operation_id)
    }

    pub fn mark_indeterminate(
        &mut self,
        operation_id: &str,
    ) -> Result<CompletionDecision, ExecutionError> {
        let operation = self
            .operations
            .get_mut(operation_id)
            .ok_or(ExecutionError::UnknownOperation)?;
        if !matches!(
            operation.state,
            HubOperationState::Dispatched | HubOperationState::CancelRequested
        ) {
            return Err(ExecutionError::InvalidTransition);
        }
        operation.state = HubOperationState::Indeterminate;
        let device_id = operation.operation.device_id.clone();
        self.active_by_device.remove(&device_id);
        self.blocked_by_indeterminate
            .insert(device_id, operation_id.to_owned());
        Ok(CompletionDecision::Idle)
    }

    pub fn resolve_indeterminate(
        &mut self,
        operation_id: &str,
        resolution: IndeterminateResolution,
    ) -> Result<CompletionDecision, ExecutionError> {
        let operation = self
            .operations
            .get_mut(operation_id)
            .ok_or(ExecutionError::UnknownOperation)?;
        if operation.state != HubOperationState::Indeterminate {
            return Err(ExecutionError::InvalidTransition);
        }
        let device_id = operation.operation.device_id.clone();
        if self
            .blocked_by_indeterminate
            .get(&device_id)
            .is_none_or(|blocked| blocked != operation_id)
        {
            return Err(ExecutionError::InvalidTransition);
        }
        operation.state = match resolution {
            IndeterminateResolution::ConfirmedCompleted => HubOperationState::Completed,
            IndeterminateResolution::ConfirmedNotExecuted => HubOperationState::Cancelled,
        };
        self.blocked_by_indeterminate.remove(&device_id);
        Ok(self
            .start_next_for_available_capacity(Some(&device_id))
            .map_or(CompletionDecision::Idle, CompletionDecision::StartNext))
    }

    pub fn state(&self, operation_id: &str) -> Option<HubOperationState> {
        self.operations.get(operation_id).map(|op| op.state)
    }

    pub fn active_count(&self) -> usize {
        self.active_by_device.len()
    }

    fn start(&mut self, operation: OperationRef) {
        self.active_by_device
            .insert(operation.device_id.clone(), operation.operation_id.clone());
        if let Some(existing) = self.operations.get_mut(&operation.operation_id) {
            existing.operation = operation;
            existing.state = HubOperationState::ActiveNotDispatched;
        } else {
            self.operations.insert(
                operation.operation_id.clone(),
                HubOperation {
                    operation,
                    state: HubOperationState::ActiveNotDispatched,
                },
            );
        }
    }

    fn start_next_for_available_capacity(
        &mut self,
        preferred_device: Option<&str>,
    ) -> Option<OperationRef> {
        if self.active_by_device.len() >= self.limits.max_global_active {
            return None;
        }

        if let Some(device_id) = preferred_device {
            if !self.active_by_device.contains_key(device_id)
                && !self.blocked_by_indeterminate.contains_key(device_id)
            {
                if let Some(next) = self.pop_queued(device_id) {
                    self.start(next.clone());
                    return Some(next);
                }
            }
        }

        let candidates: Vec<String> = self.queued_by_device.keys().cloned().collect();
        for device_id in candidates {
            if self.active_by_device.contains_key(&device_id)
                || self.blocked_by_indeterminate.contains_key(&device_id)
            {
                continue;
            }
            if let Some(next) = self.pop_queued(&device_id) {
                self.start(next.clone());
                return Some(next);
            }
        }
        None
    }

    fn pop_queued(&mut self, device_id: &str) -> Option<OperationRef> {
        let queue = self.queued_by_device.get_mut(device_id)?;
        let next = queue.pop_front();
        if queue.is_empty() {
            self.queued_by_device.remove(device_id);
        }
        next
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentOperationState {
    Running,
    CancelRequested,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentExecutionSnapshot {
    pub terminal_operation_ids: Vec<String>,
}

#[derive(Debug, Default)]
pub struct AgentExecutionGate {
    active: Option<(OperationRef, AgentOperationState)>,
    terminal_operation_ids: HashSet<String>,
}

impl AgentExecutionGate {
    pub fn snapshot_for_restart(&self) -> AgentExecutionSnapshot {
        let mut terminal_operation_ids: Vec<_> =
            self.terminal_operation_ids.iter().cloned().collect();
        if let Some((active, _)) = &self.active {
            terminal_operation_ids.push(active.operation_id.clone());
        }
        terminal_operation_ids.sort();
        terminal_operation_ids.dedup();
        AgentExecutionSnapshot {
            terminal_operation_ids,
        }
    }

    pub fn restore_after_restart(snapshot: AgentExecutionSnapshot) -> Result<Self, ExecutionError> {
        if snapshot
            .terminal_operation_ids
            .iter()
            .any(|operation_id| operation_id.trim().is_empty())
        {
            return Err(ExecutionError::InvalidSnapshot);
        }
        Ok(Self {
            active: None,
            terminal_operation_ids: snapshot.terminal_operation_ids.into_iter().collect(),
        })
    }

    pub fn begin(&mut self, operation: OperationRef) -> Result<(), ExecutionError> {
        if self
            .terminal_operation_ids
            .contains(&operation.operation_id)
        {
            return Err(ExecutionError::OperationReplay);
        }
        if self.active.is_some() {
            return Err(ExecutionError::AgentBusy);
        }
        self.active = Some((operation, AgentOperationState::Running));
        Ok(())
    }

    pub fn request_cancel(&mut self, operation_id: &str) -> Result<(), ExecutionError> {
        let (active, state) = self
            .active
            .as_mut()
            .ok_or(ExecutionError::UnknownOperation)?;
        if active.operation_id != operation_id {
            return Err(ExecutionError::UnknownOperation);
        }
        *state = AgentOperationState::CancelRequested;
        Ok(())
    }

    pub fn cancellation_requested(&self, operation_id: &str) -> bool {
        matches!(
            self.active.as_ref(),
            Some((active, AgentOperationState::CancelRequested)) if active.operation_id == operation_id
        )
    }

    pub fn finish(&mut self, operation_id: &str) -> Result<(), ExecutionError> {
        let (active, _) = self.active.take().ok_or(ExecutionError::UnknownOperation)?;
        if active.operation_id != operation_id {
            self.active = Some((active, AgentOperationState::Running));
            return Err(ExecutionError::UnknownOperation);
        }
        self.terminal_operation_ids.insert(operation_id.to_owned());
        Ok(())
    }

    pub fn abandon_on_disconnect(&mut self, operation_id: &str) -> Result<(), ExecutionError> {
        self.finish(operation_id)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExecutionError {
    InvalidGlobalLimit,
    InvalidOperation,
    OperationReplay,
    BackpressureRejected,
    UnknownOperation,
    InvalidTransition,
    AgentBusy,
    DeviceIndeterminate { operation_id: String },
    InvalidSnapshot,
}

impl fmt::Display for ExecutionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self:?}")
    }
}

impl std::error::Error for ExecutionError {}

#[cfg(test)]
mod tests {
    use super::*;

    fn op(device: &str, generation: u64, id: &str) -> OperationRef {
        OperationRef {
            device_id: device.into(),
            device_generation: generation,
            operation_id: id.into(),
        }
    }

    #[test]
    fn global_and_per_device_backpressure_are_bounded() {
        let mut hub = HubAdmissionController::new(AdmissionLimits {
            max_global_active: 1,
            max_queued_per_device: 1,
        })
        .unwrap();
        assert!(matches!(
            hub.admit(op("dev-a", 1, "a1")).unwrap(),
            AdmissionDecision::StartNow(_)
        ));
        assert_eq!(
            hub.admit(op("dev-a", 1, "a2")).unwrap(),
            AdmissionDecision::Queued { position: 1 }
        );
        assert_eq!(
            hub.admit(op("dev-a", 1, "a3")),
            Err(ExecutionError::BackpressureRejected)
        );
        assert_eq!(
            hub.admit(op("dev-b", 1, "b1")).unwrap(),
            AdmissionDecision::Queued { position: 1 }
        );
        assert_eq!(hub.active_count(), 1);
    }

    #[test]
    fn completion_starts_at_most_one_next_operation() {
        let mut hub = HubAdmissionController::new(AdmissionLimits {
            max_global_active: 1,
            max_queued_per_device: 2,
        })
        .unwrap();
        hub.admit(op("dev-a", 1, "a1")).unwrap();
        hub.mark_dispatched("a1").unwrap();
        hub.admit(op("dev-a", 1, "a2")).unwrap();
        let next = hub.complete("a1", false).unwrap();
        assert_eq!(next, CompletionDecision::StartNext(op("dev-a", 1, "a2")));
        assert_eq!(hub.state("a1"), Some(HubOperationState::Completed));
        assert_eq!(
            hub.state("a2"),
            Some(HubOperationState::ActiveNotDispatched)
        );
    }

    #[test]
    fn cancellation_before_dispatch_never_reaches_agent() {
        let mut hub = HubAdmissionController::new(AdmissionLimits {
            max_global_active: 1,
            max_queued_per_device: 1,
        })
        .unwrap();
        hub.admit(op("dev-a", 1, "a1")).unwrap();
        assert_eq!(
            hub.cancel("a1").unwrap(),
            CancellationDecision::CancelledBeforeDispatch {
                next: CompletionDecision::Idle
            }
        );
        assert_eq!(hub.state("a1"), Some(HubOperationState::Cancelled));
    }

    #[test]
    fn cancellation_before_dispatch_surfaces_the_next_queued_operation() {
        let mut hub = HubAdmissionController::new(AdmissionLimits {
            max_global_active: 1,
            max_queued_per_device: 1,
        })
        .unwrap();
        hub.admit(op("dev-a", 1, "a1")).unwrap();
        hub.admit(op("dev-a", 1, "a2")).unwrap();
        assert_eq!(
            hub.cancel("a1").unwrap(),
            CancellationDecision::CancelledBeforeDispatch {
                next: CompletionDecision::StartNext(op("dev-a", 1, "a2"))
            }
        );
    }

    #[test]
    fn cancellation_after_dispatch_is_forwarded_and_not_replayed() {
        let mut hub = HubAdmissionController::new(AdmissionLimits {
            max_global_active: 1,
            max_queued_per_device: 0,
        })
        .unwrap();
        hub.admit(op("dev-a", 4, "a1")).unwrap();
        hub.mark_dispatched("a1").unwrap();
        assert_eq!(
            hub.cancel("a1").unwrap(),
            CancellationDecision::SendCancellation(op("dev-a", 4, "a1"))
        );
        hub.complete("a1", true).unwrap();
        assert_eq!(hub.state("a1"), Some(HubOperationState::Cancelled));
        assert_eq!(
            hub.admit(op("dev-a", 4, "a1")),
            Err(ExecutionError::OperationReplay)
        );
    }

    #[test]
    fn lost_connection_marks_dispatched_operation_indeterminate_and_non_replayable() {
        let mut hub = HubAdmissionController::new(AdmissionLimits {
            max_global_active: 1,
            max_queued_per_device: 0,
        })
        .unwrap();
        hub.admit(op("dev-a", 7, "a1")).unwrap();
        hub.mark_dispatched("a1").unwrap();
        hub.mark_connection_lost("a1").unwrap();
        assert_eq!(hub.state("a1"), Some(HubOperationState::Indeterminate));
        assert_eq!(
            hub.admit(op("dev-a", 8, "a1")),
            Err(ExecutionError::OperationReplay)
        );
        assert_eq!(
            hub.admit(op("dev-a", 8, "a2")),
            Err(ExecutionError::DeviceIndeterminate {
                operation_id: "a1".into()
            })
        );
        assert_eq!(
            hub.resolve_indeterminate("a1", IndeterminateResolution::ConfirmedNotExecuted)
                .unwrap(),
            CompletionDecision::Idle
        );
        assert!(matches!(
            hub.admit(op("dev-a", 8, "a2")).unwrap(),
            AdmissionDecision::StartNow(_)
        ));
    }

    #[test]
    fn hub_restart_snapshot_never_restores_in_flight_work_as_runnable() {
        let limits = AdmissionLimits {
            max_global_active: 2,
            max_queued_per_device: 1,
        };
        let mut hub = HubAdmissionController::new(limits).unwrap();
        hub.admit(op("dev-a", 1, "not-dispatched")).unwrap();
        hub.admit(op("dev-b", 1, "dispatched")).unwrap();
        hub.mark_dispatched("dispatched").unwrap();
        let snapshot = hub.snapshot_for_restart();
        let mut restored = HubAdmissionController::restore_after_restart(limits, snapshot).unwrap();
        assert_eq!(
            restored.state("not-dispatched"),
            Some(HubOperationState::Cancelled)
        );
        assert_eq!(
            restored.state("dispatched"),
            Some(HubOperationState::Indeterminate)
        );
        assert_eq!(
            restored.admit(op("dev-a", 2, "not-dispatched")),
            Err(ExecutionError::OperationReplay)
        );
        assert_eq!(
            restored.admit(op("dev-b", 2, "dispatched")),
            Err(ExecutionError::OperationReplay)
        );
        assert_eq!(
            restored.admit(op("dev-b", 2, "new-after-restart")),
            Err(ExecutionError::DeviceIndeterminate {
                operation_id: "dispatched".into()
            })
        );
    }

    #[test]
    fn queued_operations_are_recorded_and_become_cancelled_on_restart() {
        let limits = AdmissionLimits {
            max_global_active: 1,
            max_queued_per_device: 2,
        };
        let mut hub = HubAdmissionController::new(limits).unwrap();
        hub.admit(op("dev-a", 1, "active")).unwrap();
        hub.admit(op("dev-a", 1, "queued")).unwrap();
        assert_eq!(hub.state("queued"), Some(HubOperationState::Queued));
        let restored =
            HubAdmissionController::restore_after_restart(limits, hub.snapshot_for_restart())
                .unwrap();
        assert_eq!(restored.state("queued"), Some(HubOperationState::Cancelled));
    }

    #[test]
    fn agent_restart_snapshot_marks_active_operation_terminal() {
        let mut gate = AgentExecutionGate::default();
        gate.begin(op("dev-a", 1, "a1")).unwrap();
        let snapshot = gate.snapshot_for_restart();
        let mut restored = AgentExecutionGate::restore_after_restart(snapshot).unwrap();
        assert_eq!(
            restored.begin(op("dev-a", 2, "a1")),
            Err(ExecutionError::OperationReplay)
        );
        restored.begin(op("dev-a", 2, "a2")).unwrap();
    }

    #[test]
    fn agent_enforces_single_operation_and_terminal_replay_rejection() {
        let mut gate = AgentExecutionGate::default();
        gate.begin(op("dev-a", 1, "a1")).unwrap();
        assert_eq!(
            gate.begin(op("dev-a", 1, "a2")),
            Err(ExecutionError::AgentBusy)
        );
        gate.request_cancel("a1").unwrap();
        assert!(gate.cancellation_requested("a1"));
        gate.finish("a1").unwrap();
        assert_eq!(
            gate.begin(op("dev-a", 2, "a1")),
            Err(ExecutionError::OperationReplay)
        );
        gate.begin(op("dev-a", 2, "a2")).unwrap();
    }
}
