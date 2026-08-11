//! V2-M0 trust-boundary and key-rotation model.
//!
//! This module deliberately keeps three identities separate:
//! - northbound authenticated MCP client principals;
//! - Hub↔Agent transport identities;
//! - Hub grant-signing authorities.
//!
//! It also proves continuity for Hub and Agent key rotation without silently
//! changing the logical device identifier or accepting an unproven replacement.

use crate::v2_m0::{
    CapabilityClass, ControlError, DeviceIdentity, DeviceRegistry, GrantAuthority, GrantToken,
    verifying_key_id,
};
use crate::v2_m0_transport::HubIdentity;
use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::fmt;

pub const TRUST_SCHEMA_VERSION: u16 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceKeyRotation {
    pub schema_version: u16,
    pub device_id: String,
    pub previous_key_id: String,
    pub next_public_key: [u8; 32],
    pub rotation_epoch: u64,
    pub old_signature: Vec<u8>,
    pub new_signature: Vec<u8>,
}

pub fn build_device_key_rotation(
    device_id: &str,
    old_identity: &DeviceIdentity,
    new_identity: &DeviceIdentity,
    rotation_epoch: u64,
) -> Result<DeviceKeyRotation, TrustError> {
    if rotation_epoch == 0 {
        return Err(TrustError::InvalidRotationEpoch);
    }
    let previous_key_id = verifying_key_id(&old_identity.verifying_key());
    let next_public_key = new_identity.verifying_key().to_bytes();
    let unsigned = DeviceKeyRotationUnsigned {
        schema_version: TRUST_SCHEMA_VERSION,
        device_id,
        previous_key_id: &previous_key_id,
        next_public_key: &next_public_key,
        rotation_epoch,
    };
    let bytes = serde_json::to_vec(&unsigned).map_err(TrustError::Serialization)?;
    Ok(DeviceKeyRotation {
        schema_version: TRUST_SCHEMA_VERSION,
        device_id: device_id.to_owned(),
        previous_key_id,
        next_public_key,
        rotation_epoch,
        old_signature: old_identity.sign_message(&bytes),
        new_signature: new_identity.sign_message(&bytes),
    })
}

pub fn apply_device_key_rotation(
    registry: &mut DeviceRegistry,
    rotation: &DeviceKeyRotation,
    expected_epoch: u64,
) -> Result<(), TrustError> {
    validate_schema(rotation.schema_version)?;
    if rotation.rotation_epoch != expected_epoch || expected_epoch == 0 {
        return Err(TrustError::RotationEpochMismatch {
            expected: expected_epoch,
            got: rotation.rotation_epoch,
        });
    }
    let unsigned = DeviceKeyRotationUnsigned {
        schema_version: rotation.schema_version,
        device_id: &rotation.device_id,
        previous_key_id: &rotation.previous_key_id,
        next_public_key: &rotation.next_public_key,
        rotation_epoch: rotation.rotation_epoch,
    };
    let bytes = serde_json::to_vec(&unsigned).map_err(TrustError::Serialization)?;
    registry
        .rotate_device_key(
            &rotation.device_id,
            &rotation.next_public_key,
            &bytes,
            &rotation.old_signature,
            &rotation.new_signature,
        )
        .map_err(TrustError::Control)
}

#[derive(Serialize)]
struct DeviceKeyRotationUnsigned<'a> {
    schema_version: u16,
    device_id: &'a str,
    previous_key_id: &'a str,
    next_public_key: &'a [u8; 32],
    rotation_epoch: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HubKeyRotation {
    pub schema_version: u16,
    pub previous_key_id: String,
    pub next_public_key: [u8; 32],
    pub rotation_epoch: u64,
    pub old_signature: Vec<u8>,
    pub new_signature: Vec<u8>,
}

