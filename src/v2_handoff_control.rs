//! Local operator-only control plane for the Hub relay to the Agent-owned Handoff coordinator.
//!
//! This is deliberately separate from northbound MCP. The socket is a private local Unix
//! endpoint; callers can request lifecycle transitions but cannot supply principal/device/Window
//! authority. Exact target authority comes only from CUMG's coordinator selection state.

use crate::{
    v2_handoff_coordinator::{
        HandoffCoordinator, HandoffOperatorCommand, HandoffOperatorControlError,
        HandoffSessionFence,
    },
    v2_m1_hub::HubHandle,
    v2_operator_handoff::HandoffRuntimeStatus,
};
use serde::{Deserialize, Serialize};
use std::{
    fmt,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

const MAX_CONTROL_FRAME_BYTES: usize = 8 * 1024;
const CONTROL_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum LocalHandoffControlRequest {
    Status,
    Begin,
    RecoverReissue,
    RecoverRebind {
        prior_context_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        prior_generation: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        prior_capability_revision: Option<u64>,
    },
    RebindLive,
    AbandonExpiredRecovery {
        expected_epoch: u64,
    },
    RequestResume,
    CancelBeforeHuman,
}

impl From<LocalHandoffControlRequest> for HandoffOperatorCommand {
    fn from(value: LocalHandoffControlRequest) -> Self {
        match value {
            LocalHandoffControlRequest::Status => Self::Status,
            LocalHandoffControlRequest::Begin => Self::Begin,
            LocalHandoffControlRequest::RecoverReissue => Self::RecoverReissue,
            LocalHandoffControlRequest::RecoverRebind {
                prior_context_id,
                prior_generation,
                prior_capability_revision,
            } => Self::RecoverRebind {
                prior_context_id,
                prior_generation,
                prior_capability_revision,
            },
            LocalHandoffControlRequest::RebindLive => Self::RebindLive,
            LocalHandoffControlRequest::AbandonExpiredRecovery { expected_epoch } => {
                Self::AbandonExpiredRecovery { expected_epoch }
            }
            LocalHandoffControlRequest::RequestResume => Self::RequestResume,
            LocalHandoffControlRequest::CancelBeforeHuman => Self::CancelBeforeHuman,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LocalHandoffControlResponse {
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<HandoffRuntimeStatus>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_code: Option<String>,
}

impl LocalHandoffControlResponse {
    fn success(status: HandoffRuntimeStatus) -> Self {
        Self {
            ok: true,
            status: Some(status),
            error_code: None,
        }
    }

    fn rejected(error: HandoffOperatorControlError) -> Self {
        Self {
            ok: false,
            status: None,
            error_code: Some(error.safe_code().to_owned()),
        }
    }

    fn protocol_error() -> Self {
        Self {
            ok: false,
            status: None,
            error_code: Some("handoff_control_protocol_invalid".to_owned()),
        }
    }
}

#[derive(Debug)]
pub enum HandoffLocalControlError {
    UnsafeSocketPath,
    Unavailable,
    Protocol,
    Io(std::io::Error),
}

impl fmt::Display for HandoffLocalControlError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::UnsafeSocketPath => "handoff control socket path is unsafe",
            Self::Unavailable => "handoff control socket unavailable",
            Self::Protocol => "handoff control protocol invalid",
            Self::Io(_) => "handoff control I/O failed",
        })
    }
}

impl std::error::Error for HandoffLocalControlError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            _ => None,
        }
    }
}

