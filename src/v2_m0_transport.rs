//! V2-M0 outbound Hub↔Agent authentication and bounded framing PoC.
//!
//! This module proves transport-facing identity semantics without making the
//! transport itself the product contract. The live PoC uses loopback TCP only;
//! production remote transport still requires confidentiality/integrity such as
//! authenticated TLS or an equivalently reviewed secure tunnel.

use crate::v2_m0::{
    CapabilityAdvertisement, CommandEnvelope, CommandResultEnvelope, ControlError, DeviceIdentity,
    DeviceRegistry, GrantToken,
};
use crate::v2_online_recovery::{RecoveryAuthorization, RecoveryChallenge, RecoveryResolved};
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use rand::{RngCore, rngs::OsRng};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::{
    fmt,
    io::{Read, Write},
    time::Instant,
};

pub const HUB_AGENT_SCHEMA_VERSION: u16 = 2;
pub const MAX_FRAME_BYTES: usize = 64 * 1024;

#[derive(Clone)]
pub struct HubIdentity {
    signing_key: SigningKey,
}

impl HubIdentity {
    pub fn generate() -> Self {
        Self {
            signing_key: SigningKey::generate(&mut OsRng),
        }
    }

    pub fn from_secret_key_bytes(secret_key: [u8; 32]) -> Self {
        Self {
            signing_key: SigningKey::from_bytes(&secret_key),
        }
    }

    pub(crate) fn secret_key_bytes(&self) -> [u8; 32] {
        self.signing_key.to_bytes()
    }

    pub fn verifier(&self) -> VerifyingKey {
        self.signing_key.verifying_key()
    }

    pub fn public_key(&self) -> [u8; 32] {
        self.signing_key.verifying_key().to_bytes()
    }

    pub fn sign_message(&self, message: &[u8]) -> Vec<u8> {
        self.signing_key.sign(message).to_bytes().to_vec()
    }

    pub fn challenge(&self, hello: &AgentHello) -> Result<HubChallenge, TransportError> {
        validate_schema(hello.schema_version)?;
        let mut hub_nonce = [0_u8; 32];
        OsRng.fill_bytes(&mut hub_nonce);
        let hub_public_key = self.public_key();
        let transcript = hub_challenge_bytes(hello, &hub_nonce, &hub_public_key)?;
        let signature = self.signing_key.sign(&transcript).to_bytes().to_vec();
        Ok(HubChallenge {
            schema_version: HUB_AGENT_SCHEMA_VERSION,
            agent_nonce: hello.agent_nonce,
            hub_nonce,
            hub_public_key,
            signature,
        })
    }

    pub fn accept_session(
        &self,
        hello: &AgentHello,
        challenge: &HubChallenge,
        device_generation: u64,
        capability_revision: u64,
        hub_time_ms: u64,
    ) -> Result<SessionAccepted, TransportError> {
        let mut accepted = SessionAccepted {
            schema_version: HUB_AGENT_SCHEMA_VERSION,
            device_id: hello.device_id.clone(),
            device_generation,
            capability_revision,
            hub_time_ms,
            signature: Vec::new(),
        };
        let transcript = session_accepted_bytes(hello, challenge, &accepted)?;
        accepted.signature = self.signing_key.sign(&transcript).to_bytes().to_vec();
        Ok(accepted)
    }

    pub fn remote_command(
        &self,
        hello: &AgentHello,
        challenge: &HubChallenge,
        command: CommandEnvelope,
        grant: GrantToken,
    ) -> Result<RemoteCommand, TransportError> {
        let mut remote = RemoteCommand {
            schema_version: HUB_AGENT_SCHEMA_VERSION,
            command,
            grant,
            signature: Vec::new(),
        };
        let transcript = remote_command_bytes(hello, challenge, &remote)?;
        remote.signature = self.signing_key.sign(&transcript).to_bytes().to_vec();
        Ok(remote)
    }

    pub fn remote_cancel(
        &self,
        hello: &AgentHello,
        challenge: &HubChallenge,
        device_id: String,
        device_generation: u64,
        operation_id: String,
    ) -> Result<RemoteCancel, TransportError> {
        let mut remote = RemoteCancel {
            schema_version: HUB_AGENT_SCHEMA_VERSION,
            device_id,
            device_generation,
            operation_id,
            signature: Vec::new(),
        };
        let transcript = remote_cancel_bytes(hello, challenge, &remote)?;
        remote.signature = self.signing_key.sign(&transcript).to_bytes().to_vec();
        Ok(remote)
    }

    pub fn backend_session_end(
        &self,
        hello: &AgentHello,
        challenge: &HubChallenge,
        device_id: String,
        device_generation: u64,
        context_id: String,
    ) -> Result<RemoteBackendSessionEnd, TransportError> {
        let mut remote = RemoteBackendSessionEnd {
            schema_version: HUB_AGENT_SCHEMA_VERSION,
            device_id,
            device_generation,
            context_id,
            signature: Vec::new(),
        };
        let transcript = remote_backend_session_end_bytes(hello, challenge, &remote)?;
        remote.signature = self.signing_key.sign(&transcript).to_bytes().to_vec();
        Ok(remote)
    }

