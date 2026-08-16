//! V2-M1 crash-safe replay/trust checkpoint persistence.
//!
//! Checkpoints are append-only files. A new checkpoint is written with
//! `create_new`, flushed, and fsynced before it can become the highest sequence.
//! Loading rejects symlinks, oversized files, weak Unix permissions, malformed
//! state, and unsupported schema versions instead of silently falling back.

use crate::v2_execution_safety::{AuthoritativeOperationController, AuthoritativeSafetySnapshot};
use crate::v2_m0::{DeviceRegistry, DeviceRegistrySnapshot, GrantLedger, GrantLedgerSnapshot};
use crate::v2_m0_execution::{AdmissionLimits, AgentExecutionGate, AgentExecutionSnapshot};
use crate::v2_m0_trust::TrustedHubIdentity;
use crate::v2_observability::SafeErrorCode;
use ed25519_dalek::VerifyingKey;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

#[cfg(unix)]
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};

pub const M1_STATE_SCHEMA_VERSION: u16 = 5;
pub const MAX_CHECKPOINT_BYTES: u64 = 1024 * 1024;
pub const MAX_RETAINED_CHECKPOINTS: usize = 64;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentPersistentState {
    pub schema_version: u16,
    pub device_id: String,
    pub trusted_hub_public_key: [u8; 32],
    pub trusted_hub_epoch: u64,
    pub grant_ledger: GrantLedgerSnapshot,
    pub execution: AgentExecutionSnapshot,
}

impl AgentPersistentState {
    pub fn capture(
        device_id: impl Into<String>,
        trusted_hub: &TrustedHubIdentity,
        grant_ledger: &GrantLedger,
        execution: &AgentExecutionGate,
    ) -> Result<Self, PersistenceError> {
        let device_id = device_id.into();
        if device_id.trim().is_empty() {
            return Err(PersistenceError::InvalidState);
        }
        Ok(Self {
            schema_version: M1_STATE_SCHEMA_VERSION,
            device_id,
            trusted_hub_public_key: trusted_hub.verifier().to_bytes(),
            trusted_hub_epoch: trusted_hub.epoch(),
            grant_ledger: grant_ledger.snapshot(),
            execution: execution.snapshot_for_restart(),
        })
    }

