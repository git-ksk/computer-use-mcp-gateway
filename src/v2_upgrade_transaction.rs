//! Read-only inspection contract for the durable single-Mac upgrade transaction.
//! The record is operational metadata only and never grants recovery, replay,
//! rollback, or mutation-authority permissions.

use serde::{Deserialize, Serialize};
use std::fmt;
use std::fs::OpenOptions;
use std::io::Read;
use std::path::{Path, PathBuf};

pub const UPGRADE_TRANSACTION_SCHEMA_VERSION: u16 = 1;
pub const MAX_UPGRADE_TRANSACTION_BYTES: u64 = 64 * 1024;
pub const UPGRADE_TRANSACTION_RELATIVE_PATH: &str = "v2/maintenance/upgrade-transaction.json";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UpgradeTransactionStatus {
    InProgress,
    Completed,
    FailedBeforeInstall,
    FailedClosedAfterStop,
    OperatorActionRequired,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UpgradeTransactionPhase {
    BuildOrStage,
    HandoffStage,
    Backup,
    ServiceDrain,
    AuthorityMigration,
    Install,
    Restart,
    PostVerify,
    Cleanup,
    Completed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UpgradeMutationAuthorityStatus {
    pub owner: Option<String>,
    pub epoch: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UpgradeCompletionContract {
    pub runtime_manifest_verified: bool,
    pub launchd_topology_safe: bool,
    pub mutation_authority_verified: bool,
    pub quarantine_clear: bool,
    pub handoff_runtime_paired: bool,
    pub services_restarted: bool,
    pub doctor_healthy: bool,
    pub cleanup_completed: bool,
    pub rollback_asset_created: bool,
}

impl UpgradeCompletionContract {
    pub const fn complete(&self) -> bool {
        self.runtime_manifest_verified
            && self.launchd_topology_safe
            && self.mutation_authority_verified
            && self.quarantine_clear
            && self.handoff_runtime_paired
            && self.services_restarted
            && self.doctor_healthy
            && self.cleanup_completed
            && self.rollback_asset_created
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UpgradeTransactionRecord {
    pub schema_version: u16,
    pub transaction_id: String,
    pub status: UpgradeTransactionStatus,
    pub phase: UpgradeTransactionPhase,
    pub started_at_ms: u64,
    pub updated_at_ms: u64,
    pub cumg_source_commit: String,
    pub handoff_source_commit: String,
    pub runtime_generation: Option<String>,
    pub rollback_asset: Option<String>,
    pub mutation_authority: UpgradeMutationAuthorityStatus,
    pub completion: UpgradeCompletionContract,
    pub failure_reason: Option<String>,
    pub operator_action: Option<String>,
}

impl UpgradeTransactionRecord {
    pub fn validate(&self) -> Result<(), UpgradeTransactionError> {
        if self.schema_version != UPGRADE_TRANSACTION_SCHEMA_VERSION {
            return Err(UpgradeTransactionError::UnsupportedSchema(
                self.schema_version,
            ));
        }
        if !safe_token(&self.transaction_id) {
            return Err(UpgradeTransactionError::InvalidRecord("transaction_id"));
        }
        if self.started_at_ms == 0 || self.updated_at_ms < self.started_at_ms {
            return Err(UpgradeTransactionError::InvalidRecord("timestamp"));
        }
        if !hex_commit(&self.cumg_source_commit) || !hex_commit(&self.handoff_source_commit) {
            return Err(UpgradeTransactionError::InvalidRecord("source_commit"));
        }
        if self
            .runtime_generation
            .as_deref()
            .is_some_and(|value| !safe_token(value))
        {
            return Err(UpgradeTransactionError::InvalidRecord("runtime_generation"));
        }
        if self
            .rollback_asset
            .as_deref()
            .is_some_and(|value| !safe_token(value))
        {
            return Err(UpgradeTransactionError::InvalidRecord("rollback_asset"));
        }
        if self
            .failure_reason
            .as_deref()
            .is_some_and(|value| !safe_reason(value))
            || self
                .operator_action
                .as_deref()
                .is_some_and(|value| !safe_reason(value))
        {
            return Err(UpgradeTransactionError::InvalidRecord("bounded_guidance"));
        }
        match (
            self.mutation_authority.owner.as_deref(),
            self.mutation_authority.epoch,
        ) {
            (None, None) => {}
            (Some("v1" | "v2"), Some(epoch)) if epoch > 0 => {}
            _ => return Err(UpgradeTransactionError::InvalidRecord("mutation_authority")),
        }
        match self.status {
            UpgradeTransactionStatus::Completed => {
                if self.phase != UpgradeTransactionPhase::Completed
                    || self.failure_reason.is_some()
                    || self.operator_action.is_some()
                    || !self.completion.complete()
                    || self.runtime_generation.is_none()
                    || self.rollback_asset.is_none()
                    || self.mutation_authority.owner.as_deref() != Some("v2")
                    || self.mutation_authority.epoch.is_none()
                {
                    return Err(UpgradeTransactionError::InvalidCompletedContract);
                }
            }
            _ if self.phase == UpgradeTransactionPhase::Completed => {
                return Err(UpgradeTransactionError::InvalidRecord("terminal_phase"));
            }
            _ => {}
        }
        Ok(())
    }
}

pub fn upgrade_transaction_path(install_root: &Path) -> PathBuf {
    install_root.join(UPGRADE_TRANSACTION_RELATIVE_PATH)
}
pub fn read_upgrade_transaction(
    path: &Path,
) -> Result<UpgradeTransactionRecord, UpgradeTransactionError> {
    let parent = path.parent().ok_or(UpgradeTransactionError::UnsafeRecord)?;
    let parent_metadata = std::fs::symlink_metadata(parent).map_err(UpgradeTransactionError::Io)?;
    if !parent_metadata.file_type().is_dir() || parent_metadata.file_type().is_symlink() {
        return Err(UpgradeTransactionError::UnsafeRecord);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if parent_metadata.uid() != unsafe { libc::geteuid() }
            || parent_metadata.mode() & 0o077 != 0
        {
            return Err(UpgradeTransactionError::UnsafeRecord);
        }
    }
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    let mut file = options.open(path).map_err(UpgradeTransactionError::Io)?;
    let metadata = file.metadata().map_err(UpgradeTransactionError::Io)?;
    if !metadata.file_type().is_file()
        || metadata.len() == 0
        || metadata.len() > MAX_UPGRADE_TRANSACTION_BYTES
    {
        return Err(UpgradeTransactionError::UnsafeRecord);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if metadata.uid() != unsafe { libc::geteuid() }
            || metadata.nlink() != 1
            || metadata.mode() & 0o077 != 0
        {
            return Err(UpgradeTransactionError::UnsafeRecord);
        }
    }
    let mut bytes = Vec::with_capacity(usize::try_from(metadata.len()).unwrap_or(0));
    file.by_ref()
        .take(MAX_UPGRADE_TRANSACTION_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(UpgradeTransactionError::Io)?;
    if bytes.len() as u64 > MAX_UPGRADE_TRANSACTION_BYTES {
        return Err(UpgradeTransactionError::UnsafeRecord);
    }
    let record: UpgradeTransactionRecord =
        serde_json::from_slice(&bytes).map_err(|_| UpgradeTransactionError::InvalidJson)?;
    record.validate()?;
    Ok(record)
}

fn hex_commit(value: &str) -> bool {
    value.len() == 40
        && value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
}

fn safe_token(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 180
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

fn safe_reason(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 80
        && value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
}

#[derive(Debug)]
pub enum UpgradeTransactionError {
    Io(std::io::Error),
    UnsafeRecord,
    InvalidJson,
    UnsupportedSchema(u16),
    InvalidRecord(&'static str),
    InvalidCompletedContract,
}

impl fmt::Display for UpgradeTransactionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(f, "upgrade transaction read failed: {error}"),
            Self::UnsafeRecord => {
                f.write_str("upgrade transaction record is unsafe or out of bounds")
            }
            Self::InvalidJson => f.write_str("upgrade transaction record is not valid strict JSON"),
            Self::UnsupportedSchema(schema) => {
                write!(f, "unsupported upgrade transaction schema {schema}")
            }
            Self::InvalidRecord(field) => write!(f, "invalid upgrade transaction field: {field}"),
            Self::InvalidCompletedContract => f.write_str(
                "completed upgrade transaction does not satisfy the completion contract",
            ),
        }
    }
}

impl std::error::Error for UpgradeTransactionError {}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn record() -> UpgradeTransactionRecord {
        UpgradeTransactionRecord {
            schema_version: 1,
            transaction_id: "upgrade-test".into(),
            status: UpgradeTransactionStatus::InProgress,
            phase: UpgradeTransactionPhase::PostVerify,
            started_at_ms: 1,
            updated_at_ms: 2,
            cumg_source_commit: "a".repeat(40),
            handoff_source_commit: "b".repeat(40),
            runtime_generation: Some("runtime-a-b".into()),
            rollback_asset: Some("runtime-upgrade-test".into()),
            mutation_authority: UpgradeMutationAuthorityStatus {
                owner: Some("v2".into()),
                epoch: Some(2),
            },
            completion: UpgradeCompletionContract {
                runtime_manifest_verified: true,
                launchd_topology_safe: true,
                mutation_authority_verified: true,
                quarantine_clear: true,
                handoff_runtime_paired: true,
                services_restarted: true,
                doctor_healthy: true,
                cleanup_completed: false,
                rollback_asset_created: true,
            },
            failure_reason: None,
            operator_action: None,
        }
    }

    #[test]
    fn completed_requires_every_completion_gate() {
        let mut value = record();
        value.status = UpgradeTransactionStatus::Completed;
        value.phase = UpgradeTransactionPhase::Completed;
        assert!(matches!(
            value.validate(),
            Err(UpgradeTransactionError::InvalidCompletedContract)
        ));
        value.completion.cleanup_completed = true;
        assert!(value.validate().is_ok());
    }

    #[test]
    fn terminal_phase_cannot_be_claimed_by_non_completed_status() {
        let mut value = record();
        value.phase = UpgradeTransactionPhase::Completed;
        assert!(matches!(
            value.validate(),
            Err(UpgradeTransactionError::InvalidRecord("terminal_phase"))
        ));
    }

    #[cfg(unix)]
    #[test]
    fn reader_rejects_group_readable_record_and_accepts_owner_private_record() {
        use std::os::unix::fs::PermissionsExt;
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!("cumg-upgrade-status-{nonce}"));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
        let path = root.join("upgrade-transaction.json");
        let mut value = record();
        value.status = UpgradeTransactionStatus::Completed;
        value.phase = UpgradeTransactionPhase::Completed;
        value.completion.cleanup_completed = true;
        std::fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o640)).unwrap();
        assert!(matches!(
            read_upgrade_transaction(&path),
            Err(UpgradeTransactionError::UnsafeRecord)
        ));
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert_eq!(read_upgrade_transaction(&path).unwrap(), value);
        let _ = std::fs::remove_dir_all(root);
    }
}
