use anyhow::{Result, anyhow, bail};
use computer_use_mcp_gateway::{
    v2_m0::{
        CAPABILITY_SCHEMA_VERSION, CONTROL_SCHEMA_VERSION, CapabilityAdvertisement,
        CapabilityClass, CommandEnvelope, CommandResultEnvelope, DeviceCapability, DeviceCommand,
        DeviceIdentity, DeviceRegistry, DeviceResult, DeviceSession, GrantAuthority, GrantLedger,
        LeaseManager, validate_command_result, validate_command_session,
    },
    v2_m0_execution::{
        AdmissionDecision, AdmissionLimits, AgentExecutionGate, HubAdmissionController,
        OperationRef,
    },
    v2_m0_transport::{
        AgentToHub, HubIdentity, HubToAgent, TrustedSessionClock, build_agent_heartbeat,
        build_agent_proof, build_remote_result, read_frame, verify_agent_heartbeat,
        verify_agent_proof, verify_hub_challenge, verify_hub_heartbeat_ack, verify_remote_command,
        verify_remote_result, verify_session_accepted, write_frame,
    },
    v2_m1::{
        HeartbeatTracker, ReconnectPolicy, SessionDirective, SingleDeviceRouter,
        run_outbound_lifecycle,
    },
    v2_m1_tls::{
        HUB_AGENT_ALPN, accept_hub_tls, client_config_with_pinned_root, connect_agent_tls,
        server_config_from_der,
    },
};
use rcgen::{CertifiedKey, generate_simple_self_signed};
use std::{
    net::{TcpListener, TcpStream},
    thread,
    time::Duration,
};

const HUB_TIME_MS: u64 = 100_000;

#[derive(Debug)]
struct HubEvidence {
    tls13: bool,
    alpn: bool,
    heartbeat_sequence: u64,
    result_count: u64,
}

#[derive(Debug)]
struct AgentEvidence {
    tls13: bool,
    alpn: bool,
    grant_validated: bool,
    result_signed: bool,
}

fn capabilities() -> CapabilityAdvertisement {
    CapabilityAdvertisement {
        backend: "fixture".into(),
        backend_version: "1".into(),
        platform: "integration-test".into(),
        capability_schema_version: CAPABILITY_SCHEMA_VERSION,
        revision: 7,
        supported: vec![DeviceCapability::ListApplications],
    }
}

