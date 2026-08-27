//! Local-user-authorized online recovery for V2 desktop quarantine.
//!
//! Recovery authority is deliberately separate from the Agent device identity.
//! The Hub issues a short-lived, Hub-signed challenge bound to the current
//! quarantine and authenticated Agent generation. A local user signs an exact
//! resolution decision with a separately provisioned P-256 recovery key. On
//! macOS that key is intended to live in the Secure Enclave with user-presence
//! access control; the Agent only relays the resulting authorization.

use crate::v2_execution_safety::{DesktopQuarantine, IndeterminateReason};
use crate::v2_m0_execution::IndeterminateResolution;
use crate::v2_m0_transport::HubIdentity;
use crate::v2_m1_keys::{KeyMaterialError, load_public_trust_bytes};
use ed25519_dalek::{Signature as Ed25519Signature, Verifier as _, VerifyingKey};
use rand::{RngCore, rngs::OsRng};
use ring::{digest, signature};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

pub const ONLINE_RECOVERY_SCHEMA_VERSION: u16 = 1;
pub const RECOVERY_CHALLENGE_TTL_MS: u64 = 120_000;
pub const MAX_RECOVERY_EVIDENCE_BYTES: usize = 1024;
pub const MAX_RECOVERY_FILE_BYTES: usize = 16 * 1024;
pub const RECOVERY_PUBLIC_KEY_FILENAME: &str = "recovery-public-key.p256";
pub const RECOVERY_CHALLENGE_FILENAME: &str = "recovery-challenge.json";
pub const RECOVERY_AUTHORIZATION_FILENAME: &str = "recovery-authorization.json";
pub const DEFAULT_MACOS_RECOVERY_KEY_LABEL: &str = "com.github.git-ksk.cumg-v2-recovery";

