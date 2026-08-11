//! V2-M0 control-plane prototype.
//!
//! This module is deliberately isolated from the V1 MCP gateway. It proves the
//! control semantics before any Hub/Agent transport is selected.

use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use rand::{RngCore, rngs::OsRng};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::fmt;

pub const CONTROL_SCHEMA_VERSION: u16 = 1;
pub const CAPABILITY_SCHEMA_VERSION: u16 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CapabilityClass {
    Observe,
    Interact,
    System,
    Dangerous,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeviceCapability {
    ListApplications,
    ScreenGeometry,
    PointerClick,
    ExecuteProcess,
}

impl DeviceCapability {
    pub fn class(self) -> CapabilityClass {
        match self {
            Self::ListApplications | Self::ScreenGeometry => CapabilityClass::Observe,
            Self::PointerClick => CapabilityClass::Interact,
            // Direct process execution can mutate arbitrary local state and is
            // therefore never implied by observe/interact access.
            Self::ExecuteProcess => CapabilityClass::Dangerous,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PointerButton {
    Left,
    Right,
    Middle,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProcessEnvVar {
    pub key: String,
    pub value: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProcessRequest {
    pub program: String,
    pub args: Vec<String>,
    pub cwd: String,
    pub env: Vec<ProcessEnvVar>,
    pub timeout_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProcessOutput {
    pub exit_code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    pub stdout_truncated: bool,
    pub stderr_truncated: bool,
    pub timed_out: bool,
    pub cancelled: bool,
    pub duration_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum DeviceCommand {
    ListApplications,
    ScreenGeometry,
    PointerClick {
        x: i32,
        y: i32,
        button: PointerButton,
    },
    ExecuteProcess {
        request: ProcessRequest,
    },
}

impl DeviceCommand {
    pub fn capability(&self) -> DeviceCapability {
        match self {
            Self::ListApplications => DeviceCapability::ListApplications,
            Self::ScreenGeometry => DeviceCapability::ScreenGeometry,
            Self::PointerClick { .. } => DeviceCapability::PointerClick,
            Self::ExecuteProcess { .. } => DeviceCapability::ExecuteProcess,
        }
    }

    pub fn class(&self) -> CapabilityClass {
        self.capability().class()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CapabilityAdvertisement {
    pub backend: String,
    pub backend_version: String,
    pub platform: String,
    pub capability_schema_version: u16,
    pub revision: u64,
    pub supported: Vec<DeviceCapability>,
}

impl CapabilityAdvertisement {
    pub fn supports(&self, capability: DeviceCapability) -> bool {
        self.supported.contains(&capability)
    }
}

#[derive(Clone)]
pub struct DeviceIdentity {
    signing_key: SigningKey,
}

impl DeviceIdentity {
    pub fn generate() -> Self {
        Self {
            signing_key: SigningKey::generate(&mut OsRng),
        }
    }

    pub fn from_secret_key_bytes(secret_key: [u8; 32]) -> Self {
        Self {
            signing_key: SigningKey::from_bytes(&secret_key),
        }
    }

    pub(crate) fn secret_key_bytes(&self) -> [u8; 32] {
        self.signing_key.to_bytes()
    }

    pub fn public_key(&self) -> Vec<u8> {
        self.signing_key.verifying_key().to_bytes().to_vec()
    }

    pub fn enrollment_proof(&self, challenge: &[u8]) -> Vec<u8> {
        self.signing_key.sign(challenge).to_bytes().to_vec()
    }

    pub fn sign_message(&self, message: &[u8]) -> Vec<u8> {
        self.signing_key.sign(message).to_bytes().to_vec()
    }

    pub fn verifying_key(&self) -> VerifyingKey {
        self.signing_key.verifying_key()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceSession {
    pub device_id: String,
    pub generation: u64,
    pub capabilities: CapabilityAdvertisement,
}

#[derive(Debug, Clone)]
struct EnrolledDevice {
    verifying_key: VerifyingKey,
    generation: u64,
    capabilities: Option<CapabilityAdvertisement>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnrolledDeviceSnapshot {
    pub device_id: String,
    pub verifying_key: [u8; 32],
    pub generation: u64,
    pub capabilities: Option<CapabilityAdvertisement>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceRegistrySnapshot {
    pub schema_version: u16,
    pub devices: Vec<EnrolledDeviceSnapshot>,
    pub revoked_device_ids: Vec<String>,
}

#[derive(Debug, Default)]
pub struct DeviceRegistry {
    devices: HashMap<String, EnrolledDevice>,
    revoked: HashSet<String>,
}

impl DeviceRegistry {
    pub fn snapshot(&self) -> DeviceRegistrySnapshot {
        let mut devices: Vec<_> = self
            .devices
            .iter()
            .map(|(device_id, device)| EnrolledDeviceSnapshot {
                device_id: device_id.clone(),
                verifying_key: device.verifying_key.to_bytes(),
                generation: device.generation,
                capabilities: device.capabilities.clone(),
            })
            .collect();
        devices.sort_by(|left, right| left.device_id.cmp(&right.device_id));
        let mut revoked_device_ids: Vec<_> = self.revoked.iter().cloned().collect();
        revoked_device_ids.sort();
        DeviceRegistrySnapshot {
            schema_version: CONTROL_SCHEMA_VERSION,
            devices,
            revoked_device_ids,
        }
    }

    pub fn from_snapshot(snapshot: DeviceRegistrySnapshot) -> Result<Self, ControlError> {
        if snapshot.schema_version != CONTROL_SCHEMA_VERSION {
            return Err(ControlError::UnsupportedControlSchema {
                got: snapshot.schema_version,
            });
        }
        let mut devices = HashMap::new();
        for device in snapshot.devices {
            if devices.contains_key(&device.device_id) {
                return Err(ControlError::InvalidRegistrySnapshot);
            }
            let verifying_key = VerifyingKey::from_bytes(&device.verifying_key)
                .map_err(|_| ControlError::InvalidRegistrySnapshot)?;
            if let Some(capabilities) = &device.capabilities {
                if capabilities.capability_schema_version != CAPABILITY_SCHEMA_VERSION {
                    return Err(ControlError::UnsupportedCapabilitySchema {
                        got: capabilities.capability_schema_version,
                    });
                }
            }
            devices.insert(
                device.device_id,
                EnrolledDevice {
                    verifying_key,
                    generation: device.generation,
                    capabilities: device.capabilities,
                },
            );
        }
        let revoked = snapshot.revoked_device_ids.into_iter().collect();
        Ok(Self { devices, revoked })
    }

    pub fn enrollment_challenge() -> [u8; 32] {
        let mut challenge = [0_u8; 32];
        OsRng.fill_bytes(&mut challenge);
        challenge
    }

    pub fn enroll(
        &mut self,
        public_key: &[u8],
        challenge: &[u8],
        proof: &[u8],
    ) -> Result<String, ControlError> {
        let key_bytes: [u8; 32] = public_key
            .try_into()
            .map_err(|_| ControlError::InvalidDeviceIdentity)?;
        let verifying_key = VerifyingKey::from_bytes(&key_bytes)
            .map_err(|_| ControlError::InvalidDeviceIdentity)?;
        let sig_bytes: [u8; 64] = proof
            .try_into()
            .map_err(|_| ControlError::InvalidDeviceIdentity)?;
        let signature = Signature::from_bytes(&sig_bytes);
        verifying_key
            .verify(challenge, &signature)
            .map_err(|_| ControlError::InvalidEnrollmentProof)?;

        let device_id = format!("dev_{}", hex(&key_bytes));
        self.devices
            .entry(device_id.clone())
            .or_insert(EnrolledDevice {
                verifying_key,
                generation: 0,
                capabilities: None,
            });
        Ok(device_id)
    }

    pub fn revoke_device(&mut self, device_id: &str) {
        self.revoked.insert(device_id.to_owned());
    }

    pub fn verify_device_signature(
        &self,
        device_id: &str,
        message: &[u8],
        proof: &[u8],
    ) -> Result<(), ControlError> {
        if self.revoked.contains(device_id) {
            return Err(ControlError::DeviceRevoked);
        }
        let device = self
            .devices
            .get(device_id)
            .ok_or(ControlError::UnknownDevice)?;
        let sig_bytes: [u8; 64] = proof
            .try_into()
            .map_err(|_| ControlError::InvalidDeviceSignature)?;
        let signature = Signature::from_bytes(&sig_bytes);
        device
            .verifying_key
            .verify(message, &signature)
            .map_err(|_| ControlError::InvalidDeviceSignature)
    }

    pub fn rotate_device_key(
        &mut self,
        device_id: &str,
        new_public_key: &[u8],
        rotation_message: &[u8],
        old_proof: &[u8],
        new_proof: &[u8],
    ) -> Result<(), ControlError> {
        if self.revoked.contains(device_id) {
            return Err(ControlError::DeviceRevoked);
        }
        let new_key_bytes: [u8; 32] = new_public_key
            .try_into()
            .map_err(|_| ControlError::InvalidDeviceIdentity)?;
        let new_verifying_key = VerifyingKey::from_bytes(&new_key_bytes)
            .map_err(|_| ControlError::InvalidDeviceIdentity)?;
        let old_sig_bytes: [u8; 64] = old_proof
            .try_into()
            .map_err(|_| ControlError::InvalidDeviceSignature)?;
        let new_sig_bytes: [u8; 64] = new_proof
            .try_into()
            .map_err(|_| ControlError::InvalidDeviceSignature)?;
        let old_signature = Signature::from_bytes(&old_sig_bytes);
        let new_signature = Signature::from_bytes(&new_sig_bytes);
        let device = self
            .devices
            .get_mut(device_id)
            .ok_or(ControlError::UnknownDevice)?;
        device
            .verifying_key
            .verify(rotation_message, &old_signature)
            .map_err(|_| ControlError::InvalidDeviceRotationProof)?;
        new_verifying_key
            .verify(rotation_message, &new_signature)
            .map_err(|_| ControlError::InvalidDeviceRotationProof)?;
        device.verifying_key = new_verifying_key;
        device.generation = device.generation.saturating_add(1).max(1);
        device.capabilities = None;
        Ok(())
    }

    pub fn connect(
        &mut self,
        device_id: &str,
        capabilities: CapabilityAdvertisement,
    ) -> Result<DeviceSession, ControlError> {
        if self.revoked.contains(device_id) {
            return Err(ControlError::DeviceRevoked);
        }
        if capabilities.capability_schema_version != CAPABILITY_SCHEMA_VERSION {
            return Err(ControlError::UnsupportedCapabilitySchema {
                got: capabilities.capability_schema_version,
            });
        }
        let device = self
            .devices
            .get_mut(device_id)
            .ok_or(ControlError::UnknownDevice)?;
        // Touch the key so the registry cannot accidentally become a plain name registry.
        let _cryptographic_identity = device.verifying_key.to_bytes();
        device.generation = device.generation.saturating_add(1).max(1);
        device.capabilities = Some(capabilities.clone());
        Ok(DeviceSession {
            device_id: device_id.to_owned(),
            generation: device.generation,
            capabilities,
        })
    }

    pub fn current_session(&self, device_id: &str) -> Result<DeviceSession, ControlError> {
        let device = self
            .devices
            .get(device_id)
            .ok_or(ControlError::UnknownDevice)?;
        let capabilities = device
            .capabilities
            .clone()
            .ok_or(ControlError::DeviceOffline)?;
        Ok(DeviceSession {
            device_id: device_id.to_owned(),
            generation: device.generation,
            capabilities,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GrantPayload {
    pub schema_version: u16,
    pub issuer_key_id: String,
    pub grant_id: String,
    pub device_id: String,
    pub capability: CapabilityClass,
    pub issued_at_ms: u64,
    pub expires_at_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GrantToken {
    pub payload: GrantPayload,
    pub signature: Vec<u8>,
}

#[derive(Clone)]
pub struct GrantAuthority {
    signing_key: SigningKey,
}

impl GrantAuthority {
    pub fn generate() -> Self {
        Self {
            signing_key: SigningKey::generate(&mut OsRng),
        }
    }

    pub fn from_secret_key_bytes(secret_key: [u8; 32]) -> Self {
        Self {
            signing_key: SigningKey::from_bytes(&secret_key),
        }
    }

    pub(crate) fn secret_key_bytes(&self) -> [u8; 32] {
        self.signing_key.to_bytes()
    }

    pub fn verifier(&self) -> VerifyingKey {
        self.signing_key.verifying_key()
    }

    pub fn key_id(&self) -> String {
        verifying_key_id(&self.verifier())
    }

    pub fn issue(
        &self,
        device_id: &str,
        capability: CapabilityClass,
        now_ms: u64,
        ttl_ms: u64,
    ) -> Result<GrantToken, ControlError> {
        if ttl_ms == 0 {
            return Err(ControlError::InvalidGrantLifetime);
        }
        let mut random = [0_u8; 16];
        OsRng.fill_bytes(&mut random);
        let payload = GrantPayload {
            schema_version: CONTROL_SCHEMA_VERSION,
            issuer_key_id: self.key_id(),
            grant_id: format!("grant_{}", hex(&random)),
            device_id: device_id.to_owned(),
            capability,
            issued_at_ms: now_ms,
            expires_at_ms: now_ms.saturating_add(ttl_ms),
        };
        let bytes = canonical_grant_bytes(&payload)?;
        let signature = self.signing_key.sign(&bytes).to_bytes().to_vec();
        Ok(GrantToken { payload, signature })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GrantLedgerSnapshot {
    pub schema_version: u16,
    pub verifier_keys: Vec<[u8; 32]>,
    pub consumed_grant_ids: Vec<String>,
    pub revoked_grant_ids: Vec<String>,
}

#[derive(Debug)]
pub struct GrantLedger {
    verifiers: HashMap<String, VerifyingKey>,
    consumed: HashSet<String>,
    revoked: HashSet<String>,
}

impl GrantLedger {
    pub fn new(verifier: VerifyingKey) -> Self {
        let mut verifiers = HashMap::new();
        verifiers.insert(verifying_key_id(&verifier), verifier);
        Self {
            verifiers,
            consumed: HashSet::new(),
            revoked: HashSet::new(),
        }
    }

    pub fn snapshot(&self) -> GrantLedgerSnapshot {
        let mut verifier_keys: Vec<_> = self
            .verifiers
            .values()
            .map(VerifyingKey::to_bytes)
            .collect();
        verifier_keys.sort();
        let mut consumed_grant_ids: Vec<_> = self.consumed.iter().cloned().collect();
        consumed_grant_ids.sort();
        let mut revoked_grant_ids: Vec<_> = self.revoked.iter().cloned().collect();
        revoked_grant_ids.sort();
        GrantLedgerSnapshot {
            schema_version: CONTROL_SCHEMA_VERSION,
            verifier_keys,
            consumed_grant_ids,
            revoked_grant_ids,
        }
    }

    pub fn from_snapshot(snapshot: GrantLedgerSnapshot) -> Result<Self, ControlError> {
        if snapshot.schema_version != CONTROL_SCHEMA_VERSION {
            return Err(ControlError::UnsupportedControlSchema {
                got: snapshot.schema_version,
            });
        }
        if snapshot.verifier_keys.is_empty() {
            return Err(ControlError::InvalidGrantLedgerSnapshot);
        }
        let mut verifiers = HashMap::new();
        for key_bytes in snapshot.verifier_keys {
            let verifier = VerifyingKey::from_bytes(&key_bytes)
                .map_err(|_| ControlError::InvalidGrantLedgerSnapshot)?;
            verifiers.insert(verifying_key_id(&verifier), verifier);
        }
        Ok(Self {
            verifiers,
            consumed: snapshot.consumed_grant_ids.into_iter().collect(),
            revoked: snapshot.revoked_grant_ids.into_iter().collect(),
        })
    }

    pub fn trust_verifier(&mut self, verifier: VerifyingKey) -> String {
        let key_id = verifying_key_id(&verifier);
        self.verifiers.insert(key_id.clone(), verifier);
        key_id
    }

    pub fn retire_verifier(&mut self, key_id: &str) -> bool {
        self.verifiers.remove(key_id).is_some()
    }

    pub fn revoke(&mut self, grant_id: &str) {
        self.revoked.insert(grant_id.to_owned());
    }

    pub fn authorize_once(
        &mut self,
        token: &GrantToken,
        device_id: &str,
        required: CapabilityClass,
        now_ms: u64,
    ) -> Result<(), ControlError> {
        self.verify_signature(token)?;
        let payload = &token.payload;
        if payload.schema_version != CONTROL_SCHEMA_VERSION {
            return Err(ControlError::UnsupportedControlSchema {
                got: payload.schema_version,
            });
        }
        if payload.device_id != device_id {
            return Err(ControlError::GrantDeviceMismatch);
        }
        if now_ms < payload.issued_at_ms {
            return Err(ControlError::GrantNotYetValid);
        }
        if now_ms >= payload.expires_at_ms {
            return Err(ControlError::GrantExpired);
        }
        if self.revoked.contains(&payload.grant_id) {
            return Err(ControlError::GrantRevoked);
        }
        if self.consumed.contains(&payload.grant_id) {
            return Err(ControlError::GrantReplay);
        }
        if payload.capability != required {
            return Err(ControlError::CapabilityDenied {
                granted: payload.capability,
                required,
            });
        }
        self.consumed.insert(payload.grant_id.clone());
        Ok(())
    }

    fn verify_signature(&self, token: &GrantToken) -> Result<(), ControlError> {
        let sig_bytes: [u8; 64] = token
            .signature
            .as_slice()
            .try_into()
            .map_err(|_| ControlError::InvalidGrantSignature)?;
        let signature = Signature::from_bytes(&sig_bytes);
        let bytes = canonical_grant_bytes(&token.payload)?;
        let verifier = self
            .verifiers
            .get(&token.payload.issuer_key_id)
            .ok_or(ControlError::UnknownGrantSigningKey)?;
        verifier
            .verify(&bytes, &signature)
            .map_err(|_| ControlError::InvalidGrantSignature)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommandEnvelope {
    pub schema_version: u16,
    pub device_id: String,
    pub device_generation: u64,
    pub capability_revision: u64,
    pub operation_id: String,
    pub command: DeviceCommand,
}

impl CommandEnvelope {
    pub fn required_class(&self) -> CapabilityClass {
        self.command.class()
    }
}

pub fn validate_command_session(
    command: &CommandEnvelope,
    session: &DeviceSession,
) -> Result<(), ControlError> {
    if command.schema_version != CONTROL_SCHEMA_VERSION {
        return Err(ControlError::UnsupportedControlSchema {
            got: command.schema_version,
        });
    }
    if command.device_id != session.device_id {
        return Err(ControlError::CommandDeviceMismatch);
    }
    if command.device_generation != session.generation {
        return Err(ControlError::StaleDeviceGeneration {
            expected: session.generation,
            got: command.device_generation,
        });
    }
    if command.capability_revision != session.capabilities.revision {
        return Err(ControlError::StaleCapabilityRevision {
            expected: session.capabilities.revision,
            got: command.capability_revision,
        });
    }
    let required_capability = command.command.capability();
    if !session.capabilities.supports(required_capability) {
        return Err(ControlError::UnsupportedDeviceCapability(
            required_capability,
        ));
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum DeviceResult {
    Applications {
        count: u64,
    },
    ScreenGeometry {
        width_points: u32,
        height_points: u32,
        scale_factor_milli: u32,
    },
    PointerClickCompleted,
    Process {
        output: ProcessOutput,
    },
}

impl DeviceResult {
    pub(crate) fn matches_command(&self, command: &DeviceCommand) -> bool {
        matches!(
            (self, command),
            (Self::Applications { .. }, DeviceCommand::ListApplications)
                | (Self::ScreenGeometry { .. }, DeviceCommand::ScreenGeometry)
                | (
                    Self::PointerClickCompleted,
                    DeviceCommand::PointerClick { .. }
                )
                | (Self::Process { .. }, DeviceCommand::ExecuteProcess { .. })
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommandResultEnvelope {
    pub schema_version: u16,
    pub device_id: String,
    pub device_generation: u64,
    pub capability_revision: u64,
    pub operation_id: String,
    pub result: DeviceResult,
}

pub fn validate_command_result(
    command: &CommandEnvelope,
    result: &CommandResultEnvelope,
) -> Result<(), ControlError> {
    if result.schema_version != CONTROL_SCHEMA_VERSION {
        return Err(ControlError::UnsupportedControlSchema {
            got: result.schema_version,
        });
    }
    if result.device_id != command.device_id {
        return Err(ControlError::ResultDeviceMismatch);
    }
    if result.device_generation != command.device_generation {
        return Err(ControlError::ResultGenerationMismatch);
    }
    if result.capability_revision != command.capability_revision {
        return Err(ControlError::ResultCapabilityRevisionMismatch);
    }
    if result.operation_id != command.operation_id {
        return Err(ControlError::ResultOperationMismatch);
    }
    if !result.result.matches_command(&command.command) {
        return Err(ControlError::ResultTypeMismatch);
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OperationLease {
    pub device_id: String,
    pub operation_id: String,
    pub device_generation: u64,
    pub expires_at_ms: u64,
}

#[derive(Debug, Default)]
pub struct LeaseManager {
    leases: HashMap<String, OperationLease>,
}

impl LeaseManager {
    pub fn acquire(
        &mut self,
        device_id: &str,
        operation_id: &str,
        device_generation: u64,
        now_ms: u64,
        ttl_ms: u64,
    ) -> Result<OperationLease, ControlError> {
        if ttl_ms == 0 {
            return Err(ControlError::InvalidLeaseLifetime);
        }
        if let Some(existing) = self.leases.get(device_id) {
            if existing.expires_at_ms > now_ms {
                if existing.operation_id == operation_id
                    && existing.device_generation == device_generation
                {
                    return Ok(existing.clone());
                }
                return Err(ControlError::LeaseConflict {
                    owner_operation_id: existing.operation_id.clone(),
                    owner_generation: existing.device_generation,
                });
            }
        }
        let lease = OperationLease {
            device_id: device_id.to_owned(),
            operation_id: operation_id.to_owned(),
            device_generation,
            expires_at_ms: now_ms.saturating_add(ttl_ms),
        };
        self.leases.insert(device_id.to_owned(), lease.clone());
        Ok(lease)
    }

    pub fn release(
        &mut self,
        device_id: &str,
        operation_id: &str,
        device_generation: u64,
    ) -> Result<(), ControlError> {
        let existing = self
            .leases
            .get(device_id)
            .ok_or(ControlError::LeaseNotFound)?;
        if existing.operation_id != operation_id || existing.device_generation != device_generation
        {
            return Err(ControlError::LeaseOwnershipMismatch);
        }
        self.leases.remove(device_id);
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PolicyOutcome {
    Allowed,
    Denied,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuditReason {
    GrantValidBackendCompleted,
    ObserveGrantCannotAuthorizeInteract,
    GrantReplayRejected,
    GrantRevokedRejected,
    GrantExpiredRejected,
    LeaseConflictAfterReconnect,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuditEvidence {
    pub event_id: String,
    pub occurred_at_ms: u64,
    pub device_id: String,
    pub device_generation: u64,
    pub grant_id: Option<String>,
    pub operation_id: Option<String>,
    pub capability: Option<CapabilityClass>,
    pub outcome: PolicyOutcome,
    pub reason: AuditReason,
}

#[derive(Debug, Default)]
pub struct AuditLog {
    events: Vec<AuditEvidence>,
}

impl AuditLog {
    pub fn record(&mut self, mut evidence: AuditEvidence) {
        if evidence.event_id.is_empty() {
            let mut random = [0_u8; 12];
            OsRng.fill_bytes(&mut random);
            evidence.event_id = format!("audit_{}", hex(&random));
        }
        self.events.push(evidence);
    }

    pub fn events(&self) -> &[AuditEvidence] {
        &self.events
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ControlError {
    InvalidDeviceIdentity,
    InvalidEnrollmentProof,
    InvalidDeviceSignature,
    InvalidDeviceRotationProof,
    InvalidRegistrySnapshot,
    UnknownDevice,
    DeviceRevoked,
    DeviceOffline,
    UnsupportedCapabilitySchema {
        got: u16,
    },
    UnsupportedControlSchema {
        got: u16,
    },
    InvalidGrantLifetime,
    InvalidGrantSignature,
    UnknownGrantSigningKey,
    InvalidGrantLedgerSnapshot,
    GrantDeviceMismatch,
    GrantNotYetValid,
    GrantExpired,
    GrantRevoked,
    GrantReplay,
    CapabilityDenied {
        granted: CapabilityClass,
        required: CapabilityClass,
    },
    CommandDeviceMismatch,
    StaleDeviceGeneration {
        expected: u64,
        got: u64,
    },
    StaleCapabilityRevision {
        expected: u64,
        got: u64,
    },
    UnsupportedDeviceCapability(DeviceCapability),
    ResultDeviceMismatch,
    ResultGenerationMismatch,
    ResultCapabilityRevisionMismatch,
    ResultOperationMismatch,
    ResultTypeMismatch,
    InvalidLeaseLifetime,
    LeaseConflict {
        owner_operation_id: String,
        owner_generation: u64,
    },
    LeaseNotFound,
    LeaseOwnershipMismatch,
    Serialization,
}

impl fmt::Display for ControlError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self:?}")
    }
}

impl std::error::Error for ControlError {}

fn canonical_grant_bytes(payload: &GrantPayload) -> Result<Vec<u8>, ControlError> {
    serde_json::to_vec(payload).map_err(|_| ControlError::Serialization)
}

pub fn verifying_key_id(verifier: &VerifyingKey) -> String {
    format!("key_{}", hex(&verifier.to_bytes()))
}

fn hex(bytes: &[u8]) -> String {
    const TABLE: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(TABLE[(byte >> 4) as usize] as char);
        output.push(TABLE[(byte & 0x0f) as usize] as char);
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    fn capabilities(revision: u64) -> CapabilityAdvertisement {
        CapabilityAdvertisement {
            backend: "cua".into(),
            backend_version: "0.19.3".into(),
            platform: "darwin-arm64".into(),
            capability_schema_version: CAPABILITY_SCHEMA_VERSION,
            revision,
            supported: vec![
                DeviceCapability::ListApplications,
                DeviceCapability::ScreenGeometry,
                DeviceCapability::PointerClick,
            ],
        }
    }

    fn enrolled() -> (DeviceRegistry, DeviceIdentity, String) {
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
    fn enrollment_requires_proof_of_private_key() {
        let identity = DeviceIdentity::generate();
        let attacker = DeviceIdentity::generate();
        let challenge = DeviceRegistry::enrollment_challenge();
        let bad_proof = attacker.enrollment_proof(&challenge);
        let mut registry = DeviceRegistry::default();
        assert_eq!(
            registry.enroll(&identity.public_key(), &challenge, &bad_proof),
            Err(ControlError::InvalidEnrollmentProof)
        );
    }

    #[test]
    fn observe_grant_cannot_authorize_interact_and_is_one_shot() {
        let (mut registry, _identity, device_id) = enrolled();
        let session = registry.connect(&device_id, capabilities(7)).unwrap();
        let authority = GrantAuthority::generate();
        let mut ledger = GrantLedger::new(authority.verifier());
        let token = authority
            .issue(&device_id, CapabilityClass::Observe, 1_000, 30_000)
            .unwrap();

        let interact = CommandEnvelope {
            schema_version: CONTROL_SCHEMA_VERSION,
            device_id: device_id.clone(),
            device_generation: session.generation,
            capability_revision: 7,
            operation_id: "op-interact".into(),
            command: DeviceCommand::PointerClick {
                x: 10,
                y: 20,
                button: PointerButton::Left,
            },
        };
        validate_command_session(&interact, &session).unwrap();
        assert!(matches!(
            ledger.authorize_once(&token, &device_id, interact.required_class(), 1_001),
            Err(ControlError::CapabilityDenied { .. })
        ));

        let observe = CommandEnvelope {
            command: DeviceCommand::ListApplications,
            operation_id: "op-observe".into(),
            ..interact
        };
        ledger
            .authorize_once(&token, &device_id, observe.required_class(), 1_002)
            .unwrap();
        assert_eq!(
            ledger.authorize_once(&token, &device_id, observe.required_class(), 1_003),
            Err(ControlError::GrantReplay)
        );
    }

    #[test]
    fn unknown_grant_signing_key_and_signature_tampering_are_rejected() {
        let (_registry, _identity, device_id) = enrolled();
        let authority = GrantAuthority::generate();
        let attacker = GrantAuthority::generate();
        let mut ledger = GrantLedger::new(authority.verifier());
        let forged = attacker
            .issue(&device_id, CapabilityClass::Observe, 1_000, 30_000)
            .unwrap();
        assert_eq!(
            ledger.authorize_once(&forged, &device_id, CapabilityClass::Observe, 1_001),
            Err(ControlError::UnknownGrantSigningKey)
        );

        let mut tampered = authority
            .issue(&device_id, CapabilityClass::Observe, 1_000, 30_000)
            .unwrap();
        tampered.signature[0] ^= 0x01;
        assert_eq!(
            ledger.authorize_once(&tampered, &device_id, CapabilityClass::Observe, 1_001),
            Err(ControlError::InvalidGrantSignature)
        );
    }

    #[test]
    fn expired_and_revoked_grants_fail_closed() {
        let (_registry, _identity, device_id) = enrolled();
        let authority = GrantAuthority::generate();
        let mut ledger = GrantLedger::new(authority.verifier());
        let expired = authority
            .issue(&device_id, CapabilityClass::Observe, 10, 5)
            .unwrap();
        assert_eq!(
            ledger.authorize_once(&expired, &device_id, CapabilityClass::Observe, 15),
            Err(ControlError::GrantExpired)
        );

        let revoked = authority
            .issue(&device_id, CapabilityClass::Observe, 20, 10_000)
            .unwrap();
        ledger.revoke(&revoked.payload.grant_id);
        assert_eq!(
            ledger.authorize_once(&revoked, &device_id, CapabilityClass::Observe, 21),
            Err(ControlError::GrantRevoked)
        );
    }

    #[test]
    fn reconnect_does_not_transfer_an_in_flight_lease() {
        let (mut registry, _identity, device_id) = enrolled();
        let first = registry.connect(&device_id, capabilities(1)).unwrap();
        let mut leases = LeaseManager::default();
        leases
            .acquire(&device_id, "op-1", first.generation, 1_000, 10_000)
            .unwrap();

        let second = registry.connect(&device_id, capabilities(1)).unwrap();
        assert!(second.generation > first.generation);
        assert!(matches!(
            leases.acquire(&device_id, "op-2", second.generation, 1_001, 10_000),
            Err(ControlError::LeaseConflict { owner_generation, .. }) if owner_generation == first.generation
        ));
        assert_eq!(
            leases.release(&device_id, "op-1", second.generation),
            Err(ControlError::LeaseOwnershipMismatch)
        );

        leases
            .acquire(&device_id, "op-2", second.generation, 11_001, 10_000)
            .unwrap();
    }

    #[test]
    fn stale_generation_and_capability_revision_are_rejected() {
        let (mut registry, _identity, device_id) = enrolled();
        let first = registry.connect(&device_id, capabilities(9)).unwrap();
        let second = registry.connect(&device_id, capabilities(10)).unwrap();

        let stale_generation = CommandEnvelope {
            schema_version: CONTROL_SCHEMA_VERSION,
            device_id: device_id.clone(),
            device_generation: first.generation,
            capability_revision: second.capabilities.revision,
            operation_id: "op-stale-generation".into(),
            command: DeviceCommand::ListApplications,
        };
        assert!(matches!(
            validate_command_session(&stale_generation, &second),
            Err(ControlError::StaleDeviceGeneration { .. })
        ));

        let stale_revision = CommandEnvelope {
            device_generation: second.generation,
            capability_revision: 9,
            operation_id: "op-stale-revision".into(),
            ..stale_generation
        };
        assert!(matches!(
            validate_command_session(&stale_revision, &second),
            Err(ControlError::StaleCapabilityRevision { .. })
        ));
    }

    #[test]
    fn typed_results_must_match_the_command_envelope() {
        let command = CommandEnvelope {
            schema_version: CONTROL_SCHEMA_VERSION,
            device_id: "dev-test".into(),
            device_generation: 3,
            capability_revision: 9,
            operation_id: "op-test".into(),
            command: DeviceCommand::ListApplications,
        };
        let result = CommandResultEnvelope {
            schema_version: CONTROL_SCHEMA_VERSION,
            device_id: command.device_id.clone(),
            device_generation: command.device_generation,
            capability_revision: command.capability_revision,
            operation_id: command.operation_id.clone(),
            result: DeviceResult::Applications { count: 7 },
        };
        validate_command_result(&command, &result).unwrap();

        let mismatched = CommandResultEnvelope {
            result: DeviceResult::PointerClickCompleted,
            ..result
        };
        assert_eq!(
            validate_command_result(&command, &mismatched),
            Err(ControlError::ResultTypeMismatch)
        );
    }

    #[test]
    fn registry_snapshot_round_trip_preserves_generation_capabilities_and_revocation() {
        let (mut registry, _identity, device_id) = enrolled();
        registry.connect(&device_id, capabilities(5)).unwrap();
        let other = DeviceIdentity::generate();
        let challenge = DeviceRegistry::enrollment_challenge();
        let proof = other.enrollment_proof(&challenge);
        let other_id = registry
            .enroll(&other.public_key(), &challenge, &proof)
            .unwrap();
        registry.revoke_device(&other_id);
        let restored = DeviceRegistry::from_snapshot(registry.snapshot()).unwrap();
        assert_eq!(restored.current_session(&device_id).unwrap().generation, 1);
        assert_eq!(
            restored
                .current_session(&device_id)
                .unwrap()
                .capabilities
                .revision,
            5
        );
        assert_eq!(
            restored.verify_device_signature(&other_id, b"x", &other.sign_message(b"x")),
            Err(ControlError::DeviceRevoked)
        );
    }

    #[test]
    fn grant_ledger_snapshot_preserves_consumed_and_revoked_replay_state() {
        let authority = GrantAuthority::generate();
        let mut ledger = GrantLedger::new(authority.verifier());
        let consumed = authority
            .issue("dev", CapabilityClass::Observe, 10, 100)
            .unwrap();
        ledger
            .authorize_once(&consumed, "dev", CapabilityClass::Observe, 11)
            .unwrap();
        let revoked = authority
            .issue("dev", CapabilityClass::Observe, 10, 100)
            .unwrap();
        ledger.revoke(&revoked.payload.grant_id);
        let mut restored = GrantLedger::from_snapshot(ledger.snapshot()).unwrap();
        assert_eq!(
            restored.authorize_once(&consumed, "dev", CapabilityClass::Observe, 12),
            Err(ControlError::GrantReplay)
        );
        assert_eq!(
            restored.authorize_once(&revoked, "dev", CapabilityClass::Observe, 12),
            Err(ControlError::GrantRevoked)
        );
    }

    #[test]
    fn audit_evidence_has_no_raw_command_or_result_fields() {
        let mut log = AuditLog::default();
        log.record(AuditEvidence {
            event_id: String::new(),
            occurred_at_ms: 1,
            device_id: "dev-test".into(),
            device_generation: 2,
            grant_id: Some("grant-test".into()),
            operation_id: Some("op-test".into()),
            capability: Some(CapabilityClass::Observe),
            outcome: PolicyOutcome::Allowed,
            reason: AuditReason::GrantValidBackendCompleted,
        });
        let value = serde_json::to_value(&log.events()[0]).unwrap();
        assert!(value.get("arguments").is_none());
        assert!(value.get("result").is_none());
        assert!(value.get("screenshot").is_none());
    }
}