    pub fn restore(
        self,
    ) -> Result<(String, TrustedHubIdentity, GrantLedger, AgentExecutionGate), PersistenceError>
    {
        validate_state_schema(self.schema_version)?;
        if self.device_id.trim().is_empty() {
            return Err(PersistenceError::InvalidState);
        }
        let hub_key = VerifyingKey::from_bytes(&self.trusted_hub_public_key)
            .map_err(|_| PersistenceError::InvalidState)?;
        let trusted_hub =
            TrustedHubIdentity::from_verifier_and_epoch(hub_key, self.trusted_hub_epoch);
        let grant_ledger = GrantLedger::from_persisted_snapshot(self.grant_ledger)
            .map_err(PersistenceError::Control)?;
        let execution = AgentExecutionGate::restore_after_restart(self.execution)
            .map_err(PersistenceError::Execution)?;
        Ok((self.device_id, trusted_hub, grant_ledger, execution))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HubPersistentState {
    pub schema_version: u16,
    pub registry: DeviceRegistrySnapshot,
    pub execution: AuthoritativeSafetySnapshot,
}

impl HubPersistentState {
    pub fn capture(
        registry: &DeviceRegistry,
        execution: &AuthoritativeOperationController,
    ) -> Self {
        Self {
            schema_version: M1_STATE_SCHEMA_VERSION,
            registry: registry.snapshot(),
            execution: execution.snapshot_for_restart(),
        }
    }

    pub fn restore(
        self,
        limits: AdmissionLimits,
    ) -> Result<(DeviceRegistry, AuthoritativeOperationController), PersistenceError> {
        validate_state_schema(self.schema_version)?;
        let mut registry = DeviceRegistry::from_persisted_snapshot(self.registry)
            .map_err(PersistenceError::Control)?;
        // A Hub process restart destroys every live transport session. Persisted
        // capability advertisements remain useful as history, but must not make
        // the device appear online until a fresh authenticated Agent reconnects.
        registry.mark_all_offline();
        let execution =
            AuthoritativeOperationController::restore_after_restart(limits, self.execution)
                .map_err(PersistenceError::Execution)?;
        Ok((registry, execution))
    }
}

#[derive(Debug, Clone)]
pub struct CheckpointStore {
    directory: PathBuf,
    prefix: String,
}

impl CheckpointStore {
    pub fn new(
        directory: impl Into<PathBuf>,
        prefix: impl Into<String>,
    ) -> Result<Self, PersistenceError> {
        let directory = directory.into();
        let prefix = prefix.into();
        if prefix.is_empty()
            || !prefix.chars().all(|character| {
                character.is_ascii_alphanumeric() || character == '-' || character == '_'
            })
        {
            return Err(PersistenceError::InvalidPrefix);
        }
        Ok(Self { directory, prefix })
    }

    pub fn save<T: Serialize>(&self, value: &T) -> Result<PathBuf, PersistenceError> {
        self.save_with_size(value).map(|(path, _)| path)
    }

    /// Save a checkpoint and return the exact serialized byte count that passed
    /// the checkpoint size gate. Hub runtime uses this to request a clean Agent
    /// generation rollover before sustained same-generation history approaches
    /// `MAX_CHECKPOINT_BYTES`; callers that do not need the size keep using
    /// `save`.
    pub fn save_with_size<T: Serialize>(
        &self,
        value: &T,
    ) -> Result<(PathBuf, usize), PersistenceError> {
        self.ensure_directory()?;
        let payload = serde_json::to_vec(value).map_err(PersistenceError::Serialization)?;
        if u64::try_from(payload.len()).unwrap_or(u64::MAX) > MAX_CHECKPOINT_BYTES {
            return Err(PersistenceError::CheckpointTooLarge);
        }
        let payload_len = payload.len();

        let mut sequence = self.next_sequence()?;
        for _ in 0..8 {
            let path = self.checkpoint_path(sequence);
            match secure_create_new(&path) {
                Ok(mut file) => {
                    file.write_all(&payload).map_err(PersistenceError::Io)?;
                    file.flush().map_err(PersistenceError::Io)?;
                    file.sync_all().map_err(PersistenceError::Io)?;
                    sync_directory(&self.directory)?;
                    self.prune_old_checkpoints(MAX_RETAINED_CHECKPOINTS)?;
                    return Ok((path, payload_len));
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                    sequence = sequence.saturating_add(1);
                }
                Err(error) => return Err(PersistenceError::Io(error)),
            }
        }
        Err(PersistenceError::SequenceContention)
    }

    pub fn load_latest<T: DeserializeOwned>(&self) -> Result<T, PersistenceError> {
        self.ensure_directory()?;
        let path = self
            .latest_checkpoint()?
            .ok_or(PersistenceError::NoCheckpoint)?;
        read_secure_json(&path)
    }

    fn ensure_directory(&self) -> Result<(), PersistenceError> {
        if !self.directory.exists() {
            let mut builder = fs::DirBuilder::new();
            builder.recursive(true);
            #[cfg(unix)]
            builder.mode(0o700);
            builder
                .create(&self.directory)
                .map_err(PersistenceError::Io)?;
        }
        let metadata = fs::symlink_metadata(&self.directory).map_err(PersistenceError::Io)?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err(PersistenceError::UnsafePath);
        }
        #[cfg(unix)]
        if metadata.permissions().mode() & 0o077 != 0 {
            return Err(PersistenceError::UnsafePermissions);
        }
        Ok(())
    }

    fn next_sequence(&self) -> Result<u64, PersistenceError> {
        Ok(self
            .latest_sequence()?
            .map_or(1, |sequence| sequence.saturating_add(1)))
    }

    fn latest_checkpoint(&self) -> Result<Option<PathBuf>, PersistenceError> {
        Ok(self
            .latest_sequence()?
            .map(|sequence| self.checkpoint_path(sequence)))
    }

    fn latest_sequence(&self) -> Result<Option<u64>, PersistenceError> {
        let mut latest = None;
        for entry in fs::read_dir(&self.directory).map_err(PersistenceError::Io)? {
            let entry = entry.map_err(PersistenceError::Io)?;
            let name = entry.file_name();
            let Some(name) = name.to_str() else { continue };
            let Some(sequence) = parse_checkpoint_name(&self.prefix, name) else {
                continue;
            };
            latest = Some(latest.map_or(sequence, |current: u64| current.max(sequence)));
        }
        Ok(latest)
    }

    fn checkpoint_path(&self, sequence: u64) -> PathBuf {
        self.directory
            .join(format!("{}-{sequence:020}.json", self.prefix))
    }

    fn prune_old_checkpoints(&self, retain: usize) -> Result<(), PersistenceError> {
        if retain == 0 {
            return Err(PersistenceError::InvalidRetention);
        }
        let mut checkpoints = Vec::new();
        for entry in fs::read_dir(&self.directory).map_err(PersistenceError::Io)? {
            let entry = entry.map_err(PersistenceError::Io)?;
            let name = entry.file_name();
            let Some(name) = name.to_str() else { continue };
            let Some(sequence) = parse_checkpoint_name(&self.prefix, name) else {
                continue;
            };
            checkpoints.push((sequence, entry.path()));
        }
        checkpoints.sort_by_key(|(sequence, _)| *sequence);
        let remove_count = checkpoints.len().saturating_sub(retain);
        for (_, path) in checkpoints.into_iter().take(remove_count) {
            let metadata = fs::symlink_metadata(&path).map_err(PersistenceError::Io)?;
            if metadata.file_type().is_symlink() || !metadata.is_file() {
                return Err(PersistenceError::UnsafePath);
            }
            fs::remove_file(path).map_err(PersistenceError::Io)?;
        }
        if remove_count > 0 {
            sync_directory(&self.directory)?;
        }
        Ok(())
    }
}

fn parse_checkpoint_name(prefix: &str, name: &str) -> Option<u64> {
    let number = name
        .strip_prefix(prefix)?
        .strip_prefix('-')?
        .strip_suffix(".json")?;
    if number.len() != 20 || !number.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    number.parse().ok()
}

fn secure_create_new(path: &Path) -> std::io::Result<File> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    options.mode(0o600);
    options.open(path)
}