const CHALLENGE_DOMAIN: &[u8] = b"cumg-v2-online-recovery-challenge-v1";
const AUTHORIZATION_DOMAIN: &[u8] = b"cumg-v2-online-recovery-authorization-v1";
const RESULT_DOMAIN: &[u8] = b"cumg-v2-online-recovery-result-v1";
const FINGERPRINT_DOMAIN: &[u8] = b"cumg-v2-quarantine-fingerprint-v1";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecoveryAuditAssessment {
    Completed,
    NotExecuted,
    Inconclusive,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecoveryChallenge {
    pub schema_version: u16,
    pub device_id: String,
    pub operation_id: String,
    /// Historical generation in which the ambiguous operation was dispatched.
    pub quarantine_generation: u64,
    /// Fresh authenticated generation that is allowed to relay this challenge.
    pub current_generation: u64,
    pub quarantine_fingerprint: [u8; 32],
    pub nonce: [u8; 32],
    pub issued_at_ms: u64,
    pub expires_at_ms: u64,
    pub signature: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecoveryAuthorization {
    pub schema_version: u16,
    pub request_id: String,
    pub device_id: String,
    pub operation_id: String,
    pub quarantine_generation: u64,
    pub current_generation: u64,
    pub quarantine_fingerprint: [u8; 32],
    pub challenge_nonce: [u8; 32],
    pub challenge_expires_at_ms: u64,
    pub audit_assessment: RecoveryAuditAssessment,
    pub decision: IndeterminateResolution,
    /// Bounded local-user evidence metadata only. Raw command/result/screenshot
    /// material is intentionally excluded from this protocol.
    pub evidence: String,
    /// ASN.1/X9.62 ECDSA P-256 + SHA-256 signature from the recovery key.
    pub signature: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecoveryResolved {
    pub schema_version: u16,
    pub request_id: String,
    pub device_id: String,
    pub operation_id: String,
    pub current_generation: u64,
    pub decision: IndeterminateResolution,
    pub resolved_at_ms: u64,
    pub signature: Vec<u8>,
}

#[derive(Clone)]
pub struct RecoveryVerifier {
    public_key: [u8; 65],
}

impl RecoveryVerifier {
    pub fn from_x963_bytes(bytes: &[u8]) -> Result<Self, RecoveryError> {
        let public_key: [u8; 65] = bytes
            .try_into()
            .map_err(|_| RecoveryError::InvalidPublicKey)?;
        if public_key[0] != 0x04 {
            return Err(RecoveryError::InvalidPublicKey);
        }
        Ok(Self { public_key })
    }

    pub fn load_optional(state_dir: &Path) -> Result<Option<Self>, RecoveryError> {
        let path = state_dir.join(RECOVERY_PUBLIC_KEY_FILENAME);
        let bytes = match load_public_trust_bytes(&path, 65) {
            Ok(bytes) => bytes,
            Err(KeyMaterialError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(None);
            }
            Err(_) => return Err(RecoveryError::UnsafeTrustAnchor),
        };
        Self::from_x963_bytes(&bytes).map(Some)
    }

    pub fn public_key_bytes(&self) -> [u8; 65] {
        self.public_key
    }

    pub fn key_id(&self) -> String {
        let digest = digest::digest(&digest::SHA256, &self.public_key);
        let mut out = String::from("p256:");
        for byte in &digest.as_ref()[..8] {
            use std::fmt::Write as _;
            let _ = write!(&mut out, "{byte:02x}");
        }
        out
    }

    pub fn verify_authorization(
        &self,
        challenge: &RecoveryChallenge,
        authorization: &RecoveryAuthorization,
        now_ms: u64,
    ) -> Result<(), RecoveryError> {
        validate_authorization_against_challenge(challenge, authorization, now_ms)?;
        let message = authorization_signing_bytes(authorization)?;
        signature::UnparsedPublicKey::new(&signature::ECDSA_P256_SHA256_ASN1, self.public_key)
            .verify(&message, &authorization.signature)
            .map_err(|_| RecoveryError::InvalidRecoverySignature)
    }
}

pub fn build_recovery_challenge(
    hub_identity: &HubIdentity,
    quarantine: &DesktopQuarantine,
    current_generation: u64,
    now_ms: u64,
) -> Result<RecoveryChallenge, RecoveryError> {
    if current_generation == 0
        || quarantine.device_id.trim().is_empty()
        || quarantine.operation_id.trim().is_empty()
    {
        return Err(RecoveryError::InvalidMessage);
    }
    let mut nonce = [0_u8; 32];
    OsRng.fill_bytes(&mut nonce);
    let mut challenge = RecoveryChallenge {
        schema_version: ONLINE_RECOVERY_SCHEMA_VERSION,
        device_id: quarantine.device_id.clone(),
        operation_id: quarantine.operation_id.clone(),
        quarantine_generation: quarantine.device_generation,
        current_generation,
        quarantine_fingerprint: quarantine_fingerprint(quarantine),
        nonce,
        issued_at_ms: now_ms,
        expires_at_ms: now_ms.saturating_add(RECOVERY_CHALLENGE_TTL_MS),
        signature: Vec::new(),
    };
    let bytes = challenge_signing_bytes(&challenge)?;
    challenge.signature = hub_identity.sign_message(&bytes);
    Ok(challenge)
}

pub fn verify_recovery_challenge(
    challenge: &RecoveryChallenge,
    trusted_hub: &VerifyingKey,
    expected_device_id: &str,
    expected_current_generation: u64,
    now_ms: u64,
) -> Result<(), RecoveryError> {
    validate_schema(challenge.schema_version)?;
    if challenge.device_id != expected_device_id
        || challenge.current_generation != expected_current_generation
        || challenge.quarantine_generation == 0
        || challenge.operation_id.trim().is_empty()
        || challenge.issued_at_ms > challenge.expires_at_ms
        || now_ms > challenge.expires_at_ms
    {
        return Err(if now_ms > challenge.expires_at_ms {
            RecoveryError::ExpiredChallenge
        } else {
            RecoveryError::ChallengeMismatch
        });
    }
    let signature = Ed25519Signature::from_slice(&challenge.signature)
        .map_err(|_| RecoveryError::InvalidHubSignature)?;
    let bytes = challenge_signing_bytes(challenge)?;
    trusted_hub
        .verify(&bytes, &signature)
        .map_err(|_| RecoveryError::InvalidHubSignature)
}

pub fn new_authorization(
    challenge: &RecoveryChallenge,
    audit_assessment: RecoveryAuditAssessment,
    decision: IndeterminateResolution,
    evidence: impl Into<String>,
) -> Result<RecoveryAuthorization, RecoveryError> {
    let evidence = evidence.into();
    validate_evidence(&evidence)?;
    let _ = resolution_name(&decision)?;
    let mut random = [0_u8; 16];
    OsRng.fill_bytes(&mut random);
    let mut request_id = String::from("rec_");
    for byte in random {
        use std::fmt::Write as _;
        let _ = write!(&mut request_id, "{byte:02x}");
    }
    Ok(RecoveryAuthorization {
        schema_version: ONLINE_RECOVERY_SCHEMA_VERSION,
        request_id,
        device_id: challenge.device_id.clone(),
        operation_id: challenge.operation_id.clone(),
        quarantine_generation: challenge.quarantine_generation,
        current_generation: challenge.current_generation,
        quarantine_fingerprint: challenge.quarantine_fingerprint,
        challenge_nonce: challenge.nonce,
        challenge_expires_at_ms: challenge.expires_at_ms,
        audit_assessment,
        decision,
        evidence,
        signature: Vec::new(),
    })
}

pub fn validate_authorization_against_challenge(
    challenge: &RecoveryChallenge,
    authorization: &RecoveryAuthorization,
    now_ms: u64,
) -> Result<(), RecoveryError> {
    validate_schema(challenge.schema_version)?;
    validate_schema(authorization.schema_version)?;
    validate_evidence(&authorization.evidence)?;
    let _ = resolution_name(&authorization.decision)?;
    if authorization.request_id.len() != 36
        || !authorization.request_id.starts_with("rec_")
        || !authorization.request_id[4..]
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
    {
        return Err(RecoveryError::InvalidMessage);
    }
    if now_ms > challenge.expires_at_ms || now_ms > authorization.challenge_expires_at_ms {
        return Err(RecoveryError::ExpiredChallenge);
    }
    if authorization.device_id != challenge.device_id
        || authorization.operation_id != challenge.operation_id
        || authorization.quarantine_generation != challenge.quarantine_generation
        || authorization.current_generation != challenge.current_generation
        || authorization.quarantine_fingerprint != challenge.quarantine_fingerprint
        || authorization.challenge_nonce != challenge.nonce
        || authorization.challenge_expires_at_ms != challenge.expires_at_ms
    {
        return Err(RecoveryError::ChallengeMismatch);
    }
    Ok(())
}

pub fn build_recovery_resolved(
    hub_identity: &HubIdentity,
    authorization: &RecoveryAuthorization,
    resolved_at_ms: u64,
) -> Result<RecoveryResolved, RecoveryError> {
    let mut resolved = RecoveryResolved {
        schema_version: ONLINE_RECOVERY_SCHEMA_VERSION,
        request_id: authorization.request_id.clone(),
        device_id: authorization.device_id.clone(),
        operation_id: authorization.operation_id.clone(),
        current_generation: authorization.current_generation,
        decision: authorization.decision.clone(),
        resolved_at_ms,
        signature: Vec::new(),
    };
    let bytes = resolved_signing_bytes(&resolved)?;
    resolved.signature = hub_identity.sign_message(&bytes);
    Ok(resolved)
}

pub fn verify_recovery_resolved(
    resolved: &RecoveryResolved,
    trusted_hub: &VerifyingKey,
    expected_request_id: &str,
    expected_device_id: &str,
    expected_generation: u64,
) -> Result<(), RecoveryError> {
    validate_schema(resolved.schema_version)?;
    if resolved.request_id != expected_request_id
        || resolved.device_id != expected_device_id
        || resolved.current_generation != expected_generation
    {
        return Err(RecoveryError::ChallengeMismatch);
    }
    let signature = Ed25519Signature::from_slice(&resolved.signature)
        .map_err(|_| RecoveryError::InvalidHubSignature)?;
    let bytes = resolved_signing_bytes(resolved)?;
    trusted_hub
        .verify(&bytes, &signature)
        .map_err(|_| RecoveryError::InvalidHubSignature)
}

pub fn quarantine_fingerprint(quarantine: &DesktopQuarantine) -> [u8; 32] {
    let mut bytes = Vec::new();
    push_bytes(&mut bytes, FINGERPRINT_DOMAIN);
    push_str(&mut bytes, &quarantine.device_id);
    push_str(&mut bytes, &quarantine.operation_id);
    bytes.extend_from_slice(&quarantine.device_generation.to_be_bytes());
    push_str(&mut bytes, &quarantine.owner.issuer);
    push_str(&mut bytes, &quarantine.owner.subject);
    push_str(&mut bytes, indeterminate_reason_name(quarantine.reason));
    bytes.extend_from_slice(&quarantine.since_ms.to_be_bytes());
    let digest = digest::digest(&digest::SHA256, &bytes);
    digest.as_ref().try_into().expect("SHA-256 digest length")
}

pub fn challenge_path(state_dir: &Path) -> PathBuf {
    state_dir.join(RECOVERY_CHALLENGE_FILENAME)
}

pub fn authorization_path(state_dir: &Path) -> PathBuf {
    state_dir.join(RECOVERY_AUTHORIZATION_FILENAME)
}

pub fn store_challenge(
    state_dir: &Path,
    challenge: &RecoveryChallenge,
) -> Result<(), RecoveryError> {
    atomic_write_json_private(&challenge_path(state_dir), challenge)
}

pub fn load_challenge(state_dir: &Path) -> Result<Option<RecoveryChallenge>, RecoveryError> {
    read_json_optional(&challenge_path(state_dir))
}

pub fn store_authorization(
    state_dir: &Path,
    authorization: &RecoveryAuthorization,
) -> Result<(), RecoveryError> {
    atomic_write_json_private_no_replace(&authorization_path(state_dir), authorization)
}

pub fn load_authorization(
    state_dir: &Path,
) -> Result<Option<RecoveryAuthorization>, RecoveryError> {
    read_json_optional(&authorization_path(state_dir))
}

pub fn clear_recovery_handoff(state_dir: &Path) -> Result<(), RecoveryError> {
    remove_if_exists(&challenge_path(state_dir))?;
    remove_if_exists(&authorization_path(state_dir))?;
    Ok(())
}

pub fn clear_authorization(state_dir: &Path) -> Result<(), RecoveryError> {
    remove_if_exists(&authorization_path(state_dir))
}

pub fn authorization_signing_bytes(
    authorization: &RecoveryAuthorization,
) -> Result<Vec<u8>, RecoveryError> {
    validate_schema(authorization.schema_version)?;
    validate_evidence(&authorization.evidence)?;
    let mut bytes = Vec::new();
    push_bytes(&mut bytes, AUTHORIZATION_DOMAIN);
    bytes.extend_from_slice(&authorization.schema_version.to_be_bytes());
    push_str(&mut bytes, &authorization.request_id);
    push_str(&mut bytes, &authorization.device_id);
    push_str(&mut bytes, &authorization.operation_id);
    bytes.extend_from_slice(&authorization.quarantine_generation.to_be_bytes());
    bytes.extend_from_slice(&authorization.current_generation.to_be_bytes());
    bytes.extend_from_slice(&authorization.quarantine_fingerprint);
    bytes.extend_from_slice(&authorization.challenge_nonce);
    bytes.extend_from_slice(&authorization.challenge_expires_at_ms.to_be_bytes());
    push_str(
        &mut bytes,
        audit_assessment_name(authorization.audit_assessment),
    );
    push_str(&mut bytes, resolution_name(&authorization.decision)?);
    push_str(&mut bytes, &authorization.evidence);
    Ok(bytes)
}

fn challenge_signing_bytes(challenge: &RecoveryChallenge) -> Result<Vec<u8>, RecoveryError> {
    validate_schema(challenge.schema_version)?;
    let mut bytes = Vec::new();
    push_bytes(&mut bytes, CHALLENGE_DOMAIN);
    bytes.extend_from_slice(&challenge.schema_version.to_be_bytes());
    push_str(&mut bytes, &challenge.device_id);
    push_str(&mut bytes, &challenge.operation_id);
    bytes.extend_from_slice(&challenge.quarantine_generation.to_be_bytes());
    bytes.extend_from_slice(&challenge.current_generation.to_be_bytes());
    bytes.extend_from_slice(&challenge.quarantine_fingerprint);
    bytes.extend_from_slice(&challenge.nonce);
    bytes.extend_from_slice(&challenge.issued_at_ms.to_be_bytes());
    bytes.extend_from_slice(&challenge.expires_at_ms.to_be_bytes());
    Ok(bytes)
}

fn resolved_signing_bytes(resolved: &RecoveryResolved) -> Result<Vec<u8>, RecoveryError> {
    validate_schema(resolved.schema_version)?;
    let mut bytes = Vec::new();
    push_bytes(&mut bytes, RESULT_DOMAIN);
    bytes.extend_from_slice(&resolved.schema_version.to_be_bytes());
    push_str(&mut bytes, &resolved.request_id);
    push_str(&mut bytes, &resolved.device_id);
    push_str(&mut bytes, &resolved.operation_id);
    bytes.extend_from_slice(&resolved.current_generation.to_be_bytes());
    push_str(&mut bytes, resolution_name(&resolved.decision)?);
    bytes.extend_from_slice(&resolved.resolved_at_ms.to_be_bytes());
    Ok(bytes)
}

fn validate_schema(version: u16) -> Result<(), RecoveryError> {
    if version == ONLINE_RECOVERY_SCHEMA_VERSION {
        Ok(())
    } else {
        Err(RecoveryError::UnsupportedSchema)
    }
}

fn validate_evidence(evidence: &str) -> Result<(), RecoveryError> {
    if evidence.trim().is_empty() || evidence.len() > MAX_RECOVERY_EVIDENCE_BYTES {
        Err(RecoveryError::InvalidEvidence)
    } else {
        Ok(())
    }
}

fn push_bytes(output: &mut Vec<u8>, value: &[u8]) {
    output.extend_from_slice(&(value.len() as u32).to_be_bytes());
    output.extend_from_slice(value);
}

fn push_str(output: &mut Vec<u8>, value: &str) {
    push_bytes(output, value.as_bytes());
}

fn resolution_name(value: &IndeterminateResolution) -> Result<&'static str, RecoveryError> {
    match value {
        IndeterminateResolution::ConfirmedCompleted => Ok("confirmed_completed"),
        IndeterminateResolution::ConfirmedNotExecuted => Ok("confirmed_not_executed"),
        // This bounded text-input state is intentionally offline-only. Online recovery
        // remains the reviewed two-decision local-user contract from #100.
        IndeterminateResolution::ConfirmedEffectAppliedUncommitted => {
            Err(RecoveryError::InvalidMessage)
        }
    }
}

fn audit_assessment_name(value: RecoveryAuditAssessment) -> &'static str {
    match value {
        RecoveryAuditAssessment::Completed => "completed",
        RecoveryAuditAssessment::NotExecuted => "not_executed",
        RecoveryAuditAssessment::Inconclusive => "inconclusive",
    }
}

fn indeterminate_reason_name(value: IndeterminateReason) -> &'static str {
    match value {
        IndeterminateReason::CancellationUnproven => "cancellation_unproven",
        IndeterminateReason::BackendTimedOut => "backend_timed_out",
        IndeterminateReason::BackendOutcomeUnproven => "backend_outcome_unproven",
        IndeterminateReason::ConnectionLost => "connection_lost",
        IndeterminateReason::HubRestartAfterDispatch => "hub_restart_after_dispatch",
        IndeterminateReason::AgentRestartAfterDispatch => "agent_restart_after_dispatch",
        IndeterminateReason::ResultDeliveryLost => "result_delivery_lost",
    }
}