pub fn build_hub_key_rotation(
    old_identity: &HubIdentity,
    new_identity: &HubIdentity,
    rotation_epoch: u64,
) -> Result<HubKeyRotation, TrustError> {
    if rotation_epoch == 0 {
        return Err(TrustError::InvalidRotationEpoch);
    }
    let previous_key_id = verifying_key_id(&old_identity.verifier());
    let next_public_key = new_identity.public_key();
    let bytes = hub_rotation_bytes(
        TRUST_SCHEMA_VERSION,
        &previous_key_id,
        &next_public_key,
        rotation_epoch,
    )?;
    Ok(HubKeyRotation {
        schema_version: TRUST_SCHEMA_VERSION,
        previous_key_id,
        next_public_key,
        rotation_epoch,
        old_signature: old_identity.sign_message(&bytes),
        new_signature: new_identity.sign_message(&bytes),
    })
}

#[derive(Debug, Clone)]
pub struct TrustedHubIdentity {
    current: VerifyingKey,
    epoch: u64,
}

impl TrustedHubIdentity {
    pub fn new(initial: VerifyingKey) -> Self {
        Self {
            current: initial,
            epoch: 0,
        }
    }

    pub fn verifier(&self) -> VerifyingKey {
        self.current
    }

    pub fn epoch(&self) -> u64 {
        self.epoch
    }

    pub fn apply_rotation(&mut self, rotation: &HubKeyRotation) -> Result<(), TrustError> {
        validate_schema(rotation.schema_version)?;
        let expected_epoch = self.epoch.saturating_add(1);
        if rotation.rotation_epoch != expected_epoch {
            return Err(TrustError::RotationEpochMismatch {
                expected: expected_epoch,
                got: rotation.rotation_epoch,
            });
        }
        let current_key_id = verifying_key_id(&self.current);
        if rotation.previous_key_id != current_key_id {
            return Err(TrustError::PreviousKeyMismatch);
        }
        let next = VerifyingKey::from_bytes(&rotation.next_public_key)
            .map_err(|_| TrustError::InvalidPublicKey)?;
        let bytes = hub_rotation_bytes(
            rotation.schema_version,
            &rotation.previous_key_id,
            &rotation.next_public_key,
            rotation.rotation_epoch,
        )?;
        verify_signature(&self.current, &bytes, &rotation.old_signature)?;
        verify_signature(&next, &bytes, &rotation.new_signature)?;
        self.current = next;
        self.epoch = rotation.rotation_epoch;
        Ok(())
    }
}

fn hub_rotation_bytes(
    schema_version: u16,
    previous_key_id: &str,
    next_public_key: &[u8; 32],
    rotation_epoch: u64,
) -> Result<Vec<u8>, TrustError> {
    #[derive(Serialize)]
    struct Unsigned<'a> {
        domain: &'static str,
        schema_version: u16,
        previous_key_id: &'a str,
        next_public_key: &'a [u8; 32],
        rotation_epoch: u64,
    }
    serde_json::to_vec(&Unsigned {
        domain: "cumg-v2-m0-hub-key-rotation",
        schema_version,
        previous_key_id,
        next_public_key,
        rotation_epoch,
    })
    .map_err(TrustError::Serialization)
}

