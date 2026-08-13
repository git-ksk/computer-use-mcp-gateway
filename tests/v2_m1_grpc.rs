use anyhow::{Result, anyhow, bail};
use computer_use_mcp_gateway::{
    v2_m0::{
        CAPABILITY_SCHEMA_VERSION, CONTROL_SCHEMA_VERSION, CapabilityAdvertisement,
        CommandEnvelope, CommandResultEnvelope, DeviceCapability, DeviceCommand, DeviceIdentity,
        DeviceRegistry, DeviceResult, DeviceSession, GrantAuthority, GrantLedger, ProcessRequest,
        validate_command_result, validate_command_session,
    },
    v2_m0_transport::{
        AgentToHub, HubIdentity, HubToAgent, TrustedSessionClock, build_agent_heartbeat,
        build_agent_proof, build_remote_result, verify_agent_heartbeat, verify_agent_proof,
        verify_hub_challenge, verify_hub_heartbeat_ack, verify_remote_command,
        verify_remote_result, verify_session_accepted,
    },
    v2_m1_grpc::{
        decode_agent_frame, decode_hub_frame, encode_agent_frame, encode_hub_frame,
        proto::{
            AgentFrame, HubFrame,
            agent_control_client::AgentControlClient,
            agent_control_server::{AgentControl, AgentControlServer},
        },
    },
    v2_m1_process::{ProcessCancellation, ProcessExecutor, ProcessPolicy},
};
use rcgen::{CertifiedKey, generate_simple_self_signed};
use std::{
    env,
    pin::Pin,
    sync::{Arc, Mutex},
};
use tokio::sync::mpsc;
use tokio_stream::{
    Stream, StreamExt,
    wrappers::{ReceiverStream, TcpListenerStream},
};
use tonic::transport::{
    Certificate, Channel, ClientTlsConfig, Endpoint, Identity, Server, ServerTlsConfig,
};
use tonic::{Request, Response, Status, Streaming};

const HUB_TIME_MS: u64 = 300_000;

fn capabilities() -> CapabilityAdvertisement {
    CapabilityAdvertisement {
        backend: "agent-native".into(),
        backend_version: env!("CARGO_PKG_VERSION").into(),
        platform: format!("{}-{}", env::consts::OS, env::consts::ARCH),
        capability_schema_version: CAPABILITY_SCHEMA_VERSION,
        revision: 1,
        supported: vec![DeviceCapability::ExecuteProcess],
    }
}

#[derive(Clone)]
struct TestHub {
    hub_identity: HubIdentity,
    registry: Arc<Mutex<DeviceRegistry>>,
    grant_authority: GrantAuthority,
    expected_device_id: String,
    cwd: String,
    observed_stdout_len: Arc<Mutex<Option<usize>>>,
}

#[tonic::async_trait]
impl AgentControl for TestHub {
    type OpenSessionStream = Pin<Box<dyn Stream<Item = Result<HubFrame, Status>> + Send + 'static>>;