fn atomic_write_json_private<T: Serialize>(path: &Path, value: &T) -> Result<(), RecoveryError> {
    let bytes = serde_json::to_vec(value).map_err(|_| RecoveryError::InvalidMessage)?;
    if bytes.len() > MAX_RECOVERY_FILE_BYTES {
        return Err(RecoveryError::InvalidMessage);
    }
    let parent = path.parent().ok_or(RecoveryError::InvalidPath)?;
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or(RecoveryError::InvalidPath)?;
    let mut random = [0_u8; 8];
    OsRng.fill_bytes(&mut random);
    let mut suffix = String::new();
    for byte in random {
        use std::fmt::Write as _;
        let _ = write!(&mut suffix, "{byte:02x}");
    }
    let pending = parent.join(format!(".{file_name}.pending-{suffix}.tmp"));
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    let mut file = options.open(&pending).map_err(|_| RecoveryError::Io)?;
    let write_result = (|| {
        file.write_all(&bytes).map_err(|_| RecoveryError::Io)?;
        file.flush().map_err(|_| RecoveryError::Io)?;
        file.sync_all().map_err(|_| RecoveryError::Io)?;
        fs::rename(&pending, path).map_err(|_| RecoveryError::Io)?;
        File::open(parent)
            .and_then(|directory| directory.sync_all())
            .map_err(|_| RecoveryError::Io)?;
        Ok(())
    })();
    if write_result.is_err() {
        let _ = fs::remove_file(&pending);
    }
    write_result
}

