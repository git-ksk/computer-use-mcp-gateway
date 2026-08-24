//! Bounded transport adapters between CUMG and canonical `mcp-execution-handoff` semantics.
//!
//! The managed runtime transport is process-local and uses stdio. #152 is migrating normal
//! ownership from the Hub host to the controlled Agent device so OS capture/input permissions stay
//! with the execution surface. The Unix adapter remains a compatibility/regression harness.
//! Neither transport carries CUMG grants, operation payloads or results, screenshots, clipboard
//! data, Human input, credentials, or recovery authority.

use crate::{
    v2_m0::{DeviceCommand, InputTarget, PointerTarget, ScrollTarget},
    v2_m0_trust::AuthenticatedClientPrincipal,
};
use async_trait::async_trait;
use ring::digest::{SHA256, digest};
use serde::{Deserialize, Serialize};
use std::{
    fmt,
    io::{BufRead, BufReader, Write},
    path::{Path, PathBuf},
    process::Stdio,
    sync::Arc,
    time::Duration,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader as TokioBufReader},
    process::{Child, ChildStdin, ChildStdout, Command},
    sync::Mutex,
};

const PROTOCOL_VERSION: u8 = 1;
const MAX_WIRE_BYTES: usize = 4 * 1024;
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(1);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExactWindowBinding {
    pub context_binding: String,
    pub process_id: u32,
    pub window_id: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentAuthorityRequest {
    pub principal_binding: String,
    pub device_binding: String,
    pub generation: u64,
    pub capability_revision: u64,
    pub exact_window: Option<ExactWindowBinding>,
    pub verification_candidate: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerificationToken {
    pub intervention_id: String,
    pub epoch: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentAuthorityDecision {
    Allow,
    Deny,
    Verification(VerificationToken),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerificationReport {
    pub authority: AgentAuthorityRequest,
    pub token: VerificationToken,
    pub satisfied: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HandoffInterventionStatus {
    AwaitingHuman,
    HumanActive,
    Verifying,
    ReadyToResume,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HandoffExecutionAuthority {
    Human,
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HandoffSurfaceKind {
    Native,
    Webrtc,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HandoffActiveStatus {
    pub intervention_id: String,
    pub status: HandoffInterventionStatus,
    pub epoch: u64,
    pub authority: HandoffExecutionAuthority,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HandoffRuntimeStatus {
    pub active: Option<HandoffActiveStatus>,
    pub recovery_required: bool,
    pub recovery_status: Option<HandoffInterventionStatus>,
    pub recovery_epoch: Option<u64>,
    pub recovery_expired: bool,
    pub resume_requested: bool,
    pub faulted: bool,
    pub human_surface: Option<HandoffSurfaceKind>,
    pub locator: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HandoffRuntimeControl {
    Begin {
        authority: AgentAuthorityRequest,
    },
    RecoverReissue {
        authority: AgentAuthorityRequest,
    },
    RecoverRebind {
        authority: AgentAuthorityRequest,
        prior_context_id: String,
        prior_generation: Option<u64>,
        prior_capability_revision: Option<u64>,
    },
    RequestResume {
        intervention_id: String,
        epoch: u64,
    },
    CancelBeforeHuman {
        intervention_id: String,
        epoch: u64,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HandoffControlError {
    Unavailable,
    Rejected,
    Protocol,
}

impl fmt::Display for HandoffControlError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Unavailable => "managed handoff runtime unavailable",
            Self::Rejected => "managed handoff control transition rejected",
            Self::Protocol => "managed handoff control protocol invalid",
        })
    }
}

impl std::error::Error for HandoffControlError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OperatorHandoffError {
    Unavailable,
    Protocol,
    Unsupported,
}

impl fmt::Display for OperatorHandoffError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let value = match self {
            Self::Unavailable => "operator handoff authority unavailable",
            Self::Protocol => "operator handoff authority protocol invalid",
            Self::Unsupported => "operator handoff authority unsupported on this platform",
        };
        f.write_str(value)
    }
}

impl std::error::Error for OperatorHandoffError {}

#[async_trait]
pub trait OperatorHandoffAuthority: Send + Sync {
    async fn admit_agent(
        &self,
        request: AgentAuthorityRequest,
    ) -> Result<AgentAuthorityDecision, OperatorHandoffError>;

    async fn report_verification(
        &self,
        report: VerificationReport,
    ) -> Result<(), OperatorHandoffError>;
}

#[derive(Debug, Clone)]
pub struct ManagedHandoffRuntimeConfig {
    command: PathBuf,
    script: PathBuf,
    env_file: PathBuf,
    timeout: Duration,
}

impl ManagedHandoffRuntimeConfig {
    pub fn new(
        command: PathBuf,
        script: PathBuf,
        env_file: PathBuf,
        timeout: Duration,
    ) -> Result<Self, OperatorHandoffError> {
        if !command.is_absolute()
            || !script.is_absolute()
            || !env_file.is_absolute()
            || timeout.is_zero()
        {
            return Err(OperatorHandoffError::Protocol);
        }
        if !std::fs::metadata(&command).is_ok_and(|metadata| metadata.is_file())
            || !std::fs::metadata(&script).is_ok_and(|metadata| metadata.is_file())
        {
            return Err(OperatorHandoffError::Unavailable);
        }
        let env_metadata =
            std::fs::symlink_metadata(&env_file).map_err(|_| OperatorHandoffError::Unavailable)?;
        if env_metadata.file_type().is_symlink() || !env_metadata.is_file() {
            return Err(OperatorHandoffError::Protocol);
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if env_metadata.permissions().mode() & 0o077 != 0 {
                return Err(OperatorHandoffError::Protocol);
            }
        }
        Ok(Self {
            command,
            script,
            env_file,
            timeout,
        })
    }
}

struct ManagedHandoffRuntimeState {
    child: Child,
    stdin: ChildStdin,
    stdout: TokioBufReader<ChildStdout>,
    faulted: bool,
}

#[derive(Clone)]
pub struct ManagedOperatorHandoffAuthority {
    state: Arc<Mutex<ManagedHandoffRuntimeState>>,
    timeout: Duration,
}

impl ManagedOperatorHandoffAuthority {
    pub async fn spawn(config: ManagedHandoffRuntimeConfig) -> Result<Self, OperatorHandoffError> {
        let mut command = Command::new(&config.command);
        command
            .env_clear()
            .arg(format!("--env-file={}", config.env_file.display()))
            .arg(&config.script)
            .arg("serve-stdio")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .kill_on_drop(true);
        let mut child = command
            .spawn()
            .map_err(|_| OperatorHandoffError::Unavailable)?;
        let stdin = child
            .stdin
            .take()
            .ok_or(OperatorHandoffError::Unavailable)?;
        let stdout = child
            .stdout
            .take()
            .ok_or(OperatorHandoffError::Unavailable)?;
        let runtime = Self {
            state: Arc::new(Mutex::new(ManagedHandoffRuntimeState {
                child,
                stdin,
                stdout: TokioBufReader::new(stdout),
                faulted: false,
            })),
            timeout: config.timeout,
        };
        let status = runtime
            .exchange_value(&serde_json::json!({ "action": "status" }))
            .await?;
        if status.get("ok").and_then(serde_json::Value::as_bool) != Some(true) {
            runtime.fault().await;
            return Err(OperatorHandoffError::Protocol);
        }
        Ok(runtime)
    }

    async fn stop_faulted_child(state: &mut ManagedHandoffRuntimeState, timeout: Duration) {
        state.faulted = true;
        // Fence Agent authority before attempting cleanup. EOF gives the runtime a bounded chance
        // to revoke any live Human surface and preserve the signed checkpoint for explicit recovery.
        let _ = state.stdin.shutdown().await;
        if tokio::time::timeout(timeout, state.child.wait())
            .await
            .is_err()
        {
            let _ = state.child.start_kill();
            let _ = tokio::time::timeout(timeout, state.child.wait()).await;
        }
    }

    async fn fault(&self) {
        let mut state = self.state.lock().await;
        if state.faulted {
            return;
        }
        Self::stop_faulted_child(&mut state, self.timeout).await;
    }

    async fn exchange_value<T: Serialize + ?Sized>(
        &self,
        request: &T,
    ) -> Result<serde_json::Value, OperatorHandoffError> {
        let encoded = serde_json::to_vec(request).map_err(|_| OperatorHandoffError::Protocol)?;
        if encoded.len() + 1 > MAX_WIRE_BYTES {
            return Err(OperatorHandoffError::Protocol);
        }

        let mut state = self.state.lock().await;
        if state.faulted {
            return Err(OperatorHandoffError::Unavailable);
        }
        match state.child.try_wait() {
            Ok(Some(_)) => {
                state.faulted = true;
                return Err(OperatorHandoffError::Unavailable);
            }
            Ok(None) => {}
            Err(_) => {
                Self::stop_faulted_child(&mut state, self.timeout).await;
                return Err(OperatorHandoffError::Unavailable);
            }
        }

        let result = tokio::time::timeout(self.timeout, async {
            state
                .stdin
                .write_all(&encoded)
                .await
                .map_err(|_| OperatorHandoffError::Unavailable)?;
            state
                .stdin
                .write_all(b"\n")
                .await
                .map_err(|_| OperatorHandoffError::Unavailable)?;
            state
                .stdin
                .flush()
                .await
                .map_err(|_| OperatorHandoffError::Unavailable)?;
            let mut response = Vec::new();
            let read = state
                .stdout
                .read_until(b'\n', &mut response)
                .await
                .map_err(|_| OperatorHandoffError::Unavailable)?;
            if read == 0 {
                return Err(OperatorHandoffError::Unavailable);
            }
            if read > MAX_WIRE_BYTES || response.last() != Some(&b'\n') {
                return Err(OperatorHandoffError::Protocol);
            }
            response.pop();
            serde_json::from_slice(&response).map_err(|_| OperatorHandoffError::Protocol)
        })
        .await
        .unwrap_or(Err(OperatorHandoffError::Unavailable));

        if result.is_err() {
            Self::stop_faulted_child(&mut state, self.timeout).await;
        }
        result
    }

    async fn exchange(&self, request: WireRequest) -> Result<WireResponse, OperatorHandoffError> {
        let value = self.exchange_value(&request).await?;
        match serde_json::from_value(value) {
            Ok(response) => Ok(response),
            Err(_) => {
                self.fault().await;
                Err(OperatorHandoffError::Protocol)
            }
        }
    }

    pub async fn status(&self) -> Result<HandoffRuntimeStatus, HandoffControlError> {
        let value = self
            .exchange_value(&ManagedWireControlRequest::Status)
            .await
            .map_err(map_control_transport_error)?;
        parse_runtime_status(value)
    }

    pub async fn control(
        &self,
        control: HandoffRuntimeControl,
    ) -> Result<HandoffRuntimeStatus, HandoffControlError> {
        let request = managed_wire_control(control)?;
        let value = self
            .exchange_value(&request)
            .await
            .map_err(map_control_transport_error)?;
        match value.get("ok").and_then(serde_json::Value::as_bool) {
            Some(true) => self.status().await,
            Some(false) => Err(HandoffControlError::Rejected),
            None => {
                self.fault().await;
                Err(HandoffControlError::Protocol)
            }
        }
    }

    pub async fn shutdown(&self) {
        let mut state = self.state.lock().await;
        if state.faulted {
            return;
        }
        Self::stop_faulted_child(&mut state, self.timeout).await;
    }
}

#[derive(Debug, Clone)]
pub struct UnixOperatorHandoffAuthority {
    socket_path: PathBuf,
    timeout: Duration,
}

impl UnixOperatorHandoffAuthority {
    pub fn new(socket_path: PathBuf) -> Result<Self, OperatorHandoffError> {
        if !socket_path.is_absolute() {
            return Err(OperatorHandoffError::Protocol);
        }
        Ok(Self {
            socket_path,
            timeout: DEFAULT_TIMEOUT,
        })
    }

    #[cfg(test)]
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    async fn exchange(&self, request: WireRequest) -> Result<WireResponse, OperatorHandoffError> {
        let path = self.socket_path.clone();
        let timeout = self.timeout;
        tokio::task::spawn_blocking(move || exchange_blocking(&path, timeout, request))
            .await
            .map_err(|_| OperatorHandoffError::Unavailable)?
    }
}

#[async_trait]
impl OperatorHandoffAuthority for ManagedOperatorHandoffAuthority {
    async fn admit_agent(
        &self,
        request: AgentAuthorityRequest,
    ) -> Result<AgentAuthorityDecision, OperatorHandoffError> {
        let response = self.exchange(wire_admission_request(request)).await?;
        match parse_admission_response(response) {
            Ok(decision) => Ok(decision),
            Err(error) => {
                self.fault().await;
                Err(error)
            }
        }
    }

    async fn report_verification(
        &self,
        report: VerificationReport,
    ) -> Result<(), OperatorHandoffError> {
        let response = self.exchange(wire_verification_report(report)).await?;
        match validate_verification_response(response) {
            Ok(()) => Ok(()),
            Err(error) => {
                self.fault().await;
                Err(error)
            }
        }
    }
}

#[async_trait]
impl OperatorHandoffAuthority for UnixOperatorHandoffAuthority {
    async fn admit_agent(
        &self,
        request: AgentAuthorityRequest,
    ) -> Result<AgentAuthorityDecision, OperatorHandoffError> {
        let response = self.exchange(wire_admission_request(request)).await?;
        parse_admission_response(response)
    }

    async fn report_verification(
        &self,
        report: VerificationReport,
    ) -> Result<(), OperatorHandoffError> {
        let response = self.exchange(wire_verification_report(report)).await?;
        validate_verification_response(response)
    }
}

#[derive(Debug, Serialize)]
#[serde(tag = "action", rename_all = "snake_case")]
enum ManagedWireControlRequest {
    Status,
    Begin {
        authority: ManagedWireAuthority,
    },
    RecoverReissue {
        authority: ManagedWireAuthority,
    },
    RecoverRebind {
        authority: ManagedWireAuthority,
        prior_context_id: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        prior_generation: Option<u64>,
        #[serde(skip_serializing_if = "Option::is_none")]
        prior_capability_revision: Option<u64>,
    },
    RequestResume {
        intervention_id: String,
        epoch: u64,
    },
    CancelBeforeHuman {
        intervention_id: String,
        epoch: u64,
    },
}

#[derive(Debug, Serialize)]
struct ManagedWireAuthority {
    protocol: u8,
    principal_binding: String,
    device_binding: String,
    generation: u64,
    capability_revision: u64,
    exact_window: WireExactWindow,
    verification_candidate: bool,
}

#[derive(Debug, Deserialize)]
struct WireRuntimeStatus {
    ok: bool,
    active: Option<HandoffActiveStatus>,
    recovery_required: bool,
    recovery_status: Option<HandoffInterventionStatus>,
    recovery_epoch: Option<u64>,
    recovery_expired: bool,
    resume_requested: bool,
    faulted: bool,
    human_surface: Option<HandoffSurfaceKind>,
    locator: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(tag = "action", rename_all = "snake_case")]
enum WireRequest {
    AdmitAgent {
        protocol: u8,
        principal_binding: String,
        device_binding: String,
        generation: u64,
        capability_revision: u64,
        #[serde(skip_serializing_if = "Option::is_none")]
        exact_window: Option<WireExactWindow>,
        verification_candidate: bool,
    },
    ReportVerification {
        protocol: u8,
        principal_binding: String,
        device_binding: String,
        generation: u64,
        capability_revision: u64,
        #[serde(skip_serializing_if = "Option::is_none")]
        exact_window: Option<WireExactWindow>,
        intervention_id: String,
        epoch: u64,
        satisfied: bool,
    },
}

fn map_control_transport_error(error: OperatorHandoffError) -> HandoffControlError {
    match error {
        OperatorHandoffError::Unavailable | OperatorHandoffError::Unsupported => {
            HandoffControlError::Unavailable
        }
        OperatorHandoffError::Protocol => HandoffControlError::Protocol,
    }
}

fn managed_wire_authority(
    authority: AgentAuthorityRequest,
) -> Result<ManagedWireAuthority, HandoffControlError> {
    if authority.verification_candidate {
        return Err(HandoffControlError::Protocol);
    }
    let exact_window = authority
        .exact_window
        .ok_or(HandoffControlError::Protocol)?;
    Ok(ManagedWireAuthority {
        protocol: PROTOCOL_VERSION,
        principal_binding: authority.principal_binding,
        device_binding: authority.device_binding,
        generation: authority.generation,
        capability_revision: authority.capability_revision,
        exact_window: exact_window.into(),
        verification_candidate: false,
    })
}

fn managed_wire_control(
    control: HandoffRuntimeControl,
) -> Result<ManagedWireControlRequest, HandoffControlError> {
    Ok(match control {
        HandoffRuntimeControl::Begin { authority } => ManagedWireControlRequest::Begin {
            authority: managed_wire_authority(authority)?,
        },
        HandoffRuntimeControl::RecoverReissue { authority } => {
            ManagedWireControlRequest::RecoverReissue {
                authority: managed_wire_authority(authority)?,
            }
        }
        HandoffRuntimeControl::RecoverRebind {
            authority,
            prior_context_id,
            prior_generation,
            prior_capability_revision,
        } => ManagedWireControlRequest::RecoverRebind {
            authority: managed_wire_authority(authority)?,
            prior_context_id,
            prior_generation,
            prior_capability_revision,
        },
        HandoffRuntimeControl::RequestResume {
            intervention_id,
            epoch,
        } => ManagedWireControlRequest::RequestResume {
            intervention_id,
            epoch,
        },
        HandoffRuntimeControl::CancelBeforeHuman {
            intervention_id,
            epoch,
        } => ManagedWireControlRequest::CancelBeforeHuman {
            intervention_id,
            epoch,
        },
    })
}

fn parse_runtime_status(
    value: serde_json::Value,
) -> Result<HandoffRuntimeStatus, HandoffControlError> {
    let status: WireRuntimeStatus =
        serde_json::from_value(value).map_err(|_| HandoffControlError::Protocol)?;
    if !status.ok {
        return Err(HandoffControlError::Rejected);
    }
    if status.recovery_required != status.recovery_status.is_some()
        || status.recovery_required != status.recovery_epoch.is_some()
        || status.recovery_epoch == Some(0)
    {
        return Err(HandoffControlError::Protocol);
    }
    if let Some(active) = status.active.as_ref() {
        let expected_authority = if active.status == HandoffInterventionStatus::HumanActive {
            HandoffExecutionAuthority::Human
        } else {
            HandoffExecutionAuthority::None
        };
        if active.intervention_id.is_empty()
            || active.intervention_id.len() > 160
            || active.epoch == 0
            || active.authority != expected_authority
            || !active
                .intervention_id
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
        {
            return Err(HandoffControlError::Protocol);
        }
    }
    if let Some(locator) = status.locator.as_deref()
        && (locator.is_empty()
            || locator.len() > 2048
            || locator.chars().any(char::is_control)
            || !(locator.starts_with("http://127.0.0.1:") || locator.starts_with("https://")))
    {
        return Err(HandoffControlError::Protocol);
    }
    Ok(HandoffRuntimeStatus {
        active: status.active,
        recovery_required: status.recovery_required,
        recovery_status: status.recovery_status,
        recovery_epoch: status.recovery_epoch,
        recovery_expired: status.recovery_expired,
        resume_requested: status.resume_requested,
        faulted: status.faulted,
        human_surface: status.human_surface,
        locator: status.locator,
    })
}

fn wire_admission_request(request: AgentAuthorityRequest) -> WireRequest {
    WireRequest::AdmitAgent {
        protocol: PROTOCOL_VERSION,
        principal_binding: request.principal_binding,
        device_binding: request.device_binding,
        generation: request.generation,
        capability_revision: request.capability_revision,
        exact_window: request.exact_window.map(WireExactWindow::from),
        verification_candidate: request.verification_candidate,
    }
}

fn wire_verification_report(report: VerificationReport) -> WireRequest {
    WireRequest::ReportVerification {
        protocol: PROTOCOL_VERSION,
        principal_binding: report.authority.principal_binding,
        device_binding: report.authority.device_binding,
        generation: report.authority.generation,
        capability_revision: report.authority.capability_revision,
        exact_window: report.authority.exact_window.map(WireExactWindow::from),
        intervention_id: report.token.intervention_id,
        epoch: report.token.epoch,
        satisfied: report.satisfied,
    }
}

fn parse_admission_response(
    response: WireResponse,
) -> Result<AgentAuthorityDecision, OperatorHandoffError> {
    if !response.ok {
        return Err(OperatorHandoffError::Protocol);
    }
    match response.decision.as_deref() {
        Some("allow") => Ok(AgentAuthorityDecision::Allow),
        Some("deny") => Ok(AgentAuthorityDecision::Deny),
        Some("verification") => {
            let intervention_id = bounded_intervention_id(response.intervention_id.as_deref())?;
            let epoch = response.epoch.ok_or(OperatorHandoffError::Protocol)?;
            if epoch == 0 {
                return Err(OperatorHandoffError::Protocol);
            }
            Ok(AgentAuthorityDecision::Verification(VerificationToken {
                intervention_id,
                epoch,
            }))
        }
        _ => Err(OperatorHandoffError::Protocol),
    }
}

fn validate_verification_response(response: WireResponse) -> Result<(), OperatorHandoffError> {
    if response.ok && response.decision.is_none() {
        Ok(())
    } else {
        Err(OperatorHandoffError::Protocol)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct WireExactWindow {
    context_binding: String,
    process_id: u32,
    window_id: u64,
}

impl From<ExactWindowBinding> for WireExactWindow {
    fn from(value: ExactWindowBinding) -> Self {
        Self {
            context_binding: value.context_binding,
            process_id: value.process_id,
            window_id: value.window_id,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct WireResponse {
    ok: bool,
    #[serde(default)]
    decision: Option<String>,
    #[serde(default)]
    intervention_id: Option<String>,
    #[serde(default)]
    epoch: Option<u64>,
}

#[cfg(unix)]
fn exchange_blocking(
    path: &Path,
    timeout: Duration,
    request: WireRequest,
) -> Result<WireResponse, OperatorHandoffError> {
    use std::os::unix::net::UnixStream;

    let encoded = serde_json::to_vec(&request).map_err(|_| OperatorHandoffError::Protocol)?;
    if encoded.len() + 1 > MAX_WIRE_BYTES {
        return Err(OperatorHandoffError::Protocol);
    }
    let mut stream = UnixStream::connect(path).map_err(|_| OperatorHandoffError::Unavailable)?;
    stream
        .set_read_timeout(Some(timeout))
        .map_err(|_| OperatorHandoffError::Unavailable)?;
    stream
        .set_write_timeout(Some(timeout))
        .map_err(|_| OperatorHandoffError::Unavailable)?;
    stream
        .write_all(&encoded)
        .and_then(|_| stream.write_all(b"\n"))
        .and_then(|_| stream.flush())
        .map_err(|_| OperatorHandoffError::Unavailable)?;

    let mut reader = BufReader::new(stream);
    let mut response = Vec::new();
    let read = reader
        .read_until(b'\n', &mut response)
        .map_err(|_| OperatorHandoffError::Unavailable)?;
    if read == 0 || read > MAX_WIRE_BYTES || response.last() != Some(&b'\n') {
        return Err(OperatorHandoffError::Protocol);
    }
    response.pop();
    serde_json::from_slice(&response).map_err(|_| OperatorHandoffError::Protocol)
}

#[cfg(not(unix))]
fn exchange_blocking(
    _path: &Path,
    _timeout: Duration,
    _request: WireRequest,
) -> Result<WireResponse, OperatorHandoffError> {
    Err(OperatorHandoffError::Unsupported)
}

fn bounded_intervention_id(value: Option<&str>) -> Result<String, OperatorHandoffError> {
    let value = value.ok_or(OperatorHandoffError::Protocol)?;
    if value.is_empty()
        || value.len() > 160
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err(OperatorHandoffError::Protocol);
    }
    Ok(value.to_owned())
}

pub fn principal_binding(principal: &AuthenticatedClientPrincipal) -> String {
    bounded_hash(
        b"cumg/operator-handoff/principal/v1\0",
        &[
            principal.issuer.as_bytes(),
            b"\0",
            principal.subject.as_bytes(),
        ],
    )
}

pub fn device_binding(device_id: &str) -> String {
    bounded_hash(
        b"cumg/operator-handoff/device/v1\0",
        &[device_id.as_bytes()],
    )
}

pub fn interaction_context_binding(context_id: &str) -> String {
    bounded_hash(
        b"cumg/operator-handoff/context/v1\0",
        &[context_id.as_bytes()],
    )
}

fn bounded_hash(domain: &[u8], parts: &[&[u8]]) -> String {
    let mut material =
        Vec::with_capacity(domain.len() + parts.iter().map(|part| part.len()).sum::<usize>());
    material.extend_from_slice(domain);
    for part in parts {
        material.extend_from_slice(part);
    }
    let value = digest(&SHA256, &material);
    material.fill(0);
    hex_lower(value.as_ref())
}

fn hex_lower(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut result = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        result.push(HEX[(byte >> 4) as usize] as char);
        result.push(HEX[(byte & 0x0f) as usize] as char);
    }
    result
}

/// Phase-1 handoff protects all desktop/browser Computer Use semantics. Process/shell and bounded
/// filesystem observation remain outside the `os_window` authority boundary until the PTY work in
/// Handoff #48 is deliberately enabled.
pub fn is_phase1_protected_command(command: &DeviceCommand) -> bool {
    !matches!(
        command,
        DeviceCommand::ExecuteProcess { .. }
            | DeviceCommand::Shell { .. }
            | DeviceCommand::ReadFile { .. }
            | DeviceCommand::ListDirectory { .. }
    )
}

/// Extract only an exact already-authorized CUMG window binding. This deliberately does not expose
/// query text, predicates, typed text, screenshots, element values, clipboard data, or backend IDs.
pub fn exact_window_binding(command: &DeviceCommand) -> Option<ExactWindowBinding> {
    fn pointer_window(target: &PointerTarget) -> Option<(u32, u64)> {
        match target {
            PointerTarget::WindowPhysical {
                process_id,
                window_id,
                ..
            }
            | PointerTarget::Element {
                process_id,
                window_id,
                ..
            } => Some((*process_id, *window_id)),
            PointerTarget::DesktopPhysical { .. } => None,
        }
    }

    fn input_window(target: &InputTarget) -> Option<(u32, u64)> {
        match target {
            InputTarget::Window {
                process_id,
                window_id: Some(window_id),
            }
            | InputTarget::WindowPoint {
                process_id,
                window_id,
                ..
            }
            | InputTarget::Element {
                process_id,
                window_id,
                ..
            } => Some((*process_id, *window_id)),
            InputTarget::Desktop
            | InputTarget::Window {
                window_id: None, ..
            } => None,
        }
    }

    fn scroll_window(target: &ScrollTarget) -> Option<(u32, u64)> {
        match target {
            ScrollTarget::Window {
                process_id,
                window_id: Some(window_id),
            }
            | ScrollTarget::WindowPoint {
                process_id,
                window_id,
                ..
            } => Some((*process_id, *window_id)),
            ScrollTarget::Window {
                window_id: None, ..
            }
            | ScrollTarget::DesktopPoint { .. } => None,
        }
    }

    let (context_id, process_id, window_id) = match command {
        DeviceCommand::InspectWindowContextual {
            context_id,
            process_id,
            window_id,
            ..
        }
        | DeviceCommand::VerifyUiStateContextual {
            context_id,
            process_id,
            window_id,
            ..
        } => (context_id.as_str(), *process_id, *window_id),
        DeviceCommand::PointerClickAdvanced {
            context_id: Some(context_id),
            target,
            ..
        } => {
            let (process_id, window_id) = pointer_window(target)?;
            (context_id.as_str(), process_id, window_id)
        }
        DeviceCommand::PointerDragAdvanced {
            context_id: Some(context_id),
            from,
            to,
            ..
        } => {
            let from = pointer_window(from)?;
            let to = pointer_window(to)?;
            if from != to {
                return None;
            }
            (context_id.as_str(), from.0, from.1)
        }
        DeviceCommand::TypeTextAdvanced {
            context_id: Some(context_id),
            target,
            ..
        }
        | DeviceCommand::KeyboardInput {
            context_id: Some(context_id),
            target,
            ..
        } => {
            let (process_id, window_id) = input_window(target)?;
            (context_id.as_str(), process_id, window_id)
        }
        DeviceCommand::Scroll {
            context_id: Some(context_id),
            target,
            ..
        } => {
            let (process_id, window_id) = scroll_window(target)?;
            (context_id.as_str(), process_id, window_id)
        }
        DeviceCommand::SetWindowFrame {
            context_id: Some(context_id),
            process_id,
            window_id,
            ..
        }
        | DeviceCommand::InvokeMenu {
            context_id: Some(context_id),
            process_id,
            window_id,
            ..
        }
        | DeviceCommand::SetUiValue {
            context_id,
            process_id,
            window_id,
            ..
        }
        | DeviceCommand::CaptureRegion {
            context_id: Some(context_id),
            process_id,
            window_id,
            ..
        } => (context_id.as_str(), *process_id, *window_id),
        _ => return None,
    };
    Some(ExactWindowBinding {
        context_binding: interaction_context_binding(context_id),
        process_id,
        window_id,
    })
}

pub fn is_exact_verification_candidate(command: &DeviceCommand) -> bool {
    matches!(
        command,
        DeviceCommand::VerifyUiStateContextual {
            include_screenshot: false,
            ..
        }
    ) && exact_window_binding(command).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::v2_m0::{UiPredicate, UiRect};

    #[cfg(unix)]
    fn managed_runtime_fixture(exit_after_status: bool) -> (ManagedHandoffRuntimeConfig, PathBuf) {
        use rand::RngCore;
        use std::os::unix::fs::PermissionsExt;

        let mut suffix = [0_u8; 8];
        rand::thread_rng().fill_bytes(&mut suffix);
        let root = std::env::temp_dir().join(format!(
            "cumg-handoff-runtime-test-{}-{}",
            std::process::id(),
            u64::from_le_bytes(suffix)
        ));
        std::fs::create_dir_all(&root).unwrap();
        let command = root.join("fixture.sh");
        let script = root.join("runtime.mjs");
        let env_file = root.join("runtime.env");
        let body = if exit_after_status {
            r#"#!/bin/sh
IFS= read -r line || exit 2
printf '%s\n' '{"ok":true}'
# Make startup handshake deterministic, then simulate child loss only after the next request arrives.
IFS= read -r line || exit 0
exit 0
"#
        } else {
            r#"#!/bin/sh
while IFS= read -r line; do
  case "$line" in
    *'"action":"status"'*) printf '%s\n' '{"ok":true}' ;;
    *'"action":"admit_agent"'*) printf '%s\n' '{"ok":true,"decision":"allow"}' ;;
    *'"action":"report_verification"'*) printf '%s\n' '{"ok":true}' ;;
    *) printf '%s\n' '{"ok":false}' ;;
  esac
done
"#
        };
        std::fs::write(&command, body).unwrap();
        std::fs::set_permissions(&command, std::fs::Permissions::from_mode(0o700)).unwrap();
        std::fs::write(&script, "// fixture\n").unwrap();
        std::fs::write(&env_file, "FIXTURE=1\n").unwrap();
        std::fs::set_permissions(&env_file, std::fs::Permissions::from_mode(0o600)).unwrap();
        let config =
            ManagedHandoffRuntimeConfig::new(command, script, env_file, Duration::from_secs(5))
                .unwrap();
        (config, root)
    }

    #[cfg(unix)]
    fn fixture_authority_request() -> AgentAuthorityRequest {
        AgentAuthorityRequest {
            principal_binding: "a".repeat(64),
            device_binding: "b".repeat(64),
            generation: 1,
            capability_revision: 1,
            exact_window: Some(ExactWindowBinding {
                context_binding: "c".repeat(64),
                process_id: 12,
                window_id: 34,
            }),
            verification_candidate: false,
        }
    }

    #[test]
    fn managed_control_wire_carries_only_explicit_typed_authority_and_closed_status() {
        let request = managed_wire_control(HandoffRuntimeControl::Begin {
            authority: fixture_authority_request(),
        })
        .unwrap();
        let value = serde_json::to_value(request).unwrap();
        assert_eq!(value["action"], "begin");
        assert_eq!(value["authority"]["protocol"], 1);
        assert_eq!(value["authority"]["generation"], 1);
        assert_eq!(value["authority"]["capability_revision"], 1);
        assert_eq!(value["authority"]["exact_window"]["process_id"], 12);
        assert_eq!(value["authority"]["exact_window"]["window_id"], 34);
        assert_eq!(value["authority"]["verification_candidate"], false);
        assert!(value.get("principal_binding").is_none());

        let status = parse_runtime_status(serde_json::json!({
            "ok": true,
            "active": {
                "intervention_id": "intervention_1",
                "status": "awaiting_human",
                "epoch": 1,
                "authority": "none"
            },
            "recovery_required": false,
            "recovery_status": null,
            "recovery_epoch": null,
            "recovery_expired": false,
            "resume_requested": false,
            "faulted": false,
            "human_surface": "webrtc",
            "locator": "https://handoff.example/takeover/session_1"
        }))
        .unwrap();
        assert_eq!(
            status.active.as_ref().unwrap().status,
            HandoffInterventionStatus::AwaitingHuman
        );
        assert_eq!(status.human_surface, Some(HandoffSurfaceKind::Webrtc));

        let mut invalid = fixture_authority_request();
        invalid.exact_window = None;
        assert!(matches!(
            managed_wire_control(HandoffRuntimeControl::Begin { authority: invalid }),
            Err(HandoffControlError::Protocol)
        ));
        assert_eq!(
            parse_runtime_status(serde_json::json!({
                "ok": true,
                "active": null,
                "recovery_required": false,
                "recovery_status": null,
                "recovery_epoch": null,
                "recovery_expired": false,
                "resume_requested": false,
                "faulted": false,
                "human_surface": null,
                "locator": "file:///tmp/not-a-takeover"
            })),
            Err(HandoffControlError::Protocol)
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn managed_runtime_uses_stdio_and_shutdown_never_restarts_authority() {
        let (config, root) = managed_runtime_fixture(false);
        let runtime = ManagedOperatorHandoffAuthority::spawn(config)
            .await
            .unwrap();
        assert_eq!(
            runtime
                .admit_agent(fixture_authority_request())
                .await
                .unwrap(),
            AgentAuthorityDecision::Allow
        );
        runtime.shutdown().await;
        assert_eq!(
            runtime.admit_agent(fixture_authority_request()).await,
            Err(OperatorHandoffError::Unavailable)
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn managed_runtime_process_loss_is_permanently_fail_closed_for_hub_generation() {
        let (config, root) = managed_runtime_fixture(true);
        let runtime = ManagedOperatorHandoffAuthority::spawn(config)
            .await
            .unwrap();
        assert_eq!(
            runtime.admit_agent(fixture_authority_request()).await,
            Err(OperatorHandoffError::Unavailable)
        );
        assert_eq!(
            runtime.admit_agent(fixture_authority_request()).await,
            Err(OperatorHandoffError::Unavailable)
        );
        runtime.shutdown().await;
        std::fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn managed_runtime_requires_private_env_file() {
        use rand::RngCore;
        use std::os::unix::fs::PermissionsExt;

        let mut suffix = [0_u8; 8];
        rand::thread_rng().fill_bytes(&mut suffix);
        let root = std::env::temp_dir().join(format!(
            "cumg-handoff-env-test-{}-{}",
            std::process::id(),
            u64::from_le_bytes(suffix)
        ));
        std::fs::create_dir_all(&root).unwrap();
        let command = root.join("node");
        let script = root.join("runtime.mjs");
        let env_file = root.join("runtime.env");
        std::fs::write(&command, "node\n").unwrap();
        std::fs::write(&script, "// runtime\n").unwrap();
        std::fs::write(&env_file, "SECRET=value\n").unwrap();
        std::fs::set_permissions(&env_file, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert_eq!(
            ManagedHandoffRuntimeConfig::new(
                command.clone(),
                script.clone(),
                env_file.clone(),
                Duration::from_secs(1),
            )
            .unwrap_err(),
            OperatorHandoffError::Protocol
        );

        std::fs::set_permissions(&env_file, std::fs::Permissions::from_mode(0o600)).unwrap();
        let symlink_env_file = root.join("runtime-link.env");
        std::os::unix::fs::symlink(&env_file, &symlink_env_file).unwrap();
        assert_eq!(
            ManagedHandoffRuntimeConfig::new(
                command,
                script,
                symlink_env_file,
                Duration::from_secs(1),
            )
            .unwrap_err(),
            OperatorHandoffError::Protocol
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn bindings_are_fixed_length_and_domain_separated() {
        let principal =
            AuthenticatedClientPrincipal::new("https://issuer.example", "alice").unwrap();
        let p = principal_binding(&principal);
        let d = device_binding("device-1");
        let c = interaction_context_binding("ctx-1");
        assert_eq!(p.len(), 64);
        assert_eq!(d.len(), 64);
        assert_eq!(c.len(), 64);
        assert_ne!(p, d);
        assert_ne!(d, c);
        assert!(!p.contains("alice"));
    }

    #[test]
    fn phase1_scope_leaves_process_shell_and_files_for_future_pty_work() {
        assert!(!is_phase1_protected_command(&DeviceCommand::Shell {
            request: crate::v2_m0::ShellRequest {
                command: "true".into(),
                cwd: ".".into(),
                env: Vec::new(),
                timeout_ms: 1_000,
            },
        }));
        assert!(is_phase1_protected_command(&DeviceCommand::ScreenGeometry));
    }

    #[test]
    fn verification_candidate_requires_context_exact_window_and_no_screenshot() {
        let command = DeviceCommand::VerifyUiStateContextual {
            context_id: "ctx-1".into(),
            process_id: 12,
            window_id: 34,
            predicates: vec![UiPredicate::WindowBounds {
                bounds: UiRect {
                    x: 1,
                    y: 2,
                    width: 300,
                    height: 200,
                },
                tolerance_px: 1,
            }],
            timeout_ms: 500,
            stable_samples: 2,
            include_screenshot: false,
        };
        assert!(is_exact_verification_candidate(&command));
        let exact = exact_window_binding(&command).unwrap();
        assert_eq!(exact.process_id, 12);
        assert_eq!(exact.window_id, 34);
        assert_eq!(exact.context_binding.len(), 64);

        let mut with_screenshot = command;
        if let DeviceCommand::VerifyUiStateContextual {
            include_screenshot, ..
        } = &mut with_screenshot
        {
            *include_screenshot = true;
        }
        assert!(!is_exact_verification_candidate(&with_screenshot));
    }
    #[test]
    fn exact_window_binding_covers_bounded_advanced_input_but_rejects_desktop_or_cross_window() {
        use crate::v2_m0::{InputDeliveryMode, PointerButton};

        let click = DeviceCommand::PointerClickAdvanced {
            context_id: Some("ctx-1".into()),
            target: PointerTarget::WindowPhysical {
                process_id: 12,
                window_id: 34,
                x: 10,
                y: 20,
            },
            button: PointerButton::Left,
            click_count: 1,
            action: None,
            modifiers: Vec::new(),
            delivery: InputDeliveryMode::Foreground,
        };
        let exact = exact_window_binding(&click).unwrap();
        assert_eq!((exact.process_id, exact.window_id), (12, 34));

        let desktop = DeviceCommand::PointerClickAdvanced {
            context_id: Some("ctx-1".into()),
            target: PointerTarget::DesktopPhysical { x: 10, y: 20 },
            button: PointerButton::Left,
            click_count: 1,
            action: None,
            modifiers: Vec::new(),
            delivery: InputDeliveryMode::Foreground,
        };
        assert!(exact_window_binding(&desktop).is_none());

        let cross_window_drag = DeviceCommand::PointerDragAdvanced {
            context_id: Some("ctx-1".into()),
            from: PointerTarget::WindowPhysical {
                process_id: 12,
                window_id: 34,
                x: 10,
                y: 20,
            },
            to: PointerTarget::WindowPhysical {
                process_id: 12,
                window_id: 35,
                x: 30,
                y: 40,
            },
            button: PointerButton::Left,
            modifiers: Vec::new(),
            delivery: InputDeliveryMode::Foreground,
            duration_ms: 100,
            steps: 2,
        };
        assert!(exact_window_binding(&cross_window_drag).is_none());
    }
}
