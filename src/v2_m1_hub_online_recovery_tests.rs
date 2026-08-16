use super::*;
use crate::v2_m0::{DeviceCapability, DeviceIdentity, GrantAuthority};
use crate::v2_m0_execution::{ExecutionError, HubOperationState, OperationRef};
use crate::v2_m1_grpc::decode_hub_frame;
use crate::v2_online_recovery::{
    RECOVERY_PUBLIC_KEY_FILENAME, RecoveryAuditAssessment, authorization_signing_bytes,
    new_authorization,
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

fn recovery_key() -> (EcdsaKeyPair, [u8; 65]) {
    let rng = SystemRandom::new();
    let pkcs8 = EcdsaKeyPair::generate_pkcs8(&ECDSA_P256_SHA256_ASN1_SIGNING, &rng).unwrap();
    let key =
        EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_ASN1_SIGNING, pkcs8.as_ref(), &rng).unwrap();
    let public: [u8; 65] = key.public_key().as_ref().try_into().unwrap();
    (key, public)
}

fn sign(key: &EcdsaKeyPair, mut authorization: RecoveryAuthorization) -> RecoveryAuthorization {
    let rng = SystemRandom::new();
    let bytes = authorization_signing_bytes(&authorization).unwrap();
    authorization.signature = key.sign(&rng, &bytes).unwrap().as_ref().to_vec();
    authorization
}

fn config(state_dir: PathBuf) -> HubServiceConfig {
    HubServiceConfig {
        state_dir,
        heartbeat_timeout: Duration::from_secs(2),
        max_agent_session_lifetime: Duration::from_secs(60 * 60),
        agent_session_reauth_drain: Duration::from_secs(30),
        checkpoint_generation_rollover_bytes: 512 * 1024,
        max_queued_per_device: 1,
        max_agent_sessions: 2,
        max_agent_session_starts_per_minute: 30,
    }
}