fn atomic_write_json_private_no_replace<T: Serialize>(
    path: &Path,
    value: &T,
) -> Result<(), RecoveryError> {
    if path.exists() {
        return Err(RecoveryError::AuthorizationAlreadyPending);
    }
    let bytes = serde_json::to_vec(value).map_err(|_| RecoveryError::InvalidMessage)?;
    if bytes.len() > MAX_RECOVERY_FILE_BYTES {
        return Err(RecoveryError::InvalidMessage);
    }
    let parent = path.parent().ok_or(RecoveryError::InvalidPath)?;
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or(RecoveryError::InvalidPath)?;
    let mut random = [0_u8; 8];
    OsRng.fill_bytes(&mut random);
    let mut suffix = String::new();
    for byte in random {
        use std::fmt::Write as _;
        let _ = write!(&mut suffix, "{byte:02x}");
    }
    let pending = parent.join(format!(".{file_name}.pending-{suffix}.tmp"));
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    let mut file = options.open(&pending).map_err(|_| RecoveryError::Io)?;
    let write_result = (|| {
        file.write_all(&bytes).map_err(|_| RecoveryError::Io)?;
        file.flush().map_err(|_| RecoveryError::Io)?;
        file.sync_all().map_err(|_| RecoveryError::Io)?;
        fs::hard_link(&pending, path).map_err(|error| {
            if error.kind() == std::io::ErrorKind::AlreadyExists {
                RecoveryError::AuthorizationAlreadyPending
            } else {
                RecoveryError::Io
            }
        })?;
        // Publication already succeeded. Pending-file cleanup and directory
        // durability are best effort because this local handoff is not durable
        // execution authority; reporting failure here would invite a second,
        // conflicting local decision while the first authorization is visible.
        let _ = fs::remove_file(&pending);
        let _ = File::open(parent).and_then(|directory| directory.sync_all());
        Ok(())
    })();
    if write_result.is_err() {
        let _ = fs::remove_file(&pending);
    }
    write_result
}

