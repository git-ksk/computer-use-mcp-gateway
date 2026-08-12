use anyhow::{Result, bail};
use computer_use_mcp_gateway::{
    v2_m0::{
        CAPABILITY_SCHEMA_VERSION, CONTROL_SCHEMA_VERSION, CapabilityAdvertisement,
        CommandEnvelope, CommandResultEnvelope, DeviceCapability, DeviceCommand, DeviceIdentity,
        DeviceRegistry, DeviceResult, DeviceSession, GrantAuthority, GrantLedger, LeaseManager,
        ProcessRequest, validate_command_result, validate_command_session,
    },
    v2_m0_execution::{AdmissionDecision, AdmissionLimits, HubAdmissionController, OperationRef},
    v2_m0_transport::{
        AgentToHub, HubIdentity, HubToAgent, TrustedSessionClock, build_agent_heartbeat,
        build_agent_proof, build_remote_result, read_frame, verify_agent_heartbeat,
        verify_agent_proof, verify_hub_challenge, verify_hub_heartbeat_ack, verify_remote_command,
        verify_remote_result, verify_session_accepted, write_frame,
    },
    v2_m0_trust::{AuthenticatedClientPrincipal, ClientAuthorizationPolicy},
    v2_m1::{HeartbeatTracker, SingleDeviceRouter},
    v2_m1_process::{ProcessCancellation, ProcessExecutor, ProcessPolicy},
    v2_m1_tls::{
        HUB_AGENT_ALPN, accept_hub_tls, client_config_with_pinned_root, connect_agent_tls,
        server_config_from_der,
    },
};
use rcgen::{CertifiedKey, generate_simple_self_signed};
use std::{
    env,
    net::{TcpListener, TcpStream},
    thread,
};

const HUB_TIME_MS: u64 = 200_000;

fn process_capabilities() -> CapabilityAdvertisement {
    CapabilityAdvertisement {
        backend: "agent-native".into(),
        backend_version: env!("CARGO_PKG_VERSION").into(),
        platform: format!("{}-{}", env::consts::OS, env::consts::ARCH),
        capability_schema_version: CAPABILITY_SCHEMA_VERSION,
        revision: 1,
        supported: vec![DeviceCapability::ExecuteProcess],
    }
}

