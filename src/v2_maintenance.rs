//! Offline operator maintenance for durable V2 Hub safety state.
//! No network entrypoint is provided: maintenance shares the Hub state lock.

use crate::v2_execution_safety::{ExecutionReceipt, OperationOwner, ResolutionRecord};
use crate::v2_m0_execution::{AdmissionLimits, ExecutionError, IndeterminateResolution};
use crate::v2_m1_persistence::{CheckpointStore, HubPersistentState, PersistenceError};
use crate::v2_state_lock::{StateDirectoryLock, StateDirectoryLockError};
use std::fmt;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

const RESOLVER_ISSUER: &str = "cumg://local-maintenance";
const RESOLVER_SUBJECT: &str = "operator";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OfflineResolutionResult {
    pub receipt: ExecutionReceipt,
    pub resolution: ResolutionRecord,
    pub checkpoint: PathBuf,
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
    // Checkpoints are restart-normalized before serialization, so queue capacity
    // is irrelevant to this single offline transition. V2 remains single-active.
    let limits = AdmissionLimits {
        max_global_active: 1,
        max_queued_per_device: 1,
    };
    let (registry, mut execution) = state
        .restore(limits)
        .map_err(MaintenanceError::Persistence)?;
    let resolver = OperationOwner::new(RESOLVER_ISSUER, RESOLVER_SUBJECT)
        .map_err(MaintenanceError::Execution)?;
    let (_next, receipt) = execution
        .resolve_indeterminate(operation_id, resolver, decision, evidence, now_ms)
        .map_err(MaintenanceError::Execution)?;
    let resolution = execution
        .resolutions()
        .last()
        .cloned()
        .ok_or(MaintenanceError::MissingResolutionRecord)?;
    let checkpoint_path = checkpoint
        .save(&HubPersistentState::capture(&registry, &execution))
        .map_err(MaintenanceError::Persistence)?;
    Ok(OfflineResolutionResult {
        receipt,
        resolution,
        checkpoint: checkpoint_path,
    })
}

#[derive(Debug)]
pub enum MaintenanceError {
    StateLock(StateDirectoryLockError),
    Persistence(PersistenceError),
    Execution(ExecutionError),
    MissingResolutionRecord,
    SystemClockBeforeEpoch,
}
impl fmt::Display for MaintenanceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::StateLock(error) => write!(f, "{error}"),
            Self::Persistence(error) => write!(f, "checkpoint maintenance failed: {error}"),
            Self::Execution(error) => write!(f, "quarantine resolution rejected: {error}"),
            Self::MissingResolutionRecord => {
                f.write_str("quarantine resolution produced no audit record")
            }
            Self::SystemClockBeforeEpoch => f.write_str("system clock is before the Unix epoch"),
        }
    }
}
impl std::error::Error for MaintenanceError {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::v2_execution_safety::{AuthoritativeOperationController, OperationOwner};
    use crate::v2_m0::{DeviceCapability, DeviceIdentity, DeviceRegistry};
    use crate::v2_m0_execution::{AdmissionDecision, HubOperationState, OperationRef};

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