fn read_json_optional<T: DeserializeOwned>(path: &Path) -> Result<Option<T>, RecoveryError> {
    let file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(RecoveryError::Io),
    };
    let mut bytes = Vec::new();
    file.take((MAX_RECOVERY_FILE_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|_| RecoveryError::Io)?;
    if bytes.len() > MAX_RECOVERY_FILE_BYTES {
        return Err(RecoveryError::InvalidMessage);
    }
    serde_json::from_slice(&bytes)
        .map(Some)
        .map_err(|_| RecoveryError::InvalidMessage)
}

fn remove_if_exists(path: &Path) -> Result<(), RecoveryError> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(_) => Err(RecoveryError::Io),
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum RecoveryError {
    UnsupportedSchema,
    InvalidMessage,
    InvalidEvidence,
    InvalidPublicKey,
    InvalidHubSignature,
    InvalidRecoverySignature,
    ChallengeMismatch,
    ExpiredChallenge,
    InvalidPath,
    Io,
    UnsafeTrustAnchor,
    UnsupportedPlatform,
    KeyUnavailable,
    KeyAlreadyExists,
    AuthorizationAlreadyPending,
    UserPresenceDenied,
}

impl RecoveryError {
    pub const fn safe_code(self) -> &'static str {
        match self {
            Self::UnsupportedSchema => "recovery_schema",
            Self::InvalidMessage => "recovery_invalid_message",
            Self::InvalidEvidence => "recovery_invalid_evidence",
            Self::InvalidPublicKey => "recovery_invalid_public_key",
            Self::InvalidHubSignature => "recovery_invalid_hub_signature",
            Self::InvalidRecoverySignature => "recovery_invalid_signature",
            Self::ChallengeMismatch => "recovery_challenge_mismatch",
            Self::ExpiredChallenge => "recovery_challenge_expired",
            Self::InvalidPath => "recovery_invalid_path",
            Self::Io => "recovery_io",
            Self::UnsafeTrustAnchor => "recovery_unsafe_trust_anchor",
            Self::UnsupportedPlatform => "recovery_unsupported_platform",
            Self::KeyUnavailable => "recovery_key_unavailable",
            Self::KeyAlreadyExists => "recovery_key_already_exists",
            Self::AuthorizationAlreadyPending => "recovery_authorization_pending",
            Self::UserPresenceDenied => "recovery_user_presence_denied",
        }
    }
}

