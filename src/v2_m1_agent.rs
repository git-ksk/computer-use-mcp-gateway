//! Operator-facing V2-M1 outbound Agent runtime over gRPC bidirectional streaming.
//!
//! The runtime keeps the V2 application protocol independently signed. gRPC/TLS
//! supplies the long-lived full-duplex carrier while the Agent owns heartbeat,
//! reconnect, grant validation, replay barriers, direct process execution, and
//! cancellation. Cua/GUI execution is intentionally outside this shell-first slice.

use crate::v2_m0::{
    CAPABILITY_SCHEMA_VERSION, CONTROL_SCHEMA_VERSION, CapabilityAdvertisement,
    CommandResultEnvelope, DeviceCapability, DeviceCommand, DeviceResult, DeviceSession,
    GrantLedger,
};
use crate::v2_m0_execution::{AgentExecutionGate, OperationRef};
use crate::v2_m0_transport::{
    AgentHello, AgentToHub, CancellationDisposition, HubChallenge, HubToAgent, TrustedSessionClock,
    build_agent_heartbeat, build_agent_proof, build_remote_cancellation_ack, build_remote_result,
    verify_hub_challenge, verify_hub_heartbeat_ack, verify_remote_cancel, verify_remote_command,
    verify_session_accepted,
};
use crate::v2_m1::ReconnectPolicy;
use crate::v2_m1_grpc::{
    decode_hub_frame, encode_agent_frame, proto::agent_control_client::AgentControlClient,
};
use crate::v2_m1_keys::AgentProvisionedMaterial;
use crate::v2_m1_process::{ProcessCancellation, ProcessError, ProcessExecutor, ProcessPolicy};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use std::fmt;
use std::path::PathBuf;
use std::time::{Duration, Instant};
use tokio::sync::{mpsc, watch};
use tokio_stream::wrappers::ReceiverStream;
use tonic::Code;
use tonic::transport::{Certificate, Channel, ClientTlsConfig, Endpoint};

const GRPC_QUEUE_DEPTH: usize = 8;

#[derive(Debug, Clone)]
pub struct AgentServiceConfig {
    pub hub_endpoint: String,
    pub hub_domain: String,
    pub device_id: String,
    pub allowed_cwd_roots: Vec<PathBuf>,
    pub heartbeat_interval: Duration,
    pub reconnect: ReconnectPolicy,
}

impl AgentServiceConfig {
    pub fn validate(&self) -> Result<(), AgentServiceError> {
        if !self.hub_endpoint.starts_with("https://") {
            return Err(AgentServiceError::InvalidConfig(
                "Hub endpoint must use https://",
            ));
        }
        if self.hub_domain.trim().is_empty() || self.device_id.trim().is_empty() {
            return Err(AgentServiceError::InvalidConfig(
                "Hub domain and device id must be non-empty",
            ));
        }
        if self.heartbeat_interval.is_zero() {
            return Err(AgentServiceError::InvalidConfig(
                "heartbeat interval must be non-zero",
            ));
        }
        self.reconnect
            .validate()
            .map_err(|_| AgentServiceError::InvalidConfig("invalid reconnect policy"))?;
        if self.allowed_cwd_roots.is_empty() {
            return Err(AgentServiceError::InvalidConfig(
                "at least one allowed cwd root is required",
            ));
        }
        Ok(())
    }
}

pub struct AgentService {
    config: AgentServiceConfig,
    material: AgentProvisionedMaterial,
    executor: ProcessExecutor,
    grants: GrantLedger,
    execution: AgentExecutionGate,
}

impl AgentService {
    pub fn new(
        config: AgentServiceConfig,
        material: AgentProvisionedMaterial,
    ) -> Result<Self, AgentServiceError> {
        config.validate()?;
        let executor = ProcessExecutor::new(
            ProcessPolicy::developer_defaults(config.allowed_cwd_roots.clone())
                .map_err(AgentServiceError::Process)?,
        );
        let grants = GrantLedger::new(material.grant_verifier);
        Ok(Self {
            config,
            material,
            executor,
            grants,
            execution: AgentExecutionGate::default(),
        })
    }

