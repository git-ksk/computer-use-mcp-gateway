use computer_use_mcp_gateway::v2_m0::{
    AuditEvidence, AuditLog, AuditReason, CAPABILITY_SCHEMA_VERSION, CONTROL_SCHEMA_VERSION,
    CapabilityAdvertisement, CapabilityClass, CommandEnvelope, ControlError, DeviceCapability,
    DeviceIdentity, DeviceRegistry, GrantAuthority, GrantLedger, LeaseManager, PolicyOutcome,
    validate_command_session,
};
use serde_json::json;
use std::{
    env,
    process::Command,
    time::{SystemTime, UNIX_EPOCH},
};

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock before unix epoch")
        .as_millis() as u64
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let identity = DeviceIdentity::generate();
    let challenge = DeviceRegistry::enrollment_challenge();
    let proof = identity.enrollment_proof(&challenge);
    let mut registry = DeviceRegistry::default();
    let device_id = registry.enroll(&identity.public_key(), &challenge, &proof)?;

    let backend_version = backend_version().unwrap_or_else(|| "unknown".into());
    let caps = CapabilityAdvertisement {
        backend: "cua".into(),
        backend_version: backend_version.clone(),
        platform: format!("{}-{}", env::consts::OS, env::consts::ARCH),
        capability_schema_version: CAPABILITY_SCHEMA_VERSION,
        revision: 1,
        supported: vec![
            DeviceCapability::ListApplications,
            DeviceCapability::ScreenGeometry,
            DeviceCapability::PointerClick,
        ],
    };
    let first_session = registry.connect(&device_id, caps.clone())?;

    let authority = GrantAuthority::generate();
    let mut grants = GrantLedger::new(authority.verifier());
    let mut leases = LeaseManager::default();
    let mut audit = AuditLog::default();
    let start = now_ms();

    let observe_grant = authority.issue(&device_id, CapabilityClass::Observe, start, 60_000)?;
    let observe = CommandEnvelope {
        schema_version: CONTROL_SCHEMA_VERSION,
        device_id: device_id.clone(),
        device_generation: first_session.generation,
        capability_revision: caps.revision,
        operation_id: "poc-observe-list-apps".into(),
        command: DeviceCapability::ListApplications,
    };
    validate_command_session(&observe, &first_session)?;
    grants.authorize_once(
        &observe_grant,
        &device_id,
        observe.required_class(),
        start + 1,
    )?;
    leases.acquire(
        &device_id,
        &observe.operation_id,
        first_session.generation,
        start + 1,
        30_000,
    )?;

    let app_count = call_cua_list_apps()?;
    audit.record(AuditEvidence {
        event_id: String::new(),
        occurred_at_ms: now_ms(),
        device_id: device_id.clone(),
        device_generation: first_session.generation,
        grant_id: Some(observe_grant.payload.grant_id.clone()),
        operation_id: Some(observe.operation_id.clone()),
        capability: Some(CapabilityClass::Observe),
        outcome: PolicyOutcome::Allowed,
        reason: AuditReason::GrantValidBackendCompleted,
    });
    leases.release(&device_id, &observe.operation_id, first_session.generation)?;

    let replay_rejected = matches!(
        grants.authorize_once(
            &observe_grant,
            &device_id,
            CapabilityClass::Observe,
            start + 2,
        ),
        Err(ControlError::GrantReplay)
    );

    let interact_with_observe =
        authority.issue(&device_id, CapabilityClass::Observe, start + 3, 60_000)?;
    let interact = CommandEnvelope {
        operation_id: "poc-interact-denied".into(),
        command: DeviceCapability::PointerClick,
        ..observe.clone()
    };
    validate_command_session(&interact, &first_session)?;
    let interact_rejected = matches!(
        grants.authorize_once(
            &interact_with_observe,
            &device_id,
            interact.required_class(),
            start + 4,
        ),
        Err(ControlError::CapabilityDenied { .. })
    );
    audit.record(AuditEvidence {
        event_id: String::new(),
        occurred_at_ms: now_ms(),
        device_id: device_id.clone(),
        device_generation: first_session.generation,
        grant_id: Some(interact_with_observe.payload.grant_id.clone()),
        operation_id: Some(interact.operation_id.clone()),
        capability: Some(CapabilityClass::Interact),
        outcome: PolicyOutcome::Denied,
        reason: AuditReason::ObserveGrantCannotAuthorizeInteract,
    });

    let revoked = authority.issue(&device_id, CapabilityClass::Observe, start + 5, 60_000)?;
    grants.revoke(&revoked.payload.grant_id);
    let revoked_rejected = matches!(
        grants.authorize_once(&revoked, &device_id, CapabilityClass::Observe, start + 6,),
        Err(ControlError::GrantRevoked)
    );

    let expired = authority.issue(&device_id, CapabilityClass::Observe, start + 7, 1)?;
    let expired_rejected = matches!(
        grants.authorize_once(&expired, &device_id, CapabilityClass::Observe, start + 8,),
        Err(ControlError::GrantExpired)
    );

    leases.acquire(
        &device_id,
        "poc-held-before-reconnect",
        first_session.generation,
        start + 9,
        30_000,
    )?;
    let second_session = registry.connect(&device_id, caps)?;
    let reconnect_lease_rejected = matches!(
        leases.acquire(
            &device_id,
            "poc-after-reconnect",
            second_session.generation,
            start + 10,
            30_000,
        ),
        Err(ControlError::LeaseConflict { owner_generation, .. })
            if owner_generation == first_session.generation
    );

    let passed = replay_rejected
        && interact_rejected
        && revoked_rejected
        && expired_rejected
        && reconnect_lease_rejected;

    println!(
        "{}",
        serde_json::to_string_pretty(&json!({
            "schema_version": CONTROL_SCHEMA_VERSION,
            "status": if passed { "pass" } else { "fail" },
            "device": {
                "id": device_id,
                "first_generation": first_session.generation,
                "reconnect_generation": second_session.generation,
            },
            "backend": {
                "name": "cua",
                "version": backend_version,
                "observed_app_count": app_count,
            },
            "evidence": {
                "cryptographic_enrollment": true,
                "short_lived_observe_grant_executed": true,
                "interact_without_interact_grant_rejected": interact_rejected,
                "consumed_grant_replay_rejected": replay_rejected,
                "revoked_grant_rejected": revoked_rejected,
                "expired_grant_rejected": expired_rejected,
                "reconnect_cannot_take_in_flight_lease": reconnect_lease_rejected,
                "audit_event_count": audit.events().len(),
                "raw_backend_output_logged": false,
            },
            "remaining_v2_m0_gap": "outbound authenticated Hub-Agent transport is not implemented by this local control-semantics PoC"
        }))?
    );

    if passed {
        Ok(())
    } else {
        Err("V2-M0 PoC assertions failed".into())
    }
}

fn backend_command() -> String {
    env::var("CUMG_BACKEND_COMMAND").unwrap_or_else(|_| "cua-driver".into())
}

fn backend_version() -> Option<String> {
    let output = Command::new(backend_command())
        .arg("--version")
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8(output.stdout)
        .ok()?
        .split_whitespace()
        .last()
        .map(ToOwned::to_owned)
}

fn call_cua_list_apps() -> Result<usize, Box<dyn std::error::Error>> {
    let output = Command::new(backend_command())
        .args(["call", "list_apps", "{}"])
        .output()?;
    if !output.status.success() {
        return Err(format!("cua list_apps failed with status {}", output.status).into());
    }
    let value: serde_json::Value = serde_json::from_slice(&output.stdout)?;
    let count = value
        .get("apps")
        .and_then(serde_json::Value::as_array)
        .map(Vec::len)
        .ok_or("cua list_apps did not return an apps array")?;
    Ok(count)
}