    async fn open_session(
        &self,
        request: Request<Streaming<AgentFrame>>,
    ) -> Result<Response<Self::OpenSessionStream>, Status> {
        let mut inbound = request.into_inner();
        let (tx, rx) = mpsc::channel(8);
        let state = self.clone();
        tokio::spawn(async move {
            let result: Result<()> = async {
                let hello = match decode_agent_frame(
                    inbound
                        .message()
                        .await?
                        .ok_or_else(|| anyhow!("missing hello"))?,
                )? {
                    AgentToHub::Hello(hello) => hello,
                    other => bail!("expected hello, got {other:?}"),
                };
                if hello.device_id != state.expected_device_id {
                    bail!("unexpected device id");
                }
                let challenge = state.hub_identity.challenge(&hello)?;
                tx.send(Ok(encode_hub_frame(&HubToAgent::Challenge(
                    challenge.clone(),
                ))?))
                .await?;

                let proof = match decode_agent_frame(
                    inbound
                        .message()
                        .await?
                        .ok_or_else(|| anyhow!("missing proof"))?,
                )? {
                    AgentToHub::Proof(proof) => proof,
                    other => bail!("expected proof, got {other:?}"),
                };
                let session = {
                    let mut registry = state
                        .registry
                        .lock()
                        .map_err(|_| anyhow!("registry poisoned"))?;
                    verify_agent_proof(&registry, &hello, &challenge, &proof)?;
                    registry.connect(&state.expected_device_id, hello.capabilities.clone())?
                };
                let accepted = state.hub_identity.accept_session(
                    &hello,
                    &challenge,
                    session.generation,
                    session.capabilities.revision,
                    HUB_TIME_MS,
                )?;
                tx.send(Ok(encode_hub_frame(&HubToAgent::Accepted(accepted))?))
                    .await?;

                let heartbeat = match decode_agent_frame(
                    inbound
                        .message()
                        .await?
                        .ok_or_else(|| anyhow!("missing heartbeat"))?,
                )? {
                    AgentToHub::Heartbeat(heartbeat) => heartbeat,
                    other => bail!("expected heartbeat, got {other:?}"),
                };
                {
                    let registry = state
                        .registry
                        .lock()
                        .map_err(|_| anyhow!("registry poisoned"))?;
                    verify_agent_heartbeat(&registry, &hello, &challenge, &heartbeat)?;
                }
                let ack = state.hub_identity.heartbeat_ack(
                    &hello,
                    &challenge,
                    &heartbeat,
                    HUB_TIME_MS + 1,
                )?;
                tx.send(Ok(encode_hub_frame(&HubToAgent::HeartbeatAck(ack))?))
                    .await?;

                let command = CommandEnvelope {
                    schema_version: CONTROL_SCHEMA_VERSION,
                    device_id: state.expected_device_id.clone(),
                    device_generation: session.generation,
                    capability_revision: session.capabilities.revision,
                    operation_id: "grpc-process-git-status".into(),
                    command: DeviceCommand::ExecuteProcess {
                        request: ProcessRequest {
                            program: "git".into(),
                            args: vec!["status".into(), "--short".into()],
                            cwd: state.cwd.clone(),
                            env: vec![],
                            timeout_ms: 10_000,
                        },
                    },
                };
                validate_command_session(&command, &session)?;
                let grant = state.grant_authority.issue_for_device_capability(
                    &state.expected_device_id,
                    DeviceCapability::ExecuteProcess,
                    HUB_TIME_MS,
                    30_000,
                )?;
                let remote = state.hub_identity.remote_command(
                    &hello,
                    &challenge,
                    command.clone(),
                    grant,
                )?;
                tx.send(Ok(encode_hub_frame(&HubToAgent::Command(remote))?))
                    .await?;

                let remote_result = match decode_agent_frame(
                    inbound
                        .message()
                        .await?
                        .ok_or_else(|| anyhow!("missing process result"))?,
                )? {
                    AgentToHub::Result(result) => result,
                    other => bail!("expected result, got {other:?}"),
                };
                {
                    let registry = state
                        .registry
                        .lock()
                        .map_err(|_| anyhow!("registry poisoned"))?;
                    verify_remote_result(&registry, &hello, &challenge, &remote_result)?;
                }
                validate_command_result(&command, &remote_result.result)?;
                let stdout_len = match remote_result.result.result {
                    DeviceResult::Process { output } => {
                        if output.exit_code != Some(0) || output.timed_out || output.cancelled {
                            bail!("git did not complete normally");
                        }
                        output.stdout.len()
                    }
                    other => bail!("unexpected result {other:?}"),
                };
                *state
                    .observed_stdout_len
                    .lock()
                    .map_err(|_| anyhow!("result poisoned"))? = Some(stdout_len);
                Ok(())
            }
            .await;
            if let Err(error) = result {
                let _ = tx.send(Err(Status::internal(error.to_string()))).await;
            }
        });
        Ok(Response::new(Box::pin(ReceiverStream::new(rx))))
    }
}