    pub fn capabilities(&self) -> CapabilityAdvertisement {
        CapabilityAdvertisement {
            backend: "agent-native".into(),
            backend_version: env!("CARGO_PKG_VERSION").into(),
            platform: format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH),
            capability_schema_version: CAPABILITY_SCHEMA_VERSION,
            revision: 1,
            supported: vec![DeviceCapability::ExecuteProcess],
        }
    }

    /// Run until shutdown or a bounded sequence of connection/session failures is exhausted.
    /// A successfully authenticated session resets the reconnect failure streak.
    pub async fn run(
        &mut self,
        mut shutdown: watch::Receiver<bool>,
    ) -> Result<(), AgentServiceError> {
        let mut failures = 0_u32;
        loop {
            if *shutdown.borrow() {
                return Ok(());
            }

            let channel = match self.connect_channel().await {
                Ok(channel) => channel,
                Err(error) => {
                    failures = failures.saturating_add(1);
                    if failures >= self.config.reconnect.max_attempts {
                        return Err(error);
                    }
                    let delay = self
                        .config
                        .reconnect
                        .delay_for_attempt(failures.saturating_sub(1))
                        .map_err(|_| AgentServiceError::ReconnectExhausted)?;
                    if sleep_or_shutdown(delay, &mut shutdown).await {
                        return Ok(());
                    }
                    continue;
                }
            };

            match self.run_authenticated_session(channel, &mut shutdown).await {
                Ok(SessionExit::Shutdown) => return Ok(()),
                Ok(SessionExit::Reconnect) => {
                    failures = 0;
                    if sleep_or_shutdown(self.config.reconnect.initial_delay, &mut shutdown).await {
                        return Ok(());
                    }
                }
                Err(error) if error.reconnectable() => {
                    failures = failures.saturating_add(1);
                    if failures >= self.config.reconnect.max_attempts {
                        return Err(error);
                    }
                    let delay = self
                        .config
                        .reconnect
                        .delay_for_attempt(failures.saturating_sub(1))
                        .map_err(|_| AgentServiceError::ReconnectExhausted)?;
                    if sleep_or_shutdown(delay, &mut shutdown).await {
                        return Ok(());
                    }
                }
                Err(error) => return Err(error),
            }
        }
    }

    async fn connect_channel(&self) -> Result<Channel, AgentServiceError> {
        let root_pem = der_certificate_to_pem(&self.material.tls_root_der);
        let tls = ClientTlsConfig::new()
            .ca_certificate(Certificate::from_pem(root_pem))
            .domain_name(self.config.hub_domain.clone());
        Endpoint::from_shared(self.config.hub_endpoint.clone())
            .map_err(AgentServiceError::Transport)?
            .tls_config(tls)
            .map_err(AgentServiceError::Transport)?
            .connect()
            .await
            .map_err(AgentServiceError::Transport)
    }

    async fn run_authenticated_session(
        &mut self,
        channel: Channel,
        shutdown: &mut watch::Receiver<bool>,
    ) -> Result<SessionExit, AgentServiceError> {
        let mut client = AgentControlClient::new(channel)
            .max_decoding_message_size(crate::v2_m1_grpc::MAX_GRPC_APPLICATION_MESSAGE_BYTES)
            .max_encoding_message_size(crate::v2_m1_grpc::MAX_GRPC_APPLICATION_MESSAGE_BYTES);
        let (outbound_tx, outbound_rx) = mpsc::channel(GRPC_QUEUE_DEPTH);
        let mut inbound = client
            .open_session(ReceiverStream::new(outbound_rx))
            .await
            .map_err(AgentServiceError::Status)?
            .into_inner();

        let hello = AgentHello::new(self.config.device_id.clone(), self.capabilities());
        send_agent(&outbound_tx, AgentToHub::Hello(hello.clone())).await?;
        let challenge = match next_hub(&mut inbound).await? {
            HubToAgent::Challenge(challenge) => challenge,
            other => return Err(AgentServiceError::UnexpectedMessage(format!("{other:?}"))),
        };
        verify_hub_challenge(&hello, &challenge, &self.material.trusted_hub)
            .map_err(AgentServiceError::Protocol)?;
        let proof = build_agent_proof(&self.material.device_identity, &hello, &challenge)
            .map_err(AgentServiceError::Protocol)?;
        send_agent(&outbound_tx, AgentToHub::Proof(proof)).await?;

        let accepted = match next_hub(&mut inbound).await? {
            HubToAgent::Accepted(accepted) => accepted,
            other => return Err(AgentServiceError::UnexpectedMessage(format!("{other:?}"))),
        };
        verify_session_accepted(&hello, &challenge, &accepted, &self.material.trusted_hub)
            .map_err(AgentServiceError::Protocol)?;
        let session = DeviceSession {
            device_id: self.config.device_id.clone(),
            generation: accepted.device_generation,
            capabilities: hello.capabilities.clone(),
        };
        let trusted_clock = TrustedSessionClock::new(accepted.hub_time_ms);

        self.run_session_loop(
            inbound,
            outbound_tx,
            hello,
            challenge,
            session,
            trusted_clock,
            shutdown,
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    async fn run_session_loop(
        &mut self,
        mut inbound: tonic::Streaming<crate::v2_m1_grpc::proto::HubFrame>,
        outbound_tx: mpsc::Sender<crate::v2_m1_grpc::proto::AgentFrame>,
        hello: AgentHello,
        challenge: HubChallenge,
        session: DeviceSession,
        trusted_clock: TrustedSessionClock,
        shutdown: &mut watch::Receiver<bool>,
    ) -> Result<SessionExit, AgentServiceError> {
        let mut heartbeat = tokio::time::interval(self.config.heartbeat_interval);
        heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        // The immediate first tick establishes liveness as soon as authentication finishes.
        let mut heartbeat_sequence = 0_u64;
        let mut pending_heartbeat: Option<(u64, Instant)> = None;
        let heartbeat_deadline = self.config.heartbeat_interval.saturating_mul(3);

        let (process_done_tx, mut process_done_rx) = mpsc::channel::<ProcessCompletion>(1);
        let mut active: Option<ActiveProcess> = None;

        loop {
            tokio::select! {
                changed = shutdown.changed() => {
                    if changed.is_err() || *shutdown.borrow() {
                        terminate_active(&mut active, &mut process_done_rx, &mut self.execution).await?;
                        return Ok(SessionExit::Shutdown);
                    }
                }
                _ = heartbeat.tick() => {
                    if pending_heartbeat.as_ref().is_some_and(|(_, sent)| sent.elapsed() >= heartbeat_deadline) {
                        terminate_active(&mut active, &mut process_done_rx, &mut self.execution).await?;
                        return Ok(SessionExit::Reconnect);
                    }
                    if pending_heartbeat.is_none() {
                        heartbeat_sequence = heartbeat_sequence.saturating_add(1);
                        let message = build_agent_heartbeat(
                            &self.material.device_identity,
                            &hello,
                            &challenge,
                            session.generation,
                            heartbeat_sequence,
                        ).map_err(AgentServiceError::Protocol)?;
                        send_agent(&outbound_tx, AgentToHub::Heartbeat(message)).await?;
                        pending_heartbeat = Some((heartbeat_sequence, Instant::now()));
                    }
                }
                completion = process_done_rx.recv(), if active.is_some() => {
                    let completion = completion.ok_or(AgentServiceError::ProcessWorkerClosed)?;
                    let expected = active.take().ok_or(AgentServiceError::ProcessStateMismatch)?;
                    if completion.operation_id != expected.operation_id {
                        return Err(AgentServiceError::ProcessStateMismatch);
                    }
                    self.execution.finish(&completion.operation_id)
                        .map_err(AgentServiceError::Execution)?;
                    let output = completion.output.map_err(AgentServiceError::Process)?;
                    let result = CommandResultEnvelope {
                        schema_version: CONTROL_SCHEMA_VERSION,
                        device_id: session.device_id.clone(),
                        device_generation: session.generation,
                        capability_revision: session.capabilities.revision,
                        operation_id: completion.operation_id,
                        result: DeviceResult::Process { output },
                    };
                    let signed = build_remote_result(
                        &self.material.device_identity,
                        &hello,
                        &challenge,
                        result,
                    ).map_err(AgentServiceError::Protocol)?;
                    send_agent(&outbound_tx, AgentToHub::Result(signed)).await?;
                }
                message = inbound.message() => {
                    let frame = match message.map_err(AgentServiceError::Status)? {
                        Some(frame) => frame,
                        None => {
                            terminate_active(&mut active, &mut process_done_rx, &mut self.execution).await?;
                            return Ok(SessionExit::Reconnect);
                        }
                    };
                    match decode_hub_frame(frame).map_err(AgentServiceError::Carrier)? {
                        HubToAgent::HeartbeatAck(ack) => {
                            verify_hub_heartbeat_ack(&hello, &challenge, &ack, &self.material.trusted_hub)
                                .map_err(AgentServiceError::Protocol)?;
                            let Some((expected, _)) = pending_heartbeat else {
                                return Err(AgentServiceError::HeartbeatAckMismatch);
                            };
                            if ack.device_generation != session.generation || ack.sequence != expected {
                                return Err(AgentServiceError::HeartbeatAckMismatch);
                            }
                            pending_heartbeat = None;
                        }
                        HubToAgent::Command(remote) => {
                            verify_remote_command(&hello, &challenge, &remote, &self.material.trusted_hub)
                                .map_err(AgentServiceError::Protocol)?;
                            crate::v2_m0::validate_command_session(&remote.command, &session)
                                .map_err(AgentServiceError::Control)?;
                            self.grants.authorize_once(
                                &remote.grant,
                                &session.device_id,
                                remote.command.required_class(),
                                trusted_clock.now_ms(),
                            ).map_err(AgentServiceError::Control)?;
                            if active.is_some() {
                                return Err(AgentServiceError::AgentBusy);
                            }
                            let request = match &remote.command.command {
                                DeviceCommand::ExecuteProcess { request } => request.clone(),
                                _ => return Err(AgentServiceError::UnsupportedCommand),
                            };
                            let operation = OperationRef {
                                device_id: session.device_id.clone(),
                                device_generation: session.generation,
                                operation_id: remote.command.operation_id.clone(),
                            };
                            self.execution.begin(operation).map_err(AgentServiceError::Execution)?;
                            let cancellation = ProcessCancellation::default();
                            let worker_cancel = cancellation.clone();
                            let executor = self.executor.clone();
                            let operation_id = remote.command.operation_id.clone();
                            let worker_operation_id = operation_id.clone();
                            let done = process_done_tx.clone();
                            tokio::spawn(async move {
                                let output = tokio::task::spawn_blocking(move || {
                                    executor.execute(&request, &worker_cancel)
                                }).await
                                    .map_err(|_| ProcessError::ReaderPanicked)
                                    .and_then(|result| result);
                                let _ = done.send(ProcessCompletion {
                                    operation_id: worker_operation_id,
                                    output,
                                }).await;
                            });
                            active = Some(ActiveProcess { operation_id, cancellation });
                        }
                        HubToAgent::Cancel(cancel) => {
                            verify_remote_cancel(&hello, &challenge, &cancel, &self.material.trusted_hub)
                                .map_err(AgentServiceError::Protocol)?;
                            if cancel.device_generation != session.generation {
                                return Err(AgentServiceError::CancellationMismatch);
                            }
                            let disposition = match active.as_ref() {
                                Some(process) if process.operation_id == cancel.operation_id => {
                                    process.cancellation.cancel();
                                    self.execution.request_cancel(&cancel.operation_id)
                                        .map_err(AgentServiceError::Execution)?;
                                    CancellationDisposition::CancellationRequested
                                }
                                Some(_) => return Err(AgentServiceError::CancellationMismatch),
                                None => return Err(AgentServiceError::CancellationMismatch),
                            };
                            let ack = build_remote_cancellation_ack(
                                &self.material.device_identity,
                                &hello,
                                &challenge,
                                &cancel,
                                disposition,
                            ).map_err(AgentServiceError::Protocol)?;
                            send_agent(&outbound_tx, AgentToHub::CancellationAck(ack)).await?;
                        }
                        other => return Err(AgentServiceError::UnexpectedMessage(format!("{other:?}"))),
                    }
                }
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SessionExit {
    Reconnect,
    Shutdown,
}

#[derive(Debug)]
struct ActiveProcess {
    operation_id: String,
    cancellation: ProcessCancellation,
}

#[derive(Debug)]
struct ProcessCompletion {
    operation_id: String,
    output: Result<crate::v2_m0::ProcessOutput, ProcessError>,
}

async fn terminate_active(
    active: &mut Option<ActiveProcess>,
    process_done_rx: &mut mpsc::Receiver<ProcessCompletion>,
    execution: &mut AgentExecutionGate,
) -> Result<(), AgentServiceError> {
    let Some(process) = active.take() else {
        return Ok(());
    };
    process.cancellation.cancel();
    let completion = tokio::time::timeout(Duration::from_secs(5), process_done_rx.recv())
        .await
        .map_err(|_| AgentServiceError::ProcessTerminationTimeout)?
        .ok_or(AgentServiceError::ProcessWorkerClosed)?;
    if completion.operation_id != process.operation_id {
        return Err(AgentServiceError::ProcessStateMismatch);
    }
    // The direct child was killed and waited by ProcessExecutor. Keep the ID terminal
    // across reconnect so an ambiguous transport outcome cannot cause replay.
    execution
        .abandon_on_disconnect(&process.operation_id)
        .map_err(AgentServiceError::Execution)?;
    Ok(())
}

async fn send_agent(
    sender: &mpsc::Sender<crate::v2_m1_grpc::proto::AgentFrame>,
    message: AgentToHub,
) -> Result<(), AgentServiceError> {
    sender
        .send(encode_agent_frame(&message).map_err(AgentServiceError::Carrier)?)
        .await
        .map_err(|_| AgentServiceError::OutboundClosed)
}

async fn next_hub(
    inbound: &mut tonic::Streaming<crate::v2_m1_grpc::proto::HubFrame>,
) -> Result<HubToAgent, AgentServiceError> {
    let frame = inbound
        .message()
        .await
        .map_err(AgentServiceError::Status)?
        .ok_or(AgentServiceError::InboundClosed)?;
    decode_hub_frame(frame).map_err(AgentServiceError::Carrier)
}

async fn sleep_or_shutdown(duration: Duration, shutdown: &mut watch::Receiver<bool>) -> bool {
    tokio::select! {
        _ = tokio::time::sleep(duration) => false,
        result = shutdown.changed() => result.is_err() || *shutdown.borrow(),
    }
}

fn der_certificate_to_pem(der: &[u8]) -> Vec<u8> {
    let encoded = STANDARD.encode(der);
    let mut pem = String::from("-----BEGIN CERTIFICATE-----\n");
    for chunk in encoded.as_bytes().chunks(64) {
        pem.push_str(std::str::from_utf8(chunk).expect("base64 is ASCII"));
        pem.push('\n');
    }
    pem.push_str("-----END CERTIFICATE-----\n");
    pem.into_bytes()
}

#[derive(Debug)]
pub enum AgentServiceError {
    InvalidConfig(&'static str),
    Transport(tonic::transport::Error),
    Status(tonic::Status),
    Carrier(crate::v2_m1_grpc::GrpcCarrierError),
    Protocol(crate::v2_m0_transport::TransportError),
    Control(crate::v2_m0::ControlError),
    Execution(crate::v2_m0_execution::ExecutionError),
    Process(ProcessError),
    UnexpectedMessage(String),
    HeartbeatAckMismatch,
    CancellationMismatch,
    AgentBusy,
    UnsupportedCommand,
    InboundClosed,
    OutboundClosed,
    ProcessWorkerClosed,
    ProcessStateMismatch,
    ProcessTerminationTimeout,
    ReconnectExhausted,
}

impl AgentServiceError {
    fn reconnectable(&self) -> bool {
        match self {
            Self::Transport(_) | Self::InboundClosed | Self::OutboundClosed => true,
            Self::Status(status) => matches!(
                status.code(),
                Code::Unavailable | Code::Cancelled | Code::DeadlineExceeded | Code::Unknown
            ),
            _ => false,
        }
    }
}

impl fmt::Display for AgentServiceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self:?}")
    }
}

impl std::error::Error for AgentServiceError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_requires_https_and_bounded_liveness_settings() {
        let config = AgentServiceConfig {
            hub_endpoint: "http://localhost:7443".into(),
            hub_domain: "localhost".into(),
            device_id: "dev-a".into(),
            allowed_cwd_roots: vec![std::env::current_dir().unwrap()],
            heartbeat_interval: Duration::from_secs(5),
            reconnect: ReconnectPolicy {
                initial_delay: Duration::from_millis(10),
                max_delay: Duration::from_secs(1),
                max_attempts: 3,
            },
        };
        assert!(matches!(
            config.validate(),
            Err(AgentServiceError::InvalidConfig(_))
        ));
    }

    #[test]
    fn der_to_pem_is_bounded_ascii_certificate_encoding() {
        let pem = String::from_utf8(der_certificate_to_pem(&[1, 2, 3, 4])).unwrap();
        assert!(pem.starts_with("-----BEGIN CERTIFICATE-----\n"));
        assert!(pem.contains("AQIDBA=="));
        assert!(pem.ends_with("-----END CERTIFICATE-----\n"));
    }
}