impl fmt::Debug for RecoveryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.safe_code())
    }
}

impl fmt::Display for RecoveryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.safe_code())
    }
}

impl std::error::Error for RecoveryError {}

#[cfg(target_os = "macos")]
pub mod macos {
    use super::*;
    use security_framework::access_control::{ProtectionMode, SecAccessControl};
    use security_framework::item::{
        ItemSearchOptions, KeyClass, Location, Reference, SearchResult,
    };
    use security_framework::key::{Algorithm, GenerateKeyOptions, KeyType, SecKey, Token};
    use security_framework_sys::access_control::{
        kSecAccessControlPrivateKeyUsage, kSecAccessControlUserPresence,
    };
    use security_framework_sys::base::errSecItemNotFound;

    pub struct MacRecoveryKey {
        private_key: SecKey,
    }

    impl MacRecoveryKey {
        pub fn create_new(label: &str) -> Result<Self, RecoveryError> {
            if label.trim().is_empty() {
                return Err(RecoveryError::KeyUnavailable);
            }
            // Initial provisioning never trusts a pre-existing label. This
            // prevents a software key planted under the default label from
            // being exported and pinned as recovery authority.
            if load_private_key(label)?.is_some() {
                return Err(RecoveryError::KeyAlreadyExists);
            }
            let flags = kSecAccessControlUserPresence | kSecAccessControlPrivateKeyUsage;
            let access_control = SecAccessControl::create_with_protection(
                Some(ProtectionMode::AccessibleWhenUnlockedThisDeviceOnly),
                flags,
            )
            .map_err(|_| RecoveryError::KeyUnavailable)?;
            let mut options = GenerateKeyOptions::default();
            options
                .set_key_type(KeyType::ec())
                .set_size_in_bits(256)
                .set_token(Token::SecureEnclave)
                .set_location(Location::DataProtectionKeychain)
                .set_label(label)
                .set_access_control(access_control);
            let private_key = SecKey::new(&options).map_err(|_| RecoveryError::KeyUnavailable)?;
            Ok(Self { private_key })
        }