fn read_secure_json<T: DeserializeOwned>(path: &Path) -> Result<T, PersistenceError> {
    let metadata = fs::symlink_metadata(path).map_err(PersistenceError::Io)?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(PersistenceError::UnsafePath);
    }
    if metadata.len() > MAX_CHECKPOINT_BYTES {
        return Err(PersistenceError::CheckpointTooLarge);
    }
    #[cfg(unix)]
    if metadata.permissions().mode() & 0o077 != 0 {
        return Err(PersistenceError::UnsafePermissions);
    }
    let file = File::open(path).map_err(PersistenceError::Io)?;
    let mut payload = Vec::with_capacity(usize::try_from(metadata.len()).unwrap_or(0));
    file.take(MAX_CHECKPOINT_BYTES + 1)
        .read_to_end(&mut payload)
        .map_err(PersistenceError::Io)?;
    if u64::try_from(payload.len()).unwrap_or(u64::MAX) > MAX_CHECKPOINT_BYTES {
        return Err(PersistenceError::CheckpointTooLarge);
    }
    serde_json::from_slice(&payload).map_err(PersistenceError::Serialization)
}

fn sync_directory(directory: &Path) -> Result<(), PersistenceError> {
    #[cfg(unix)]
    {
        File::open(directory)
            .and_then(|file| file.sync_all())
            .map_err(PersistenceError::Io)?;
    }
    Ok(())
}

fn validate_state_schema(got: u16) -> Result<(), PersistenceError> {
    if got == M1_STATE_SCHEMA_VERSION {
        Ok(())
    } else {
        Err(PersistenceError::UnsupportedSchema { got })
    }
}

pub enum PersistenceError {
    Io(std::io::Error),
    Serialization(serde_json::Error),
    Control(crate::v2_m0::ControlError),
    Execution(crate::v2_m0_execution::ExecutionError),
    UnsupportedSchema { got: u16 },
    InvalidPrefix,
    UnsafePath,
    UnsafePermissions,
    CheckpointTooLarge,
    NoCheckpoint,
    SequenceContention,
    InvalidRetention,
    InvalidState,
}