fn verify_signature(
    verifier: &VerifyingKey,
    bytes: &[u8],
    signature: &[u8],
) -> Result<(), TrustError> {
    let raw: [u8; 64] = signature
        .try_into()
        .map_err(|_| TrustError::InvalidRotationSignature)?;
    verifier
        .verify(bytes, &Signature::from_bytes(&raw))
        .map_err(|_| TrustError::InvalidRotationSignature)
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct AuthenticatedClientPrincipal {
    pub issuer: String,
    pub subject: String,
}

impl AuthenticatedClientPrincipal {
    pub fn new(issuer: impl Into<String>, subject: impl Into<String>) -> Result<Self, TrustError> {
        let issuer = issuer.into();
        let subject = subject.into();
        if issuer.trim().is_empty() || subject.trim().is_empty() {
            return Err(TrustError::InvalidClientPrincipal);
        }
        Ok(Self { issuer, subject })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct ClientDeviceKey {
    issuer: String,
    subject: String,
    device_id: String,
}

#[derive(Debug, Default)]
pub struct ClientAuthorizationPolicy {
    allowed: HashMap<ClientDeviceKey, HashSet<CapabilityClass>>,
}

impl ClientAuthorizationPolicy {
    pub fn allow(
        &mut self,
        principal: &AuthenticatedClientPrincipal,
        device_id: &str,
        capability: CapabilityClass,
    ) {
        self.allowed
            .entry(ClientDeviceKey {
                issuer: principal.issuer.clone(),
                subject: principal.subject.clone(),
                device_id: device_id.to_owned(),
            })
            .or_default()
            .insert(capability);
    }

    pub fn authorize(
        &self,
        principal: &AuthenticatedClientPrincipal,
        device_id: &str,
        capability: CapabilityClass,
    ) -> Result<(), TrustError> {
        let key = ClientDeviceKey {
            issuer: principal.issuer.clone(),
            subject: principal.subject.clone(),
            device_id: device_id.to_owned(),
        };
        let allowed = self
            .allowed
            .get(&key)
            .is_some_and(|classes| classes.contains(&capability));
        if allowed {
            Ok(())
        } else {
            Err(TrustError::ClientCapabilityDenied)
        }
    }

    pub fn issue_grant(
        &self,
        principal: &AuthenticatedClientPrincipal,
        authority: &GrantAuthority,
        device_id: &str,
        capability: CapabilityClass,
        now_ms: u64,
        ttl_ms: u64,
    ) -> Result<GrantToken, TrustError> {
        self.authorize(principal, device_id, capability)?;
        authority
            .issue(device_id, capability, now_ms, ttl_ms)
            .map_err(TrustError::Control)
    }
}

fn validate_schema(got: u16) -> Result<(), TrustError> {
    if got == TRUST_SCHEMA_VERSION {
        Ok(())
    } else {
        Err(TrustError::UnsupportedSchema { got })
    }
}

#[derive(Debug)]
pub enum TrustError {
    Serialization(serde_json::Error),
    Control(ControlError),
    UnsupportedSchema { got: u16 },
    InvalidRotationEpoch,
    RotationEpochMismatch { expected: u64, got: u64 },
    PreviousKeyMismatch,
    InvalidPublicKey,
    InvalidRotationSignature,
    InvalidClientPrincipal,
    ClientCapabilityDenied,
}

impl fmt::Display for TrustError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self:?}")
    }
}

impl std::error::Error for TrustError {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::v2_m0::{
        CAPABILITY_SCHEMA_VERSION, CapabilityAdvertisement, DeviceCapability, GrantLedger,
    };

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

    fn caps() -> CapabilityAdvertisement {
        CapabilityAdvertisement {
            backend: "test".into(),
            backend_version: "1".into(),
            platform: "test".into(),
            capability_schema_version: CAPABILITY_SCHEMA_VERSION,
            revision: 1,
            supported: vec![DeviceCapability::ListApplications],
        }
    }

    #[test]
    fn device_key_rotation_requires_old_and_new_proof_and_invalidates_session() {
        let (mut registry, old, device_id) = enrolled();
        let first = registry.connect(&device_id, caps()).unwrap();
        let next = DeviceIdentity::generate();
        let rotation = build_device_key_rotation(&device_id, &old, &next, 1).unwrap();
        apply_device_key_rotation(&mut registry, &rotation, 1).unwrap();
        assert!(matches!(
            registry.current_session(&device_id),
            Err(ControlError::DeviceOffline)
        ));
        assert!(
            registry
                .verify_device_signature(&device_id, b"next", &next.sign_message(b"next"))
                .is_ok()
        );
        assert!(
            registry
                .verify_device_signature(&device_id, b"old", &old.sign_message(b"old"))
                .is_err()
        );
        let second = registry.connect(&device_id, caps()).unwrap();
        assert!(second.generation > first.generation);
    }

