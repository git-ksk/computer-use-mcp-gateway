use computer_use_mcp_gateway::{
    v2_execution_safety::{AuthoritativeOperationController, IndeterminateReason, OperationOwner},
    v2_hosted_handoff_routing::HostedHandoffRouteRegistry,
    v2_hub_state_store::{
        HubAuthoritativeStateStore, HubStateStoreError, HubWriterEpoch, MemoryHubStateStore,
    },
    v2_m0::{
        CAPABILITY_SCHEMA_VERSION, CapabilityAdvertisement, CapabilityClass, ControlError,
        DeviceCapability, DeviceIdentity, DeviceRegistry, GrantAuthority, GrantLedger,
    },
    v2_m0_execution::{AdmissionLimits, ExecutionError, HubOperationState, OperationRef},
    v2_m0_transport::HubIdentity,
    v2_m0_trust::{
        TrustedHubIdentity, apply_device_key_rotation, build_device_key_rotation,
        build_hub_key_rotation,
    },
    v2_m1_northbound::OAuthIntrospectionConfig,
    v2_m1_persistence::HubPersistentState,
};
use std::time::Duration;

fn enrolled_registry() -> (DeviceRegistry, DeviceIdentity, String) {
    let mut registry = DeviceRegistry::default();
    let device = DeviceIdentity::generate();
    let challenge = DeviceRegistry::enrollment_challenge();
    let device_id = registry
        .enroll(
            device.verifying_key().as_bytes(),
            &challenge,
            &device.enrollment_proof(&challenge),
        )
        .unwrap();
    (registry, device, device_id)
}

fn caps() -> CapabilityAdvertisement {
    CapabilityAdvertisement {
        backend: "hosted-rotation-acceptance".into(),
        backend_version: "1".into(),
        platform: "test".into(),
        capability_schema_version: CAPABILITY_SCHEMA_VERSION,
        revision: 1,
        supported: vec![DeviceCapability::TypeText],
    }
}

#[test]
fn revision_overlap_has_one_authoritative_writer_epoch() {
    let (registry, _, _) = enrolled_registry();
    let execution = AuthoritativeOperationController::new(AdmissionLimits {
        max_global_active: 1,
        max_queued_per_device: 1,
    })
    .unwrap();
    let initial = HubPersistentState::capture(&registry, &execution);
    let store = MemoryHubStateStore::default();

    let revision_a = store.acquire_writer(&initial).unwrap();
    let revision_b = store.acquire_writer(&initial).unwrap();

    assert_eq!(revision_a.writer_epoch, HubWriterEpoch(1));
    assert_eq!(revision_b.writer_epoch, HubWriterEpoch(2));
    assert!(matches!(
        store.compare_and_commit(revision_a.lease(), &initial),
        Err(HubStateStoreError::StaleWriter)
    ));

    let committed = store
        .compare_and_commit(revision_b.lease(), &initial)
        .unwrap();
    assert_eq!(committed.writer_epoch, HubWriterEpoch(2));
    assert!(committed.revision.0 > revision_b.revision.0);
}

#[test]
fn device_rotation_invalidates_old_session_without_settling_or_replaying_ambiguous_work() {
    let (mut registry, old_device, device_id) = enrolled_registry();
    let session = registry.connect(&device_id, caps()).unwrap();

    let mut execution = AuthoritativeOperationController::new(AdmissionLimits {
        max_global_active: 1,
        max_queued_per_device: 1,
    })
    .unwrap();
    let owner = OperationOwner::new("https://issuer.example", "operator").unwrap();
    let operation = OperationRef {
        device_id: device_id.clone(),
        device_generation: session.generation,
        operation_id: "op_hosted_rotation_ambiguous".into(),
    };
    execution
        .prepare(
            operation.clone(),
            owner.clone(),
            DeviceCapability::TypeText,
            10,
        )
        .unwrap();
    execution
        .mark_dispatched(
            &operation.operation_id,
            &owner,
            operation.device_generation,
            20,
        )
        .unwrap();
    execution
        .mark_indeterminate(
            &operation.operation_id,
            &owner,
            operation.device_generation,
            IndeterminateReason::ConnectionLost,
            30,
        )
        .unwrap();

    let next_device = DeviceIdentity::generate();
    let rotation = build_device_key_rotation(&device_id, &old_device, &next_device, 1).unwrap();
    apply_device_key_rotation(&mut registry, &rotation, 1).unwrap();

    assert!(matches!(
        registry.current_session(&device_id),
        Err(ControlError::DeviceOffline)
    ));
    assert!(
        registry
            .verify_device_signature(&device_id, b"old", &old_device.sign_message(b"old"))
            .is_err()
    );
    registry
        .verify_device_signature(&device_id, b"next", &next_device.sign_message(b"next"))
        .unwrap();

    let state = HubPersistentState::capture(&registry, &execution);
    let (restored_registry, mut restored_execution) = state
        .restore(AdmissionLimits {
            max_global_active: 1,
            max_queued_per_device: 1,
        })
        .unwrap();

    assert_eq!(
        restored_execution.state(&operation.operation_id),
        Some(HubOperationState::Indeterminate)
    );
    let quarantine = restored_execution.quarantine(&device_id).unwrap();
    assert_eq!(quarantine.operation_id, operation.operation_id);
    assert_eq!(quarantine.reason, IndeterminateReason::ConnectionLost);
    assert_eq!(
        restored_execution.prepare(operation, owner, DeviceCapability::TypeText, 40),
        Err(ExecutionError::OperationReplay)
    );

    let fresh_session = restored_registry
        .clone()
        .connect(&device_id, caps())
        .expect("rotated device must reconnect through a fresh generation");
    assert!(fresh_session.generation > session.generation);
}