async fn connect_with_retry(endpoint: Endpoint) -> Result<Channel> {
    let mut last = None;
    for _ in 0..50 {
        match endpoint.clone().connect().await {
            Ok(channel) => return Ok(channel),
            Err(error) => last = Some(error),
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    Err(anyhow!("gRPC server did not become ready: {last:?}"))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn grpc_bidi_tls_preserves_v2_security_and_executes_agent_native_git() -> Result<()> {
    let repo_root = env::current_dir()?;
    let cwd = repo_root.to_string_lossy().into_owned();
    let CertifiedKey { cert, signing_key } = generate_simple_self_signed(vec!["localhost".into()])?;
    let cert_pem = cert.pem();
    let key_pem = signing_key.serialize_pem();

    let identity = DeviceIdentity::generate();
    let challenge = DeviceRegistry::enrollment_challenge();
    let proof = identity.enrollment_proof(&challenge);
    let mut registry = DeviceRegistry::default();
    let device_id = registry.enroll(&identity.public_key(), &challenge, &proof)?;
    let hub_identity = HubIdentity::generate();
    let trusted_hub = hub_identity.verifier();
    let grant_authority = GrantAuthority::generate();
    let grant_verifier = grant_authority.verifier();
    let observed_stdout_len = Arc::new(Mutex::new(None));

    let service = TestHub {
        hub_identity,
        registry: Arc::new(Mutex::new(registry)),
        grant_authority,
        expected_device_id: device_id.clone(),
        cwd,
        observed_stdout_len: observed_stdout_len.clone(),
    };

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let incoming = TcpListenerStream::new(listener);
    let server_tls = ServerTlsConfig::new().identity(Identity::from_pem(cert_pem.clone(), key_pem));
    let server = tokio::spawn(async move {
        Server::builder()
            .tls_config(server_tls)?
            .add_service(AgentControlServer::new(service))
            .serve_with_incoming(incoming)
            .await
    });

    let endpoint = Endpoint::from_shared(format!("https://localhost:{}", address.port()))?
        .tls_config(
            ClientTlsConfig::new()
                .ca_certificate(Certificate::from_pem(cert_pem))
                .domain_name("localhost"),
        )?;
    let channel = connect_with_retry(endpoint).await?;
    let mut client = AgentControlClient::new(channel)
        .max_decoding_message_size(64 * 1024)
        .max_encoding_message_size(64 * 1024);
    let (tx, rx) = mpsc::channel(8);
    let mut response = client
        .open_session(ReceiverStream::new(rx))
        .await?
        .into_inner();

    let hello = computer_use_mcp_gateway::v2_m0_transport::AgentHello::new(
        device_id.clone(),
        capabilities(),
    );
    tx.send(encode_agent_frame(&AgentToHub::Hello(hello.clone()))?)
        .await?;
    let hub_challenge = match decode_hub_frame(
        response
            .message()
            .await?
            .ok_or_else(|| anyhow!("missing challenge"))?,
    )? {
        HubToAgent::Challenge(challenge) => challenge,
        other => bail!("expected challenge, got {other:?}"),
    };
    verify_hub_challenge(&hello, &hub_challenge, &trusted_hub)?;
    let proof = build_agent_proof(&identity, &hello, &hub_challenge)?;
    tx.send(encode_agent_frame(&AgentToHub::Proof(proof))?)
        .await?;

    let accepted = match decode_hub_frame(
        response
            .message()
            .await?
            .ok_or_else(|| anyhow!("missing accepted"))?,
    )? {
        HubToAgent::Accepted(accepted) => accepted,
        other => bail!("expected accepted, got {other:?}"),
    };
    verify_session_accepted(&hello, &hub_challenge, &accepted, &trusted_hub)?;
    let session = DeviceSession {
        device_id: device_id.clone(),
        generation: accepted.device_generation,
        capabilities: capabilities(),
    };
    let trusted_clock = TrustedSessionClock::new(accepted.hub_time_ms);

    let heartbeat =
        build_agent_heartbeat(&identity, &hello, &hub_challenge, session.generation, 1)?;
    tx.send(encode_agent_frame(&AgentToHub::Heartbeat(heartbeat))?)
        .await?;
    let ack = match decode_hub_frame(
        response
            .message()
            .await?
            .ok_or_else(|| anyhow!("missing heartbeat ack"))?,
    )? {
        HubToAgent::HeartbeatAck(ack) => ack,
        other => bail!("expected heartbeat ack, got {other:?}"),
    };
    verify_hub_heartbeat_ack(&hello, &hub_challenge, &ack, &trusted_hub)?;

    let remote = match decode_hub_frame(
        response
            .message()
            .await?
            .ok_or_else(|| anyhow!("missing process command"))?,
    )? {
        HubToAgent::Command(remote) => remote,
        other => bail!("expected command, got {other:?}"),
    };
    verify_remote_command(&hello, &hub_challenge, &remote, &trusted_hub)?;
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
    let output = executor.execute(request, &ProcessCancellation::default())?;
    let result = CommandResultEnvelope {
        schema_version: CONTROL_SCHEMA_VERSION,
        device_id,
        device_generation: remote.command.device_generation,
        capability_revision: remote.command.capability_revision,
        operation_id: remote.command.operation_id,
        result: DeviceResult::Process { output },
    };
    let signed = build_remote_result(&identity, &hello, &hub_challenge, result)?;
    tx.send(encode_agent_frame(&AgentToHub::Result(signed))?)
        .await?;
    drop(tx);

    while let Some(message) = response.next().await {
        message?;
    }
    let observed = observed_stdout_len
        .lock()
        .map_err(|_| anyhow!("result poisoned"))?;
    assert!(observed.is_some_and(|len| len <= 16 * 1024));

    server.abort();
    Ok(())
}