fn parse_local_request(
    bytes: &[u8],
) -> Result<LocalHandoffControlRequest, HandoffLocalControlError> {
    let value: serde_json::Value =
        serde_json::from_slice(bytes).map_err(|_| HandoffLocalControlError::Protocol)?;
    let object = value
        .as_object()
        .ok_or(HandoffLocalControlError::Protocol)?;
    let action = object
        .get("action")
        .and_then(serde_json::Value::as_str)
        .ok_or(HandoffLocalControlError::Protocol)?;
    let allowed: &[&str] = match action {
        "status"
        | "begin"
        | "recover_reissue"
        | "rebind_live"
        | "request_resume"
        | "cancel_before_human" => &["action"],
        "recover_rebind" => &[
            "action",
            "prior_context_id",
            "prior_generation",
            "prior_capability_revision",
        ],
        "abandon_expired_recovery" => &["action", "expected_epoch"],
        _ => return Err(HandoffLocalControlError::Protocol),
    };
    if object.keys().any(|key| !allowed.contains(&key.as_str())) {
        return Err(HandoffLocalControlError::Protocol);
    }
    serde_json::from_value(value).map_err(|_| HandoffLocalControlError::Protocol)
}

#[cfg(unix)]
pub struct UnixHandoffControlServer {
    listener: tokio::net::UnixListener,
    socket_path: PathBuf,
    _lock: std::fs::File,
}

#[cfg(unix)]
impl UnixHandoffControlServer {
    pub fn bind(socket_path: &Path) -> Result<Self, HandoffLocalControlError> {
        use fs2::FileExt as _;
        use std::{
            fs::OpenOptions,
            io::ErrorKind,
            os::unix::{
                fs::{FileTypeExt, OpenOptionsExt, PermissionsExt},
                net::{UnixListener, UnixStream},
            },
        };

        if !socket_path.is_absolute() {
            return Err(HandoffLocalControlError::UnsafeSocketPath);
        }
        let parent = socket_path
            .parent()
            .ok_or(HandoffLocalControlError::UnsafeSocketPath)?;
        let parent_metadata =
            std::fs::symlink_metadata(parent).map_err(HandoffLocalControlError::Io)?;
        if parent_metadata.file_type().is_symlink()
            || !parent_metadata.is_dir()
            || parent_metadata.permissions().mode() & 0o022 != 0
        {
            return Err(HandoffLocalControlError::UnsafeSocketPath);
        }

        let lock_path = parent.join(".cumg-v2-handoff-control.lock");
        match std::fs::symlink_metadata(&lock_path) {
            Ok(metadata)
                if metadata.file_type().is_symlink()
                    || !metadata.is_file()
                    || metadata.permissions().mode() & 0o077 != 0 =>
            {
                return Err(HandoffLocalControlError::UnsafeSocketPath);
            }
            Ok(_) => {}
            Err(error) if error.kind() == ErrorKind::NotFound => {}
            Err(error) => return Err(HandoffLocalControlError::Io(error)),
        }
        let mut options = OpenOptions::new();
        options.read(true).write(true).create(true).mode(0o600);
        let lock = options
            .open(lock_path)
            .map_err(HandoffLocalControlError::Io)?;
        let lock_metadata = lock.metadata().map_err(HandoffLocalControlError::Io)?;
        if !lock_metadata.is_file() || lock_metadata.permissions().mode() & 0o077 != 0 {
            return Err(HandoffLocalControlError::UnsafeSocketPath);
        }
        match lock.try_lock_exclusive() {
            Ok(()) => {}
            Err(error) if error.kind() == ErrorKind::WouldBlock => {
                return Err(HandoffLocalControlError::Unavailable);
            }
            Err(error) => return Err(HandoffLocalControlError::Io(error)),
        }

        match std::fs::symlink_metadata(socket_path) {
            Ok(metadata) => {
                if metadata.file_type().is_symlink() || !metadata.file_type().is_socket() {
                    return Err(HandoffLocalControlError::UnsafeSocketPath);
                }
                match UnixStream::connect(socket_path) {
                    Ok(_) => return Err(HandoffLocalControlError::Unavailable),
                    Err(error) if error.kind() == ErrorKind::ConnectionRefused => {
                        std::fs::remove_file(socket_path).map_err(HandoffLocalControlError::Io)?;
                    }
                    Err(error) if error.kind() == ErrorKind::NotFound => {}
                    Err(error) => return Err(HandoffLocalControlError::Io(error)),
                }
            }
            Err(error) if error.kind() == ErrorKind::NotFound => {}
            Err(error) => return Err(HandoffLocalControlError::Io(error)),
        }

        let listener = UnixListener::bind(socket_path).map_err(HandoffLocalControlError::Io)?;
        std::fs::set_permissions(socket_path, std::fs::Permissions::from_mode(0o600))
            .map_err(HandoffLocalControlError::Io)?;
        listener
            .set_nonblocking(true)
            .map_err(HandoffLocalControlError::Io)?;
        let listener =
            tokio::net::UnixListener::from_std(listener).map_err(HandoffLocalControlError::Io)?;
        Ok(Self {
            listener,
            socket_path: socket_path.to_owned(),
            _lock: lock,
        })
    }