    #[test]
    fn forged_device_rotation_is_rejected() {
        let (mut registry, old, device_id) = enrolled();
        let next = DeviceIdentity::generate();
        let attacker = DeviceIdentity::generate();
        let mut rotation = build_device_key_rotation(&device_id, &old, &next, 1).unwrap();
        let replacement = build_device_key_rotation(&device_id, &old, &attacker, 1).unwrap();
        rotation.new_signature = replacement.new_signature;
        assert!(matches!(
            apply_device_key_rotation(&mut registry, &rotation, 1),
            Err(TrustError::Control(
                ControlError::InvalidDeviceRotationProof
            ))
        ));
    }

    #[test]
    fn hub_rotation_requires_continuity_and_dual_proof() {
        let old = HubIdentity::generate();
        let next = HubIdentity::generate();
        let mut trusted = TrustedHubIdentity::new(old.verifier());
        let rotation = build_hub_key_rotation(&old, &next, 1).unwrap();
        trusted.apply_rotation(&rotation).unwrap();
        assert_eq!(trusted.epoch(), 1);
        assert_eq!(trusted.verifier(), next.verifier());

        let attacker = HubIdentity::generate();
        let forged = build_hub_key_rotation(&attacker, &HubIdentity::generate(), 2).unwrap();
        assert!(matches!(
            trusted.apply_rotation(&forged),
            Err(TrustError::PreviousKeyMismatch)
        ));
    }

    #[test]
    fn grant_signing_rotation_supports_overlap_then_retirement() {
        let old = GrantAuthority::generate();
        let next = GrantAuthority::generate();
        let mut ledger = GrantLedger::new(old.verifier());
        let old_token = old.issue("dev", CapabilityClass::Observe, 10, 100).unwrap();
        ledger.trust_verifier(next.verifier());
        let next_token = next
            .issue("dev", CapabilityClass::Observe, 10, 100)
            .unwrap();
        ledger
            .authorize_once(&old_token, "dev", CapabilityClass::Observe, 11)
            .unwrap();
        ledger
            .authorize_once(&next_token, "dev", CapabilityClass::Observe, 11)
            .unwrap();

        let old_after_retirement = old.issue("dev", CapabilityClass::Observe, 20, 100).unwrap();
        assert!(ledger.retire_verifier(&old.key_id()));
        assert_eq!(
            ledger.authorize_once(&old_after_retirement, "dev", CapabilityClass::Observe, 21,),
            Err(ControlError::UnknownGrantSigningKey)
        );
    }

    #[test]
    fn northbound_principal_must_be_explicitly_authorized_for_device_and_class() {
        let principal = AuthenticatedClientPrincipal::new("https://issuer", "user-123").unwrap();
        let other = AuthenticatedClientPrincipal::new("https://issuer", "user-456").unwrap();
        let authority = GrantAuthority::generate();
        let mut policy = ClientAuthorizationPolicy::default();
        policy.allow(&principal, "dev-a", CapabilityClass::Observe);

        let grant = policy
            .issue_grant(
                &principal,
                &authority,
                "dev-a",
                CapabilityClass::Observe,
                1_000,
                30_000,
            )
            .unwrap();
        assert_eq!(grant.payload.device_id, "dev-a");
        assert_eq!(grant.payload.capability, CapabilityClass::Observe);
        assert!(matches!(
            policy.issue_grant(
                &principal,
                &authority,
                "dev-a",
                CapabilityClass::Interact,
                1_000,
                30_000,
            ),
            Err(TrustError::ClientCapabilityDenied)
        ));
        assert!(matches!(
            policy.issue_grant(
                &other,
                &authority,
                "dev-a",
                CapabilityClass::Observe,
                1_000,
                30_000,
            ),
            Err(TrustError::ClientCapabilityDenied)
        ));
    }
}