#[test]
fn outbound_tls_connection_composes_the_full_authenticated_single_device_path() -> Result<()> {
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

    let listener = TcpListener::bind("127.0.0.1:0")?;
    let address = listener.local_addr()?;
    let expected_device_id = device_id.clone();

    let hub = thread::spawn(move || -> Result<HubEvidence> {
        let (stream, peer) = listener.accept()?;
        if !peer.ip().is_loopback() {
            bail!("integration test accepts only the loopback Agent peer");
        }
        let mut tls = accept_hub_tls(stream, server_tls)?;
        let tls13 = tls.conn.protocol_version() == Some(rustls::ProtocolVersion::TLSv1_3);
        let alpn = tls.conn.alpn_protocol() == Some(HUB_AGENT_ALPN);

        let hello = match read_frame::<_, AgentToHub>(&mut tls)? {
            AgentToHub::Hello(hello) => hello,
            other => bail!("expected hello, got {other:?}"),
        };
        if hello.device_id != expected_device_id {
            bail!("unexpected device id");
        }
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
        let heartbeat_ack =
            hub_identity.heartbeat_ack(&hello, &challenge, &heartbeat, HUB_TIME_MS + 1)?;
        write_frame(&mut tls, &HubToAgent::HeartbeatAck(heartbeat_ack))?;

        let command = CommandEnvelope {
            schema_version: CONTROL_SCHEMA_VERSION,
            device_id: expected_device_id.clone(),
            device_generation: session.generation,
            capability_revision: session.capabilities.revision,
            operation_id: "m1-e2e-list-applications".into(),
            command: DeviceCommand::ListApplications,
        };
        router.route(&command)?;

        let operation = OperationRef {
            device_id: expected_device_id.clone(),
            device_generation: session.generation,
            operation_id: command.operation_id.clone(),
        };
        let mut admission = HubAdmissionController::new(AdmissionLimits {
            max_global_active: 1,
            max_queued_per_device: 1,
        })?;
        if !matches!(admission.admit(operation)?, AdmissionDecision::StartNow(_)) {
            bail!("first operation unexpectedly queued");
        }
        let mut leases = LeaseManager::default();
        leases.acquire(
            &expected_device_id,
            &command.operation_id,
            session.generation,
            HUB_TIME_MS,
            10_000,
        )?;
        let grant = grant_authority.issue(
            &expected_device_id,
            CapabilityClass::Observe,
            HUB_TIME_MS,
            30_000,
        )?;
        let remote = hub_identity.remote_command(&hello, &challenge, command.clone(), grant)?;
        admission.mark_dispatched(&command.operation_id)?;
        write_frame(&mut tls, &HubToAgent::Command(remote))?;

        let remote_result = match read_frame::<_, AgentToHub>(&mut tls)? {
            AgentToHub::Result(result) => result,
            other => bail!("expected result, got {other:?}"),
        };
        verify_remote_result(&registry, &hello, &challenge, &remote_result)?;
        validate_command_result(&command, &remote_result.result)?;
        let result_count = match remote_result.result.result {
            DeviceResult::Applications { count } => count,
            other => bail!("unexpected result {other:?}"),
        };
        admission.complete(&command.operation_id, false)?;
        leases.release(
            &expected_device_id,
            &command.operation_id,
            session.generation,
        )?;

        Ok(HubEvidence {
            tls13,
            alpn,
            heartbeat_sequence: heartbeat_tracker
                .last_sequence()
                .ok_or_else(|| anyhow!("heartbeat was not recorded"))?,
            result_count,
        })
    });

    let mut tls = connect_agent_tls(TcpStream::connect(address)?, "localhost", agent_tls)?;
    let agent_tls13 = tls.conn.protocol_version() == Some(rustls::ProtocolVersion::TLSv1_3);
    let agent_alpn = tls.conn.alpn_protocol() == Some(HUB_AGENT_ALPN);

    let hello = computer_use_mcp_gateway::v2_m0_transport::AgentHello::new(
        device_id.clone(),
        capabilities(),
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
        capabilities: capabilities(),
    };

    let heartbeat =
        build_agent_heartbeat(&device_identity, &hello, &challenge, session.generation, 1)?;
    write_frame(&mut tls, &AgentToHub::Heartbeat(heartbeat))?;
    let heartbeat_ack = match read_frame::<_, HubToAgent>(&mut tls)? {
        HubToAgent::HeartbeatAck(ack) => ack,
        other => bail!("expected heartbeat ack, got {other:?}"),
    };
    verify_hub_heartbeat_ack(&hello, &challenge, &heartbeat_ack, &trusted_hub)?;
    if heartbeat_ack.sequence != 1 {
        bail!("heartbeat sequence mismatch");
    }

    let remote = match read_frame::<_, HubToAgent>(&mut tls)? {
        HubToAgent::Command(remote) => remote,
        other => bail!("expected command, got {other:?}"),
    };
    verify_remote_command(&hello, &challenge, &remote, &trusted_hub)?;
    validate_command_session(&remote.command, &session)?;

    let operation = OperationRef {
        device_id: device_id.clone(),
        device_generation: session.generation,
        operation_id: remote.command.operation_id.clone(),
    };
    let mut execution = AgentExecutionGate::default();
    execution.begin(operation)?;
    let mut grants = GrantLedger::new(grant_verifier);
    grants.authorize_once(
        &remote.grant,
        &device_id,
        remote.command.required_class(),
        trusted_clock.now_ms(),
    )?;

    let result = match remote.command.command {
        DeviceCommand::ListApplications => DeviceResult::Applications { count: 3 },
        other => bail!("unexpected command {other:?}"),
    };
    let operation_id = remote.command.operation_id.clone();
    let result = CommandResultEnvelope {
        schema_version: CONTROL_SCHEMA_VERSION,
        device_id,
        device_generation: remote.command.device_generation,
        capability_revision: remote.command.capability_revision,
        operation_id: operation_id.clone(),
        result,
    };
    execution.finish(&operation_id)?;
    let remote_result = build_remote_result(&device_identity, &hello, &challenge, result)?;
    write_frame(&mut tls, &AgentToHub::Result(remote_result))?;

    let agent_evidence = AgentEvidence {
        tls13: agent_tls13,
        alpn: agent_alpn,
        grant_validated: true,
        result_signed: true,
    };
    let hub_evidence = hub.join().map_err(|_| anyhow!("Hub thread panicked"))??;

    assert!(agent_evidence.tls13 && hub_evidence.tls13);
    assert!(agent_evidence.alpn && hub_evidence.alpn);
    assert!(agent_evidence.grant_validated && agent_evidence.result_signed);
    assert_eq!(hub_evidence.heartbeat_sequence, 1);
    assert_eq!(hub_evidence.result_count, 3);
    Ok(())
}