    pub async fn serve(
        self,
        coordinator: Arc<HandoffCoordinator>,
        hub: HubHandle,
        mut shutdown: tokio::sync::watch::Receiver<bool>,
    ) -> Result<(), HandoffLocalControlError> {
        loop {
            tokio::select! {
                changed = shutdown.changed() => {
                    if changed.is_err() || *shutdown.borrow() {
                        return Ok(());
                    }
                }
                accepted = self.listener.accept() => {
                    let (stream, _) = accepted.map_err(HandoffLocalControlError::Io)?;
                    let coordinator = coordinator.clone();
                    let hub = hub.clone();
                    tokio::spawn(async move {
                        let _ = handle_connection(stream, coordinator, hub).await;
                    });
                }
            }
        }
    }
}

#[cfg(unix)]
impl Drop for UnixHandoffControlServer {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.socket_path);
    }
}

#[cfg(unix)]
async fn handle_connection(
    stream: tokio::net::UnixStream,
    coordinator: Arc<HandoffCoordinator>,
    hub: HubHandle,
) -> Result<(), HandoffLocalControlError> {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

    let (reader, mut writer) = stream.into_split();
    let mut reader = BufReader::new(reader);
    let mut line = Vec::new();
    let read = tokio::time::timeout(CONTROL_TIMEOUT, reader.read_until(b'\n', &mut line))
        .await
        .map_err(|_| HandoffLocalControlError::Unavailable)?
        .map_err(HandoffLocalControlError::Io)?;
    let response = if read == 0 || read > MAX_CONTROL_FRAME_BYTES || line.last() != Some(&b'\n') {
        LocalHandoffControlResponse::protocol_error()
    } else {
        line.pop();
        match parse_local_request(&line) {
            Ok(request) => {
                let session =
                    hub.current_session_binding()
                        .await
                        .map(|(generation, capabilities)| HandoffSessionFence {
                            generation,
                            capability_revision: capabilities.revision,
                        });
                match coordinator.operator_control(request.into(), session).await {
                    Ok(status) => LocalHandoffControlResponse::success(status),
                    Err(error) => LocalHandoffControlResponse::rejected(error),
                }
            }
            Err(_) => LocalHandoffControlResponse::protocol_error(),
        }
    };
    let mut encoded =
        serde_json::to_vec(&response).map_err(|_| HandoffLocalControlError::Protocol)?;
    encoded.push(b'\n');
    if encoded.len() > MAX_CONTROL_FRAME_BYTES {
        return Err(HandoffLocalControlError::Protocol);
    }
    tokio::time::timeout(CONTROL_TIMEOUT, writer.write_all(&encoded))
        .await
        .map_err(|_| HandoffLocalControlError::Unavailable)?
        .map_err(HandoffLocalControlError::Io)?;
    writer
        .shutdown()
        .await
        .map_err(HandoffLocalControlError::Io)
}