    pub fn heartbeat_ack(
        &self,
        hello: &AgentHello,
        challenge: &HubChallenge,
        heartbeat: &AgentHeartbeat,
        hub_time_ms: u64,
    ) -> Result<HubHeartbeatAck, TransportError> {
        let mut ack = HubHeartbeatAck {
            schema_version: HUB_AGENT_SCHEMA_VERSION,
            device_id: heartbeat.device_id.clone(),
            device_generation: heartbeat.device_generation,
            sequence: heartbeat.sequence,
            hub_time_ms,
            signature: Vec::new(),
        };
        let transcript = hub_heartbeat_ack_bytes(hello, challenge, &ack)?;
        ack.signature = self.signing_key.sign(&transcript).to_bytes().to_vec();
        Ok(ack)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentHello {
    pub schema_version: u16,
    pub device_id: String,
    pub agent_nonce: [u8; 32],
    pub capabilities: CapabilityAdvertisement,
}

impl AgentHello {
    pub fn new(device_id: String, capabilities: CapabilityAdvertisement) -> Self {
        let mut agent_nonce = [0_u8; 32];
        OsRng.fill_bytes(&mut agent_nonce);
        Self {
            schema_version: HUB_AGENT_SCHEMA_VERSION,
            device_id,
            agent_nonce,
            capabilities,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HubChallenge {
    pub schema_version: u16,
    pub agent_nonce: [u8; 32],
    pub hub_nonce: [u8; 32],
    pub hub_public_key: [u8; 32],
    pub signature: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentProof {
    pub schema_version: u16,
    pub device_id: String,
    pub agent_nonce: [u8; 32],
    pub hub_nonce: [u8; 32],
    pub signature: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionAccepted {
    pub schema_version: u16,
    pub device_id: String,
    pub device_generation: u64,
    pub capability_revision: u64,
    pub hub_time_ms: u64,
    pub signature: Vec<u8>,
}

#[derive(Debug, Clone)]
pub struct TrustedSessionClock {
    hub_time_ms: u64,
    started: Instant,
}

impl TrustedSessionClock {
    pub fn new(hub_time_ms: u64) -> Self {
        Self {
            hub_time_ms,
            started: Instant::now(),
        }
    }

    pub fn now_ms(&self) -> u64 {
        let elapsed = u64::try_from(self.started.elapsed().as_millis()).unwrap_or(u64::MAX);
        self.hub_time_ms.saturating_add(elapsed)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoteCommand {
    pub schema_version: u16,
    pub command: CommandEnvelope,
    pub grant: GrantToken,
    pub signature: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoteResult {
    pub schema_version: u16,
    pub result: CommandResultEnvelope,
    pub signature: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoteCancel {
    pub schema_version: u16,
    pub device_id: String,
    pub device_generation: u64,
    pub operation_id: String,
    pub signature: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoteBackendSessionEnd {
    pub schema_version: u16,
    pub device_id: String,
    pub device_generation: u64,
    pub context_id: String,
    pub signature: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoteBackendSessionEnded {
    pub schema_version: u16,
    pub device_id: String,
    pub device_generation: u64,
    pub context_id: String,
    pub ended: bool,
    pub signature: Vec<u8>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CancellationDisposition {
    CancelledBeforeExecution,
    CancellationRequested,
    IndeterminateAfterPropagation,
    AlreadyTerminal,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoteCancellationAck {
    pub schema_version: u16,
    pub device_id: String,
    pub device_generation: u64,
    pub operation_id: String,
    pub disposition: CancellationDisposition,
    pub signature: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentHeartbeat {
    pub schema_version: u16,
    pub device_id: String,
    pub device_generation: u64,
    pub sequence: u64,
    pub signature: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HubHeartbeatAck {
    pub schema_version: u16,
    pub device_id: String,
    pub device_generation: u64,
    pub sequence: u64,
    pub hub_time_ms: u64,
    pub signature: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", content = "body", rename_all = "snake_case")]
pub enum AgentToHub {
    Hello(AgentHello),
    Proof(AgentProof),
    Result(RemoteResult),
    CancellationAck(RemoteCancellationAck),
    BackendSessionEnded(RemoteBackendSessionEnded),
    RecoveryAuthorization(RecoveryAuthorization),
    Heartbeat(AgentHeartbeat),
}

// The signed wire envelope intentionally keeps command payload ownership direct; boxing the
// largest variant would add representation churn without changing the bounded transport contract.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", content = "body", rename_all = "snake_case")]
pub enum HubToAgent {
    Challenge(HubChallenge),
    Accepted(SessionAccepted),
    Command(RemoteCommand),
    Cancel(RemoteCancel),
    BackendSessionEnd(RemoteBackendSessionEnd),
    RecoveryChallenge(RecoveryChallenge),
    RecoveryResolved(RecoveryResolved),
    HeartbeatAck(HubHeartbeatAck),
}

impl AgentToHub {
    pub const fn kind(&self) -> &'static str {
        match self {
            Self::Hello(_) => "hello",
            Self::Proof(_) => "proof",
            Self::Result(_) => "result",
            Self::CancellationAck(_) => "cancellation_ack",
            Self::BackendSessionEnded(_) => "backend_session_ended",
            Self::RecoveryAuthorization(_) => "recovery_authorization",
            Self::Heartbeat(_) => "heartbeat",
        }
    }
}

impl HubToAgent {
    pub const fn kind(&self) -> &'static str {
        match self {
            Self::Challenge(_) => "challenge",
            Self::Accepted(_) => "accepted",
            Self::Command(_) => "command",
            Self::Cancel(_) => "cancel",
            Self::BackendSessionEnd(_) => "backend_session_end",
            Self::RecoveryChallenge(_) => "recovery_challenge",
            Self::RecoveryResolved(_) => "recovery_resolved",
            Self::HeartbeatAck(_) => "heartbeat_ack",
        }
    }
}

pub fn verify_hub_challenge(
    hello: &AgentHello,
    challenge: &HubChallenge,
    trusted_hub: &VerifyingKey,
) -> Result<(), TransportError> {
    validate_schema(hello.schema_version)?;
    validate_schema(challenge.schema_version)?;
    if challenge.agent_nonce != hello.agent_nonce {
        return Err(TransportError::HandshakeMismatch);
    }
    if challenge.hub_public_key != trusted_hub.to_bytes() {
        return Err(TransportError::HubKeyMismatch);
    }
    let signature = signature_from_slice(&challenge.signature)
        .map_err(|_| TransportError::InvalidHubSignature)?;
    let transcript = hub_challenge_bytes(hello, &challenge.hub_nonce, &challenge.hub_public_key)?;
    trusted_hub
        .verify(&transcript, &signature)
        .map_err(|_| TransportError::InvalidHubSignature)
}

pub fn build_agent_proof(
    identity: &DeviceIdentity,
    hello: &AgentHello,
    challenge: &HubChallenge,
) -> Result<AgentProof, TransportError> {
    validate_schema(hello.schema_version)?;
    validate_schema(challenge.schema_version)?;
    if challenge.agent_nonce != hello.agent_nonce {
        return Err(TransportError::HandshakeMismatch);
    }
    let transcript = agent_proof_bytes(hello, challenge)?;
    Ok(AgentProof {
        schema_version: HUB_AGENT_SCHEMA_VERSION,
        device_id: hello.device_id.clone(),
        agent_nonce: hello.agent_nonce,
        hub_nonce: challenge.hub_nonce,
        signature: identity.sign_message(&transcript),
    })
}

pub fn verify_agent_proof(
    registry: &DeviceRegistry,
    hello: &AgentHello,
    challenge: &HubChallenge,
    proof: &AgentProof,
) -> Result<(), TransportError> {
    validate_schema(proof.schema_version)?;
    if proof.device_id != hello.device_id
        || proof.agent_nonce != hello.agent_nonce
        || proof.hub_nonce != challenge.hub_nonce
    {
        return Err(TransportError::HandshakeMismatch);
    }
    let transcript = agent_proof_bytes(hello, challenge)?;
    registry
        .verify_device_signature(&hello.device_id, &transcript, &proof.signature)
        .map_err(TransportError::Control)
}

pub fn verify_session_accepted(
    hello: &AgentHello,
    challenge: &HubChallenge,
    accepted: &SessionAccepted,
    trusted_hub: &VerifyingKey,
) -> Result<(), TransportError> {
    validate_schema(accepted.schema_version)?;
    if accepted.device_id != hello.device_id {
        return Err(TransportError::HandshakeMismatch);
    }
    let signature = signature_from_slice(&accepted.signature)
        .map_err(|_| TransportError::InvalidHubSignature)?;
    let transcript = session_accepted_bytes(hello, challenge, accepted)?;
    trusted_hub
        .verify(&transcript, &signature)
        .map_err(|_| TransportError::InvalidHubSignature)
}

pub fn verify_remote_command(
    hello: &AgentHello,
    challenge: &HubChallenge,
    remote: &RemoteCommand,
    trusted_hub: &VerifyingKey,
) -> Result<(), TransportError> {
    validate_schema(remote.schema_version)?;
    let signature =
        signature_from_slice(&remote.signature).map_err(|_| TransportError::InvalidHubSignature)?;
    let transcript = remote_command_bytes(hello, challenge, remote)?;
    trusted_hub
        .verify(&transcript, &signature)
        .map_err(|_| TransportError::InvalidHubSignature)
}

pub fn verify_remote_cancel(
    hello: &AgentHello,
    challenge: &HubChallenge,
    remote: &RemoteCancel,
    trusted_hub: &VerifyingKey,
) -> Result<(), TransportError> {
    validate_schema(remote.schema_version)?;
    if remote.device_id != hello.device_id {
        return Err(TransportError::HandshakeMismatch);
    }
    let signature =
        signature_from_slice(&remote.signature).map_err(|_| TransportError::InvalidHubSignature)?;
    let transcript = remote_cancel_bytes(hello, challenge, remote)?;
    trusted_hub
        .verify(&transcript, &signature)
        .map_err(|_| TransportError::InvalidHubSignature)
}

pub fn verify_remote_backend_session_end(
    hello: &AgentHello,
    challenge: &HubChallenge,
    remote: &RemoteBackendSessionEnd,
    trusted_hub: &VerifyingKey,
) -> Result<(), TransportError> {
    validate_schema(remote.schema_version)?;
    if remote.device_id != hello.device_id {
        return Err(TransportError::HandshakeMismatch);
    }
    let signature =
        signature_from_slice(&remote.signature).map_err(|_| TransportError::InvalidHubSignature)?;
    let transcript = remote_backend_session_end_bytes(hello, challenge, remote)?;
    trusted_hub
        .verify(&transcript, &signature)
        .map_err(|_| TransportError::InvalidHubSignature)
}

pub fn build_remote_backend_session_ended(
    identity: &DeviceIdentity,
    hello: &AgentHello,
    challenge: &HubChallenge,
    request: &RemoteBackendSessionEnd,
    ended: bool,
) -> Result<RemoteBackendSessionEnded, TransportError> {
    let mut ack = RemoteBackendSessionEnded {
        schema_version: HUB_AGENT_SCHEMA_VERSION,
        device_id: request.device_id.clone(),
        device_generation: request.device_generation,
        context_id: request.context_id.clone(),
        ended,
        signature: Vec::new(),
    };
    let transcript = remote_backend_session_ended_bytes(hello, challenge, &ack)?;
    ack.signature = identity.sign_message(&transcript);
    Ok(ack)
}

pub fn verify_remote_backend_session_ended(
    registry: &DeviceRegistry,
    hello: &AgentHello,
    challenge: &HubChallenge,
    ack: &RemoteBackendSessionEnded,
) -> Result<(), TransportError> {
    validate_schema(ack.schema_version)?;
    if ack.device_id != hello.device_id {
        return Err(TransportError::HandshakeMismatch);
    }
    let transcript = remote_backend_session_ended_bytes(hello, challenge, ack)?;
    registry
        .verify_device_signature(&hello.device_id, &transcript, &ack.signature)
        .map_err(TransportError::Control)
}

pub fn build_remote_cancellation_ack(
    identity: &DeviceIdentity,
    hello: &AgentHello,
    challenge: &HubChallenge,
    cancel: &RemoteCancel,
    disposition: CancellationDisposition,
) -> Result<RemoteCancellationAck, TransportError> {
    let mut ack = RemoteCancellationAck {
        schema_version: HUB_AGENT_SCHEMA_VERSION,
        device_id: cancel.device_id.clone(),
        device_generation: cancel.device_generation,
        operation_id: cancel.operation_id.clone(),
        disposition,
        signature: Vec::new(),
    };
    let transcript = remote_cancellation_ack_bytes(hello, challenge, &ack)?;
    ack.signature = identity.sign_message(&transcript);
    Ok(ack)
}

pub fn verify_remote_cancellation_ack(
    registry: &DeviceRegistry,
    hello: &AgentHello,
    challenge: &HubChallenge,
    ack: &RemoteCancellationAck,
) -> Result<(), TransportError> {
    validate_schema(ack.schema_version)?;
    if ack.device_id != hello.device_id {
        return Err(TransportError::HandshakeMismatch);
    }
    let transcript = remote_cancellation_ack_bytes(hello, challenge, ack)?;
    registry
        .verify_device_signature(&hello.device_id, &transcript, &ack.signature)
        .map_err(TransportError::Control)
}

pub fn build_agent_heartbeat(
    identity: &DeviceIdentity,
    hello: &AgentHello,
    challenge: &HubChallenge,
    device_generation: u64,
    sequence: u64,
) -> Result<AgentHeartbeat, TransportError> {
    let mut heartbeat = AgentHeartbeat {
        schema_version: HUB_AGENT_SCHEMA_VERSION,
        device_id: hello.device_id.clone(),
        device_generation,
        sequence,
        signature: Vec::new(),
    };
    let transcript = agent_heartbeat_bytes(hello, challenge, &heartbeat)?;
    heartbeat.signature = identity.sign_message(&transcript);
    Ok(heartbeat)
}

pub fn verify_agent_heartbeat(
    registry: &DeviceRegistry,
    hello: &AgentHello,
    challenge: &HubChallenge,
    heartbeat: &AgentHeartbeat,
) -> Result<(), TransportError> {
    validate_schema(heartbeat.schema_version)?;
    if heartbeat.device_id != hello.device_id {
        return Err(TransportError::HandshakeMismatch);
    }
    let transcript = agent_heartbeat_bytes(hello, challenge, heartbeat)?;
    registry
        .verify_device_signature(&hello.device_id, &transcript, &heartbeat.signature)
        .map_err(TransportError::Control)
}

pub fn verify_hub_heartbeat_ack(
    hello: &AgentHello,
    challenge: &HubChallenge,
    ack: &HubHeartbeatAck,
    trusted_hub: &VerifyingKey,
) -> Result<(), TransportError> {
    validate_schema(ack.schema_version)?;
    if ack.device_id != hello.device_id {
        return Err(TransportError::HandshakeMismatch);
    }
    let signature =
        signature_from_slice(&ack.signature).map_err(|_| TransportError::InvalidHubSignature)?;
    let transcript = hub_heartbeat_ack_bytes(hello, challenge, ack)?;
    trusted_hub
        .verify(&transcript, &signature)
        .map_err(|_| TransportError::InvalidHubSignature)
}

pub fn build_remote_result(
    identity: &DeviceIdentity,
    hello: &AgentHello,
    challenge: &HubChallenge,
    result: CommandResultEnvelope,
) -> Result<RemoteResult, TransportError> {
    let mut remote = RemoteResult {
        schema_version: HUB_AGENT_SCHEMA_VERSION,
        result,
        signature: Vec::new(),
    };
    let transcript = remote_result_bytes(hello, challenge, &remote)?;
    remote.signature = identity.sign_message(&transcript);
    Ok(remote)
}

pub fn verify_remote_result(
    registry: &DeviceRegistry,
    hello: &AgentHello,
    challenge: &HubChallenge,
    remote: &RemoteResult,
) -> Result<(), TransportError> {
    validate_schema(remote.schema_version)?;
    let transcript = remote_result_bytes(hello, challenge, remote)?;
    registry
        .verify_device_signature(&hello.device_id, &transcript, &remote.signature)
        .map_err(TransportError::Control)
}

pub fn write_frame<W: Write, T: Serialize>(
    writer: &mut W,
    value: &T,
) -> Result<(), TransportError> {
    let payload = serde_json::to_vec(value).map_err(TransportError::Serialization)?;
    if payload.len() > MAX_FRAME_BYTES {
        return Err(TransportError::FrameTooLarge(payload.len()));
    }
    let len =
        u32::try_from(payload.len()).map_err(|_| TransportError::FrameTooLarge(payload.len()))?;
    writer
        .write_all(&len.to_be_bytes())
        .map_err(TransportError::Io)?;
    writer.write_all(&payload).map_err(TransportError::Io)?;
    writer.flush().map_err(TransportError::Io)
}

pub fn read_frame<R: Read, T: DeserializeOwned>(reader: &mut R) -> Result<T, TransportError> {
    let mut len_bytes = [0_u8; 4];
    reader
        .read_exact(&mut len_bytes)
        .map_err(TransportError::Io)?;
    let len = u32::from_be_bytes(len_bytes) as usize;
    if len > MAX_FRAME_BYTES {
        return Err(TransportError::FrameTooLarge(len));
    }
    let mut payload = vec![0_u8; len];
    reader
        .read_exact(&mut payload)
        .map_err(TransportError::Io)?;
    serde_json::from_slice(&payload).map_err(TransportError::Serialization)
}

fn validate_schema(got: u16) -> Result<(), TransportError> {
    if got == HUB_AGENT_SCHEMA_VERSION {
        Ok(())
    } else {
        Err(TransportError::UnsupportedSchema { got })
    }
}

fn signature_from_slice(bytes: &[u8]) -> Result<Signature, ()> {
    let sig_bytes: [u8; 64] = bytes.try_into().map_err(|_| ())?;
    Ok(Signature::from_bytes(&sig_bytes))
}

fn hub_challenge_bytes(
    hello: &AgentHello,
    hub_nonce: &[u8; 32],
    hub_public_key: &[u8; 32],
) -> Result<Vec<u8>, TransportError> {
    #[derive(Serialize)]
    struct Transcript<'a> {
        domain: &'static str,
        schema_version: u16,
        hello: &'a AgentHello,
        hub_nonce: &'a [u8; 32],
        hub_public_key: &'a [u8; 32],
    }
    serde_json::to_vec(&Transcript {
        domain: "cumg-v2-m0-hub-challenge",
        schema_version: HUB_AGENT_SCHEMA_VERSION,
        hello,
        hub_nonce,
        hub_public_key,
    })
    .map_err(TransportError::Serialization)
}

fn agent_proof_bytes(
    hello: &AgentHello,
    challenge: &HubChallenge,
) -> Result<Vec<u8>, TransportError> {
    #[derive(Serialize)]
    struct Transcript<'a> {
        domain: &'static str,
        schema_version: u16,
        hello: &'a AgentHello,
        hub_nonce: &'a [u8; 32],
        hub_public_key: &'a [u8; 32],
    }
    serde_json::to_vec(&Transcript {
        domain: "cumg-v2-m0-agent-proof",
        schema_version: HUB_AGENT_SCHEMA_VERSION,
        hello,
        hub_nonce: &challenge.hub_nonce,
        hub_public_key: &challenge.hub_public_key,
    })
    .map_err(TransportError::Serialization)
}

fn session_accepted_bytes(
    hello: &AgentHello,
    challenge: &HubChallenge,
    accepted: &SessionAccepted,
) -> Result<Vec<u8>, TransportError> {
    #[derive(Serialize)]
    struct Transcript<'a> {
        domain: &'static str,
        schema_version: u16,
        hello: &'a AgentHello,
        hub_nonce: &'a [u8; 32],
        device_generation: u64,
        capability_revision: u64,
        hub_time_ms: u64,
    }
    serde_json::to_vec(&Transcript {
        domain: "cumg-v2-m0-session-accepted",
        schema_version: HUB_AGENT_SCHEMA_VERSION,
        hello,
        hub_nonce: &challenge.hub_nonce,
        device_generation: accepted.device_generation,
        capability_revision: accepted.capability_revision,
        hub_time_ms: accepted.hub_time_ms,
    })
    .map_err(TransportError::Serialization)
}

fn remote_command_bytes(
    hello: &AgentHello,
    challenge: &HubChallenge,
    remote: &RemoteCommand,
) -> Result<Vec<u8>, TransportError> {
    #[derive(Serialize)]
    struct Transcript<'a> {
        domain: &'static str,
        schema_version: u16,
        device_id: &'a str,
        agent_nonce: &'a [u8; 32],
        hub_nonce: &'a [u8; 32],
        command: &'a CommandEnvelope,
        grant: &'a GrantToken,
    }
    serde_json::to_vec(&Transcript {
        domain: "cumg-v2-m0-remote-command",
        schema_version: HUB_AGENT_SCHEMA_VERSION,
        device_id: &hello.device_id,
        agent_nonce: &hello.agent_nonce,
        hub_nonce: &challenge.hub_nonce,
        command: &remote.command,
        grant: &remote.grant,
    })
    .map_err(TransportError::Serialization)
}

fn agent_heartbeat_bytes(
    hello: &AgentHello,
    challenge: &HubChallenge,
    heartbeat: &AgentHeartbeat,
) -> Result<Vec<u8>, TransportError> {
    #[derive(Serialize)]
    struct Transcript<'a> {
        domain: &'static str,
        schema_version: u16,
        device_id: &'a str,
        agent_nonce: &'a [u8; 32],
        hub_nonce: &'a [u8; 32],
        device_generation: u64,
        sequence: u64,
    }
    serde_json::to_vec(&Transcript {
        domain: "cumg-v2-m1-agent-heartbeat",
        schema_version: HUB_AGENT_SCHEMA_VERSION,
        device_id: &heartbeat.device_id,
        agent_nonce: &hello.agent_nonce,
        hub_nonce: &challenge.hub_nonce,
        device_generation: heartbeat.device_generation,
        sequence: heartbeat.sequence,
    })
    .map_err(TransportError::Serialization)
}

fn hub_heartbeat_ack_bytes(
    hello: &AgentHello,
    challenge: &HubChallenge,
    ack: &HubHeartbeatAck,
) -> Result<Vec<u8>, TransportError> {
    #[derive(Serialize)]
    struct Transcript<'a> {
        domain: &'static str,
        schema_version: u16,
        device_id: &'a str,
        agent_nonce: &'a [u8; 32],
        hub_nonce: &'a [u8; 32],
        device_generation: u64,
        sequence: u64,
        hub_time_ms: u64,
    }
    serde_json::to_vec(&Transcript {
        domain: "cumg-v2-m1-hub-heartbeat-ack",
        schema_version: HUB_AGENT_SCHEMA_VERSION,
        device_id: &ack.device_id,
        agent_nonce: &hello.agent_nonce,
        hub_nonce: &challenge.hub_nonce,
        device_generation: ack.device_generation,
        sequence: ack.sequence,
        hub_time_ms: ack.hub_time_ms,
    })
    .map_err(TransportError::Serialization)
}

fn remote_backend_session_end_bytes(
    hello: &AgentHello,
    challenge: &HubChallenge,
    remote: &RemoteBackendSessionEnd,
) -> Result<Vec<u8>, TransportError> {
    #[derive(Serialize)]
    struct Transcript<'a> {
        domain: &'static str,
        schema_version: u16,
        device_id: &'a str,
        agent_nonce: &'a [u8; 32],
        hub_nonce: &'a [u8; 32],
        device_generation: u64,
        context_id: &'a str,
    }
    serde_json::to_vec(&Transcript {
        domain: "cumg-v2-m1-backend-session-end",
        schema_version: HUB_AGENT_SCHEMA_VERSION,
        device_id: &remote.device_id,
        agent_nonce: &hello.agent_nonce,
        hub_nonce: &challenge.hub_nonce,
        device_generation: remote.device_generation,
        context_id: &remote.context_id,
    })
    .map_err(TransportError::Serialization)
}

fn remote_backend_session_ended_bytes(
    hello: &AgentHello,
    challenge: &HubChallenge,
    ack: &RemoteBackendSessionEnded,
) -> Result<Vec<u8>, TransportError> {
    #[derive(Serialize)]
    struct Transcript<'a> {
        domain: &'static str,
        schema_version: u16,
        device_id: &'a str,
        agent_nonce: &'a [u8; 32],
        hub_nonce: &'a [u8; 32],
        device_generation: u64,
        context_id: &'a str,
        ended: bool,
    }
    serde_json::to_vec(&Transcript {
        domain: "cumg-v2-m1-backend-session-ended",
        schema_version: HUB_AGENT_SCHEMA_VERSION,
        device_id: &ack.device_id,
        agent_nonce: &hello.agent_nonce,
        hub_nonce: &challenge.hub_nonce,
        device_generation: ack.device_generation,
        context_id: &ack.context_id,
        ended: ack.ended,
    })
    .map_err(TransportError::Serialization)
}

fn remote_cancel_bytes(
    hello: &AgentHello,
    challenge: &HubChallenge,
    remote: &RemoteCancel,
) -> Result<Vec<u8>, TransportError> {
    #[derive(Serialize)]
    struct Transcript<'a> {
        domain: &'static str,
        schema_version: u16,
        device_id: &'a str,
        agent_nonce: &'a [u8; 32],
        hub_nonce: &'a [u8; 32],
        device_generation: u64,
        operation_id: &'a str,
    }
    serde_json::to_vec(&Transcript {
        domain: "cumg-v2-m0-remote-cancel",
        schema_version: HUB_AGENT_SCHEMA_VERSION,
        device_id: &remote.device_id,
        agent_nonce: &hello.agent_nonce,
        hub_nonce: &challenge.hub_nonce,
        device_generation: remote.device_generation,
        operation_id: &remote.operation_id,
    })
    .map_err(TransportError::Serialization)
}

fn remote_cancellation_ack_bytes(
    hello: &AgentHello,
    challenge: &HubChallenge,
    ack: &RemoteCancellationAck,
) -> Result<Vec<u8>, TransportError> {
    #[derive(Serialize)]
    struct Transcript<'a> {
        domain: &'static str,
        schema_version: u16,
        device_id: &'a str,
        agent_nonce: &'a [u8; 32],
        hub_nonce: &'a [u8; 32],
        device_generation: u64,
        operation_id: &'a str,
        disposition: CancellationDisposition,
    }
    serde_json::to_vec(&Transcript {
        domain: "cumg-v2-m0-remote-cancellation-ack",
        schema_version: HUB_AGENT_SCHEMA_VERSION,
        device_id: &ack.device_id,
        agent_nonce: &hello.agent_nonce,
        hub_nonce: &challenge.hub_nonce,
        device_generation: ack.device_generation,
        operation_id: &ack.operation_id,
        disposition: ack.disposition,
    })
    .map_err(TransportError::Serialization)
}

fn remote_result_bytes(
    hello: &AgentHello,
    challenge: &HubChallenge,
    remote: &RemoteResult,
) -> Result<Vec<u8>, TransportError> {
    #[derive(Serialize)]
    struct Transcript<'a> {
        domain: &'static str,
        schema_version: u16,
        device_id: &'a str,
        agent_nonce: &'a [u8; 32],
        hub_nonce: &'a [u8; 32],
        result: &'a CommandResultEnvelope,
    }
    serde_json::to_vec(&Transcript {
        domain: "cumg-v2-m0-remote-result",
        schema_version: HUB_AGENT_SCHEMA_VERSION,
        device_id: &hello.device_id,
        agent_nonce: &hello.agent_nonce,
        hub_nonce: &challenge.hub_nonce,
        result: &remote.result,
    })
    .map_err(TransportError::Serialization)
}

#[derive(Debug)]
pub enum TransportError {
    Io(std::io::Error),
    Serialization(serde_json::Error),
    FrameTooLarge(usize),
    UnsupportedSchema { got: u16 },
    HubKeyMismatch,
    InvalidHubSignature,
    HandshakeMismatch,
    Control(ControlError),
}

impl fmt::Display for TransportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(f, "I/O error: {error}"),
            Self::Serialization(error) => write!(f, "serialization error: {error}"),
            Self::FrameTooLarge(size) => {
                write!(f, "wire frame {size} bytes exceeds {MAX_FRAME_BYTES}")
            }
            Self::UnsupportedSchema { got } => write!(f, "unsupported Hub-Agent schema {got}"),
            Self::HubKeyMismatch => write!(f, "Hub public key does not match the pinned identity"),
            Self::InvalidHubSignature => write!(f, "Hub challenge signature is invalid"),
            Self::HandshakeMismatch => write!(f, "Hub-Agent handshake transcript mismatch"),
            Self::Control(error) => write!(f, "control-plane rejection: {error}"),
        }
    }
}

impl std::error::Error for TransportError {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::v2_m0::{CAPABILITY_SCHEMA_VERSION, DeviceCapability};
    use std::io::Cursor;

    fn caps() -> CapabilityAdvertisement {
        CapabilityAdvertisement {
            backend: "test".into(),
            backend_version: "1".into(),
            platform: "test-platform".into(),
            capability_schema_version: CAPABILITY_SCHEMA_VERSION,
            revision: 1,
            supported: vec![DeviceCapability::ListApplications],
        }
    }

    fn enrolled() -> (DeviceRegistry, DeviceIdentity, String) {
        let identity = DeviceIdentity::generate();
        let challenge = DeviceRegistry::enrollment_challenge();
        let proof = identity.enrollment_proof(&challenge);
        let mut registry = DeviceRegistry::default();
        let device_id = registry
            .enroll(&identity.public_key(), &challenge, &proof)
            .unwrap();
        (registry, identity, device_id)
    }

    #[test]
    fn mutually_authenticated_handshake_accepts_an_enrolled_device() {
        let (registry, identity, device_id) = enrolled();
        let hub = HubIdentity::generate();
        let hello = AgentHello::new(device_id, caps());
        let challenge = hub.challenge(&hello).unwrap();
        verify_hub_challenge(&hello, &challenge, &hub.verifier()).unwrap();
        let proof = build_agent_proof(&identity, &hello, &challenge).unwrap();
        verify_agent_proof(&registry, &hello, &challenge, &proof).unwrap();
    }

    #[test]
    fn agent_rejects_an_unpinned_hub_identity() {
        let (_registry, _identity, device_id) = enrolled();
        let trusted = HubIdentity::generate();
        let attacker = HubIdentity::generate();
        let hello = AgentHello::new(device_id, caps());
        let challenge = attacker.challenge(&hello).unwrap();
        assert!(matches!(
            verify_hub_challenge(&hello, &challenge, &trusted.verifier()),
            Err(TransportError::HubKeyMismatch)
        ));
    }

    #[test]
    fn hub_rejects_a_forged_agent_proof() {
        let (registry, _identity, device_id) = enrolled();
        let attacker = DeviceIdentity::generate();
        let hub = HubIdentity::generate();
        let hello = AgentHello::new(device_id, caps());
        let challenge = hub.challenge(&hello).unwrap();
        let forged = build_agent_proof(&attacker, &hello, &challenge).unwrap();
        assert!(matches!(
            verify_agent_proof(&registry, &hello, &challenge, &forged),
            Err(TransportError::Control(
                ControlError::InvalidDeviceSignature
            ))
        ));
    }

    #[test]
    fn proof_replay_fails_against_a_fresh_hub_nonce() {
        let (registry, identity, device_id) = enrolled();
        let hub = HubIdentity::generate();
        let hello = AgentHello::new(device_id, caps());
        let first = hub.challenge(&hello).unwrap();
        let replayed = build_agent_proof(&identity, &hello, &first).unwrap();
        let second = hub.challenge(&hello).unwrap();
        assert!(matches!(
            verify_agent_proof(&registry, &hello, &second, &replayed),
            Err(TransportError::HandshakeMismatch)
        ));
    }

    #[test]
    fn session_acceptance_binds_hub_time_for_grant_expiry_clocking() {
        let (_registry, _identity, device_id) = enrolled();
        let hub = HubIdentity::generate();
        let hello = AgentHello::new(device_id, caps());
        let challenge = hub.challenge(&hello).unwrap();
        let accepted = hub
            .accept_session(&hello, &challenge, 1, 1, 50_000)
            .unwrap();
        verify_session_accepted(&hello, &challenge, &accepted, &hub.verifier()).unwrap();
        let clock = TrustedSessionClock::new(accepted.hub_time_ms);
        assert!(clock.now_ms() >= 50_000);

        let mut tampered = accepted;
        tampered.hub_time_ms = 1;
        assert!(matches!(
            verify_session_accepted(&hello, &challenge, &tampered, &hub.verifier()),
            Err(TransportError::InvalidHubSignature)
        ));
    }

    #[test]
    fn signed_command_is_bound_to_the_connection_and_payload() {
        use crate::v2_m0::{
            CONTROL_SCHEMA_VERSION, CapabilityClass, CommandEnvelope, DeviceCommand, GrantAuthority,
        };
        let (_registry, _identity, device_id) = enrolled();
        let hub = HubIdentity::generate();
        let hello = AgentHello::new(device_id.clone(), caps());
        let challenge = hub.challenge(&hello).unwrap();
        let authority = GrantAuthority::generate();
        let grant = authority
            .issue(&device_id, CapabilityClass::Observe, 1_000, 30_000)
            .unwrap();
        let command = CommandEnvelope {
            schema_version: CONTROL_SCHEMA_VERSION,
            device_id,
            device_generation: 1,
            capability_revision: 1,
            operation_id: "op-1".into(),
            command: DeviceCommand::ListApplications,
        };
        let remote = hub
            .remote_command(&hello, &challenge, command, grant)
            .unwrap();
        verify_remote_command(&hello, &challenge, &remote, &hub.verifier()).unwrap();

        let mut tampered = remote.clone();
        tampered.command.operation_id = "op-tampered".into();
        assert!(matches!(
            verify_remote_command(&hello, &challenge, &tampered, &hub.verifier()),
            Err(TransportError::InvalidHubSignature)
        ));
    }

    #[test]
    fn signed_result_rejects_payload_tampering() {
        use crate::v2_m0::{CONTROL_SCHEMA_VERSION, CommandResultEnvelope, DeviceResult};
        let (registry, identity, device_id) = enrolled();
        let hub = HubIdentity::generate();
        let hello = AgentHello::new(device_id.clone(), caps());
        let challenge = hub.challenge(&hello).unwrap();
        let result = CommandResultEnvelope {
            schema_version: CONTROL_SCHEMA_VERSION,
            device_id,
            device_generation: 1,
            capability_revision: 1,
            operation_id: "op-1".into(),
            result: DeviceResult::Applications { count: 7 },
        };
        let remote = build_remote_result(&identity, &hello, &challenge, result).unwrap();
        verify_remote_result(&registry, &hello, &challenge, &remote).unwrap();

        let mut tampered = remote.clone();
        tampered.result.result = DeviceResult::Applications { count: 8 };
        assert!(matches!(
            verify_remote_result(&registry, &hello, &challenge, &tampered),
            Err(TransportError::Control(
                ControlError::InvalidDeviceSignature
            ))
        ));
    }

    #[test]
    fn heartbeat_and_ack_are_signed_and_connection_bound() {
        let (registry, identity, device_id) = enrolled();
        let hub = HubIdentity::generate();
        let hello = AgentHello::new(device_id, caps());
        let challenge = hub.challenge(&hello).unwrap();
        let heartbeat = build_agent_heartbeat(&identity, &hello, &challenge, 4, 9).unwrap();
        verify_agent_heartbeat(&registry, &hello, &challenge, &heartbeat).unwrap();
        let ack = hub
            .heartbeat_ack(&hello, &challenge, &heartbeat, 50_000)
            .unwrap();
        verify_hub_heartbeat_ack(&hello, &challenge, &ack, &hub.verifier()).unwrap();

        let mut tampered = heartbeat;
        tampered.sequence += 1;
        assert!(matches!(
            verify_agent_heartbeat(&registry, &hello, &challenge, &tampered),
            Err(TransportError::Control(
                ControlError::InvalidDeviceSignature
            ))
        ));
    }

    #[test]
    fn cancellation_and_ack_are_signed_and_connection_bound() {
        let (registry, identity, device_id) = enrolled();
        let hub = HubIdentity::generate();
        let hello = AgentHello::new(device_id.clone(), caps());
        let challenge = hub.challenge(&hello).unwrap();
        let cancel = hub
            .remote_cancel(&hello, &challenge, device_id, 3, "op-cancel".into())
            .unwrap();
        verify_remote_cancel(&hello, &challenge, &cancel, &hub.verifier()).unwrap();
        let ack = build_remote_cancellation_ack(
            &identity,
            &hello,
            &challenge,
            &cancel,
            CancellationDisposition::CancellationRequested,
        )
        .unwrap();
        verify_remote_cancellation_ack(&registry, &hello, &challenge, &ack).unwrap();

        let mut tampered = cancel.clone();
        tampered.operation_id = "op-other".into();
        assert!(matches!(
            verify_remote_cancel(&hello, &challenge, &tampered, &hub.verifier()),
            Err(TransportError::InvalidHubSignature)
        ));

        let mut tampered_ack = ack.clone();
        tampered_ack.disposition = CancellationDisposition::AlreadyTerminal;
        assert!(matches!(
            verify_remote_cancellation_ack(&registry, &hello, &challenge, &tampered_ack),
            Err(TransportError::Control(
                ControlError::InvalidDeviceSignature
            ))
        ));
    }

    #[test]
    fn backend_session_lifecycle_control_is_signed_and_connection_bound() {
        let (registry, identity, device_id) = enrolled();
        let hub = HubIdentity::generate();
        let hello = AgentHello::new(device_id.clone(), caps());
        let challenge = hub.challenge(&hello).unwrap();
        let request = hub
            .backend_session_end(
                &hello,
                &challenge,
                device_id,
                3,
                "ctx_0123456789abcdef0123456789abcdef".into(),
            )
            .unwrap();
        verify_remote_backend_session_end(&hello, &challenge, &request, &hub.verifier()).unwrap();
        let ack = build_remote_backend_session_ended(&identity, &hello, &challenge, &request, true)
            .unwrap();
        verify_remote_backend_session_ended(&registry, &hello, &challenge, &ack).unwrap();

        let mut tampered = request.clone();
        tampered.context_id = "ctx_ffffffffffffffffffffffffffffffff".into();
        assert!(matches!(
            verify_remote_backend_session_end(&hello, &challenge, &tampered, &hub.verifier()),
            Err(TransportError::InvalidHubSignature)
        ));

        let mut tampered_ack = ack.clone();
        tampered_ack.ended = false;
        assert!(matches!(
            verify_remote_backend_session_ended(&registry, &hello, &challenge, &tampered_ack),
            Err(TransportError::Control(
                ControlError::InvalidDeviceSignature
            ))
        ));
    }

    #[test]
    fn oversized_declared_frame_is_rejected_before_payload_read() {
        let declared = u32::try_from(MAX_FRAME_BYTES + 1).unwrap().to_be_bytes();
        let mut cursor = Cursor::new(declared.to_vec());
        let result: Result<AgentToHub, TransportError> = read_frame(&mut cursor);
        assert!(
            matches!(result, Err(TransportError::FrameTooLarge(size)) if size == MAX_FRAME_BYTES + 1)
        );
    }

    #[test]
    fn bounded_frame_round_trip_is_typed() {
        let hello = AgentToHub::Hello(AgentHello::new("dev-test".into(), caps()));
        let mut bytes = Vec::new();
        write_frame(&mut bytes, &hello).unwrap();
        let decoded: AgentToHub = read_frame(&mut Cursor::new(bytes)).unwrap();
        assert_eq!(decoded, hello);
    }
}
