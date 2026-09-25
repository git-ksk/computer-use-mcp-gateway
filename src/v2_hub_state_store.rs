//! Provider-neutral authoritative Hub state store with monotonic revision and writer fencing.
//!
//! The store owns persistence ordering only. CUMG's operation/quarantine/replay semantics remain
//! encoded by `HubPersistentState`; a commit publishes that complete snapshot atomically or fails.
//! Hosted backends must provide the same compare-and-commit contract.

use crate::v2_m1_persistence::{CheckpointStore, HubPersistentState, PersistenceError};
use serde::{Deserialize, Serialize};
use std::{
    fmt,
    path::PathBuf,
    sync::{Arc, Mutex},
};

pub const HUB_DURABLE_RECORD_SCHEMA_VERSION: u16 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct HubStateRevision(pub u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct HubWriterEpoch(pub u64);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DurableHubState {
    pub store_schema_version: u16,
    pub revision: HubStateRevision,
    pub writer_epoch: HubWriterEpoch,
    pub state: HubPersistentState,
}

impl DurableHubState {
    fn validate(&self) -> Result<(), HubStateStoreError> {
        if self.store_schema_version != HUB_DURABLE_RECORD_SCHEMA_VERSION
            || self.revision.0 == 0
            || self.writer_epoch.0 == 0
        {
            return Err(HubStateStoreError::InvalidState);
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HubWriterLease {
    pub epoch: HubWriterEpoch,
    pub revision: HubStateRevision,
}

pub trait HubAuthoritativeStateStore: Send + Sync {
    fn load_current(&self) -> Result<Option<DurableHubState>, HubStateStoreError>;

    /// Acquire a strictly newer writer epoch and durably publish it with the unchanged authoritative
    /// state. The returned revision is the exact revision against which the first mutation commits.
    fn acquire_writer(&self) -> Result<Option<HubWriterLease>, HubStateStoreError>;

    /// Atomically publish a complete new state iff both revision and writer epoch still match.
    /// Implementations must verify the committed record with a durable read before returning success.
    fn compare_and_commit(
        &self,
        lease: HubWriterLease,
        state: &HubPersistentState,
    ) -> Result<DurableHubState, HubStateStoreError>;
}

#[derive(Debug)]
pub enum HubStateStoreError {
    Persistence(PersistenceError),
    RevisionConflict,
    StaleWriter,
    RevisionOverflow,
    EpochOverflow,
    InvalidState,
    ReadAfterCommitMismatch,
}

impl fmt::Display for HubStateStoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Persistence(error) => write!(f, "hub_state_store_persistence:{error:?}"),
            Self::RevisionConflict => f.write_str("hub_state_store_revision_conflict"),
            Self::StaleWriter => f.write_str("hub_state_store_stale_writer"),
            Self::RevisionOverflow => f.write_str("hub_state_store_revision_overflow"),
            Self::EpochOverflow => f.write_str("hub_state_store_epoch_overflow"),
            Self::InvalidState => f.write_str("hub_state_store_invalid_state"),
            Self::ReadAfterCommitMismatch => {
                f.write_str("hub_state_store_read_after_commit_mismatch")
            }
        }
    }
}

impl std::error::Error for HubStateStoreError {}

impl From<PersistenceError> for HubStateStoreError {
    fn from(value: PersistenceError) -> Self {
        Self::Persistence(value)
    }
}

#[derive(Clone)]
pub struct LocalCheckpointHubStateStore {
    checkpoint: CheckpointStore,
}

impl LocalCheckpointHubStateStore {
    pub fn new(directory: impl Into<PathBuf>) -> Result<Self, HubStateStoreError> {
        Ok(Self {
            checkpoint: CheckpointStore::new(directory, "hub")?,
        })
    }

    fn load_compat(&self) -> Result<Option<DurableHubState>, HubStateStoreError> {
        let value = match self.checkpoint.load_latest::<serde_json::Value>() {
            Ok(value) => value,
            Err(PersistenceError::NoCheckpoint) => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        if let Ok(record) = serde_json::from_value::<DurableHubState>(value.clone()) {
            record.validate()?;
            return Ok(Some(record));
        }
        let legacy = serde_json::from_value::<HubPersistentState>(value)
            .map_err(|_| HubStateStoreError::InvalidState)?;
        // Legacy checkpoints have no writer authority. They are migration input only; acquiring a
        // writer publishes the first fenced record before any later mutation can dispatch.
        Ok(Some(DurableHubState {
            store_schema_version: HUB_DURABLE_RECORD_SCHEMA_VERSION,
            revision: HubStateRevision(0),
            writer_epoch: HubWriterEpoch(0),
            state: legacy,
        }))
    }

    fn save_and_verify(
        &self,
        record: &DurableHubState,
    ) -> Result<DurableHubState, HubStateStoreError> {
        record.validate()?;
        self.checkpoint.save(record)?;
        let verified = self
            .load_compat()?
            .ok_or(HubStateStoreError::ReadAfterCommitMismatch)?;
        if verified != *record {
            return Err(HubStateStoreError::ReadAfterCommitMismatch);
        }
        Ok(verified)
    }
}

impl HubAuthoritativeStateStore for LocalCheckpointHubStateStore {
    fn load_current(&self) -> Result<Option<DurableHubState>, HubStateStoreError> {
        self.load_compat()
    }

    fn acquire_writer(&self) -> Result<Option<HubWriterLease>, HubStateStoreError> {
        let Some(current) = self.load_compat()? else {
            return Ok(None);
        };
        let revision = current
            .revision
            .0
            .checked_add(1)
            .ok_or(HubStateStoreError::RevisionOverflow)?;
        let epoch = current
            .writer_epoch
            .0
            .checked_add(1)
            .ok_or(HubStateStoreError::EpochOverflow)?;
        let committed = self.save_and_verify(&DurableHubState {
            store_schema_version: HUB_DURABLE_RECORD_SCHEMA_VERSION,
            revision: HubStateRevision(revision),
            writer_epoch: HubWriterEpoch(epoch),
            state: current.state,
        })?;
        Ok(Some(HubWriterLease {
            epoch: committed.writer_epoch,
            revision: committed.revision,
        }))
    }

    fn compare_and_commit(
        &self,
        lease: HubWriterLease,
        state: &HubPersistentState,
    ) -> Result<DurableHubState, HubStateStoreError> {
        let current = self
            .load_compat()?
            .ok_or(HubStateStoreError::RevisionConflict)?;
        if current.writer_epoch != lease.epoch {
            return Err(HubStateStoreError::StaleWriter);
        }
        if current.revision != lease.revision {
            return Err(HubStateStoreError::RevisionConflict);
        }
        let revision = current
            .revision
            .0
            .checked_add(1)
            .ok_or(HubStateStoreError::RevisionOverflow)?;
        self.save_and_verify(&DurableHubState {
            store_schema_version: HUB_DURABLE_RECORD_SCHEMA_VERSION,
            revision: HubStateRevision(revision),
            writer_epoch: lease.epoch,
            state: state.clone(),
        })
    }
}

#[derive(Clone, Default)]
pub struct MemoryHubStateStore {
    inner: Arc<Mutex<Option<DurableHubState>>>,
}

impl MemoryHubStateStore {
    pub fn seeded(state: HubPersistentState) -> Self {
        Self {
            inner: Arc::new(Mutex::new(Some(DurableHubState {
                store_schema_version: HUB_DURABLE_RECORD_SCHEMA_VERSION,
                revision: HubStateRevision(1),
                writer_epoch: HubWriterEpoch(1),
                state,
            }))),
        }
    }
}

impl HubAuthoritativeStateStore for MemoryHubStateStore {
    fn load_current(&self) -> Result<Option<DurableHubState>, HubStateStoreError> {
        Ok(self
            .inner
            .lock()
            .map_err(|_| HubStateStoreError::InvalidState)?
            .clone())
    }

    fn acquire_writer(&self) -> Result<Option<HubWriterLease>, HubStateStoreError> {
        let mut guard = self
            .inner
            .lock()
            .map_err(|_| HubStateStoreError::InvalidState)?;
        let Some(current) = guard.as_mut() else {
            return Ok(None);
        };
        current.revision = HubStateRevision(
            current
                .revision
                .0
                .checked_add(1)
                .ok_or(HubStateStoreError::RevisionOverflow)?,
        );
        current.writer_epoch = HubWriterEpoch(
            current
                .writer_epoch
                .0
                .checked_add(1)
                .ok_or(HubStateStoreError::EpochOverflow)?,
        );
        Ok(Some(HubWriterLease {
            epoch: current.writer_epoch,
            revision: current.revision,
        }))
    }

    fn compare_and_commit(
        &self,
        lease: HubWriterLease,
        state: &HubPersistentState,
    ) -> Result<DurableHubState, HubStateStoreError> {
        let mut guard = self
            .inner
            .lock()
            .map_err(|_| HubStateStoreError::InvalidState)?;
        let current = guard.as_mut().ok_or(HubStateStoreError::RevisionConflict)?;
        if current.writer_epoch != lease.epoch {
            return Err(HubStateStoreError::StaleWriter);
        }
        if current.revision != lease.revision {
            return Err(HubStateStoreError::RevisionConflict);
        }
        current.revision = HubStateRevision(
            current
                .revision
                .0
                .checked_add(1)
                .ok_or(HubStateStoreError::RevisionOverflow)?,
        );
        current.state = state.clone();
        Ok(current.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        v2_execution_safety::AuthoritativeOperationController,
        v2_m0::{DeviceIdentity, DeviceRegistry},
        v2_m0_execution::AdmissionLimits,
    };

    fn state() -> HubPersistentState {
        let identity = DeviceIdentity::generate();
        let mut registry = DeviceRegistry::default();
        registry.provision_trusted_device(identity.verifying_key());
        let execution = AuthoritativeOperationController::new(AdmissionLimits {
            max_global_active: 1,
            max_queued_per_device: 2,
        })
        .unwrap();
        HubPersistentState::capture(&registry, &execution)
    }

    fn conformance(store: &dyn HubAuthoritativeStateStore) {
        let first = store.load_current().unwrap().unwrap();
        let lease = store.acquire_writer().unwrap().unwrap();
        assert!(lease.epoch.0 > first.writer_epoch.0);
        assert!(lease.revision.0 > first.revision.0);

        let current = store.load_current().unwrap().unwrap();
        assert_eq!(current.writer_epoch, lease.epoch);
        assert_eq!(current.revision, lease.revision);

        let committed = store.compare_and_commit(lease, &current.state).unwrap();
        assert_eq!(committed.writer_epoch, lease.epoch);
        assert_eq!(committed.revision.0, lease.revision.0 + 1);

        assert!(matches!(
            store.compare_and_commit(lease, &committed.state),
            Err(HubStateStoreError::RevisionConflict)
        ));
    }

    #[test]
    fn memory_backend_conforms_to_revision_cas_and_writer_epoch_contract() {
        let store = MemoryHubStateStore::seeded(state());
        conformance(&store);
    }

    #[test]
    fn second_writer_fences_first_writer_before_commit() {
        let store = MemoryHubStateStore::seeded(state());
        let first = store.acquire_writer().unwrap().unwrap();
        let second = store.acquire_writer().unwrap().unwrap();
        assert!(second.epoch.0 > first.epoch.0);
        let state = store.load_current().unwrap().unwrap().state;
        assert!(matches!(
            store.compare_and_commit(first, &state),
            Err(HubStateStoreError::StaleWriter)
        ));
        assert!(store.compare_and_commit(second, &state).is_ok());
    }

    #[test]
    fn local_backend_migrates_legacy_checkpoint_before_first_mutation() {
        let root = std::env::temp_dir().join(format!("cumg-hub-store-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let checkpoint = CheckpointStore::new(root.clone(), "hub").unwrap();
        checkpoint.save(&state()).unwrap();

        let store = LocalCheckpointHubStateStore::new(root.clone()).unwrap();
        let legacy = store.load_current().unwrap().unwrap();
        assert_eq!(legacy.revision, HubStateRevision(0));
        assert_eq!(legacy.writer_epoch, HubWriterEpoch(0));
        let lease = store.acquire_writer().unwrap().unwrap();
        assert_eq!(lease.revision, HubStateRevision(1));
        assert_eq!(lease.epoch, HubWriterEpoch(1));
        let migrated = store.load_current().unwrap().unwrap();
        assert_eq!(migrated.revision, HubStateRevision(1));
        assert_eq!(migrated.writer_epoch, HubWriterEpoch(1));
        assert_eq!(
            migrated.store_schema_version,
            HUB_DURABLE_RECORD_SCHEMA_VERSION
        );
        let _ = std::fs::remove_dir_all(&root);
    }
}