#[test]
fn encrypted_reconnect_advances_generation_and_rejects_the_stale_session() -> Result<()> {
    let CertifiedKey { cert, signing_key } = generate_simple_self_signed(vec!["localhost".into()])?;
    let certificate_der = cert.der().to_vec();
    let server_tls =
        server_config_from_der(vec![certificate_der.clone()], signing_key.serialize_der())?;
    let agent_tls = client_config_with_pinned_root(certificate_der)?;

    let identity = DeviceIdentity::generate();
    let enrollment_challenge = DeviceRegistry::enrollment_challenge();
    let proof = identity.enrollment_proof(&enrollment_challenge);
    let mut registry = DeviceRegistry::default();
    let device_id = registry.enroll(&identity.public_key(), &enrollment_challenge, &proof)?;
    let hub_identity = HubIdentity::generate();
    let trusted_hub = hub_identity.verifier();

    let listener = TcpListener::bind("127.0.0.1:0")?;
    let address = listener.local_addr()?;
    let expected_device_id = device_id.clone();
    let hub = thread::spawn(move || -> Result<(Vec<u64>, bool)> {
        let mut generations = Vec::new();
        let mut router = SingleDeviceRouter::new(expected_device_id.clone())?;
        for index in 0..2_u64 {
            let (stream, _) = listener.accept()?;
            let mut tls = accept_hub_tls(stream, server_tls.clone())?;
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
            router.connect(session.clone())?;
            generations.push(session.generation);
            let accepted = hub_identity.accept_session(
                &hello,
                &challenge,
                session.generation,
                session.capabilities.revision,
                HUB_TIME_MS + index,
            )?;
            write_frame(&mut tls, &HubToAgent::Accepted(accepted))?;

            let heartbeat = match read_frame::<_, AgentToHub>(&mut tls)? {
                AgentToHub::Heartbeat(heartbeat) => heartbeat,
                other => bail!("expected heartbeat, got {other:?}"),
            };
            verify_agent_heartbeat(&registry, &hello, &challenge, &heartbeat)?;
            let mut tracker = HeartbeatTracker::new(
                expected_device_id.clone(),
                session.generation,
                HUB_TIME_MS + index,
                10_000,
            )?;
            tracker.observe(&heartbeat, HUB_TIME_MS + index + 1)?;
            let ack = hub_identity.heartbeat_ack(
                &hello,
                &challenge,
                &heartbeat,
                HUB_TIME_MS + index + 1,
            )?;
            write_frame(&mut tls, &HubToAgent::HeartbeatAck(ack))?;
        }

        let stale = CommandEnvelope {
            schema_version: CONTROL_SCHEMA_VERSION,
            device_id: expected_device_id,
            device_generation: generations[0],
            capability_revision: 7,
            operation_id: "stale-after-reconnect".into(),
            command: DeviceCommand::ListApplications,
        };
        let stale_rejected = matches!(
            router.route(&stale),
            Err(computer_use_mcp_gateway::v2_m1::M1Error::Control(
                computer_use_mcp_gateway::v2_m0::ControlError::StaleDeviceGeneration { .. }
            ))
        );
        Ok((generations, stale_rejected))
    });

    let mut established_sessions = 0_u32;
    let lifecycle = run_outbound_lifecycle(
        ReconnectPolicy {
            initial_delay: Duration::from_millis(1),
            max_delay: Duration::from_millis(8),
            max_attempts: 3,
        },
        || {
            let tcp = TcpStream::connect(address).map_err(|error| error.to_string())?;
            connect_agent_tls(tcp, "localhost", agent_tls.clone())
                .map_err(|error| error.to_string())
        },
        |mut tls| {
            let result = (|| -> Result<SessionDirective> {
                let hello = computer_use_mcp_gateway::v2_m0_transport::AgentHello::new(
                    device_id.clone(),
                    capabilities(),
                );
                write_frame(&mut tls, &AgentToHub::Hello(hello.clone()))?;
                let challenge = match read_frame::<_, HubToAgent>(&mut tls)? {
                    HubToAgent::Challenge(challenge) => challenge,
                    other => bail!("expected challenge, got {other:?}"),
                };
                verify_hub_challenge(&hello, &challenge, &trusted_hub)?;
                let proof = build_agent_proof(&identity, &hello, &challenge)?;
                write_frame(&mut tls, &AgentToHub::Proof(proof))?;
                let accepted = match read_frame::<_, HubToAgent>(&mut tls)? {
                    HubToAgent::Accepted(accepted) => accepted,
                    other => bail!("expected acceptance, got {other:?}"),
                };
                verify_session_accepted(&hello, &challenge, &accepted, &trusted_hub)?;
                let heartbeat = build_agent_heartbeat(
                    &identity,
                    &hello,
                    &challenge,
                    accepted.device_generation,
                    1,
                )?;
                write_frame(&mut tls, &AgentToHub::Heartbeat(heartbeat))?;
                let ack = match read_frame::<_, HubToAgent>(&mut tls)? {
                    HubToAgent::HeartbeatAck(ack) => ack,
                    other => bail!("expected heartbeat ack, got {other:?}"),
                };
                verify_hub_heartbeat_ack(&hello, &challenge, &ack, &trusted_hub)?;
                established_sessions += 1;
                Ok(if established_sessions == 1 {
                    SessionDirective::Reconnect
                } else {
                    SessionDirective::Shutdown
                })
            })();
            result.map_err(|error| format!("{error:#}"))
        },
        thread::sleep,
    );
    lifecycle.map_err(|error| anyhow!("reconnect lifecycle failed: {error:?}"))?;
    let (generations, stale_rejected) = hub
        .join()
        .map_err(|_| anyhow!("Hub reconnect thread panicked"))??;
    assert_eq!(established_sessions, 2);
    assert_eq!(generations, vec![1, 2]);
    assert!(stale_rejected);
    Ok(())
}