        pub fn load(label: &str) -> Result<Self, RecoveryError> {
            load_private_key(label)?
                .map(|private_key| Self { private_key })
                .ok_or(RecoveryError::KeyUnavailable)
        }

        pub fn public_key_bytes(&self) -> Result<[u8; 65], RecoveryError> {
            let public = self
                .private_key
                .public_key()
                .ok_or(RecoveryError::KeyUnavailable)?;
            let bytes = public
                .external_representation()
                .ok_or(RecoveryError::KeyUnavailable)?
                .to_vec();
            RecoveryVerifier::from_x963_bytes(&bytes).map(|verifier| verifier.public_key_bytes())
        }

        pub fn sign_authorization(
            &self,
            mut authorization: RecoveryAuthorization,
        ) -> Result<RecoveryAuthorization, RecoveryError> {
            if !authorization.signature.is_empty() {
                return Err(RecoveryError::InvalidMessage);
            }
            let bytes = authorization_signing_bytes(&authorization)?;
            authorization.signature = self
                .private_key
                .create_signature(Algorithm::ECDSASignatureMessageX962SHA256, &bytes)
                .map_err(|_| RecoveryError::UserPresenceDenied)?;
            Ok(authorization)
        }
    }

    fn load_private_key(label: &str) -> Result<Option<SecKey>, RecoveryError> {
        let mut search = ItemSearchOptions::new();
        search
            .key_class(KeyClass::private())
            .label(label)
            .load_refs(true)
            .ignore_legacy_keychains();
        let results = match search.search() {
            Ok(results) => results,
            Err(error) if error.code() == errSecItemNotFound => return Ok(None),
            Err(_) => return Err(RecoveryError::KeyUnavailable),
        };
        for result in results {
            if let SearchResult::Ref(Reference::Key(key)) = result {
                return Ok(Some(key));
            }
        }
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::v2_execution_safety::{DesktopQuarantine, OperationOwner};
    use crate::v2_m0_transport::HubIdentity;
    use ring::rand::SystemRandom;
    use ring::signature::{ECDSA_P256_SHA256_ASN1_SIGNING, EcdsaKeyPair, KeyPair as _};

    fn quarantine() -> DesktopQuarantine {
        DesktopQuarantine {
            device_id: "dev_test".into(),
            operation_id: "op_test".into(),
            device_generation: 4,
            owner: OperationOwner::new("issuer", "subject").unwrap(),
            reason: IndeterminateReason::ConnectionLost,
            since_ms: 10,
        }
    }

    #[test]
    fn challenge_is_hub_signed_and_generation_bound() {
        let hub = HubIdentity::generate();
        let challenge = build_recovery_challenge(&hub, &quarantine(), 5, 100).unwrap();
        verify_recovery_challenge(&challenge, &hub.verifier(), "dev_test", 5, 101).unwrap();
        assert!(matches!(
            verify_recovery_challenge(&challenge, &hub.verifier(), "dev_test", 6, 101),
            Err(RecoveryError::ChallengeMismatch)
        ));
    }

    #[test]
    fn authorization_binds_decision_and_fresh_challenge() {
        let hub = HubIdentity::generate();
        let challenge = build_recovery_challenge(&hub, &quarantine(), 5, 100).unwrap();
        let rng = SystemRandom::new();
        let pkcs8 = EcdsaKeyPair::generate_pkcs8(&ECDSA_P256_SHA256_ASN1_SIGNING, &rng).unwrap();
        let key = EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_ASN1_SIGNING, pkcs8.as_ref(), &rng)
            .unwrap();
        let verifier = RecoveryVerifier::from_x963_bytes(key.public_key().as_ref()).unwrap();
        let mut authorization = new_authorization(
            &challenge,
            RecoveryAuditAssessment::Inconclusive,
            IndeterminateResolution::ConfirmedCompleted,
            "user inspected the local desktop",
        )
        .unwrap();
        let bytes = authorization_signing_bytes(&authorization).unwrap();
        authorization.signature = key.sign(&rng, &bytes).unwrap().as_ref().to_vec();
        verifier
            .verify_authorization(&challenge, &authorization, 101)
            .unwrap();

        let mut tampered = authorization.clone();
        tampered.decision = IndeterminateResolution::ConfirmedNotExecuted;
        assert!(matches!(
            verifier.verify_authorization(&challenge, &tampered, 101),
            Err(RecoveryError::InvalidRecoverySignature)
        ));
    }

