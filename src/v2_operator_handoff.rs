//! Optional consumer-owned bridge to `mcp-execution-handoff`.
//!
//! This module deliberately contains only bounded control-plane metadata. It does not carry CUMG
//! grants, operation payloads/results, screenshots, clipboard data, Human input, or recovery
//! authority. The Handoff process owns Agent/Human authority and epochs; CUMG remains authoritative
//! for principal/device/capability authorization, dispatch, durable execution state, quarantine,
//! replay admission, and postcondition verification.

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
    time::Duration,
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
impl OperatorHandoffAuthority for UnixOperatorHandoffAuthority {
    async fn admit_agent(
        &self,
        request: AgentAuthorityRequest,
    ) -> Result<AgentAuthorityDecision, OperatorHandoffError> {
        let response = self
            .exchange(WireRequest::AdmitAgent {
                protocol: PROTOCOL_VERSION,
                principal_binding: request.principal_binding,
                device_binding: request.device_binding,
                generation: request.generation,
                capability_revision: request.capability_revision,
                exact_window: request.exact_window.map(WireExactWindow::from),
                verification_candidate: request.verification_candidate,
            })
            .await?;
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

    async fn report_verification(
        &self,
        report: VerificationReport,
    ) -> Result<(), OperatorHandoffError> {
        let response = self
            .exchange(WireRequest::ReportVerification {
                protocol: PROTOCOL_VERSION,
                principal_binding: report.authority.principal_binding,
                device_binding: report.authority.device_binding,
                generation: report.authority.generation,
                capability_revision: report.authority.capability_revision,
                exact_window: report.authority.exact_window.map(WireExactWindow::from),
                intervention_id: report.token.intervention_id,
                epoch: report.token.epoch,
                satisfied: report.satisfied,
            })
            .await?;
        if response.ok && response.decision.is_none() {
            Ok(())
        } else {
            Err(OperatorHandoffError::Protocol)
        }
    }
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
