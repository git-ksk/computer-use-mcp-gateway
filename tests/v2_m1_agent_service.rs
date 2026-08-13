#![cfg(unix)]

use anyhow::{Result, anyhow, bail};
use computer_use_mcp_gateway::{
    v2_m0::{
        CONTROL_SCHEMA_VERSION, CommandEnvelope, DeviceCommand, DeviceIdentity, DeviceRegistry,
        DeviceResult, GrantAuthority, ProcessRequest, validate_command_result,
    },
    v2_m0_transport::{
        AgentToHub, CancellationDisposition, HubIdentity, HubToAgent, verify_agent_heartbeat,
        verify_agent_proof, verify_remote_cancellation_ack, verify_remote_result,
    },
    v2_m1::ReconnectPolicy,
    v2_m1_agent::{AgentService, AgentServiceConfig},
    v2_m1_grpc::{
        decode_agent_frame, encode_hub_frame,
        proto::{
            AgentFrame, HubFrame,
            agent_control_server::{AgentControl, AgentControlServer},
        },
    },
    v2_m1_keys::AgentProvisionedMaterial,
    v2_m1_persistence::{AgentPersistentState, CheckpointStore},
};
use rcgen::{CertifiedKey, generate_simple_self_signed};
use std::{
    env,
    pin::Pin,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio::sync::{mpsc, watch};
use tokio_stream::{
    Stream,
    wrappers::{ReceiverStream, TcpListenerStream},
};
use tonic::transport::{Identity, Server, ServerTlsConfig};
use tonic::{Request, Response, Status, Streaming};

const HUB_TIME_MS: u64 = 400_000;

#[derive(Debug, Default)]
struct Evidence {
    generations: Vec<u64>,
    cancellation_ack: bool,
    cancelled_result: bool,
}

#[derive(Clone)]
struct LifecycleHub {
    hub_identity: HubIdentity,
    registry: Arc<Mutex<DeviceRegistry>>,
    grant_authority: GrantAuthority,
    expected_device_id: String,
    cwd: String,
    sessions: Arc<AtomicUsize>,
    evidence: Arc<Mutex<Evidence>>,
    shutdown: watch::Sender<bool>,
}

#[tonic::async_trait]
impl AgentControl for LifecycleHub {
    type OpenSessionStream = Pin<Box<dyn Stream<Item = Result<HubFrame, Status>> + Send + 'static>>;

    async fn open_session(
        &self,
        request: Request<Streaming<AgentFrame>>,
    ) -> Result<Response<Self::OpenSessionStream>, Status> {
        let session_index = self.sessions.fetch_add(1, Ordering::SeqCst);
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
                state
                    .evidence
                    .lock()
                    .map_err(|_| anyhow!("evidence poisoned"))?
                    .generations
                    .push(session.generation);
                let accepted = state.hub_identity.accept_session(
                    &hello,
                    &challenge,
                    session.generation,
                    session.capabilities.revision,
                    HUB_TIME_MS + session_index as u64,
                )?;
                tx.send(Ok(encode_hub_frame(&HubToAgent::Accepted(accepted))?))
                    .await?;

                if session_index == 0 {
                    // Deliberately end the first authenticated stream. The Agent must
                    // reconnect outbound and must not reuse generation 1.
                    return Ok(());
                }

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
                    HUB_TIME_MS + 10,
                )?;
                tx.send(Ok(encode_hub_frame(&HubToAgent::HeartbeatAck(ack))?))
                    .await?;

                let operation_id = "agent-service-cancel-sleep".to_string();
                let command = CommandEnvelope {
                    schema_version: CONTROL_SCHEMA_VERSION,
                    device_id: state.expected_device_id.clone(),
                    device_generation: session.generation,
                    capability_revision: session.capabilities.revision,
                    operation_id: operation_id.clone(),
                    command: DeviceCommand::ExecuteProcess {
                        request: ProcessRequest {
                            program: "/bin/sleep".into(),
                            args: vec!["30".into()],
                            cwd: state.cwd.clone(),
                            env: vec![],
                            timeout_ms: 60_000,
                        },
                    },
                };
                let grant = state.grant_authority.issue_for_device_capability(
                    &state.expected_device_id,
                    computer_use_mcp_gateway::v2_m0::DeviceCapability::ExecuteProcess,
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

                let cancel = state.hub_identity.remote_cancel(
                    &hello,
                    &challenge,
                    state.expected_device_id.clone(),
                    session.generation,
                    operation_id.clone(),
                )?;
                tx.send(Ok(encode_hub_frame(&HubToAgent::Cancel(cancel))?))
                    .await?;

                let mut saw_ack = false;
                let mut saw_result = false;
                while !(saw_ack && saw_result) {
                    let message = decode_agent_frame(
                        tokio::time::timeout(Duration::from_secs(5), inbound.message())
                            .await
                            .map_err(|_| {
                                anyhow!("timed out waiting for Agent cancellation evidence")
                            })??
                            .ok_or_else(|| anyhow!("Agent stream ended early"))?,
                    )?;
                    match message {
                        AgentToHub::CancellationAck(ack) => {
                            let registry = state
                                .registry
                                .lock()
                                .map_err(|_| anyhow!("registry poisoned"))?;
                            verify_remote_cancellation_ack(&registry, &hello, &challenge, &ack)?;
                            if ack.operation_id != operation_id
                                || ack.disposition != CancellationDisposition::CancellationRequested
                            {
                                bail!("unexpected cancellation acknowledgement");
                            }
                            saw_ack = true;
                        }
                        AgentToHub::Result(result) => {
                            let registry = state
                                .registry
                                .lock()
                                .map_err(|_| anyhow!("registry poisoned"))?;
                            verify_remote_result(&registry, &hello, &challenge, &result)?;
                            validate_command_result(&command, &result.result)?;
                            match result.result.result {
                                DeviceResult::Process { output }
                                    if output.cancelled && !output.timed_out => {}
                                other => bail!("unexpected cancelled process result: {other:?}"),
                            }
                            saw_result = true;
                        }
                        AgentToHub::Heartbeat(heartbeat) => {
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
                                HUB_TIME_MS + 20,
                            )?;
                            tx.send(Ok(encode_hub_frame(&HubToAgent::HeartbeatAck(ack))?))
                                .await?;
                        }
                        other => bail!("unexpected Agent message: {other:?}"),
                    }
                }
                {
                    let mut evidence = state
                        .evidence
                        .lock()
                        .map_err(|_| anyhow!("evidence poisoned"))?;
                    evidence.cancellation_ack = saw_ack;
                    evidence.cancelled_result = saw_result;
                }
                let _ = state.shutdown.send(true);
                Ok(())
            }
            .await;
            if let Err(error) = result {
                let _ = tx.send(Err(Status::internal(error.to_string()))).await;
                let _ = state.shutdown.send(true);
            }
        });
        Ok(Response::new(Box::pin(ReceiverStream::new(rx))))
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn long_lived_agent_reconnects_and_cancels_process_without_blocking_the_stream() -> Result<()>
{
    let cwd = env::current_dir()?;
    let CertifiedKey { cert, signing_key } = generate_simple_self_signed(vec!["localhost".into()])?;
    let cert_pem = cert.pem();
    let cert_der = cert.der().to_vec();
    let key_pem = signing_key.serialize_pem();

    let device_identity = DeviceIdentity::generate();
    let enrollment = DeviceRegistry::enrollment_challenge();
    let proof = device_identity.enrollment_proof(&enrollment);
    let mut registry = DeviceRegistry::default();
    let device_id = registry.enroll(&device_identity.public_key(), &enrollment, &proof)?;
    let hub_identity = HubIdentity::generate();
    let grant_authority = GrantAuthority::generate();
    let evidence = Arc::new(Mutex::new(Evidence::default()));
    let sessions = Arc::new(AtomicUsize::new(0));
    let (shutdown_tx, shutdown_rx) = watch::channel(false);

    let hub = LifecycleHub {
        hub_identity: hub_identity.clone(),
        registry: Arc::new(Mutex::new(registry)),
        grant_authority: grant_authority.clone(),
        expected_device_id: device_id.clone(),
        cwd: cwd.to_string_lossy().into_owned(),
        sessions: sessions.clone(),
        evidence: evidence.clone(),
        shutdown: shutdown_tx.clone(),
    };

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let incoming = TcpListenerStream::new(listener);
    let server = tokio::spawn(async move {
        Server::builder()
            .tls_config(ServerTlsConfig::new().identity(Identity::from_pem(cert_pem, key_pem)))?
            .add_service(AgentControlServer::new(hub))
            .serve_with_incoming(incoming)
            .await
    });

    let material = AgentProvisionedMaterial {
        device_identity,
        trusted_hub: hub_identity.verifier(),
        grant_verifier: grant_authority.verifier(),
        additional_grant_verifiers: vec![],
        hub_rotation: None,
        tls_root_der: cert_der,
    };
    let material_for_restart = material.clone();
    let state_dir = std::env::temp_dir().join(format!(
        "cumg-agent-service-state-{}",
        rand::random::<u64>()
    ));
    let config = AgentServiceConfig {
        hub_endpoint: format!("https://localhost:{}", address.port()),
        hub_domain: "localhost".into(),
        device_id,
        allowed_cwd_roots: vec![cwd],
        state_dir: state_dir.clone(),
        heartbeat_interval: Duration::from_millis(50),
        reconnect: ReconnectPolicy {
            initial_delay: Duration::from_millis(5),
            max_delay: Duration::from_millis(50),
            max_attempts: 4,
        },
        cua: None,
    };
    let restart_config = config.clone();
    let mut agent = AgentService::new(config, material)?;
    tokio::time::timeout(Duration::from_secs(8), agent.run(shutdown_rx))
        .await
        .map_err(|_| anyhow!("Agent service lifecycle timed out"))??;

    let observed = evidence.lock().map_err(|_| anyhow!("evidence poisoned"))?;
    assert_eq!(observed.generations, vec![1, 2]);
    assert!(observed.cancellation_ack && observed.cancelled_result);
    assert_eq!(sessions.load(Ordering::SeqCst), 2);
    drop(observed);

    let checkpoint = CheckpointStore::new(&state_dir, "agent")?;
    let persisted: AgentPersistentState = checkpoint.load_latest()?;
    assert_eq!(persisted.device_id, restart_config.device_id);
    assert!(!persisted.grant_ledger.consumed_grants.is_empty());
    assert!(
        persisted
            .execution
            .terminal_operation_ids
            .iter()
            .any(|id| id == "agent-service-cancel-sleep")
    );
    // Constructor restore is the actual process-restart boundary used by v2_agent.
    let _restored_agent = AgentService::new(restart_config, material_for_restart)?;

    server.abort();
    std::fs::remove_dir_all(state_dir)?;
    Ok(())
}