#[test]
fn encrypted_agent_executes_structured_git_without_cua_or_terminal_gui() -> Result<()> {
    let repo_root = env::current_dir()?;
    let cwd = repo_root.to_string_lossy().into_owned();
    let CertifiedKey { cert, signing_key } = generate_simple_self_signed(vec!["localhost".into()])?;
    let certificate_der = cert.der().to_vec();
    let server_tls =
        server_config_from_der(vec![certificate_der.clone()], signing_key.serialize_der())?;
    let agent_tls = client_config_with_pinned_root(certificate_der)?;

    let device_identity = DeviceIdentity::generate();
    let enrollment_challenge = DeviceRegistry::enrollment_challenge();
    let enrollment_proof = device_identity.enrollment_proof(&enrollment_challenge);
    let mut registry = DeviceRegistry::default();
    let device_id = registry.enroll(
        &device_identity.public_key(),
        &enrollment_challenge,
        &enrollment_proof,
    )?;
    let hub_identity = HubIdentity::generate();
    let trusted_hub = hub_identity.verifier();
    let grant_authority = GrantAuthority::generate();
    let grant_verifier = grant_authority.verifier();
    let client = AuthenticatedClientPrincipal::new("test://northbound", "process-client")?;
    let mut policy = ClientAuthorizationPolicy::default();
    policy.allow_device_capability(&client, &device_id, DeviceCapability::ExecuteProcess);

    let listener = TcpListener::bind("127.0.0.1:0")?;
    let address = listener.local_addr()?;
    let expected_device_id = device_id.clone();
    let hub_cwd = cwd.clone();

    let hub = thread::spawn(move || -> Result<String> {
        let (stream, peer) = listener.accept()?;
        if !peer.ip().is_loopback() {
            bail!("test accepts only loopback Agent peer");
        }
        let mut tls = accept_hub_tls(stream, server_tls)?;
        if tls.conn.protocol_version() != Some(rustls::ProtocolVersion::TLSv1_3)
            || tls.conn.alpn_protocol() != Some(HUB_AGENT_ALPN)
        {
            bail!("expected TLS 1.3 + dedicated Hub-Agent ALPN");
        }

        let hello = match read_frame::<_, AgentToHub>(&mut tls)? {
            AgentToHub::Hello(hello) => hello,
            other => bail!("expected hello, got {other:?}"),
        };
        let challenge = hub_identity.challenge(&hello)?;
        write_frame(&mut tls, &HubToAgent::Challenge(challenge.clone()))?;
        let proof = match read_frame::<_, AgentToHub>(&mut tls)? {
            AgentToHub::Proof(proof) => proof,
            other => bail!("expected proof, got {other:?}"),
        };
        verify_agent_proof(&registry, &hello, &challenge, &proof)?;
        let session = registry.connect(&expected_device_id, hello.capabilities.clone())?;
        let mut router = SingleDeviceRouter::new(expected_device_id.clone())?;
        router.connect(session.clone())?;
        let accepted = hub_identity.accept_session(
            &hello,
            &challenge,
            session.generation,
            session.capabilities.revision,
            HUB_TIME_MS,
        )?;
        write_frame(&mut tls, &HubToAgent::Accepted(accepted))?;

        let heartbeat = match read_frame::<_, AgentToHub>(&mut tls)? {
            AgentToHub::Heartbeat(heartbeat) => heartbeat,
            other => bail!("expected heartbeat, got {other:?}"),
        };
        verify_agent_heartbeat(&registry, &hello, &challenge, &heartbeat)?;
        let mut heartbeat_tracker = HeartbeatTracker::new(
            expected_device_id.clone(),
            session.generation,
            HUB_TIME_MS,
            10_000,
        )?;
        heartbeat_tracker.observe(&heartbeat, HUB_TIME_MS + 1)?;
        let ack = hub_identity.heartbeat_ack(&hello, &challenge, &heartbeat, HUB_TIME_MS + 1)?;
        write_frame(&mut tls, &HubToAgent::HeartbeatAck(ack))?;

        let command = CommandEnvelope {
            schema_version: CONTROL_SCHEMA_VERSION,
            device_id: expected_device_id.clone(),
            device_generation: session.generation,
            capability_revision: session.capabilities.revision,
            operation_id: "m1-process-git-status".into(),
            command: DeviceCommand::ExecuteProcess {
                request: ProcessRequest {
                    program: "git".into(),
                    args: vec!["status".into(), "--short".into()],
                    cwd: hub_cwd,
                    env: vec![],
                    timeout_ms: 10_000,
                },
            },
        };
        router.route(&command)?;
        let operation = OperationRef {
            device_id: expected_device_id.clone(),
            device_generation: session.generation,
            operation_id: command.operation_id.clone(),
        };
        let mut admission = HubAdmissionController::new(AdmissionLimits {
            max_global_active: 1,
            max_queued_per_device: 0,
        })?;
        if !matches!(admission.admit(operation)?, AdmissionDecision::StartNow(_)) {
            bail!("first process operation unexpectedly queued");
        }
        let mut leases = LeaseManager::default();
        leases.acquire(
            &expected_device_id,
            &command.operation_id,
            session.generation,
            HUB_TIME_MS,
            30_000,
        )?;
        let grant = policy.issue_device_grant(
            &client,
            &grant_authority,
            &expected_device_id,
            DeviceCapability::ExecuteProcess,
            HUB_TIME_MS,
            30_000,
        )?;
        let remote = hub_identity.remote_command(&hello, &challenge, command.clone(), grant)?;
        admission.mark_dispatched(&command.operation_id)?;
        write_frame(&mut tls, &HubToAgent::Command(remote))?;

        let result = match read_frame::<_, AgentToHub>(&mut tls)? {
            AgentToHub::Result(result) => result,
            other => bail!("expected process result, got {other:?}"),
        };
        verify_remote_result(&registry, &hello, &challenge, &result)?;
        validate_command_result(&command, &result.result)?;
        let output = match result.result.result {
            DeviceResult::Process { output } => output,
            other => bail!("expected process output, got {other:?}"),
        };
        if output.exit_code != Some(0) || output.timed_out || output.cancelled {
            bail!("git process did not complete normally: {output:?}");
        }
        admission.complete(&command.operation_id, false)?;
        leases.release(
            &expected_device_id,
            &command.operation_id,
            session.generation,
        )?;
        Ok(output.stdout)
    });

    let mut tls = connect_agent_tls(TcpStream::connect(address)?, "localhost", agent_tls)?;
    let hello = computer_use_mcp_gateway::v2_m0_transport::AgentHello::new(
        device_id.clone(),
        process_capabilities(),
    );
    write_frame(&mut tls, &AgentToHub::Hello(hello.clone()))?;
    let challenge = match read_frame::<_, HubToAgent>(&mut tls)? {
        HubToAgent::Challenge(challenge) => challenge,
        other => bail!("expected challenge, got {other:?}"),
    };
    verify_hub_challenge(&hello, &challenge, &trusted_hub)?;
    let proof = build_agent_proof(&device_identity, &hello, &challenge)?;
    write_frame(&mut tls, &AgentToHub::Proof(proof))?;
    let accepted = match read_frame::<_, HubToAgent>(&mut tls)? {
        HubToAgent::Accepted(accepted) => accepted,
        other => bail!("expected acceptance, got {other:?}"),
    };
    verify_session_accepted(&hello, &challenge, &accepted, &trusted_hub)?;
    let trusted_clock = TrustedSessionClock::new(accepted.hub_time_ms);
    let session = DeviceSession {
        device_id: device_id.clone(),
        generation: accepted.device_generation,
        capabilities: process_capabilities(),
    };

    let heartbeat =
        build_agent_heartbeat(&device_identity, &hello, &challenge, session.generation, 1)?;
    write_frame(&mut tls, &AgentToHub::Heartbeat(heartbeat))?;
    let ack = match read_frame::<_, HubToAgent>(&mut tls)? {
        HubToAgent::HeartbeatAck(ack) => ack,
        other => bail!("expected heartbeat ack, got {other:?}"),
    };
    verify_hub_heartbeat_ack(&hello, &challenge, &ack, &trusted_hub)?;

    let remote = match read_frame::<_, HubToAgent>(&mut tls)? {
        HubToAgent::Command(remote) => remote,
        other => bail!("expected process command, got {other:?}"),
    };
    verify_remote_command(&hello, &challenge, &remote, &trusted_hub)?;
    validate_command_session(&remote.command, &session)?;
    let mut grants = GrantLedger::new(grant_verifier);
    grants.authorize_device_capability_once(
        &remote.grant,
        &device_id,
        remote.command.command.capability(),
        trusted_clock.now_ms(),
    )?;

    let request = match &remote.command.command {
        DeviceCommand::ExecuteProcess { request } => request,
        other => bail!("expected ExecuteProcess, got {other:?}"),
    };
    let executor = ProcessExecutor::new(ProcessPolicy::developer_defaults(vec![repo_root])?);
    let mut gate = computer_use_mcp_gateway::v2_m0_execution::AgentExecutionGate::default();
    let operation = OperationRef {
        device_id: device_id.clone(),
        device_generation: session.generation,
        operation_id: remote.command.operation_id.clone(),
    };
    let output = executor.execute_operation(
        &mut gate,
        operation,
        request,
        &ProcessCancellation::default(),
    )?;
    let result = CommandResultEnvelope {
        schema_version: CONTROL_SCHEMA_VERSION,
        device_id,
        device_generation: remote.command.device_generation,
        capability_revision: remote.command.capability_revision,
        operation_id: remote.command.operation_id,
        result: DeviceResult::Process { output },
    };
    let signed = build_remote_result(&device_identity, &hello, &challenge, result)?;
    write_frame(&mut tls, &AgentToHub::Result(signed))?;

    let stdout = hub
        .join()
        .map_err(|_| anyhow::anyhow!("Hub thread panicked"))??;
    // Do not print repository status contents from the test. Successful exit is
    // sufficient proof that the direct Agent process path executed git.
    assert!(stdout.len() <= 16 * 1024);
    Ok(())
}