#[tokio::test]
async fn signed_online_resolution_is_persistence_gated_idempotent_and_never_replays_old_operation()
{
    let state_dir = temp_dir("online-recovery-hub-state");
    let (recovery_key, recovery_public) = recovery_key();
    let recovery_public_path = state_dir.join(RECOVERY_PUBLIC_KEY_FILENAME);
    std::fs::write(&recovery_public_path, recovery_public).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(
            &recovery_public_path,
            std::fs::Permissions::from_mode(0o644),
        )
        .unwrap();
    }

    let device_identity = DeviceIdentity::generate();
    let hub_identity = HubIdentity::generate();
    let grant_authority = GrantAuthority::generate();
    let material = HubProvisionedMaterial {
        hub_identity: hub_identity.clone(),
        grant_signer: grant_authority.into(),
        device_verifier: device_identity.verifying_key(),
        device_rotation: None,
    };
    let (hub, handle) = SingleDeviceHub::new(config(state_dir.clone()), material.clone()).unwrap();
    let operation_id = "op_99999999999999999999999999999999".to_string();
    let owner = OperationOwner::new("https://issuer.example", "alice").unwrap();
    let historical_generation = 7;
    let current_generation = 8;

    {
        let mut persistent = hub.inner.persistent.lock().await;
        let operation = OperationRef {
            device_id: hub.device_id().to_owned(),
            device_generation: historical_generation,
            operation_id: operation_id.clone(),
        };
        let decision = persistent
            .execution
            .prepare(operation, owner.clone(), DeviceCapability::Shell, 10)
            .unwrap();
        assert!(matches!(decision, AdmissionDecision::StartNow(_)));
        persistent
            .execution
            .mark_dispatched(&operation_id, &owner, historical_generation, 11)
            .unwrap();
        persistent
            .execution
            .mark_indeterminate(
                &operation_id,
                &owner,
                historical_generation,
                IndeterminateReason::BackendOutcomeUnproven,
                12,
            )
            .unwrap();
        persist_locked(&hub.inner, &persistent).unwrap();
    }
    assert_eq!(
        handle.desktop_quarantine().await.unwrap().operation_id,
        operation_id
    );

    let quarantine = handle.desktop_quarantine().await.unwrap();
    let challenge =
        build_recovery_challenge(&hub_identity, &quarantine, current_generation, 100).unwrap();
    hub.inner.recovery_runtime.lock().await.pending = Some(challenge.clone());
    let authorization = sign(
        &recovery_key,
        new_authorization(
            &challenge,
            RecoveryAuditAssessment::Inconclusive,
            IndeterminateResolution::ConfirmedCompleted,
            "local user inspected the current desktop",
        )
        .unwrap(),
    );

    let (outbound, mut inbound) = mpsc::channel(4);
    let clock = TrustedSessionClock::new(100);
    hub.handle_recovery_authorization(authorization.clone(), &outbound, current_generation, &clock)
        .await
        .unwrap();

    assert!(handle.desktop_quarantine().await.is_none());
    let receipt = handle.operation_receipt(&operation_id).await.unwrap();
    assert_eq!(receipt.terminal_state, HubOperationState::Completed);
    assert_eq!(receipt.evidence, ExecutionEvidence::OperatorResolution);
    assert_eq!(
        handle
            .resolution_records()
            .await
            .last()
            .unwrap()
            .resolver
            .issuer,
        "cumg://local-user-recovery"
    );

    let first_ack = inbound.recv().await.unwrap().unwrap();
    let HubToAgent::RecoveryResolved(first_ack) = decode_hub_frame(first_ack).unwrap() else {
        panic!("expected signed recovery resolution acknowledgement");
    };
    assert_eq!(first_ack.request_id, authorization.request_id);
    assert_eq!(first_ack.operation_id, operation_id);

    // Lost ACK / retransmission in the same live recovery exchange is idempotent.
    hub.handle_recovery_authorization(authorization.clone(), &outbound, current_generation, &clock)
        .await
        .unwrap();
    let duplicate_ack = inbound.recv().await.unwrap().unwrap();
    let HubToAgent::RecoveryResolved(duplicate_ack) = decode_hub_frame(duplicate_ack).unwrap()
    else {
        panic!("expected duplicate recovery acknowledgement");
    };
    assert_eq!(duplicate_ack.request_id, authorization.request_id);
    assert_eq!(handle.resolution_records().await.len(), 1);

    // Reusing the request id with a conflicting decision must not be treated as
    // an idempotent replay of the original authorization.
    let mut conflicting = authorization.clone();
    conflicting.decision = IndeterminateResolution::ConfirmedNotExecuted;
    assert!(matches!(
        hub.handle_recovery_authorization(conflicting, &outbound, current_generation, &clock,)
            .await,
        Err(HubServiceError::OnlineRecovery(
            RecoveryError::ChallengeMismatch
        ))
    ));
    assert_eq!(handle.resolution_records().await.len(), 1);

    // The durable resolved operation remains a replay tombstone after restart.
    drop(handle);
    drop(hub);
    let (restarted, restarted_handle) =
        SingleDeviceHub::new(config(state_dir.clone()), material).unwrap();
    assert!(restarted_handle.desktop_quarantine().await.is_none());
    let recovery = restarted_handle
        .operation_recovery_as(owner.clone(), &operation_id)
        .await
        .unwrap();
    assert_eq!(recovery.state, HubOperationState::Completed);
    {
        let mut persistent = restarted.inner.persistent.lock().await;
        assert!(matches!(
            persistent.execution.prepare(
                OperationRef {
                    device_id: restarted.device_id().to_owned(),
                    device_generation: current_generation + 1,
                    operation_id: operation_id.clone(),
                },
                owner,
                DeviceCapability::Shell,
                200,
            ),
            Err(ExecutionError::OperationReplay)
        ));
    }

    drop(restarted_handle);
    drop(restarted);
    let _ = std::fs::remove_dir_all(state_dir);
}
