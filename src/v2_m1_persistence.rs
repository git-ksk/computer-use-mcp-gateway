//! V2-M1 crash-safe replay/trust checkpoint persistence.
//!
//! Checkpoints are append-only files. A new checkpoint is written with
//! `create_new`, flushed, and fsynced before it can become the highest sequence.
//! Loading rejects symlinks, oversized files, weak Unix permissions, malformed
//! state, and unsupported schema versions instead of silently falling back.

use crate::v2_m0::{
    CONTROL_SCHEMA_VERSION, DeviceRegistry, DeviceRegistrySnapshot, GrantLedger,
    GrantLedgerSnapshot,
};
use crate::v2_m0_execution::{
    AdmissionLimits, AgentExecutionGate, AgentExecutionSnapshot, HubAdmissionController,
    HubAdmissionSnapshot,
};
use crate::v2_m0_trust::TrustedHubIdentity;
use ed25519_dalek::VerifyingKey;
use rand::{RngCore, rngs::OsRng};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

#[cfg(unix)]
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};

pub const M1_STATE_SCHEMA_VERSION: u16 = 1;
pub const MAX_CHECKPOINT_BYTES: u64 = 1024 * 1024;

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
        let grant_ledger =
            GrantLedger::from_snapshot(self.grant_ledger).map_err(PersistenceError::Control)?;
        let execution = AgentExecutionGate::restore_after_restart(self.execution)
            .map_err(PersistenceError::Execution)?;
        Ok((self.device_id, trusted_hub, grant_ledger, execution))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HubPersistentState {
    pub schema_version: u16,
    pub registry: DeviceRegistrySnapshot,
    pub admission: HubAdmissionSnapshot,
}

impl HubPersistentState {
    pub fn capture(registry: &DeviceRegistry, admission: &HubAdmissionController) -> Self {
        Self {
            schema_version: M1_STATE_SCHEMA_VERSION,
            registry: registry.snapshot(),
            admission: admission.snapshot_for_restart(),
        }
    }

    pub fn restore(
        self,
        limits: AdmissionLimits,
    ) -> Result<(DeviceRegistry, HubAdmissionController), PersistenceError> {
        validate_state_schema(self.schema_version)?;
        let registry =
            DeviceRegistry::from_snapshot(self.registry).map_err(PersistenceError::Control)?;
        let admission = HubAdmissionController::restore_after_restart(limits, self.admission)
            .map_err(PersistenceError::Execution)?;
        Ok((registry, admission))
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
        self.ensure_directory()?;
        let payload = serde_json::to_vec(value).map_err(PersistenceError::Serialization)?;
        if u64::try_from(payload.len()).unwrap_or(u64::MAX) > MAX_CHECKPOINT_BYTES {
            return Err(PersistenceError::CheckpointTooLarge);
        }

        let mut sequence = self.next_sequence()?;
        for _ in 0..8 {
            let path = self.checkpoint_path(sequence);
            match secure_create_new(&path) {
                Ok(mut file) => {
                    file.write_all(&payload).map_err(PersistenceError::Io)?;
                    file.flush().map_err(PersistenceError::Io)?;
                    file.sync_all().map_err(PersistenceError::Io)?;
                    sync_directory(&self.directory)?;
                    return Ok(path);
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
    let mut file = File::open(path).map_err(PersistenceError::Io)?;
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

#[derive(Debug)]
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
    InvalidState,
}

impl fmt::Display for PersistenceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self:?}")
    }
}

impl std::error::Error for PersistenceError {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::v2_m0::{CapabilityClass, DeviceIdentity, GrantAuthority};
    use crate::v2_m0_execution::{AdmissionDecision, OperationRef};
    use crate::v2_m0_transport::HubIdentity;
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
                device_generation: 3,
                operation_id: "op-active".into(),
            }),
            Err(crate::v2_m0_execution::ExecutionError::OperationReplay)
        );
    }

    #[test]
    fn hub_state_restore_marks_dispatched_work_indeterminate() {
        let (registry, _identity, device_id) = enrolled_registry();
        let limits = AdmissionLimits {
            max_global_active: 1,
            max_queued_per_device: 1,
        };
        let mut admission = HubAdmissionController::new(limits).unwrap();
        let operation = OperationRef {
            device_id,
            device_generation: 1,
            operation_id: "op-1".into(),
        };
        assert!(matches!(
            admission.admit(operation).unwrap(),
            AdmissionDecision::StartNow(_)
        ));
        admission.mark_dispatched("op-1").unwrap();
        let state = HubPersistentState::capture(&registry, &admission);
        let (_registry, restored) = state.restore(limits).unwrap();
        assert_eq!(
            restored.state("op-1"),
            Some(crate::v2_m0_execution::HubOperationState::Indeterminate)
        );
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
}