    #[test]
    fn stale_challenge_and_stale_quarantine_fingerprint_fail_closed() {
        let hub = HubIdentity::generate();
        let challenge = build_recovery_challenge(&hub, &quarantine(), 5, 100).unwrap();
        assert!(matches!(
            verify_recovery_challenge(
                &challenge,
                &hub.verifier(),
                "dev_test",
                5,
                challenge.expires_at_ms + 1,
            ),
            Err(RecoveryError::ExpiredChallenge)
        ));
        let mut changed = quarantine();
        changed.since_ms += 1;
        assert_ne!(
            quarantine_fingerprint(&quarantine()),
            quarantine_fingerprint(&changed)
        );
    }

    #[test]
    fn private_handoff_files_round_trip_and_clear() {
        let state_dir = std::env::temp_dir().join(format!(
            "cumg-recovery-test-{}-{}",
            std::process::id(),
            rand::random::<u64>()
        ));
        fs::create_dir_all(&state_dir).unwrap();
        let hub = HubIdentity::generate();
        let challenge = build_recovery_challenge(&hub, &quarantine(), 5, 100).unwrap();
        store_challenge(&state_dir, &challenge).unwrap();
        assert_eq!(load_challenge(&state_dir).unwrap(), Some(challenge));
        clear_recovery_handoff(&state_dir).unwrap();
        assert!(load_challenge(&state_dir).unwrap().is_none());
        assert!(load_authorization(&state_dir).unwrap().is_none());
        let _ = fs::remove_dir_all(state_dir);
    }
}