#[test]
fn hub_and_grant_keys_rotate_independently_with_bounded_overlap() {
    let old_hub = HubIdentity::generate();
    let next_hub = HubIdentity::generate();
    let mut trusted_hub = TrustedHubIdentity::new(old_hub.verifier());
    trusted_hub
        .apply_rotation(&build_hub_key_rotation(&old_hub, &next_hub, 1).unwrap())
        .unwrap();
    assert_eq!(trusted_hub.verifier(), next_hub.verifier());
    assert_eq!(trusted_hub.epoch(), 1);

    let old_grant = GrantAuthority::generate();
    let next_grant = GrantAuthority::generate();
    let mut grants = GrantLedger::new(old_grant.verifier());
    grants.trust_verifier(next_grant.verifier());

    let old_token = old_grant
        .issue("dev_hosted_rotation", CapabilityClass::Observe, 10, 100)
        .unwrap();
    let next_token = next_grant
        .issue("dev_hosted_rotation", CapabilityClass::Observe, 10, 100)
        .unwrap();
    grants
        .authorize_once(
            &old_token,
            "dev_hosted_rotation",
            CapabilityClass::Observe,
            11,
        )
        .unwrap();
    grants
        .authorize_once(
            &next_token,
            "dev_hosted_rotation",
            CapabilityClass::Observe,
            11,
        )
        .unwrap();

    assert!(grants.retire_verifier(&old_grant.key_id()));
    let retired_old = old_grant
        .issue("dev_hosted_rotation", CapabilityClass::Observe, 20, 100)
        .unwrap();
    assert_eq!(
        grants.authorize_once(
            &retired_old,
            "dev_hosted_rotation",
            CapabilityClass::Observe,
            21,
        ),
        Err(ControlError::UnknownGrantSigningKey)
    );

    // Rotating grant-signing authority never changes the Hub application identity epoch.
    assert_eq!(trusted_hub.epoch(), 1);
}

#[test]
fn handoff_viewer_and_transport_rotation_never_changes_agent_or_intervention_authority() {
    let routes = HostedHandoffRouteRegistry::new(Duration::from_secs(60), 4).unwrap();
    let route = routes
        .create_route("device-a", 7, "intervention-a", 3)
        .unwrap();

    let viewer_a = routes
        .attach_viewer(&route, "device-a", "intervention-a")
        .unwrap();
    let transport_a = routes.attach_transport(&viewer_a).unwrap();
    routes.validate_transport(&transport_a).unwrap();

    routes.detach_viewer(&viewer_a).unwrap();
    let viewer_b = routes
        .attach_viewer(&route, "device-a", "intervention-a")
        .unwrap();
    let transport_b = routes.attach_transport(&viewer_b).unwrap();

    assert!(viewer_b.viewer_generation > viewer_a.viewer_generation);
    assert!(transport_b.transport_generation > transport_a.transport_generation);
    assert_eq!(route.agent_generation, 7);
    assert_eq!(route.intervention_epoch, 3);
    assert!(routes.validate_transport(&transport_a).is_err());
    routes.validate_transport(&transport_b).unwrap();
}

#[test]
fn hosted_auth_debug_output_redacts_client_secret_value() {
    let sentinel = "hosted-rotation-secret-SHOULD-NEVER-APPEAR";
    let config = OAuthIntrospectionConfig::new(
        "https://issuer.example",
        "https://resource.example/mcp",
        "https://issuer.example/introspect",
        "cumg-hosted",
        sentinel,
    );

    let rendered = format!("{config:?}");
    assert!(rendered.contains("[REDACTED]"));
    assert!(!rendered.contains(sentinel));
}