impl SafeErrorCode for PersistenceError {
    fn safe_error_code(&self) -> &'static str {
        match self {
            Self::Io(_) => "persistence_io",
            Self::Serialization(_) => "persistence_serialization",
            Self::Control(_) => "persistence_control_state",
            Self::Execution(_) => "persistence_execution_state",
            Self::UnsupportedSchema { .. } => "persistence_unsupported_schema",
            Self::InvalidPrefix => "persistence_invalid_prefix",
            Self::UnsafePath => "persistence_unsafe_path",
            Self::UnsafePermissions => "persistence_unsafe_permissions",
            Self::CheckpointTooLarge => "persistence_checkpoint_too_large",
            Self::NoCheckpoint => "persistence_no_checkpoint",
            Self::SequenceContention => "persistence_sequence_contention",
            Self::InvalidRetention => "persistence_invalid_retention",
            Self::InvalidState => "persistence_invalid_state",
        }
    }
}

impl fmt::Debug for PersistenceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.safe_error_code())
    }
}

impl fmt::Display for PersistenceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.safe_error_code())
    }
}

impl std::error::Error for PersistenceError {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::v2_execution_safety::{ExecutionEvidence, IndeterminateReason, OperationOwner};
    use crate::v2_m0::{
        CapabilityAdvertisement, CapabilityClass, DeviceCapability, DeviceIdentity, GrantAuthority,
    };
    use crate::v2_m0_execution::{AdmissionDecision, IndeterminateResolution, OperationRef};
    use crate::v2_m0_transport::HubIdentity;
    use rand::{RngCore, rngs::OsRng};
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_directory(name: &str) -> PathBuf {
        let mut random = [0_u8; 8];
        OsRng.fill_bytes(&mut random);
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!(
            "cumg-{name}-{stamp}-{}",
            u64::from_le_bytes(random)
        ))
    }

    fn enrolled_registry() -> (DeviceRegistry, DeviceIdentity, String) {
        let identity = DeviceIdentity::generate();
        let challenge = DeviceRegistry::enrollment_challenge();
        let proof = identity.enrollment_proof(&challenge);
        let mut registry = DeviceRegistry::default();
        let device_id = registry
            .enroll(&identity.public_key(), &challenge, &proof)
            .unwrap();
        (registry, identity, device_id)
    }

    #[test]
    fn checkpoint_store_round_trips_latest_append_only_state() {
        let directory = temp_directory("checkpoint");
        let store = CheckpointStore::new(&directory, "agent").unwrap();
        store.save(&serde_json::json!({"version": 1})).unwrap();
        store.save(&serde_json::json!({"version": 2})).unwrap();
        let latest: serde_json::Value = store.load_latest().unwrap();
        assert_eq!(latest["version"], 2);
        let count = fs::read_dir(&directory).unwrap().count();
        assert_eq!(count, 2);
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn checkpoint_store_reports_exact_serialized_size() {
        let directory = temp_directory("checkpoint-size");
        let store = CheckpointStore::new(&directory, "hub").unwrap();
        let value = serde_json::json!({"operation": "op-size", "state": "completed"});
        let expected = serde_json::to_vec(&value).unwrap().len();
        let (path, actual) = store.save_with_size(&value).unwrap();
        assert_eq!(actual, expected);
        assert_eq!(
            fs::metadata(path).unwrap().len(),
            u64::try_from(expected).unwrap()
        );
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn checkpoint_store_prunes_old_state_to_a_bounded_window() {
        let directory = temp_directory("checkpoint-retention");
        let store = CheckpointStore::new(&directory, "agent").unwrap();
        for version in 0..(MAX_RETAINED_CHECKPOINTS + 7) {
            store
                .save(&serde_json::json!({"version": version}))
                .unwrap();
        }
        let count = fs::read_dir(&directory).unwrap().count();
        assert_eq!(count, MAX_RETAINED_CHECKPOINTS);
        let latest: serde_json::Value = store.load_latest().unwrap();
        assert_eq!(latest["version"], MAX_RETAINED_CHECKPOINTS + 6);
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn agent_state_restore_preserves_grant_and_operation_replay_barriers() {
        let hub = HubIdentity::generate();
        let trusted_hub = TrustedHubIdentity::new(hub.verifier());
        let authority = GrantAuthority::generate();
        let mut grants = GrantLedger::new(authority.verifier());
        let consumed = authority
            .issue("dev-a", CapabilityClass::Observe, 1_000, 30_000)
            .unwrap();
        grants
            .authorize_once(&consumed, "dev-a", CapabilityClass::Observe, 1_001)
            .unwrap();
        let mut execution = AgentExecutionGate::default();
        execution
            .begin(OperationRef {
                device_id: "dev-a".into(),
                device_generation: 2,
                operation_id: "op-active".into(),
            })
            .unwrap();
        let state =
            AgentPersistentState::capture("dev-a", &trusted_hub, &grants, &execution).unwrap();
        let (_, restored_hub, mut restored_grants, mut restored_execution) =
            state.restore().unwrap();
        assert_eq!(restored_hub.verifier(), hub.verifier());
        assert_eq!(
            restored_grants.authorize_once(&consumed, "dev-a", CapabilityClass::Observe, 1_002,),
            Err(crate::v2_m0::ControlError::GrantReplay)
        );
        assert_eq!(
            restored_execution.begin(OperationRef {
                device_id: "dev-a".into(),
                device_generation: 2,
                operation_id: "op-active".into(),
            }),
            Err(crate::v2_m0_execution::ExecutionError::OperationReplay)
        );
        // Generation 3 is a fresh authenticated session; stale generation-2
        // commands are rejected before the Agent execution gate, so its old
        // operation tombstone can be dropped safely.
        restored_execution
            .begin(OperationRef {
                device_id: "dev-a".into(),
                device_generation: 3,
                operation_id: "op-active".into(),
            })
            .unwrap();
    }

    #[test]
    fn historical_agent_checkpoint_preserves_grant_and_operation_replay_barriers() {
        let hub = HubIdentity::generate();
        let trusted_hub = TrustedHubIdentity::new(hub.verifier());
        let authority = GrantAuthority::generate();
        let mut grants = GrantLedger::new(authority.verifier());
        let consumed = authority
            .issue("dev-a", CapabilityClass::Observe, 1_000, 30_000)
            .unwrap();
        grants
            .authorize_once(&consumed, "dev-a", CapabilityClass::Observe, 1_001)
            .unwrap();
        let mut execution = AgentExecutionGate::default();
        execution
            .begin(OperationRef {
                device_id: "dev-a".into(),
                device_generation: 2,
                operation_id: "op-v020-active".into(),
            })
            .unwrap();
        let mut state =
            AgentPersistentState::capture("dev-a", &trusted_hub, &grants, &execution).unwrap();
        state.grant_ledger.schema_version = 2;

        let (_, _, mut restored_grants, mut restored_execution) = state.restore().unwrap();
        assert_eq!(
            restored_grants.authorize_once(&consumed, "dev-a", CapabilityClass::Observe, 1_002),
            Err(crate::v2_m0::ControlError::GrantReplay)
        );
        assert_eq!(
            restored_execution.begin(OperationRef {
                device_id: "dev-a".into(),
                device_generation: 2,
                operation_id: "op-v020-active".into(),
            }),
            Err(crate::v2_m0_execution::ExecutionError::OperationReplay)
        );
    }

    #[test]
    fn historical_hub_checkpoints_preserve_ambiguity_receipts_and_resolution_audit() {
        let (mut registry, _identity, device_id) = enrolled_registry();
        registry
            .connect(
                &device_id,
                CapabilityAdvertisement {
                    backend: "cua".into(),
                    backend_version: "0.19.3".into(),
                    platform: "darwin-arm64".into(),
                    capability_schema_version: crate::v2_m0::CAPABILITY_SCHEMA_VERSION,
                    revision: 1,
                    supported: vec![DeviceCapability::PointerClick],
                },
            )
            .unwrap();

        let limits = AdmissionLimits {
            max_global_active: 1,
            max_queued_per_device: 2,
        };
        let mut execution = AuthoritativeOperationController::new(limits).unwrap();
        let owner = OperationOwner::new("https://issuer.example", "alice").unwrap();

        let resolved_op = OperationRef {
            device_id: device_id.clone(),
            device_generation: 1,
            operation_id: "op-resolved-audit".into(),
        };
        execution
            .prepare(
                resolved_op,
                owner.clone(),
                DeviceCapability::PointerClick,
                10,
            )
            .unwrap();
        execution
            .mark_dispatched("op-resolved-audit", &owner, 1, 11)
            .unwrap();
        execution
            .mark_indeterminate(
                "op-resolved-audit",
                &owner,
                1,
                IndeterminateReason::ConnectionLost,
                12,
            )
            .unwrap();
        execution
            .resolve_indeterminate(
                "op-resolved-audit",
                owner.clone(),
                IndeterminateResolution::ConfirmedCompleted,
                "operator reconciled external state",
                13,
            )
            .unwrap();

        let ambiguous_op = OperationRef {
            device_id: device_id.clone(),
            device_generation: 1,
            operation_id: "op-still-ambiguous".into(),
        };
        execution
            .prepare(
                ambiguous_op,
                owner.clone(),
                DeviceCapability::PointerClick,
                14,
            )
            .unwrap();
        execution
            .mark_dispatched("op-still-ambiguous", &owner, 1, 15)
            .unwrap();
        execution
            .mark_indeterminate(
                "op-still-ambiguous",
                &owner,
                1,
                IndeterminateReason::ConnectionLost,
                16,
            )
            .unwrap();

        let current = HubPersistentState::capture(&registry, &execution);
        for (legacy_registry_schema, legacy_capability_schema) in [(2, 2), (5, 4)] {
            let mut fixture = current.clone();
            fixture.registry.schema_version = legacy_registry_schema;
            fixture.registry.devices[0]
                .capabilities
                .as_mut()
                .unwrap()
                .capability_schema_version = legacy_capability_schema;

            let (restored_registry, restored) = fixture.restore(limits).unwrap();
            let migrated_registry = restored_registry.snapshot();
            assert_eq!(migrated_registry.devices[0].generation, 1);
            assert_eq!(migrated_registry.devices[0].capabilities, None);
            assert_eq!(
                restored.state("op-still-ambiguous"),
                Some(crate::v2_m0_execution::HubOperationState::Indeterminate)
            );
            let quarantine = restored.quarantine(&device_id).unwrap();
            assert_eq!(quarantine.operation_id, "op-still-ambiguous");
            assert_eq!(quarantine.owner, owner);
            assert_eq!(restored.resolutions().len(), 1);
            assert_eq!(restored.resolutions()[0].operation_id, "op-resolved-audit");
            let receipt = restored.receipt("op-resolved-audit").unwrap();
            assert_eq!(receipt.owner, owner);
            assert_eq!(receipt.evidence, ExecutionEvidence::OperatorResolution);
        }
    }

    #[test]
    fn hub_state_restore_marks_dispatched_work_indeterminate() {
        let (registry, _identity, device_id) = enrolled_registry();
        let limits = AdmissionLimits {
            max_global_active: 1,
            max_queued_per_device: 1,
        };
        let mut execution = AuthoritativeOperationController::new(limits).unwrap();
        let operation = OperationRef {
            device_id: device_id.clone(),
            device_generation: 1,
            operation_id: "op-1".into(),
        };
        let owner = OperationOwner::new("https://issuer.example", "alice").unwrap();
        assert!(matches!(
            execution
                .prepare(operation, owner.clone(), DeviceCapability::PointerClick, 10)
                .unwrap(),
            AdmissionDecision::StartNow(_)
        ));
        execution.mark_dispatched("op-1", &owner, 1, 11).unwrap();
        let state = HubPersistentState::capture(&registry, &execution);
        let (_registry, restored) = state.restore(limits).unwrap();
        assert_eq!(
            restored.state("op-1"),
            Some(crate::v2_m0_execution::HubOperationState::Indeterminate)
        );
        let quarantine = restored.quarantine(&device_id).unwrap();
        assert_eq!(quarantine.operation_id, "op-1");
        assert_eq!(quarantine.device_generation, 1);
    }

    #[test]
    fn historical_checkpoint_migration_is_read_only_until_a_new_append() {
        let directory = temp_directory("checkpoint-migration-append");
        let store = CheckpointStore::new(&directory, "hub").unwrap();
        let (registry, _identity, _device_id) = enrolled_registry();
        let limits = AdmissionLimits {
            max_global_active: 1,
            max_queued_per_device: 1,
        };
        let execution = AuthoritativeOperationController::new(limits).unwrap();
        let mut legacy = HubPersistentState::capture(&registry, &execution);
        legacy.registry.schema_version = 5;
        let old_path = store.save(&legacy).unwrap();
        let old_bytes = fs::read(&old_path).unwrap();

        let loaded: HubPersistentState = store.load_latest().unwrap();
        let (restored_registry, restored_execution) = loaded.restore(limits).unwrap();
        assert_eq!(fs::read(&old_path).unwrap(), old_bytes);
        assert_eq!(fs::read_dir(&directory).unwrap().count(), 1);

        let migrated = HubPersistentState::capture(&restored_registry, &restored_execution);
        assert_eq!(
            migrated.registry.schema_version,
            crate::v2_m0::DEVICE_REGISTRY_SNAPSHOT_SCHEMA_VERSION
        );
        let new_path = store.save(&migrated).unwrap();
        assert_ne!(new_path, old_path);
        assert!(old_path.exists());
        assert_eq!(fs::read_dir(&directory).unwrap().count(), 2);
        let latest: HubPersistentState = store.load_latest().unwrap();
        assert_eq!(
            latest.registry.schema_version,
            crate::v2_m0::DEVICE_REGISTRY_SNAPSHOT_SCHEMA_VERSION
        );
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn rejected_historical_checkpoint_is_not_rewritten() {
        let directory = temp_directory("checkpoint-migration-reject");
        let store = CheckpointStore::new(&directory, "hub").unwrap();
        let (mut registry, _identity, device_id) = enrolled_registry();
        registry
            .connect(
                &device_id,
                CapabilityAdvertisement {
                    backend: "cua".into(),
                    backend_version: "0.19.3".into(),
                    platform: "darwin-arm64".into(),
                    capability_schema_version: crate::v2_m0::CAPABILITY_SCHEMA_VERSION,
                    revision: 1,
                    supported: vec![DeviceCapability::PointerClick],
                },
            )
            .unwrap();
        let limits = AdmissionLimits {
            max_global_active: 1,
            max_queued_per_device: 1,
        };
        let execution = AuthoritativeOperationController::new(limits).unwrap();
        let mut invalid = HubPersistentState::capture(&registry, &execution);
        // control schema 2 historically paired with capability schema 2, so
        // schema 2 + capability schema 4 must never be silently reinterpreted.
        invalid.registry.schema_version = 2;
        let path = store.save(&invalid).unwrap();
        let before = fs::read(&path).unwrap();

        let loaded: HubPersistentState = store.load_latest().unwrap();
        assert!(matches!(
            loaded.restore(limits),
            Err(PersistenceError::Control(
                crate::v2_m0::ControlError::InvalidRegistrySnapshot
            ))
        ));
        assert_eq!(fs::read(&path).unwrap(), before);
        assert_eq!(fs::read_dir(&directory).unwrap().count(), 1);
        fs::remove_dir_all(directory).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn weak_checkpoint_permissions_fail_closed() {
        let directory = temp_directory("permissions");
        let store = CheckpointStore::new(&directory, "hub").unwrap();
        let path = store.save(&serde_json::json!({"ok": true})).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        let result: Result<serde_json::Value, _> = store.load_latest();
        assert!(matches!(result, Err(PersistenceError::UnsafePermissions)));
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn persistence_error_formatting_does_not_expose_io_path_or_contents() {
        let marker = "/secret/path/oauth-token.txt contents=SUPER_SECRET";
        let error = PersistenceError::Io(std::io::Error::other(marker));
        assert_eq!(format!("{error:?}"), "persistence_io");
        assert_eq!(error.to_string(), "persistence_io");
        assert!(!format!("{error:?} {error}").contains(marker));
    }
}
