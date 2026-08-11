use anyhow::{Context, Result, anyhow, bail};
use computer_use_mcp_gateway::{
    v2_m0::{
        AuditEvidence, AuditLog, AuditReason, CAPABILITY_SCHEMA_VERSION, CONTROL_SCHEMA_VERSION,
        CapabilityAdvertisement, CapabilityClass, CommandEnvelope, CommandResultEnvelope,
        DeviceCapability, DeviceCommand, DeviceIdentity, DeviceRegistry, DeviceResult,
        DeviceSession, GrantAuthority, GrantLedger, LeaseManager, PolicyOutcome,
        validate_command_result, validate_command_session,
    },
    v2_m0_transport::{
        AgentHello, AgentProof, AgentToHub, HUB_AGENT_SCHEMA_VERSION, HubIdentity, HubToAgent,
        build_agent_proof, build_remote_result, read_frame, verify_agent_proof,
        verify_hub_challenge, verify_remote_command, verify_remote_result, verify_session_accepted,
        write_frame,
    },
};
use ed25519_dalek::VerifyingKey;
use serde_json::json;
use std::{
    env,
    net::{SocketAddr, TcpListener, TcpStream},
    process::Command,
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

const IO_TIMEOUT: Duration = Duration::from_secs(5);

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock before unix epoch")
        .as_millis() as u64
}