#[cfg(unix)]
pub fn exchange_unix_handoff_control(
    socket_path: &Path,
    request: &LocalHandoffControlRequest,
) -> Result<LocalHandoffControlResponse, HandoffLocalControlError> {
    use std::{
        io::{BufRead, BufReader, Write},
        os::unix::net::UnixStream,
    };

    if !socket_path.is_absolute() {
        return Err(HandoffLocalControlError::UnsafeSocketPath);
    }
    let mut encoded =
        serde_json::to_vec(request).map_err(|_| HandoffLocalControlError::Protocol)?;
    encoded.push(b'\n');
    if encoded.len() > MAX_CONTROL_FRAME_BYTES {
        return Err(HandoffLocalControlError::Protocol);
    }
    let mut stream =
        UnixStream::connect(socket_path).map_err(|_| HandoffLocalControlError::Unavailable)?;
    stream
        .set_read_timeout(Some(CONTROL_TIMEOUT))
        .map_err(HandoffLocalControlError::Io)?;
    stream
        .set_write_timeout(Some(CONTROL_TIMEOUT))
        .map_err(HandoffLocalControlError::Io)?;
    stream
        .write_all(&encoded)
        .map_err(HandoffLocalControlError::Io)?;
    stream.flush().map_err(HandoffLocalControlError::Io)?;
    let mut reader = BufReader::new(stream);
    let mut response = Vec::new();
    let read = reader
        .read_until(b'\n', &mut response)
        .map_err(HandoffLocalControlError::Io)?;
    if read == 0 || read > MAX_CONTROL_FRAME_BYTES || response.last() != Some(&b'\n') {
        return Err(HandoffLocalControlError::Protocol);
    }
    response.pop();
    serde_json::from_slice(&response).map_err(|_| HandoffLocalControlError::Protocol)
}

#[cfg(not(unix))]
pub fn exchange_unix_handoff_control(
    _socket_path: &Path,
    _request: &LocalHandoffControlRequest,
) -> Result<LocalHandoffControlResponse, HandoffLocalControlError> {
    Err(HandoffLocalControlError::Unavailable)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_request_surface_never_accepts_target_or_principal_authority() {
        for field in [
            "principal_binding",
            "device_binding",
            "generation",
            "process_id",
            "window_id",
        ] {
            let payload = format!(r#"{{"action":"begin","{field}":1}}"#);
            assert!(parse_local_request(payload.as_bytes()).is_err());
        }
        assert_eq!(
            parse_local_request(br#"{"action":"begin"}"#).unwrap(),
            LocalHandoffControlRequest::Begin
        );
        assert_eq!(
            parse_local_request(br#"{"action":"abandon_expired_recovery","expected_epoch":7}"#)
                .unwrap(),
            LocalHandoffControlRequest::AbandonExpiredRecovery { expected_epoch: 7 }
        );
        assert!(
            parse_local_request(
                br#"{"action":"abandon_expired_recovery","expected_epoch":7,"process_id":1}"#
            )
            .is_err()
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn control_socket_rejects_world_writable_parent_and_live_listener() {
        use rand::RngCore;
        use std::os::unix::fs::PermissionsExt;

        let mut suffix = [0_u8; 8];
        rand::thread_rng().fill_bytes(&mut suffix);
        let root = PathBuf::from(format!(
            "/tmp/cumg-hc-{}-{}",
            std::process::id(),
            u64::from_le_bytes(suffix)
        ));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o777)).unwrap();
        let socket = root.join("control.sock");
        assert!(matches!(
            UnixHandoffControlServer::bind(&socket),
            Err(HandoffLocalControlError::UnsafeSocketPath)
        ));
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
        let first = UnixHandoffControlServer::bind(&socket).unwrap();
        assert!(matches!(
            UnixHandoffControlServer::bind(&socket),
            Err(HandoffLocalControlError::Unavailable)
        ));
        drop(first);
        std::fs::remove_dir_all(root).unwrap();
    }
}
