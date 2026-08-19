use computer_use_mcp_gateway::{
    v2_execution_safety::{DesktopQuarantine, IndeterminateReason, OperationOwner},
    v2_m0_execution::IndeterminateResolution,
    v2_m0_transport::HubIdentity,
    v2_online_recovery::{
        RECOVERY_PUBLIC_KEY_FILENAME, RecoveryAuditAssessment, RecoveryError, RecoveryVerifier,
        authorization_signing_bytes, build_recovery_challenge, clear_recovery_handoff,
        load_authorization, new_authorization, quarantine_fingerprint, store_authorization,
        validate_authorization_against_challenge, verify_recovery_challenge,
    },
};
use ring::{
    rand::SystemRandom,
    signature::{ECDSA_P256_SHA256_ASN1_SIGNING, EcdsaKeyPair, KeyPair as _},
};
use std::path::PathBuf;

fn temp_dir(name: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!(
        "cumg-{name}-{}-{}",
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

fn quarantine() -> DesktopQuarantine {
    DesktopQuarantine {
        device_id: "dev_online_recovery".into(),
        operation_id: "op_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".into(),
        device_generation: 7,
        owner: OperationOwner::new("https://issuer.example", "alice").unwrap(),
        reason: IndeterminateReason::BackendOutcomeUnproven,
        since_ms: 1_700_000_000_000,
    }
}

fn test_recovery_key() -> (EcdsaKeyPair, RecoveryVerifier) {
    let rng = SystemRandom::new();
    let pkcs8 = EcdsaKeyPair::generate_pkcs8(&ECDSA_P256_SHA256_ASN1_SIGNING, &rng).unwrap();
    let key =
        EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_ASN1_SIGNING, pkcs8.as_ref(), &rng).unwrap();
    let verifier = RecoveryVerifier::from_x963_bytes(key.public_key().as_ref()).unwrap();
    (key, verifier)
}

fn sign_authorization(
    key: &EcdsaKeyPair,
    mut authorization: computer_use_mcp_gateway::v2_online_recovery::RecoveryAuthorization,
) -> computer_use_mcp_gateway::v2_online_recovery::RecoveryAuthorization {
    let rng = SystemRandom::new();
    let bytes = authorization_signing_bytes(&authorization).unwrap();
    authorization.signature = key.sign(&rng, &bytes).unwrap().as_ref().to_vec();
    authorization
}

#[test]
fn recovery_authority_is_bound_to_exact_challenge_and_decision() {
    let hub = HubIdentity::generate();
    let quarantine = quarantine();
    let challenge = build_recovery_challenge(&hub, &quarantine, 8, 100).unwrap();
    verify_recovery_challenge(&challenge, &hub.verifier(), &quarantine.device_id, 8, 101).unwrap();

    let (key, verifier) = test_recovery_key();
    let authorization = sign_authorization(
        &key,
        new_authorization(
            &challenge,
            RecoveryAuditAssessment::Inconclusive,
            IndeterminateResolution::ConfirmedCompleted,
            "local user inspected the current desktop",
        )
        .unwrap(),
    );
    verifier
        .verify_authorization(&challenge, &authorization, 101)
        .unwrap();

    let mut changed_decision = authorization.clone();
    changed_decision.decision = IndeterminateResolution::ConfirmedNotExecuted;
    assert_eq!(
        verifier.verify_authorization(&challenge, &changed_decision, 101),
        Err(RecoveryError::InvalidRecoverySignature)
    );

    let mut changed_fingerprint = authorization.clone();
    changed_fingerprint.quarantine_fingerprint[0] ^= 1;
    assert_eq!(
        verifier.verify_authorization(&challenge, &changed_fingerprint, 101),
        Err(RecoveryError::ChallengeMismatch)
    );
}

#[test]
fn stale_or_wrong_generation_challenge_fails_closed() {
    let hub = HubIdentity::generate();
    let quarantine = quarantine();
    let challenge = build_recovery_challenge(&hub, &quarantine, 8, 100).unwrap();

    assert_eq!(
        verify_recovery_challenge(&challenge, &hub.verifier(), &quarantine.device_id, 9, 101),
        Err(RecoveryError::ChallengeMismatch)
    );
    assert_eq!(
        verify_recovery_challenge(
            &challenge,
            &hub.verifier(),
            &quarantine.device_id,
            8,
            challenge.expires_at_ms + 1,
        ),
        Err(RecoveryError::ExpiredChallenge)
    );

    let mut changed_quarantine = quarantine.clone();
    changed_quarantine.since_ms += 1;
    assert_ne!(
        quarantine_fingerprint(&quarantine),
        quarantine_fingerprint(&changed_quarantine)
    );
}

#[test]
fn local_authorization_handoff_is_no_clobber() {
    let state_dir = temp_dir("online-recovery-no-clobber");
    let hub = HubIdentity::generate();
    let challenge = build_recovery_challenge(&hub, &quarantine(), 8, 100).unwrap();
    let first = new_authorization(
        &challenge,
        RecoveryAuditAssessment::Inconclusive,
        IndeterminateResolution::ConfirmedCompleted,
        "first local decision",
    )
    .unwrap();
    let second = new_authorization(
        &challenge,
        RecoveryAuditAssessment::Inconclusive,
        IndeterminateResolution::ConfirmedNotExecuted,
        "conflicting second decision",
    )
    .unwrap();

    store_authorization(&state_dir, &first).unwrap();
    assert_eq!(
        store_authorization(&state_dir, &second),
        Err(RecoveryError::AuthorizationAlreadyPending)
    );
    assert_eq!(load_authorization(&state_dir).unwrap(), Some(first));

    clear_recovery_handoff(&state_dir).unwrap();
    let _ = std::fs::remove_dir_all(state_dir);
}

#[test]
fn evidence_must_be_bounded_and_nonempty() {
    let hub = HubIdentity::generate();
    let challenge = build_recovery_challenge(&hub, &quarantine(), 8, 100).unwrap();
    assert_eq!(
        new_authorization(
            &challenge,
            RecoveryAuditAssessment::Inconclusive,
            IndeterminateResolution::ConfirmedCompleted,
            "",
        ),
        Err(RecoveryError::InvalidEvidence)
    );
    assert_eq!(
        new_authorization(
            &challenge,
            RecoveryAuditAssessment::Inconclusive,
            IndeterminateResolution::ConfirmedCompleted,
            "x".repeat(1025),
        ),
        Err(RecoveryError::InvalidEvidence)
    );
}

#[cfg(unix)]
#[test]
fn recovery_public_key_uses_existing_trust_anchor_file_hardening() {
    use std::os::unix::fs::{PermissionsExt, symlink};

    let state_dir = temp_dir("online-recovery-trust-anchor");
    let (_, verifier) = test_recovery_key();
    let public_path = state_dir.join(RECOVERY_PUBLIC_KEY_FILENAME);
    std::fs::write(&public_path, verifier.public_key_bytes()).unwrap();
    std::fs::set_permissions(&public_path, std::fs::Permissions::from_mode(0o644)).unwrap();
    assert!(
        RecoveryVerifier::load_optional(&state_dir)
            .unwrap()
            .is_some()
    );

    std::fs::set_permissions(&public_path, std::fs::Permissions::from_mode(0o666)).unwrap();
    assert_eq!(
        RecoveryVerifier::load_optional(&state_dir).err(),
        Some(RecoveryError::UnsafeTrustAnchor)
    );

    std::fs::remove_file(&public_path).unwrap();
    let actual = state_dir.join("actual-recovery-key.p256");
    std::fs::write(&actual, verifier.public_key_bytes()).unwrap();
    std::fs::set_permissions(&actual, std::fs::Permissions::from_mode(0o644)).unwrap();
    symlink(&actual, &public_path).unwrap();
    assert_eq!(
        RecoveryVerifier::load_optional(&state_dir).err(),
        Some(RecoveryError::UnsafeTrustAnchor)
    );

    let _ = std::fs::remove_dir_all(state_dir);
}

#[test]
fn authorization_validation_separates_historical_and_current_generation() {
    let hub = HubIdentity::generate();
    let quarantine = quarantine();
    let challenge = build_recovery_challenge(&hub, &quarantine, 19, 100).unwrap();
    assert_eq!(challenge.quarantine_generation, 7);
    assert_eq!(challenge.current_generation, 19);

    let authorization = new_authorization(
        &challenge,
        RecoveryAuditAssessment::Inconclusive,
        IndeterminateResolution::ConfirmedNotExecuted,
        "local user verified no effect",
    )
    .unwrap();
    validate_authorization_against_challenge(&challenge, &authorization, 101).unwrap();
}