fn main() -> Result<()> {
    let device_identity = DeviceIdentity::generate();
    let enrollment_challenge = DeviceRegistry::enrollment_challenge();
    let enrollment_proof = device_identity.enrollment_proof(&enrollment_challenge);
    let mut registry = DeviceRegistry::default();
    let device_id = registry.enroll(
        &device_identity.public_key(),
        &enrollment_challenge,
        &enrollment_proof,
    )?;

    let backend_version = backend_version().unwrap_or_else(|| "unknown".into());
    let capabilities = CapabilityAdvertisement {
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

    let hub_identity = HubIdentity::generate();
    let trusted_hub = hub_identity.verifier();
    let grant_authority = GrantAuthority::generate();
    let agent_grant_verifier = grant_authority.verifier();

    let listener = TcpListener::bind("127.0.0.1:0")?;
    let hub_addr = listener.local_addr()?;
    let backend_command = backend_command();
    let agent_device_id = device_id.clone();
    let agent_capabilities = capabilities.clone();
    let agent_identity = device_identity.clone();

    let agent = thread::spawn(move || {
        run_agent(
            hub_addr,
            agent_identity,
            agent_device_id,
            agent_capabilities,
            trusted_hub,
            agent_grant_verifier,
            backend_command,
        )
        .map_err(|error| format!("{error:#}"))
    });

    let (mut stream, peer_addr) = listener.accept()?;
    configure_stream(&stream)?;
    if !peer_addr.ip().is_loopback() {
        bail!("V2-M0 network PoC refuses a non-loopback Agent peer");
    }

    let hello = match read_frame::<_, AgentToHub>(&mut stream)? {
        AgentToHub::Hello(hello) => hello,
        other => bail!("expected Agent hello, got {other:?}"),
    };
    if hello.device_id != device_id {
        bail!("Agent hello used an unexpected device id");
    }

    let challenge = hub_identity.challenge(&hello)?;
    write_frame(&mut stream, &HubToAgent::Challenge(challenge.clone()))?;

    let proof = match read_frame::<_, AgentToHub>(&mut stream)? {
        AgentToHub::Proof(proof) => proof,
        other => bail!("expected Agent proof, got {other:?}"),
    };
    verify_agent_proof(&registry, &hello, &challenge, &proof)?;

    let session = registry.connect(&device_id, hello.capabilities.clone())?;
    let accepted = hub_identity.accept_session(
        &hello,
        &challenge,
        session.generation,
        session.capabilities.revision,
    )?;
    write_frame(&mut stream, &HubToAgent::Accepted(accepted))?;

    let started = now_ms();
    let grant = grant_authority.issue(&device_id, CapabilityClass::Observe, started, 60_000)?;
    let command = CommandEnvelope {
        schema_version: CONTROL_SCHEMA_VERSION,
        device_id: device_id.clone(),
        device_generation: session.generation,
        capability_revision: session.capabilities.revision,
        operation_id: "poc-network-list-apps".into(),
        command: DeviceCommand::ListApplications,
    };
    validate_command_session(&command, &session)?;

    let mut leases = LeaseManager::default();
    leases.acquire(
        &device_id,
        &command.operation_id,
        session.generation,
        started,
        30_000,
    )?;

    let remote_command =
        hub_identity.remote_command(&hello, &challenge, command.clone(), grant.clone())?;
    write_frame(&mut stream, &HubToAgent::Command(remote_command))?;

    let remote_result = match read_frame::<_, AgentToHub>(&mut stream)? {
        AgentToHub::Result(result) => result,
        other => bail!("expected Agent result, got {other:?}"),
    };
    verify_remote_result(&registry, &hello, &challenge, &remote_result)?;
    validate_command_result(&command, &remote_result.result)?;
    let observed_app_count = match remote_result.result.result {
        DeviceResult::Applications { count } => count,
        _ => bail!("ListApplications returned a mismatched result type"),
    };
    leases.release(&device_id, &command.operation_id, session.generation)?;

    let mut audit = AuditLog::default();
    audit.record(AuditEvidence {
        event_id: String::new(),
        occurred_at_ms: now_ms(),
        device_id: device_id.clone(),
        device_generation: session.generation,
        grant_id: Some(grant.payload.grant_id),
        operation_id: Some(command.operation_id),
        capability: Some(CapabilityClass::Observe),
        outcome: PolicyOutcome::Allowed,
        reason: AuditReason::GrantValidBackendCompleted,
    });

    let agent_evidence = agent
        .join()
        .map_err(|_| anyhow!("Agent thread panicked"))?
        .map_err(|error| anyhow!(error))?;

    println!(
        "{}",
        serde_json::to_string_pretty(&json!({
            "schema_version": HUB_AGENT_SCHEMA_VERSION,
            "status": "pass",
            "transport": {
                "kind": "loopback_tcp_poc",
                "agent_initiated_outbound_connection": true,
                "agent_listener_exposed": false,
                "peer_loopback": peer_addr.ip().is_loopback(),
                "bounded_frame_bytes": computer_use_mcp_gateway::v2_m0_transport::MAX_FRAME_BYTES,
                "encrypted": false,
                "production_requirement": "authenticated TLS or equivalently reviewed secure tunnel",
            },
            "authentication": {
                "hub_identity_pinned_by_agent": agent_evidence.hub_authenticated,
                "enrolled_device_identity_verified_by_hub": true,
                "grant_signing_key_separate_from_transport_identity": true,
                "fresh_connection_nonces": true,
                "session_acceptance_signed_and_connection_bound": true,
                "commands_signed_and_connection_bound": true,
                "results_signed_and_connection_bound": true,
            },
            "device": {
                "id": device_id,
                "generation": session.generation,
                "capability_revision": session.capabilities.revision,
            },
            "backend": {
                "name": "cua",
                "version": backend_version,
                "observed_app_count": observed_app_count,
            },
            "evidence": {
                "typed_versioned_wire_protocol": true,
                "typed_backend_neutral_command_result": true,
                "short_lived_grant_validated_on_agent": agent_evidence.grant_validated,
                "real_backend_call_executed_on_agent": agent_evidence.backend_executed,
                "audit_event_count": audit.events().len(),
                "raw_backend_output_logged": false,
            },
            "remaining_v2_m0_gaps": [
                "remote transport confidentiality is not proven by this loopback-only TCP slice",
                "Hub identity/key rotation and Agent credential rotation",
                "MCP-client-to-Hub authorization mapping",
                "distributed cancellation/backpressure",
                "compromised-component threat model"
            ]
        }))?
    );

    Ok(())
}

#[derive(Debug)]
struct AgentEvidence {
    hub_authenticated: bool,
    grant_validated: bool,
    backend_executed: bool,
}

fn run_agent(
    hub_addr: SocketAddr,
    identity: DeviceIdentity,
    device_id: String,
    capabilities: CapabilityAdvertisement,
    trusted_hub: VerifyingKey,
    grant_verifier: VerifyingKey,
    backend_command: String,
) -> Result<AgentEvidence> {
    let mut stream =
        TcpStream::connect(hub_addr).context("Agent failed to connect outbound to Hub")?;
    configure_stream(&stream)?;

    let hello = AgentHello::new(device_id.clone(), capabilities.clone());
    write_frame(&mut stream, &AgentToHub::Hello(hello.clone()))?;

    let challenge = match read_frame::<_, HubToAgent>(&mut stream)? {
        HubToAgent::Challenge(challenge) => challenge,
        other => bail!("expected Hub challenge, got {other:?}"),
    };
    verify_hub_challenge(&hello, &challenge, &trusted_hub)?;

    let proof: AgentProof = build_agent_proof(&identity, &hello, &challenge)?;
    write_frame(&mut stream, &AgentToHub::Proof(proof))?;

    let accepted = match read_frame::<_, HubToAgent>(&mut stream)? {
        HubToAgent::Accepted(accepted) => accepted,
        other => bail!("expected Hub acceptance, got {other:?}"),
    };
    verify_session_accepted(&hello, &challenge, &accepted, &trusted_hub)?;
    if accepted.device_id != device_id || accepted.capability_revision != capabilities.revision {
        bail!("Hub acceptance did not match the Agent session");
    }
    let session = DeviceSession {
        device_id: device_id.clone(),
        generation: accepted.device_generation,
        capabilities,
    };

    let remote = match read_frame::<_, HubToAgent>(&mut stream)? {
        HubToAgent::Command(remote) => remote,
        other => bail!("expected Hub command, got {other:?}"),
    };
    verify_remote_command(&hello, &challenge, &remote, &trusted_hub)?;
    validate_command_session(&remote.command, &session)?;

    let mut grants = GrantLedger::new(grant_verifier);
    grants.authorize_once(
        &remote.grant,
        &device_id,
        remote.command.required_class(),
        now_ms(),
    )?;

    let result = match &remote.command.command {
        DeviceCommand::ListApplications => DeviceResult::Applications {
            count: call_cua_list_apps(&backend_command)?,
        },
        _ => bail!("network PoC only executes ListApplications"),
    };
    let result = CommandResultEnvelope {
        schema_version: CONTROL_SCHEMA_VERSION,
        device_id,
        device_generation: remote.command.device_generation,
        capability_revision: remote.command.capability_revision,
        operation_id: remote.command.operation_id,
        result,
    };
    let remote_result = build_remote_result(&identity, &hello, &challenge, result)?;
    write_frame(&mut stream, &AgentToHub::Result(remote_result))?;

    Ok(AgentEvidence {
        hub_authenticated: true,
        grant_validated: true,
        backend_executed: true,
    })
}

fn configure_stream(stream: &TcpStream) -> Result<()> {
    stream.set_nodelay(true)?;
    stream.set_read_timeout(Some(IO_TIMEOUT))?;
    stream.set_write_timeout(Some(IO_TIMEOUT))?;
    Ok(())
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

fn call_cua_list_apps(backend_command: &str) -> Result<u64> {
    let output = Command::new(backend_command)
        .args(["call", "list_apps", "{}"])
        .output()?;
    if !output.status.success() {
        bail!("cua list_apps failed with status {}", output.status);
    }
    let value: serde_json::Value = serde_json::from_slice(&output.stdout)?;
    let count = value
        .get("apps")
        .and_then(serde_json::Value::as_array)
        .map(Vec::len)
        .ok_or_else(|| anyhow!("cua list_apps did not return an apps array"))?;
    u64::try_from(count).context("app count exceeded u64")
}
