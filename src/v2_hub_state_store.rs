//! Provider-neutral authoritative Hub state store with monotonic revision and writer fencing.
//!
//! The store owns persistence ordering only. CUMG's operation/quarantine/replay semantics remain
//! encoded by `HubPersistentState`; a commit publishes that complete snapshot atomically or fails.
//! Hosted backends must provide the same compare-and-commit contract.

use crate::v2_m1_persistence::{
    CheckpointStore, HUB_M1_STATE_SCHEMA_VERSION, HUB_PERSISTENCE_FENCE_SCHEMA_VERSION,
    HubPersistenceFenceSnapshot, HubPersistentState, PersistenceError,
};
use crate::v2_observability::SafeErrorCode;
use async_trait::async_trait;
use std::{
    fmt,
    path::PathBuf,
    sync::{Arc, Mutex},
};

pub const HUB_DURABLE_RECORD_SCHEMA_VERSION: u16 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HubStateRevision(pub u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HubWriterEpoch(pub u64);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DurableHubState {
    pub store_schema_version: u16,
    pub revision: HubStateRevision,
    pub writer_epoch: HubWriterEpoch,
    pub state: HubPersistentState,
}

impl DurableHubState {
    pub(crate) fn committed(
        revision: HubStateRevision,
        writer_epoch: HubWriterEpoch,
        mut state: HubPersistentState,
    ) -> Result<Self, HubStateStoreError> {
        if revision.0 == 0 || writer_epoch.0 == 0 {
            return Err(HubStateStoreError::InvalidState);
        }
        state.schema_version = HUB_M1_STATE_SCHEMA_VERSION;
        state.durable_fence = Some(HubPersistenceFenceSnapshot {
            schema_version: HUB_PERSISTENCE_FENCE_SCHEMA_VERSION,
            revision: revision.0,
            writer_epoch: writer_epoch.0,
        });
        let record = Self {
            store_schema_version: HUB_DURABLE_RECORD_SCHEMA_VERSION,
            revision,
            writer_epoch,
            state,
        };
        record.validate_committed()?;
        Ok(record)
    }

    fn legacy(state: HubPersistentState) -> Self {
        Self {
            store_schema_version: HUB_DURABLE_RECORD_SCHEMA_VERSION,
            revision: HubStateRevision(0),
            writer_epoch: HubWriterEpoch(0),
            state,
        }
    }

    pub(crate) fn validate_committed(&self) -> Result<(), HubStateStoreError> {
        if self.store_schema_version != HUB_DURABLE_RECORD_SCHEMA_VERSION
            || self.revision.0 == 0
            || self.writer_epoch.0 == 0
        {
            return Err(HubStateStoreError::InvalidState);
        }
        let fence = self
            .state
            .durable_fence
            .ok_or(HubStateStoreError::InvalidState)?;
        fence.validate()?;
        if self.state.schema_version != HUB_M1_STATE_SCHEMA_VERSION
            || fence.schema_version != HUB_PERSISTENCE_FENCE_SCHEMA_VERSION
            || fence.revision != self.revision.0
            || fence.writer_epoch != self.writer_epoch.0
        {
            return Err(HubStateStoreError::InvalidState);
        }
        Ok(())
    }

    pub const fn is_legacy_unfenced(&self) -> bool {
        self.revision.0 == 0 && self.writer_epoch.0 == 0
    }

    pub const fn lease(&self) -> HubWriterLease {
        HubWriterLease {
            epoch: self.writer_epoch,
            revision: self.revision,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HubWriterLease {
    pub epoch: HubWriterEpoch,
    pub revision: HubStateRevision,
}

pub trait HubAuthoritativeStateStore: Send + Sync {
    /// Read the exact latest committed snapshot. Implementations must never silently fall back to an
    /// older committed snapshot when the latest record is unavailable, corrupt, or ambiguous.
    fn load_current(&self) -> Result<Option<DurableHubState>, HubStateStoreError>;

    /// Acquire a strictly newer writer epoch and durably publish it with the unchanged authoritative
    /// state. If the store is empty, `initial_state` is published as the first authoritative state.
    /// The returned record is the durable read-after-commit result and its revision is the exact
    /// revision against which the first mutation commits.
    fn acquire_writer(
        &self,
        initial_state: &HubPersistentState,
    ) -> Result<DurableHubState, HubStateStoreError>;

    /// Atomically publish a complete new state iff both revision and writer epoch still match.
    /// Implementations must verify the committed record with a durable read before returning success.
    ///
    /// Error classification is part of the safety contract:
    /// - `Unavailable` means the provider can prove that no candidate became authoritative;
    /// - revision/epoch conflicts mean another writer won and the current writer is stale;
    /// - any commit whose publication may have happened but cannot be durably verified must return
    ///   `ReadAfterCommitMismatch`, never `Unavailable`.
    fn compare_and_commit(
        &self,
        lease: HubWriterLease,
        state: &HubPersistentState,
    ) -> Result<DurableHubState, HubStateStoreError>;
}

#[async_trait]
pub trait AsyncHubAuthoritativeStateStore: Send + Sync {
    /// Async hosted equivalent of `HubAuthoritativeStateStore::load_current`.
    async fn load_current_async(&self) -> Result<Option<DurableHubState>, HubStateStoreError>;

    /// Acquire and durably publish a strictly newer writer epoch without blocking a Tokio worker.
    async fn acquire_writer_async(
        &self,
        initial_state: &HubPersistentState,
    ) -> Result<DurableHubState, HubStateStoreError>;

    /// Compare-and-commit through an external provider. A returned success must already include
    /// provider read-after-commit verification; ambiguous publication must fail closed.
    async fn compare_and_commit_async(
        &self,
        lease: HubWriterLease,
        state: &HubPersistentState,
    ) -> Result<DurableHubState, HubStateStoreError>;
}

#[derive(Debug)]
pub enum HubStateStoreError {
    Persistence(PersistenceError),
    Unavailable,
    RevisionConflict,
    StaleWriter,
    RevisionOverflow,
    EpochOverflow,
    InvalidState,
    InvalidConfiguration,
    ProviderSchemaMismatch,
    ReadAfterCommitMismatch,
}

impl HubStateStoreError {
    pub const fn fences_current_writer(&self) -> bool {
        matches!(
            self,
            Self::RevisionConflict
                | Self::StaleWriter
                | Self::RevisionOverflow
                | Self::InvalidState
                | Self::ProviderSchemaMismatch
                | Self::ReadAfterCommitMismatch
        )
    }

    pub fn safe_error_code(&self) -> &'static str {
        match self {
            Self::Persistence(error) => error.safe_error_code(),
            Self::Unavailable => "hub_state_store_unavailable",
            Self::RevisionConflict => "hub_state_store_revision_conflict",
            Self::StaleWriter => "hub_state_store_stale_writer",
            Self::RevisionOverflow => "hub_state_store_revision_overflow",
            Self::EpochOverflow => "hub_state_store_epoch_overflow",
            Self::InvalidState => "hub_state_store_invalid_state",
            Self::InvalidConfiguration => "hub_state_store_invalid_configuration",
            Self::ProviderSchemaMismatch => "hub_state_store_provider_schema_mismatch",
            Self::ReadAfterCommitMismatch => "hub_state_store_read_after_commit_mismatch",
        }
    }
}

impl fmt::Display for HubStateStoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.safe_error_code())
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
    gate: Arc<Mutex<()>>,
}

impl LocalCheckpointHubStateStore {
    pub fn new(directory: impl Into<PathBuf>) -> Result<Self, HubStateStoreError> {
        Ok(Self {
            checkpoint: CheckpointStore::new(directory, "hub")?,
            gate: Arc::new(Mutex::new(())),
        })
    }

    fn load_compat_with_sequence_unlocked(
        &self,
    ) -> Result<Option<(u64, DurableHubState)>, HubStateStoreError> {
        let (sequence, state) = match self
            .checkpoint
            .load_latest_with_sequence::<HubPersistentState>()
        {
            Ok(value) => value,
            Err(PersistenceError::NoCheckpoint) => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        let Some(fence) = state.durable_fence else {
            if state.schema_version == HUB_M1_STATE_SCHEMA_VERSION {
                // Current Hub state is never authoritative without its fencing
                // metadata. Treating a stripped current record as legacy would
                // reset writer authority on the next acquisition.
                return Err(HubStateStoreError::InvalidState);
            }
            return Ok(Some((sequence, DurableHubState::legacy(state))));
        };
        fence.validate()?;
        let record = DurableHubState {
            store_schema_version: HUB_DURABLE_RECORD_SCHEMA_VERSION,
            revision: HubStateRevision(fence.revision),
            writer_epoch: HubWriterEpoch(fence.writer_epoch),
            state,
        };
        record.validate_committed()?;
        Ok(Some((sequence, record)))
    }

    fn load_compat_unlocked(&self) -> Result<Option<DurableHubState>, HubStateStoreError> {
        Ok(self
            .load_compat_with_sequence_unlocked()?
            .map(|(_, record)| record))
    }

    fn save_and_verify_unlocked(
        &self,
        expected_sequence: Option<u64>,
        record: &DurableHubState,
    ) -> Result<DurableHubState, HubStateStoreError> {
        record.validate_committed()?;
        self.checkpoint
            .save_if_latest_sequence_with_size(expected_sequence, &record.state)
            .map_err(|error| match error {
                PersistenceError::PublicationUncertain => {
                    HubStateStoreError::ReadAfterCommitMismatch
                }
                other => HubStateStoreError::Persistence(other),
            })?;
        let (verified_sequence, verified) = self
            .load_compat_with_sequence_unlocked()?
            .ok_or(HubStateStoreError::ReadAfterCommitMismatch)?;
        if verified_sequence != expected_sequence.unwrap_or(0).saturating_add(1)
            || verified != *record
        {
            return Err(HubStateStoreError::ReadAfterCommitMismatch);
        }
        Ok(verified)
    }

    fn classify_conditional_conflict(
        &self,
        lease: HubWriterLease,
    ) -> Result<HubStateStoreError, HubStateStoreError> {
        let current = self
            .load_compat_unlocked()?
            .ok_or(HubStateStoreError::RevisionConflict)?;
        Ok(if current.writer_epoch != lease.epoch {
            HubStateStoreError::StaleWriter
        } else {
            HubStateStoreError::RevisionConflict
        })
    }
}

impl HubAuthoritativeStateStore for LocalCheckpointHubStateStore {
    fn load_current(&self) -> Result<Option<DurableHubState>, HubStateStoreError> {
        let _guard = self
            .gate
            .lock()
            .map_err(|_| HubStateStoreError::InvalidState)?;
        self.load_compat_unlocked()
    }

    fn acquire_writer(
        &self,
        initial_state: &HubPersistentState,
    ) -> Result<DurableHubState, HubStateStoreError> {
        let _guard = self
            .gate
            .lock()
            .map_err(|_| HubStateStoreError::InvalidState)?;
        let current = self.load_compat_with_sequence_unlocked()?;
        let (expected_sequence, revision, epoch, state) = match current {
            Some((sequence, current)) => (
                Some(sequence),
                current
                    .revision
                    .0
                    .checked_add(1)
                    .ok_or(HubStateStoreError::RevisionOverflow)?,
                current
                    .writer_epoch
                    .0
                    .checked_add(1)
                    .ok_or(HubStateStoreError::EpochOverflow)?,
                current.state,
            ),
            None => (None, 1, 1, initial_state.clone()),
        };
        let record =
            DurableHubState::committed(HubStateRevision(revision), HubWriterEpoch(epoch), state)?;
        match self.save_and_verify_unlocked(expected_sequence, &record) {
            Err(HubStateStoreError::Persistence(PersistenceError::ConditionalConflict)) => {
                Err(HubStateStoreError::RevisionConflict)
            }
            result => result,
        }
    }

    fn compare_and_commit(
        &self,
        lease: HubWriterLease,
        state: &HubPersistentState,
    ) -> Result<DurableHubState, HubStateStoreError> {
        let _guard = self
            .gate
            .lock()
            .map_err(|_| HubStateStoreError::InvalidState)?;
        let (sequence, current) = self
            .load_compat_with_sequence_unlocked()?
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
        let record =
            DurableHubState::committed(HubStateRevision(revision), lease.epoch, state.clone())?;
        match self.save_and_verify_unlocked(Some(sequence), &record) {
            Err(HubStateStoreError::Persistence(PersistenceError::ConditionalConflict)) => {
                Err(self.classify_conditional_conflict(lease)?)
            }
            result => result,
        }
    }
}

#[derive(Default)]
struct MemoryHubStateStoreInner {
    current: Option<DurableHubState>,
    fail_next_commit: bool,
}

#[derive(Clone, Default)]
pub struct MemoryHubStateStore {
    inner: Arc<Mutex<MemoryHubStateStoreInner>>,
}

impl MemoryHubStateStore {
    pub fn seeded(state: HubPersistentState) -> Self {
        let current = DurableHubState::committed(HubStateRevision(1), HubWriterEpoch(1), state)
            .expect("seed state must be valid");
        Self {
            inner: Arc::new(Mutex::new(MemoryHubStateStoreInner {
                current: Some(current),
                fail_next_commit: false,
            })),
        }
    }

    #[cfg(test)]
    pub fn fail_next_commit(&self) {
        self.inner
            .lock()
            .expect("memory Hub state store lock")
            .fail_next_commit = true;
    }
}

impl HubAuthoritativeStateStore for MemoryHubStateStore {
    fn load_current(&self) -> Result<Option<DurableHubState>, HubStateStoreError> {
        Ok(self
            .inner
            .lock()
            .map_err(|_| HubStateStoreError::InvalidState)?
            .current
            .clone())
    }

    fn acquire_writer(
        &self,
        initial_state: &HubPersistentState,
    ) -> Result<DurableHubState, HubStateStoreError> {
        let mut guard = self
            .inner
            .lock()
            .map_err(|_| HubStateStoreError::InvalidState)?;
        let (revision, epoch, state) = match guard.current.as_ref() {
            Some(current) => (
                current
                    .revision
                    .0
                    .checked_add(1)
                    .ok_or(HubStateStoreError::RevisionOverflow)?,
                current
                    .writer_epoch
                    .0
                    .checked_add(1)
                    .ok_or(HubStateStoreError::EpochOverflow)?,
                current.state.clone(),
            ),
            None => (1, 1, initial_state.clone()),
        };
        let committed =
            DurableHubState::committed(HubStateRevision(revision), HubWriterEpoch(epoch), state)?;
        guard.current = Some(committed.clone());
        Ok(committed)
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
        let current = guard
            .current
            .as_ref()
            .ok_or(HubStateStoreError::RevisionConflict)?;
        if current.writer_epoch != lease.epoch {
            return Err(HubStateStoreError::StaleWriter);
        }
        if current.revision != lease.revision {
            return Err(HubStateStoreError::RevisionConflict);
        }
        if guard.fail_next_commit {
            guard.fail_next_commit = false;
            return Err(HubStateStoreError::Unavailable);
        }
        let revision = current
            .revision
            .0
            .checked_add(1)
            .ok_or(HubStateStoreError::RevisionOverflow)?;
        let committed =
            DurableHubState::committed(HubStateRevision(revision), lease.epoch, state.clone())?;
        guard.current = Some(committed.clone());
        Ok(committed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        v2_execution_safety::AuthoritativeOperationController,
        v2_m0::{DeviceIdentity, DeviceRegistry},
        v2_m0_execution::AdmissionLimits,
        v2_m1_persistence::M1_STATE_SCHEMA_VERSION,
    };
    use std::thread;

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

    fn temp_root(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "cumg-hub-store-{name}-{}-{}",
            std::process::id(),
            rand::random::<u64>()
        ));
        std::fs::create_dir_all(&root).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        root
    }

    fn conformance(store: &dyn HubAuthoritativeStateStore) {
        let initial = state();
        let first = store.acquire_writer(&initial).unwrap();
        assert!(first.revision.0 > 0);
        assert!(first.writer_epoch.0 > 0);

        let second = store
            .compare_and_commit(first.lease(), &first.state)
            .unwrap();
        assert_eq!(second.writer_epoch, first.writer_epoch);
        assert_eq!(second.revision.0, first.revision.0 + 1);

        assert!(matches!(
            store.compare_and_commit(first.lease(), &second.state),
            Err(HubStateStoreError::RevisionConflict)
        ));

        let replacement = store.acquire_writer(&initial).unwrap();
        assert!(replacement.writer_epoch.0 > first.writer_epoch.0);
        assert!(replacement.revision.0 > second.revision.0);
        assert!(matches!(
            store.compare_and_commit(second.lease(), &replacement.state),
            Err(HubStateStoreError::StaleWriter)
        ));
    }

    #[test]
    fn memory_backend_conforms_to_revision_cas_and_writer_epoch_contract() {
        conformance(&MemoryHubStateStore::default());
    }

    #[test]
    fn conditional_write_race_has_exactly_one_winner() {
        let store = MemoryHubStateStore::default();
        let current = store.acquire_writer(&state()).unwrap();
        let left = store.clone();
        let right = store.clone();
        let lease = current.lease();
        let candidate = current.state.clone();
        let left_candidate = candidate.clone();
        let a = thread::spawn(move || left.compare_and_commit(lease, &left_candidate));
        let b = thread::spawn(move || right.compare_and_commit(lease, &candidate));
        let results = [a.join().unwrap(), b.join().unwrap()];
        assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
        assert_eq!(
            results
                .iter()
                .filter(|result| matches!(result, Err(HubStateStoreError::RevisionConflict)))
                .count(),
            1
        );
    }

    #[test]
    fn unavailable_commit_does_not_publish_candidate() {
        let store = MemoryHubStateStore::default();
        let current = store.acquire_writer(&state()).unwrap();
        let before = store.load_current().unwrap().unwrap();
        store.fail_next_commit();
        assert!(matches!(
            store.compare_and_commit(current.lease(), &current.state),
            Err(HubStateStoreError::Unavailable)
        ));
        assert_eq!(store.load_current().unwrap().unwrap(), before);
    }

    #[test]
    fn local_backend_conforms_to_revision_cas_and_writer_epoch_contract() {
        let root = temp_root("conformance");
        let store = LocalCheckpointHubStateStore::new(root.clone()).unwrap();
        conformance(&store);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn current_schema_without_fence_is_rejected_instead_of_reinterpreted_as_legacy() {
        let root = temp_root("missing-fence");
        let checkpoint = CheckpointStore::new(root.clone(), "hub").unwrap();
        checkpoint.save(&state()).unwrap();
        let store = LocalCheckpointHubStateStore::new(root.clone()).unwrap();
        assert!(matches!(
            store.load_current(),
            Err(HubStateStoreError::InvalidState)
        ));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn local_backend_migrates_legacy_checkpoint_and_keeps_old_reader_shape() {
        let root = temp_root("legacy");
        let checkpoint = CheckpointStore::new(root.clone(), "hub").unwrap();
        let mut legacy = state();
        legacy.schema_version = M1_STATE_SCHEMA_VERSION;
        legacy.durable_fence = None;
        let legacy_path = checkpoint.save(&legacy).unwrap();
        let legacy_bytes = std::fs::read(&legacy_path).unwrap();

        let store = LocalCheckpointHubStateStore::new(root.clone()).unwrap();
        let loaded = store.load_current().unwrap().unwrap();
        assert!(loaded.is_legacy_unfenced());
        let acquired = store.acquire_writer(&legacy).unwrap();
        assert_eq!(acquired.revision, HubStateRevision(1));
        assert_eq!(acquired.writer_epoch, HubWriterEpoch(1));
        assert_eq!(std::fs::read(&legacy_path).unwrap(), legacy_bytes);

        let direct: HubPersistentState = checkpoint.load_latest().unwrap();
        assert_eq!(direct.schema_version, HUB_M1_STATE_SCHEMA_VERSION);
        assert_eq!(
            direct.durable_fence,
            Some(HubPersistenceFenceSnapshot {
                schema_version: HUB_PERSISTENCE_FENCE_SCHEMA_VERSION,
                revision: 1,
                writer_epoch: 1,
            })
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn local_backend_cross_instance_compare_and_commit_race_has_one_winner() {
        let root = temp_root("local-race");
        let left = LocalCheckpointHubStateStore::new(root.clone()).unwrap();
        let right = LocalCheckpointHubStateStore::new(root.clone()).unwrap();
        let current = left.acquire_writer(&state()).unwrap();
        let lease = current.lease();
        let left_candidate = current.state.clone();
        let right_candidate = current.state.clone();
        let a = thread::spawn(move || left.compare_and_commit(lease, &left_candidate));
        let b = thread::spawn(move || right.compare_and_commit(lease, &right_candidate));
        let results = [a.join().unwrap(), b.join().unwrap()];
        assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
        assert_eq!(
            results
                .iter()
                .filter(|result| matches!(result, Err(HubStateStoreError::RevisionConflict)))
                .count(),
            1
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn local_backend_does_not_fall_back_from_malformed_latest_commit() {
        let root = temp_root("malformed-latest");
        let checkpoint = CheckpointStore::new(root.clone(), "hub").unwrap();
        let store = LocalCheckpointHubStateStore::new(root.clone()).unwrap();
        store.acquire_writer(&state()).unwrap();
        checkpoint
            .save(&serde_json::json!({"malformed_committed_candidate": true}))
            .unwrap();
        assert!(matches!(
            store.load_current(),
            Err(HubStateStoreError::Persistence(
                PersistenceError::Serialization(_)
            ))
        ));
        let _ = std::fs::remove_dir_all(root);
    }
}
