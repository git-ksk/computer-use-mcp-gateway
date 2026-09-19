//! Bounded ephemeral data references for V2 least-privilege workspace flows.
//!
//! Public reference authority remains Hub-side because ordinary southbound grants do not
//! carry the authenticated northbound principal. The Agent owns only private staged bytes
//! and opaque locators. Neither value is a bearer capability, durable recovery truth, or
//! permission to widen the underlying device capability.

use crate::v2_execution_safety::OperationOwner;
use rand::{RngCore, rngs::OsRng};
use std::{
    collections::HashMap,
    fmt,
    fs::{self, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
};

pub const DEFAULT_EPHEMERAL_REF_TTL_MS: u64 = 5 * 60 * 1_000;
pub const DEFAULT_MAX_HUB_EPHEMERAL_REFS: usize = 1_024;
pub const DEFAULT_MAX_HUB_EPHEMERAL_REFS_PER_OWNER: usize = 128;
pub const DEFAULT_MAX_HUB_EPHEMERAL_BYTES_PER_OWNER: u64 = 16 * 1024 * 1024;
pub const DEFAULT_MAX_AGENT_EPHEMERAL_OBJECTS: usize = 1_024;
pub const DEFAULT_MAX_AGENT_EPHEMERAL_BYTES: u64 = 64 * 1024 * 1024;
pub const DEFAULT_MAX_AGENT_EPHEMERAL_OBJECT_BYTES: u64 = 8 * 1024 * 1024;
pub const DEFAULT_MAX_AGENT_EPHEMERAL_READ_BYTES: usize = 64 * 1024;
const MAX_AGENT_LOCATOR_BYTES: usize = 512;
const EPHEMERAL_DATA_CHILD: &str = "workspace-ephemeral-data";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EphemeralDataKind {
    ProcessStdout,
    ProcessStderr,
    DirectoryContinuation,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HubEphemeralRefLimits {
    pub max_refs: usize,
    pub max_refs_per_owner: usize,
    pub max_bytes_per_owner: u64,
    pub ttl_ms: u64,
}

impl Default for HubEphemeralRefLimits {
    fn default() -> Self {
        Self {
            max_refs: DEFAULT_MAX_HUB_EPHEMERAL_REFS,
            max_refs_per_owner: DEFAULT_MAX_HUB_EPHEMERAL_REFS_PER_OWNER,
            max_bytes_per_owner: DEFAULT_MAX_HUB_EPHEMERAL_BYTES_PER_OWNER,
            ttl_ms: DEFAULT_EPHEMERAL_REF_TTL_MS,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AgentEphemeralDataLimits {
    pub max_objects: usize,
    pub max_total_bytes: u64,
    pub max_object_bytes: u64,
    pub max_read_bytes: usize,
    pub ttl_ms: u64,
}

impl Default for AgentEphemeralDataLimits {
    fn default() -> Self {
        Self {
            max_objects: DEFAULT_MAX_AGENT_EPHEMERAL_OBJECTS,
            max_total_bytes: DEFAULT_MAX_AGENT_EPHEMERAL_BYTES,
            max_object_bytes: DEFAULT_MAX_AGENT_EPHEMERAL_OBJECT_BYTES,
            max_read_bytes: DEFAULT_MAX_AGENT_EPHEMERAL_READ_BYTES,
            ttl_ms: DEFAULT_EPHEMERAL_REF_TTL_MS,
        }
    }
}

#[derive(Clone)]
struct HubEphemeralRefRecord {
    public_ref: String,
    owner: OperationOwner,
    device_id: String,
    device_generation: u64,
    capability_revision: u64,
    operation_id: Option<String>,
    kind: EphemeralDataKind,
    agent_locator: String,
    bytes: u64,
    expires_at_ms: u64,
}

impl fmt::Debug for HubEphemeralRefRecord {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HubEphemeralRefRecord")
            .field("public_ref", &"[redacted]")
            .field("owner", &"[redacted]")
            .field("device_id", &self.device_id)
            .field("device_generation", &self.device_generation)
            .field("capability_revision", &self.capability_revision)
            .field(
                "operation_id",
                &self.operation_id.as_ref().map(|_| "[redacted]"),
            )
            .field("kind", &self.kind)
            .field("agent_locator", &"[redacted]")
            .field("bytes", &self.bytes)
            .field("expires_at_ms", &self.expires_at_ms)
            .field(
                "operation_id",
                &self.operation_id.as_ref().map(|_| "[redacted]"),
            )
            .field("kind", &self.kind)
            .finish()
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct ResolvedHubEphemeralRef {
    agent_locator: String,
    pub bytes: u64,
    pub expires_at_ms: u64,
    pub operation_id: Option<String>,
    pub kind: EphemeralDataKind,
}

impl fmt::Debug for ResolvedHubEphemeralRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ResolvedHubEphemeralRef")
            .field("agent_locator", &"[redacted]")
            .field("bytes", &self.bytes)
            .field("expires_at_ms", &self.expires_at_ms)
            .finish()
    }
}

impl ResolvedHubEphemeralRef {
    pub fn agent_locator(&self) -> &str {
        &self.agent_locator
    }
}

pub struct HubEphemeralRefRegistry {
    limits: HubEphemeralRefLimits,
    refs: HashMap<String, HubEphemeralRefRecord>,
}

impl HubEphemeralRefRegistry {
    pub fn new(limits: HubEphemeralRefLimits) -> Result<Self, HubEphemeralRefError> {
        if limits.max_refs == 0
            || limits.max_refs_per_owner == 0
            || limits.max_refs_per_owner > limits.max_refs
            || limits.max_bytes_per_owner == 0
            || limits.ttl_ms == 0
        {
            return Err(HubEphemeralRefError::InvalidLimits);
        }
        Ok(Self {
            limits,
            refs: HashMap::new(),
        })
    }

    #[allow(clippy::too_many_arguments)]
    pub fn mint(
        &mut self,
        owner: OperationOwner,
        device_id: &str,
        device_generation: u64,
        capability_revision: u64,
        operation_id: Option<&str>,
        kind: EphemeralDataKind,
        agent_locator: &str,
        bytes: u64,
        now_ms: u64,
    ) -> Result<String, HubEphemeralRefError> {
        self.prune(now_ms);
        validate_binding(
            device_id,
            device_generation,
            capability_revision,
            operation_id,
        )
        .map_err(|_| HubEphemeralRefError::InvalidBinding)?;
        if agent_locator.is_empty()
            || agent_locator.len() > MAX_AGENT_LOCATOR_BYTES
            || agent_locator.chars().any(char::is_control)
        {
            return Err(HubEphemeralRefError::InvalidAgentLocator);
        }
        if self.refs.len() >= self.limits.max_refs {
            return Err(HubEphemeralRefError::RefLimitExceeded);
        }

        let mut owner_refs = 0usize;
        let mut owner_bytes = 0u64;
        for record in self.refs.values().filter(|record| record.owner == owner) {
            owner_refs = owner_refs.saturating_add(1);
            owner_bytes = owner_bytes.saturating_add(record.bytes);
        }
        if owner_refs >= self.limits.max_refs_per_owner {
            return Err(HubEphemeralRefError::OwnerRefLimitExceeded);
        }
        if owner_bytes.saturating_add(bytes) > self.limits.max_bytes_per_owner {
            return Err(HubEphemeralRefError::OwnerByteLimitExceeded);
        }

        let expires_at_ms = now_ms
            .checked_add(self.limits.ttl_ms)
            .ok_or(HubEphemeralRefError::InvalidBinding)?;

        for _ in 0..8 {
            let public_ref = random_id("eref_");
            if self.refs.contains_key(&public_ref) {
                continue;
            }
            self.refs.insert(
                public_ref.clone(),
                HubEphemeralRefRecord {
                    public_ref: public_ref.clone(),
                    owner,
                    device_id: device_id.to_owned(),
                    device_generation,
                    capability_revision,
                    operation_id: operation_id.map(str::to_owned),
                    kind,
                    agent_locator: agent_locator.to_owned(),
                    bytes,
                    expires_at_ms,
                },
            );
            return Ok(public_ref);
        }
        Err(HubEphemeralRefError::IdentifierCollision)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn resolve(
        &mut self,
        public_ref: &str,
        owner: &OperationOwner,
        device_id: &str,
        device_generation: u64,
        capability_revision: u64,
        operation_id: Option<&str>,
        expected_kind: EphemeralDataKind,
        now_ms: u64,
    ) -> Result<ResolvedHubEphemeralRef, HubEphemeralRefError> {
        let resolved = self.resolve_owned(
            public_ref,
            owner,
            device_id,
            device_generation,
            capability_revision,
            now_ms,
        )?;
        if resolved.kind != expected_kind {
            return Err(HubEphemeralRefError::KindMismatch);
        }
        if resolved.operation_id.as_deref() != operation_id {
            return Err(HubEphemeralRefError::OperationMismatch);
        }
        Ok(resolved)
    }

    pub fn resolve_owned(
        &mut self,
        public_ref: &str,
        owner: &OperationOwner,
        device_id: &str,
        device_generation: u64,
        capability_revision: u64,
        now_ms: u64,
    ) -> Result<ResolvedHubEphemeralRef, HubEphemeralRefError> {
        let Some(record) = self.refs.get(public_ref) else {
            return Err(HubEphemeralRefError::UnknownRef);
        };
        if now_ms > record.expires_at_ms {
            self.refs.remove(public_ref);
            return Err(HubEphemeralRefError::Expired);
        }
        if record.public_ref != public_ref {
            return Err(HubEphemeralRefError::UnknownRef);
        }
        if &record.owner != owner {
            return Err(HubEphemeralRefError::OwnerMismatch);
        }
        if record.device_id != device_id {
            return Err(HubEphemeralRefError::DeviceMismatch);
        }
        if record.device_generation != device_generation {
            return Err(HubEphemeralRefError::GenerationMismatch);
        }
        if record.capability_revision != capability_revision {
            return Err(HubEphemeralRefError::CapabilityRevisionMismatch);
        }
        Ok(ResolvedHubEphemeralRef {
            agent_locator: record.agent_locator.clone(),
            bytes: record.bytes,
            expires_at_ms: record.expires_at_ms,
            operation_id: record.operation_id.clone(),
            kind: record.kind,
        })
    }

    pub fn remove(&mut self, public_ref: &str, owner: &OperationOwner) -> bool {
        if self
            .refs
            .get(public_ref)
            .is_some_and(|record| &record.owner == owner)
        {
            self.refs.remove(public_ref);
            true
        } else {
            false
        }
    }

    pub fn prune(&mut self, now_ms: u64) -> usize {
        let before = self.refs.len();
        self.refs.retain(|_, record| now_ms <= record.expires_at_ms);
        before.saturating_sub(self.refs.len())
    }

    pub fn invalidate_device_generation(
        &mut self,
        device_id: &str,
        current_generation: u64,
    ) -> usize {
        let before = self.refs.len();
        self.refs.retain(|_, record| {
            record.device_id != device_id || record.device_generation == current_generation
        });
        before.saturating_sub(self.refs.len())
    }

    pub fn invalidate_capability_revision(
        &mut self,
        device_id: &str,
        current_revision: u64,
    ) -> usize {
        let before = self.refs.len();
        self.refs.retain(|_, record| {
            record.device_id != device_id || record.capability_revision == current_revision
        });
        before.saturating_sub(self.refs.len())
    }

    pub fn len(&self) -> usize {
        self.refs.len()
    }

    pub fn is_empty(&self) -> bool {
        self.refs.is_empty()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HubEphemeralRefError {
    InvalidLimits,
    InvalidBinding,
    InvalidAgentLocator,
    RefLimitExceeded,
    OwnerRefLimitExceeded,
    OwnerByteLimitExceeded,
    IdentifierCollision,
    UnknownRef,
    Expired,
    OwnerMismatch,
    DeviceMismatch,
    GenerationMismatch,
    CapabilityRevisionMismatch,
    KindMismatch,
    OperationMismatch,
}

impl HubEphemeralRefError {
    pub const fn safe_error_code(self) -> &'static str {
        match self {
            Self::InvalidLimits => "ephemeral_ref_invalid_limits",
            Self::InvalidBinding => "ephemeral_ref_invalid_binding",
            Self::InvalidAgentLocator => "ephemeral_ref_invalid_locator",
            Self::RefLimitExceeded => "ephemeral_ref_limit",
            Self::OwnerRefLimitExceeded => "ephemeral_ref_owner_limit",
            Self::OwnerByteLimitExceeded => "ephemeral_ref_owner_bytes",
            Self::IdentifierCollision => "ephemeral_ref_collision",
            Self::UnknownRef => "ephemeral_ref_stale",
            Self::Expired => "ephemeral_ref_expired",
            Self::OwnerMismatch => "ephemeral_ref_stale",
            Self::DeviceMismatch => "ephemeral_ref_device_mismatch",
            Self::GenerationMismatch => "ephemeral_ref_generation_mismatch",
            Self::CapabilityRevisionMismatch => "ephemeral_ref_revision_mismatch",
            Self::KindMismatch => "ephemeral_ref_kind_mismatch",
            Self::OperationMismatch => "ephemeral_ref_operation_mismatch",
        }
    }
}

impl fmt::Display for HubEphemeralRefError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.safe_error_code())
    }
}

impl std::error::Error for HubEphemeralRefError {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentEphemeralBinding {
    pub device_id: String,
    pub device_generation: u64,
    pub capability_revision: u64,
    pub operation_id: Option<String>,
    pub kind: EphemeralDataKind,
}

impl AgentEphemeralBinding {
    pub fn new(
        device_id: impl Into<String>,
        device_generation: u64,
        capability_revision: u64,
        operation_id: Option<String>,
        kind: EphemeralDataKind,
    ) -> Result<Self, AgentEphemeralDataError> {
        let binding = Self {
            device_id: device_id.into(),
            device_generation,
            capability_revision,
            operation_id,
            kind,
        };
        validate_binding(
            &binding.device_id,
            binding.device_generation,
            binding.capability_revision,
            binding.operation_id.as_deref(),
        )
        .map_err(|_| AgentEphemeralDataError::InvalidBinding)?;
        Ok(binding)
    }
}

struct AgentEphemeralRecord {
    binding: AgentEphemeralBinding,
    path: PathBuf,
    bytes: u64,
    expires_at_ms: u64,
}

impl fmt::Debug for AgentEphemeralRecord {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AgentEphemeralRecord")
            .field("binding", &self.binding)
            .field("path", &"[redacted]")
            .field("bytes", &self.bytes)
            .field("expires_at_ms", &self.expires_at_ms)
            .finish()
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct StagedEphemeralData {
    locator: String,
    pub bytes: u64,
    pub expires_at_ms: u64,
}

impl fmt::Debug for StagedEphemeralData {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("StagedEphemeralData")
            .field("locator", &"[redacted]")
            .field("bytes", &self.bytes)
            .field("expires_at_ms", &self.expires_at_ms)
            .finish()
    }
}

impl StagedEphemeralData {
    pub fn locator(&self) -> &str {
        &self.locator
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EphemeralDataRange {
    pub bytes: Vec<u8>,
    pub offset: u64,
    pub next_offset: u64,
    pub total_bytes: u64,
    pub eof: bool,
}

pub struct AgentEphemeralDataStore {
    root: PathBuf,
    canonical_root: PathBuf,
    limits: AgentEphemeralDataLimits,
    records: HashMap<String, AgentEphemeralRecord>,
    total_bytes: u64,
}

impl AgentEphemeralDataStore {
    /// Create one ephemeral store beneath an operator/runtime-selected private parent.
    ///
    /// The parent must be outside the authoritative Agent checkpoint/rollback tree.
    /// Only the fixed ephemeral child is ever removed during startup cleanup.
    pub fn new(
        storage_parent: &Path,
        limits: AgentEphemeralDataLimits,
    ) -> Result<Self, AgentEphemeralDataError> {
        validate_agent_limits(limits)?;
        let root = storage_parent.join(EPHEMERAL_DATA_CHILD);
        remove_private_root(&root)?;
        fs::create_dir_all(&root).map_err(|_| AgentEphemeralDataError::Io)?;
        harden_directory_permissions(&root).map_err(|_| AgentEphemeralDataError::Io)?;
        let metadata = fs::symlink_metadata(&root).map_err(|_| AgentEphemeralDataError::Io)?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err(AgentEphemeralDataError::InvalidPrivateRoot);
        }
        let canonical_root = fs::canonicalize(&root).map_err(|_| AgentEphemeralDataError::Io)?;
        Ok(Self {
            root,
            canonical_root,
            limits,
            records: HashMap::new(),
            total_bytes: 0,
        })
    }

    pub fn stage(
        &mut self,
        binding: AgentEphemeralBinding,
        data: &[u8],
        now_ms: u64,
    ) -> Result<StagedEphemeralData, AgentEphemeralDataError> {
        self.prune(now_ms)?;
        validate_binding(
            &binding.device_id,
            binding.device_generation,
            binding.capability_revision,
            binding.operation_id.as_deref(),
        )
        .map_err(|_| AgentEphemeralDataError::InvalidBinding)?;
        let bytes =
            u64::try_from(data.len()).map_err(|_| AgentEphemeralDataError::ObjectTooLarge)?;
        if bytes > self.limits.max_object_bytes {
            return Err(AgentEphemeralDataError::ObjectTooLarge);
        }
        if self.records.len() >= self.limits.max_objects {
            return Err(AgentEphemeralDataError::ObjectLimitExceeded);
        }
        if self.total_bytes.saturating_add(bytes) > self.limits.max_total_bytes {
            return Err(AgentEphemeralDataError::ByteLimitExceeded);
        }
        let expires_at_ms = now_ms
            .checked_add(self.limits.ttl_ms)
            .ok_or(AgentEphemeralDataError::InvalidBinding)?;

        for _ in 0..8 {
            let locator = random_id("edata_");
            if self.records.contains_key(&locator) {
                continue;
            }
            let path = self.root.join(&locator);
            let mut options = OpenOptions::new();
            options.write(true).create_new(true);
            harden_file_open_options(&mut options);
            let mut file = options
                .open(&path)
                .map_err(|_| AgentEphemeralDataError::Io)?;
            let write_result = (|| {
                file.write_all(data)
                    .map_err(|_| AgentEphemeralDataError::Io)?;
                file.sync_all().map_err(|_| AgentEphemeralDataError::Io)?;
                prove_private_regular_file(&path, &self.canonical_root, bytes)?;
                Ok(())
            })();
            if let Err(error) = write_result {
                let _ = fs::remove_file(&path);
                return Err(error);
            }
            self.records.insert(
                locator.clone(),
                AgentEphemeralRecord {
                    binding,
                    path,
                    bytes,
                    expires_at_ms,
                },
            );
            self.total_bytes = self.total_bytes.saturating_add(bytes);
            return Ok(StagedEphemeralData {
                locator,
                bytes,
                expires_at_ms,
            });
        }
        Err(AgentEphemeralDataError::IdentifierCollision)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn read_range(
        &mut self,
        locator: &str,
        device_id: &str,
        device_generation: u64,
        capability_revision: u64,
        operation_id: Option<&str>,
        expected_kind: EphemeralDataKind,
        offset: u64,
        max_bytes: usize,
        now_ms: u64,
    ) -> Result<EphemeralDataRange, AgentEphemeralDataError> {
        if max_bytes == 0 || max_bytes > self.limits.max_read_bytes {
            return Err(AgentEphemeralDataError::ReadTooLarge);
        }
        let Some(record) = self.records.get(locator) else {
            return Err(AgentEphemeralDataError::UnknownLocator);
        };
        if now_ms > record.expires_at_ms {
            self.remove_locator(locator)?;
            return Err(AgentEphemeralDataError::Expired);
        }
        validate_agent_record(
            record,
            device_id,
            device_generation,
            capability_revision,
            operation_id,
            expected_kind,
        )?;
        if offset > record.bytes {
            return Err(AgentEphemeralDataError::InvalidRange);
        }

        prove_private_regular_file(&record.path, &self.canonical_root, record.bytes)?;
        let path = record.path.clone();
        let total_bytes = record.bytes;
        let mut file = OpenOptions::new()
            .read(true)
            .open(path)
            .map_err(|_| AgentEphemeralDataError::Io)?;
        file.seek(SeekFrom::Start(offset))
            .map_err(|_| AgentEphemeralDataError::Io)?;

        let remaining = total_bytes.saturating_sub(offset);
        let requested =
            u64::try_from(max_bytes).map_err(|_| AgentEphemeralDataError::InvalidRange)?;
        let limit = remaining.min(requested);
        let capacity = usize::try_from(limit).map_err(|_| AgentEphemeralDataError::InvalidRange)?;
        let mut bytes = Vec::with_capacity(capacity);
        file.take(limit)
            .read_to_end(&mut bytes)
            .map_err(|_| AgentEphemeralDataError::Io)?;
        let next_offset = offset
            .checked_add(
                u64::try_from(bytes.len()).map_err(|_| AgentEphemeralDataError::InvalidRange)?,
            )
            .ok_or(AgentEphemeralDataError::InvalidRange)?;

        Ok(EphemeralDataRange {
            bytes,
            offset,
            next_offset,
            total_bytes,
            eof: next_offset >= total_bytes,
        })
    }

    pub fn remove(
        &mut self,
        locator: &str,
        binding: &AgentEphemeralBinding,
    ) -> Result<bool, AgentEphemeralDataError> {
        let Some(record) = self.records.get(locator) else {
            return Ok(false);
        };
        validate_agent_record(
            record,
            &binding.device_id,
            binding.device_generation,
            binding.capability_revision,
            binding.operation_id.as_deref(),
            binding.kind,
        )?;
        self.remove_locator(locator)?;
        Ok(true)
    }

    pub fn prune(&mut self, now_ms: u64) -> Result<usize, AgentEphemeralDataError> {
        let expired: Vec<_> = self
            .records
            .iter()
            .filter(|(_, record)| now_ms > record.expires_at_ms)
            .map(|(locator, _)| locator.clone())
            .collect();
        for locator in &expired {
            self.remove_locator(locator)?;
        }
        Ok(expired.len())
    }

    fn remove_locator(&mut self, locator: &str) -> Result<(), AgentEphemeralDataError> {
        let Some(record) = self.records.get(locator) else {
            return Ok(());
        };
        let path = record.path.clone();
        let bytes = record.bytes;
        match fs::remove_file(&path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return Err(AgentEphemeralDataError::Io),
        }
        self.records.remove(locator);
        self.total_bytes = self.total_bytes.saturating_sub(bytes);
        Ok(())
    }

    pub fn len(&self) -> usize {
        self.records.len()
    }

    pub fn is_empty(&self) -> bool {
        self.records.is_empty()
    }

    pub fn total_bytes(&self) -> u64 {
        self.total_bytes
    }
}

impl Drop for AgentEphemeralDataStore {
    fn drop(&mut self) {
        let _ = remove_private_root(&self.root);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentEphemeralDataError {
    InvalidLimits,
    InvalidBinding,
    InvalidPrivateRoot,
    ObjectTooLarge,
    ObjectLimitExceeded,
    ByteLimitExceeded,
    IdentifierCollision,
    UnknownLocator,
    Expired,
    DeviceMismatch,
    GenerationMismatch,
    CapabilityRevisionMismatch,
    KindMismatch,
    OperationMismatch,
    ReadTooLarge,
    InvalidRange,
    InvalidObject,
    Io,
}

impl AgentEphemeralDataError {
    pub const fn safe_error_code(self) -> &'static str {
        match self {
            Self::InvalidLimits => "ephemeral_data_invalid_limits",
            Self::InvalidBinding => "ephemeral_data_invalid_binding",
            Self::InvalidPrivateRoot => "ephemeral_data_invalid_root",
            Self::ObjectTooLarge => "ephemeral_data_object_too_large",
            Self::ObjectLimitExceeded => "ephemeral_data_object_limit",
            Self::ByteLimitExceeded => "ephemeral_data_byte_limit",
            Self::IdentifierCollision => "ephemeral_data_collision",
            Self::UnknownLocator => "ephemeral_data_stale",
            Self::Expired => "ephemeral_data_expired",
            Self::DeviceMismatch => "ephemeral_data_device_mismatch",
            Self::GenerationMismatch => "ephemeral_data_generation_mismatch",
            Self::CapabilityRevisionMismatch => "ephemeral_data_revision_mismatch",
            Self::KindMismatch => "ephemeral_data_kind_mismatch",
            Self::OperationMismatch => "ephemeral_data_operation_mismatch",
            Self::ReadTooLarge => "ephemeral_data_read_too_large",
            Self::InvalidRange => "ephemeral_data_invalid_range",
            Self::InvalidObject => "ephemeral_data_invalid_object",
            Self::Io => "ephemeral_data_io",
        }
    }
}

impl fmt::Display for AgentEphemeralDataError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.safe_error_code())
    }
}

impl std::error::Error for AgentEphemeralDataError {}

fn validate_agent_limits(limits: AgentEphemeralDataLimits) -> Result<(), AgentEphemeralDataError> {
    if limits.max_objects == 0
        || limits.max_total_bytes == 0
        || limits.max_object_bytes == 0
        || limits.max_object_bytes > limits.max_total_bytes
        || limits.max_read_bytes == 0
        || u64::try_from(limits.max_read_bytes)
            .ok()
            .is_none_or(|value| value > limits.max_object_bytes)
        || limits.ttl_ms == 0
    {
        return Err(AgentEphemeralDataError::InvalidLimits);
    }
    Ok(())
}

fn validate_binding(
    device_id: &str,
    device_generation: u64,
    capability_revision: u64,
    operation_id: Option<&str>,
) -> Result<(), ()> {
    if device_id.trim().is_empty()
        || device_generation == 0
        || capability_revision == 0
        || operation_id.is_some_and(|value| value.trim().is_empty())
    {
        return Err(());
    }
    Ok(())
}

fn validate_agent_record(
    record: &AgentEphemeralRecord,
    device_id: &str,
    device_generation: u64,
    capability_revision: u64,
    operation_id: Option<&str>,
    expected_kind: EphemeralDataKind,
) -> Result<(), AgentEphemeralDataError> {
    if record.binding.device_id != device_id {
        return Err(AgentEphemeralDataError::DeviceMismatch);
    }
    if record.binding.device_generation != device_generation {
        return Err(AgentEphemeralDataError::GenerationMismatch);
    }
    if record.binding.capability_revision != capability_revision {
        return Err(AgentEphemeralDataError::CapabilityRevisionMismatch);
    }
    if record.binding.kind != expected_kind {
        return Err(AgentEphemeralDataError::KindMismatch);
    }
    if record.binding.operation_id.as_deref() != operation_id {
        return Err(AgentEphemeralDataError::OperationMismatch);
    }
    Ok(())
}

fn prove_private_regular_file(
    path: &Path,
    canonical_root: &Path,
    expected_bytes: u64,
) -> Result<(), AgentEphemeralDataError> {
    let metadata =
        fs::symlink_metadata(path).map_err(|_| AgentEphemeralDataError::InvalidObject)?;
    if metadata.file_type().is_symlink() || !metadata.is_file() || metadata.len() != expected_bytes
    {
        return Err(AgentEphemeralDataError::InvalidObject);
    }
    let canonical = fs::canonicalize(path).map_err(|_| AgentEphemeralDataError::InvalidObject)?;
    if canonical.parent() != Some(canonical_root) {
        return Err(AgentEphemeralDataError::InvalidObject);
    }
    Ok(())
}

fn remove_private_root(root: &Path) -> Result<(), AgentEphemeralDataError> {
    match fs::symlink_metadata(root) {
        Ok(metadata) if metadata.file_type().is_symlink() || metadata.is_file() => {
            fs::remove_file(root).map_err(|_| AgentEphemeralDataError::Io)
        }
        Ok(metadata) if metadata.is_dir() => {
            fs::remove_dir_all(root).map_err(|_| AgentEphemeralDataError::Io)
        }
        Ok(_) => Err(AgentEphemeralDataError::InvalidPrivateRoot),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(_) => Err(AgentEphemeralDataError::Io),
    }
}

#[cfg(unix)]
fn harden_directory_permissions(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
}

#[cfg(not(unix))]
fn harden_directory_permissions(_path: &Path) -> std::io::Result<()> {
    Ok(())
}

#[cfg(unix)]
fn harden_file_open_options(options: &mut OpenOptions) {
    use std::os::unix::fs::OpenOptionsExt;
    options.mode(0o600);
}

#[cfg(not(unix))]
fn harden_file_open_options(_options: &mut OpenOptions) {}

fn random_id(prefix: &str) -> String {
    let mut random = [0_u8; 16];
    OsRng.fill_bytes(&mut random);
    let mut output = String::with_capacity(prefix.len() + random.len() * 2);
    output.push_str(prefix);
    for byte in random {
        use std::fmt::Write as _;
        let _ = write!(&mut output, "{byte:02x}");
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn owner(subject: &str) -> OperationOwner {
        OperationOwner::new("https://issuer.example", subject).unwrap()
    }

    fn temp_state(name: &str) -> PathBuf {
        let suffix = rand::random::<u64>();
        std::env::temp_dir().join(format!(
            "cumg-ephemeral-{name}-{}-{}-{suffix:016x}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }

    fn binding(kind: EphemeralDataKind) -> AgentEphemeralBinding {
        AgentEphemeralBinding::new("dev-a", 3, 7, Some("op-a".into()), kind).unwrap()
    }

    #[test]
    fn hub_registry_fences_owner_device_generation_revision_kind_operation_and_expiry() {
        let limits = HubEphemeralRefLimits {
            max_refs: 8,
            max_refs_per_owner: 4,
            max_bytes_per_owner: 64,
            ttl_ms: 100,
        };
        let mut refs = HubEphemeralRefRegistry::new(limits).unwrap();
        let alice = owner("alice");
        let bob = owner("bob");
        let public_ref = refs
            .mint(
                alice.clone(),
                "dev-a",
                3,
                7,
                Some("op-a"),
                EphemeralDataKind::ProcessStdout,
                "edata_private",
                12,
                1_000,
            )
            .unwrap();

        assert_eq!(
            refs.resolve(
                &public_ref,
                &bob,
                "dev-a",
                3,
                7,
                Some("op-a"),
                EphemeralDataKind::ProcessStdout,
                1_001,
            ),
            Err(HubEphemeralRefError::OwnerMismatch)
        );
        assert_eq!(
            refs.resolve(
                &public_ref,
                &alice,
                "dev-b",
                3,
                7,
                Some("op-a"),
                EphemeralDataKind::ProcessStdout,
                1_001,
            ),
            Err(HubEphemeralRefError::DeviceMismatch)
        );
        assert_eq!(
            refs.resolve(
                &public_ref,
                &alice,
                "dev-a",
                4,
                7,
                Some("op-a"),
                EphemeralDataKind::ProcessStdout,
                1_001,
            ),
            Err(HubEphemeralRefError::GenerationMismatch)
        );
        assert_eq!(
            refs.resolve(
                &public_ref,
                &alice,
                "dev-a",
                3,
                8,
                Some("op-a"),
                EphemeralDataKind::ProcessStdout,
                1_001,
            ),
            Err(HubEphemeralRefError::CapabilityRevisionMismatch)
        );
        assert_eq!(
            refs.resolve(
                &public_ref,
                &alice,
                "dev-a",
                3,
                7,
                Some("op-a"),
                EphemeralDataKind::ProcessStderr,
                1_001,
            ),
            Err(HubEphemeralRefError::KindMismatch)
        );
        assert_eq!(
            refs.resolve(
                &public_ref,
                &alice,
                "dev-a",
                3,
                7,
                Some("op-b"),
                EphemeralDataKind::ProcessStdout,
                1_001,
            ),
            Err(HubEphemeralRefError::OperationMismatch)
        );

        let resolved = refs
            .resolve(
                &public_ref,
                &alice,
                "dev-a",
                3,
                7,
                Some("op-a"),
                EphemeralDataKind::ProcessStdout,
                1_001,
            )
            .unwrap();
        assert_eq!(resolved.agent_locator(), "edata_private");
        assert_eq!(resolved.bytes, 12);

        assert_eq!(
            refs.resolve(
                &public_ref,
                &alice,
                "dev-a",
                3,
                7,
                Some("op-a"),
                EphemeralDataKind::ProcessStdout,
                1_101,
            ),
            Err(HubEphemeralRefError::Expired)
        );
        assert!(refs.is_empty());
    }

    #[test]
    fn hub_registry_enforces_per_owner_reference_and_byte_budgets() {
        let mut refs = HubEphemeralRefRegistry::new(HubEphemeralRefLimits {
            max_refs: 8,
            max_refs_per_owner: 2,
            max_bytes_per_owner: 10,
            ttl_ms: 100,
        })
        .unwrap();
        let alice = owner("alice");
        for (locator, bytes) in [("a", 4), ("b", 5)] {
            refs.mint(
                alice.clone(),
                "dev-a",
                1,
                1,
                None,
                EphemeralDataKind::DirectoryContinuation,
                locator,
                bytes,
                0,
            )
            .unwrap();
        }
        assert_eq!(
            refs.mint(
                alice.clone(),
                "dev-a",
                1,
                1,
                None,
                EphemeralDataKind::DirectoryContinuation,
                "c",
                1,
                0,
            ),
            Err(HubEphemeralRefError::OwnerRefLimitExceeded)
        );

        let mut byte_limited = HubEphemeralRefRegistry::new(HubEphemeralRefLimits {
            max_refs: 8,
            max_refs_per_owner: 4,
            max_bytes_per_owner: 10,
            ttl_ms: 100,
        })
        .unwrap();
        byte_limited
            .mint(
                alice.clone(),
                "dev-a",
                1,
                1,
                None,
                EphemeralDataKind::DirectoryContinuation,
                "a",
                6,
                0,
            )
            .unwrap();
        assert_eq!(
            byte_limited.mint(
                alice,
                "dev-a",
                1,
                1,
                None,
                EphemeralDataKind::DirectoryContinuation,
                "b",
                5,
                0,
            ),
            Err(HubEphemeralRefError::OwnerByteLimitExceeded)
        );
    }

    #[test]
    fn hub_generation_and_revision_invalidation_are_bounded() {
        let mut refs = HubEphemeralRefRegistry::new(HubEphemeralRefLimits::default()).unwrap();
        let alice = owner("alice");
        for (generation, revision) in [(1, 1), (2, 1), (2, 2)] {
            refs.mint(
                alice.clone(),
                "dev-a",
                generation,
                revision,
                None,
                EphemeralDataKind::DirectoryContinuation,
                &format!("loc-{generation}-{revision}"),
                1,
                0,
            )
            .unwrap();
        }
        assert_eq!(refs.invalidate_device_generation("dev-a", 2), 1);
        assert_eq!(refs.len(), 2);
        assert_eq!(refs.invalidate_capability_revision("dev-a", 2), 1);
        assert_eq!(refs.len(), 1);
    }

    #[test]
    fn agent_store_reads_bounded_ranges_and_fences_bindings() {
        let state = temp_state("range");
        fs::create_dir_all(&state).unwrap();
        let limits = AgentEphemeralDataLimits {
            max_objects: 4,
            max_total_bytes: 32,
            max_object_bytes: 16,
            max_read_bytes: 4,
            ttl_ms: 100,
        };
        let mut store = AgentEphemeralDataStore::new(&state, limits).unwrap();
        let binding = binding(EphemeralDataKind::ProcessStdout);
        let staged = store.stage(binding.clone(), b"abcdefgh", 1_000).unwrap();

        let first = store
            .read_range(
                staged.locator(),
                "dev-a",
                3,
                7,
                Some("op-a"),
                EphemeralDataKind::ProcessStdout,
                0,
                4,
                1_001,
            )
            .unwrap();
        assert_eq!(first.bytes, b"abcd");
        assert_eq!(first.next_offset, 4);
        assert!(!first.eof);

        let second = store
            .read_range(
                staged.locator(),
                "dev-a",
                3,
                7,
                Some("op-a"),
                EphemeralDataKind::ProcessStdout,
                4,
                4,
                1_001,
            )
            .unwrap();
        assert_eq!(second.bytes, b"efgh");
        assert!(second.eof);

        let mismatch_cases = [
            (
                "dev-b",
                3,
                7,
                Some("op-a"),
                EphemeralDataKind::ProcessStdout,
                AgentEphemeralDataError::DeviceMismatch,
            ),
            (
                "dev-a",
                4,
                7,
                Some("op-a"),
                EphemeralDataKind::ProcessStdout,
                AgentEphemeralDataError::GenerationMismatch,
            ),
            (
                "dev-a",
                3,
                8,
                Some("op-a"),
                EphemeralDataKind::ProcessStdout,
                AgentEphemeralDataError::CapabilityRevisionMismatch,
            ),
            (
                "dev-a",
                3,
                7,
                Some("op-b"),
                EphemeralDataKind::ProcessStdout,
                AgentEphemeralDataError::OperationMismatch,
            ),
            (
                "dev-a",
                3,
                7,
                Some("op-a"),
                EphemeralDataKind::ProcessStderr,
                AgentEphemeralDataError::KindMismatch,
            ),
        ];
        for (device, generation, revision, operation, kind, expected) in mismatch_cases {
            assert_eq!(
                store.read_range(
                    staged.locator(),
                    device,
                    generation,
                    revision,
                    operation,
                    kind,
                    0,
                    4,
                    1_001,
                ),
                Err(expected)
            );
        }

        assert_eq!(
            store.read_range(
                staged.locator(),
                "dev-a",
                3,
                7,
                Some("op-a"),
                EphemeralDataKind::ProcessStdout,
                0,
                5,
                1_001,
            ),
            Err(AgentEphemeralDataError::ReadTooLarge)
        );
        assert_eq!(
            store.read_range(
                staged.locator(),
                "dev-a",
                3,
                7,
                Some("op-a"),
                EphemeralDataKind::ProcessStdout,
                9,
                4,
                1_001,
            ),
            Err(AgentEphemeralDataError::InvalidRange)
        );
        drop(store);
        let _ = fs::remove_dir_all(state);
    }

    #[test]
    fn agent_store_enforces_quotas_and_prunes_expiry() {
        let state = temp_state("quota");
        fs::create_dir_all(&state).unwrap();
        let limits = AgentEphemeralDataLimits {
            max_objects: 2,
            max_total_bytes: 6,
            max_object_bytes: 4,
            max_read_bytes: 4,
            ttl_ms: 10,
        };
        let mut store = AgentEphemeralDataStore::new(&state, limits).unwrap();
        let binding = binding(EphemeralDataKind::ProcessStdout);
        store.stage(binding.clone(), b"abc", 100).unwrap();
        assert_eq!(
            store.stage(binding.clone(), b"defg", 100),
            Err(AgentEphemeralDataError::ByteLimitExceeded)
        );
        store.stage(binding.clone(), b"def", 100).unwrap();
        assert_eq!(
            store.stage(binding.clone(), b"x", 100),
            Err(AgentEphemeralDataError::ObjectLimitExceeded)
        );
        assert_eq!(store.prune(111).unwrap(), 2);
        assert_eq!(store.total_bytes(), 0);
        assert!(store.is_empty());
        assert_eq!(
            store.stage(binding, b"12345", 200),
            Err(AgentEphemeralDataError::ObjectTooLarge)
        );
        drop(store);
        let _ = fs::remove_dir_all(state);
    }

    #[test]
    fn agent_restart_removes_orphaned_ephemeral_bytes() {
        let state = temp_state("restart");
        fs::create_dir_all(&state).unwrap();
        let limits = AgentEphemeralDataLimits {
            max_objects: 4,
            max_total_bytes: 64,
            max_object_bytes: 32,
            max_read_bytes: 8,
            ttl_ms: 100,
        };
        let mut first = AgentEphemeralDataStore::new(&state, limits).unwrap();
        let staged = first
            .stage(
                binding(EphemeralDataKind::ProcessStdout),
                b"secret-output",
                1,
            )
            .unwrap();
        let orphan = first.root.join(staged.locator());
        assert!(orphan.is_file());
        std::mem::forget(first);

        let second = AgentEphemeralDataStore::new(&state, limits).unwrap();
        assert!(!orphan.exists());
        assert!(second.is_empty());
        drop(second);
        let _ = fs::remove_dir_all(state);
    }

    #[test]
    fn repeated_stage_prune_cycles_stay_bounded() {
        let state = temp_state("soak");
        fs::create_dir_all(&state).unwrap();
        let limits = AgentEphemeralDataLimits {
            max_objects: 8,
            max_total_bytes: 64,
            max_object_bytes: 8,
            max_read_bytes: 8,
            ttl_ms: 1,
        };
        let mut store = AgentEphemeralDataStore::new(&state, limits).unwrap();
        let binding = binding(EphemeralDataKind::DirectoryContinuation);
        for cycle in 0..250u64 {
            for _ in 0..limits.max_objects {
                store
                    .stage(binding.clone(), b"12345678", cycle * 10)
                    .unwrap();
            }
            assert_eq!(store.len(), limits.max_objects);
            assert_eq!(store.total_bytes(), limits.max_total_bytes);
            assert_eq!(store.prune(cycle * 10 + 2).unwrap(), limits.max_objects);
            assert!(store.is_empty());
            assert_eq!(store.total_bytes(), 0);
        }
        drop(store);
        let _ = fs::remove_dir_all(state);
    }

    #[test]
    fn debug_and_errors_do_not_expose_locator_or_private_path() {
        let state = temp_state("redaction");
        fs::create_dir_all(&state).unwrap();
        let limits = AgentEphemeralDataLimits {
            max_objects: 2,
            max_total_bytes: 16,
            max_object_bytes: 8,
            max_read_bytes: 8,
            ttl_ms: 100,
        };
        let mut store = AgentEphemeralDataStore::new(&state, limits).unwrap();
        let staged = store
            .stage(binding(EphemeralDataKind::ProcessStdout), b"payload", 1)
            .unwrap();
        let record = store.records.get(staged.locator()).unwrap();
        let rendered = format!("{record:?} {staged:?}");
        assert!(!rendered.contains(staged.locator()));
        assert!(!rendered.contains(state.to_string_lossy().as_ref()));
        assert_eq!(
            AgentEphemeralDataError::UnknownLocator.to_string(),
            "ephemeral_data_stale"
        );
        assert_eq!(
            HubEphemeralRefError::OwnerMismatch.safe_error_code(),
            HubEphemeralRefError::UnknownRef.safe_error_code()
        );
        drop(store);
        let _ = fs::remove_dir_all(state);
    }
}
