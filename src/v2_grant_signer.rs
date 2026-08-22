//! External grant-signing boundary for the production V2 Hub.
//!
//! The Hub may retain the legacy in-process authority for explicitly single-host
//! deployments, but the packaged production path uses a separate Unix-socket
//! signer. The external protocol accepts only typed grant fields; raw bytes are
//! never supplied by the Hub for arbitrary signing. The signer independently
//! enforces an exact device/capability policy ceiling before using its key.

use crate::v2_m0::{
    CONTROL_SCHEMA_VERSION, DeviceCapability, GrantAuthority, GrantToken, MAX_GRANT_LIFETIME_MS,
    verify_grant_token_with_verifier,
};
use ed25519_dalek::VerifyingKey;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub const GRANT_SIGNER_PROTOCOL_VERSION: u16 = 1;
pub const GRANT_SIGNER_POLICY_SCHEMA_VERSION: u16 = 1;
const MAX_GRANT_SIGNER_FRAME_BYTES: usize = 16 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GrantSigningPolicyDocument {
    pub schema_version: u16,
    pub device_id: String,
    pub allowed_device_capabilities: Vec<DeviceCapability>,
    #[serde(default = "default_max_grant_lifetime_ms")]
    pub max_grant_lifetime_ms: u64,
    #[serde(default = "default_max_clock_skew_ms")]
    pub max_clock_skew_ms: u64,
}

const fn default_max_grant_lifetime_ms() -> u64 {
    MAX_GRANT_LIFETIME_MS
}

const fn default_max_clock_skew_ms() -> u64 {
    15_000
}

#[derive(Clone)]
pub struct GrantSigningPolicy {
    device_id: String,
    allowed_device_capabilities: HashSet<DeviceCapability>,
    max_grant_lifetime_ms: u64,
    max_clock_skew_ms: u64,
}

impl GrantSigningPolicy {
    pub fn from_document(document: GrantSigningPolicyDocument) -> Result<Self, GrantSignerError> {
        if document.schema_version != GRANT_SIGNER_POLICY_SCHEMA_VERSION
            || document.device_id.trim().is_empty()
            || document.device_id.len() > 256
            || document.allowed_device_capabilities.is_empty()
            || document.max_grant_lifetime_ms == 0
            || document.max_grant_lifetime_ms > MAX_GRANT_LIFETIME_MS
            || document.max_clock_skew_ms == 0
            || document.max_clock_skew_ms > 60_000
        {
            return Err(GrantSignerError::InvalidPolicy);
        }
        let allowed_device_capabilities = document
            .allowed_device_capabilities
            .into_iter()
            .collect::<HashSet<_>>();
        if allowed_device_capabilities.is_empty() {
            return Err(GrantSignerError::InvalidPolicy);
        }
        Ok(Self {
            device_id: document.device_id,
            allowed_device_capabilities,
            max_grant_lifetime_ms: document.max_grant_lifetime_ms,
            max_clock_skew_ms: document.max_clock_skew_ms,
        })
    }

    fn authorize(
        &self,
        request: &GrantSignRequest,
        signer_now_ms: u64,
    ) -> Result<(), GrantSignerError> {
        let issued_delta = request.issued_at_ms.abs_diff(signer_now_ms);
        if request.device_id != self.device_id
            || !self
                .allowed_device_capabilities
                .contains(&request.device_capability)
            || request.ttl_ms == 0
            || request.ttl_ms > self.max_grant_lifetime_ms
            || request.ttl_ms > MAX_GRANT_LIFETIME_MS
            || issued_delta > self.max_clock_skew_ms
        {
            return Err(GrantSignerError::PolicyDenied);
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct GrantSignRequest {
    protocol_version: u16,
    device_id: String,
    device_capability: DeviceCapability,
    issued_at_ms: u64,
    ttl_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct GrantSignResponse {
    protocol_version: u16,
    token: Option<GrantToken>,
    error_code: Option<String>,
}

#[derive(Clone)]
pub enum HubGrantSigner {
    InProcess(Arc<GrantAuthority>),
    #[cfg(unix)]
    ExternalUnix(Arc<UnixGrantSignerClient>),
}

impl HubGrantSigner {
    pub fn in_process(authority: GrantAuthority) -> Self {
        Self::InProcess(Arc::new(authority))
    }

    #[cfg(unix)]
    pub fn external_unix(
        socket_path: PathBuf,
        verifier: VerifyingKey,
        timeout: Duration,
    ) -> Result<Self, GrantSignerError> {
        Ok(Self::ExternalUnix(Arc::new(UnixGrantSignerClient::new(
            socket_path,
            verifier,
            timeout,
        )?)))
    }

    pub fn verifier(&self) -> VerifyingKey {
        match self {
            Self::InProcess(authority) => authority.verifier(),
            #[cfg(unix)]
            Self::ExternalUnix(client) => client.verifier(),
        }
    }

    pub async fn issue_for_device_capability(
        &self,
        device_id: &str,
        capability: DeviceCapability,
        now_ms: u64,
        ttl_ms: u64,
    ) -> Result<GrantToken, GrantSignerError> {
        match self {
            Self::InProcess(authority) => authority
                .issue_for_device_capability(device_id, capability, now_ms, ttl_ms)
                .map_err(GrantSignerError::Control),
            #[cfg(unix)]
            Self::ExternalUnix(client) => {
                client
                    .issue_for_device_capability(device_id, capability, now_ms, ttl_ms)
                    .await
            }
        }
    }
}

impl From<GrantAuthority> for HubGrantSigner {
    fn from(value: GrantAuthority) -> Self {
        Self::in_process(value)
    }
}

#[cfg(unix)]
#[derive(Clone)]
pub struct UnixGrantSignerClient {
    socket_path: PathBuf,
    verifier: VerifyingKey,
    timeout: Duration,
}

#[cfg(unix)]
impl UnixGrantSignerClient {
    pub fn new(
        socket_path: PathBuf,
        verifier: VerifyingKey,
        timeout: Duration,
    ) -> Result<Self, GrantSignerError> {
        if socket_path.as_os_str().is_empty() || timeout.is_zero() {
            return Err(GrantSignerError::InvalidConfig);
        }
        Ok(Self {
            socket_path,
            verifier,
            timeout,
        })
    }

    pub fn verifier(&self) -> VerifyingKey {
        self.verifier
    }

    pub async fn issue_for_device_capability(
        &self,
        device_id: &str,
        capability: DeviceCapability,
        now_ms: u64,
        ttl_ms: u64,
    ) -> Result<GrantToken, GrantSignerError> {
        if device_id.trim().is_empty()
            || device_id.len() > 256
            || ttl_ms == 0
            || ttl_ms > MAX_GRANT_LIFETIME_MS
        {
            return Err(GrantSignerError::InvalidRequest);
        }
        let request = GrantSignRequest {
            protocol_version: GRANT_SIGNER_PROTOCOL_VERSION,
            device_id: device_id.to_owned(),
            device_capability: capability,
            issued_at_ms: now_ms,
            ttl_ms,
        };
        let socket_path = self.socket_path.clone();
        let timeout = self.timeout;
        let response = tokio::task::spawn_blocking(move || {
            exchange_unix_request(&socket_path, timeout, &request)
        })
        .await
        .map_err(|_| GrantSignerError::Unavailable)??;
        if response.protocol_version != GRANT_SIGNER_PROTOCOL_VERSION {
            return Err(GrantSignerError::InvalidResponse);
        }
        let token = response.token.ok_or_else(|| {
            if response.error_code.as_deref() == Some("policy_denied") {
                GrantSignerError::PolicyDenied
            } else {
                GrantSignerError::Unavailable
            }
        })?;
        if response.error_code.is_some()
            || token.payload.schema_version != CONTROL_SCHEMA_VERSION
            || token.payload.device_id != device_id
            || token.payload.device_capability != Some(capability)
            || token.payload.capability != capability.class()
            || token.payload.issued_at_ms != now_ms
            || token.payload.expires_at_ms
                != now_ms
                    .checked_add(ttl_ms)
                    .ok_or(GrantSignerError::InvalidRequest)?
        {
            return Err(GrantSignerError::InvalidResponse);
        }
        verify_grant_token_with_verifier(&token, &self.verifier)
            .map_err(GrantSignerError::Control)?;
        Ok(token)
    }
}

#[cfg(unix)]
fn exchange_unix_request(
    socket_path: &Path,
    timeout: Duration,
    request: &GrantSignRequest,
) -> Result<GrantSignResponse, GrantSignerError> {
    use std::os::unix::net::UnixStream;

    let mut stream = UnixStream::connect(socket_path).map_err(|_| GrantSignerError::Unavailable)?;
    stream
        .set_read_timeout(Some(timeout))
        .map_err(|_| GrantSignerError::Unavailable)?;
    stream
        .set_write_timeout(Some(timeout))
        .map_err(|_| GrantSignerError::Unavailable)?;
    write_frame(&mut stream, request)?;
    read_frame(&mut stream)
}

#[cfg(unix)]
pub fn serve_unix_grant_signer(
    socket_path: &Path,
    authority: GrantAuthority,
    policy: GrantSigningPolicy,
) -> Result<(), GrantSignerError> {
    let (_socket_lock, listener) = bind_unix_grant_signer_listener(socket_path)?;
    for stream in listener.incoming() {
        let mut stream = stream.map_err(GrantSignerError::Io)?;
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .map_err(GrantSignerError::Io)?;
        stream
            .set_write_timeout(Some(Duration::from_secs(2)))
            .map_err(GrantSignerError::Io)?;
        let response = match read_frame::<_, GrantSignRequest>(&mut stream) {
            Ok(request) => sign_request(
                &authority,
                &policy,
                request,
                unix_time_ms().unwrap_or(u64::MAX),
            ),
            Err(error) => GrantSignResponse {
                protocol_version: GRANT_SIGNER_PROTOCOL_VERSION,
                token: None,
                error_code: Some(error.safe_error_code().to_owned()),
            },
        };
        write_frame(&mut stream, &response)?;
    }
    Ok(())
}

#[cfg(unix)]
fn sign_request(
    authority: &GrantAuthority,
    policy: &GrantSigningPolicy,
    request: GrantSignRequest,
    signer_now_ms: u64,
) -> GrantSignResponse {
    let device_id = request.device_id.clone();
    let capability = request.device_capability;
    let result = (|| {
        if request.protocol_version != GRANT_SIGNER_PROTOCOL_VERSION {
            return Err(GrantSignerError::ProtocolVersion);
        }
        policy.authorize(&request, signer_now_ms)?;
        authority
            .issue_for_device_capability(
                &request.device_id,
                request.device_capability,
                request.issued_at_ms,
                request.ttl_ms,
            )
            .map_err(GrantSignerError::Control)
    })();
    match result {
        Ok(token) => {
            crate::v2_observability::external_grant_signed();
            tracing::info!(
                event = "v2_external_grant_signed",
                device_id = %device_id,
                capability = crate::v2_observability::capability_name(capability),
                grant_id = %token.payload.grant_id,
                outcome = "signed",
                "external signing authority issued an exact-capability grant"
            );
            GrantSignResponse {
                protocol_version: GRANT_SIGNER_PROTOCOL_VERSION,
                token: Some(token),
                error_code: None,
            }
        }
        Err(error) => {
            crate::v2_observability::external_grant_rejected();
            tracing::warn!(
                event = "v2_external_grant_rejected",
                device_id = %device_id,
                capability = crate::v2_observability::capability_name(capability),
                outcome = "rejected",
                error_code = error.safe_error_code(),
                "external signing authority rejected a grant request"
            );
            GrantSignResponse {
                protocol_version: GRANT_SIGNER_PROTOCOL_VERSION,
                token: None,
                error_code: Some(error.safe_error_code().to_owned()),
            }
        }
    }
}

#[cfg(unix)]
fn bind_unix_grant_signer_listener(
    socket_path: &Path,
) -> Result<(std::fs::File, std::os::unix::net::UnixListener), GrantSignerError> {
    use fs2::FileExt as _;
    use std::fs::OpenOptions;
    use std::io::{ErrorKind, Write as _};
    use std::os::unix::fs::{FileTypeExt, OpenOptionsExt, PermissionsExt};
    use std::os::unix::net::{UnixListener, UnixStream};

    let parent = socket_path
        .parent()
        .ok_or(GrantSignerError::UnsafeSocketPath)?;
    let parent_metadata = std::fs::symlink_metadata(parent).map_err(GrantSignerError::Io)?;
    if parent_metadata.file_type().is_symlink()
        || !parent_metadata.is_dir()
        || parent_metadata.permissions().mode() & 0o022 != 0
    {
        return Err(GrantSignerError::UnsafeSocketPath);
    }

    // launchd leaves Unix-domain socket pathnames behind when a process is killed.
    // Serialize startup with a private advisory lock, then remove only a proven
    // stale socket. A live listener is never unlinked or replaced.
    let lock_path = parent.join(".cumg-v2-grant-signer.lock");
    match std::fs::symlink_metadata(&lock_path) {
        Ok(metadata)
            if metadata.file_type().is_symlink()
                || !metadata.is_file()
                || metadata.permissions().mode() & 0o077 != 0 =>
        {
            return Err(GrantSignerError::UnsafeSocketPath);
        }
        Ok(_) => {}
        Err(error) if error.kind() == ErrorKind::NotFound => {}
        Err(error) => return Err(GrantSignerError::Io(error)),
    }
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true).mode(0o600);
    let lock = options.open(&lock_path).map_err(GrantSignerError::Io)?;
    let lock_metadata = lock.metadata().map_err(GrantSignerError::Io)?;
    if !lock_metadata.is_file() || lock_metadata.permissions().mode() & 0o077 != 0 {
        return Err(GrantSignerError::UnsafeSocketPath);
    }
    match lock.try_lock_exclusive() {
        Ok(()) => {}
        Err(error) if error.kind() == ErrorKind::WouldBlock => {
            return Err(GrantSignerError::Unavailable);
        }
        Err(error) => return Err(GrantSignerError::Io(error)),
    }

    match std::fs::symlink_metadata(socket_path) {
        Ok(metadata) => {
            if metadata.file_type().is_symlink() || !metadata.file_type().is_socket() {
                return Err(GrantSignerError::UnsafeSocketPath);
            }
            match UnixStream::connect(socket_path) {
                Ok(mut stream) => {
                    // Do not simply connect-and-drop: an older signer may try to
                    // write its bounded protocol error to the closed peer and
                    // treat that BrokenPipe as a process-level failure. Send the
                    // deliberately invalid zero-length frame and keep the peer
                    // open long enough to consume the error response. This probe
                    // cannot request or produce a grant.
                    let probe_timeout = Duration::from_millis(250);
                    let _ = stream.set_read_timeout(Some(probe_timeout));
                    let _ = stream.set_write_timeout(Some(probe_timeout));
                    let _ = stream.write_all(&0_u32.to_be_bytes());
                    let _ = stream.flush();
                    let _ = read_frame::<_, GrantSignResponse>(&mut stream);
                    return Err(GrantSignerError::Unavailable);
                }
                Err(error) if error.kind() == ErrorKind::ConnectionRefused => {
                    std::fs::remove_file(socket_path).map_err(GrantSignerError::Io)?;
                }
                Err(error) if error.kind() == ErrorKind::NotFound => {}
                Err(error) => return Err(GrantSignerError::Io(error)),
            }
        }
        Err(error) if error.kind() == ErrorKind::NotFound => {}
        Err(error) => return Err(GrantSignerError::Io(error)),
    }

    let listener = UnixListener::bind(socket_path).map_err(GrantSignerError::Io)?;
    std::fs::set_permissions(socket_path, std::fs::Permissions::from_mode(0o660))
        .map_err(GrantSignerError::Io)?;
    Ok((lock, listener))
}

#[cfg(unix)]
fn write_frame<W: std::io::Write, T: Serialize>(
    writer: &mut W,
    value: &T,
) -> Result<(), GrantSignerError> {
    let bytes = serde_json::to_vec(value).map_err(|_| GrantSignerError::Serialization)?;
    if bytes.is_empty() || bytes.len() > MAX_GRANT_SIGNER_FRAME_BYTES {
        return Err(GrantSignerError::FrameTooLarge);
    }
    let length = u32::try_from(bytes.len()).map_err(|_| GrantSignerError::FrameTooLarge)?;
    writer
        .write_all(&length.to_be_bytes())
        .map_err(GrantSignerError::Io)?;
    writer.write_all(&bytes).map_err(GrantSignerError::Io)?;
    writer.flush().map_err(GrantSignerError::Io)
}

#[cfg(unix)]
fn read_frame<R: std::io::Read, T: for<'de> Deserialize<'de>>(
    reader: &mut R,
) -> Result<T, GrantSignerError> {
    let mut length = [0_u8; 4];
    reader
        .read_exact(&mut length)
        .map_err(GrantSignerError::Io)?;
    let length =
        usize::try_from(u32::from_be_bytes(length)).map_err(|_| GrantSignerError::FrameTooLarge)?;
    if length == 0 || length > MAX_GRANT_SIGNER_FRAME_BYTES {
        return Err(GrantSignerError::FrameTooLarge);
    }
    let mut bytes = vec![0_u8; length];
    reader
        .read_exact(&mut bytes)
        .map_err(GrantSignerError::Io)?;
    serde_json::from_slice(&bytes).map_err(|_| GrantSignerError::Serialization)
}

fn unix_time_ms() -> Result<u64, GrantSignerError> {
    let duration = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| GrantSignerError::InvalidSystemClock)?;
    u64::try_from(duration.as_millis()).map_err(|_| GrantSignerError::InvalidSystemClock)
}

#[derive(Debug)]
pub enum GrantSignerError {
    InvalidConfig,
    InvalidPolicy,
    InvalidRequest,
    PolicyDenied,
    ProtocolVersion,
    InvalidResponse,
    Unavailable,
    FrameTooLarge,
    Serialization,
    UnsafeSocketPath,
    InvalidSystemClock,
    Io(std::io::Error),
    Control(crate::v2_m0::ControlError),
}

impl GrantSignerError {
    pub const fn safe_error_code(&self) -> &'static str {
        match self {
            Self::InvalidConfig => "grant_signer_invalid_config",
            Self::InvalidPolicy => "grant_signer_invalid_policy",
            Self::InvalidRequest => "grant_signer_invalid_request",
            Self::PolicyDenied => "policy_denied",
            Self::ProtocolVersion => "grant_signer_protocol_version",
            Self::InvalidResponse => "grant_signer_invalid_response",
            Self::Unavailable => "grant_signer_unavailable",
            Self::FrameTooLarge => "grant_signer_frame_too_large",
            Self::Serialization => "grant_signer_serialization",
            Self::UnsafeSocketPath => "grant_signer_unsafe_socket_path",
            Self::InvalidSystemClock => "grant_signer_invalid_system_clock",
            Self::Io(_) => "grant_signer_io",
            Self::Control(_) => "grant_signer_control",
        }
    }
}

impl fmt::Display for GrantSignerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.safe_error_code())
    }
}

impl std::error::Error for GrantSignerError {}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::net::UnixListener;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_dir(label: &str) -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("cumg-{label}-{unique}"))
    }

    fn policy(device_id: &str) -> GrantSigningPolicy {
        GrantSigningPolicy::from_document(GrantSigningPolicyDocument {
            schema_version: GRANT_SIGNER_POLICY_SCHEMA_VERSION,
            device_id: device_id.into(),
            allowed_device_capabilities: vec![DeviceCapability::Screenshot],
            max_grant_lifetime_ms: 30_000,
            max_clock_skew_ms: 15_000,
        })
        .unwrap()
    }

    #[tokio::test]
    async fn external_client_never_receives_key_and_has_no_in_process_fallback() {
        use std::os::unix::fs::PermissionsExt;

        let root = temp_dir("grant-signer");
        std::fs::create_dir(&root).unwrap();
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
        let socket = root.join("signer.sock");
        let authority = GrantAuthority::generate();
        let verifier = authority.verifier();
        let listener = UnixListener::bind(&socket).unwrap();
        std::fs::set_permissions(&socket, std::fs::Permissions::from_mode(0o660)).unwrap();
        let policy = policy("dev-test");
        let signer_now_ms = unix_time_ms().unwrap();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let request = read_frame::<_, GrantSignRequest>(&mut stream).unwrap();
            let response = sign_request(&authority, &policy, request, signer_now_ms);
            write_frame(&mut stream, &response).unwrap();
        });
        let client =
            UnixGrantSignerClient::new(socket.clone(), verifier, Duration::from_secs(1)).unwrap();
        let now_ms = unix_time_ms().unwrap();
        let token = client
            .issue_for_device_capability("dev-test", DeviceCapability::Screenshot, now_ms, 10_000)
            .await
            .unwrap();
        assert_eq!(
            token.payload.device_capability,
            Some(DeviceCapability::Screenshot)
        );
        server.join().unwrap();

        // Once the external authority is absent there is deliberately no local
        // key or signing fallback in this client.
        assert!(matches!(
            client
                .issue_for_device_capability(
                    "dev-test",
                    DeviceCapability::Screenshot,
                    unix_time_ms().unwrap(),
                    10_000,
                )
                .await,
            Err(GrantSignerError::Unavailable)
        ));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn signer_listener_recovers_stale_socket_but_never_replaces_live_listener() {
        use std::os::unix::fs::PermissionsExt;
        use std::os::unix::net::UnixListener;

        let root = std::env::temp_dir().join(format!(
            "cgs-{}-{}",
            std::process::id(),
            rand::random::<u32>()
        ));
        std::fs::create_dir(&root).unwrap();
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();

        let stale_socket = root.join("stale.sock");
        let stale = UnixListener::bind(&stale_socket).unwrap();
        drop(stale);
        assert!(stale_socket.exists());
        let (lock, listener) = bind_unix_grant_signer_listener(&stale_socket).unwrap();
        assert!(stale_socket.exists());
        assert!(matches!(
            bind_unix_grant_signer_listener(&stale_socket),
            Err(GrantSignerError::Unavailable)
        ));
        drop(listener);
        drop(lock);

        // Simulate the pre-lock signer version. The new startup probe must send
        // a complete invalid frame, consume the bounded error response, and then
        // refuse to unlink the still-live listener.
        let live_socket = root.join("live.sock");
        let legacy_listener = UnixListener::bind(&live_socket).unwrap();
        let legacy = std::thread::spawn(move || {
            let (mut stream, _) = legacy_listener.accept().unwrap();
            let error = read_frame::<_, GrantSignRequest>(&mut stream).unwrap_err();
            assert!(matches!(error, GrantSignerError::FrameTooLarge));
            write_frame(
                &mut stream,
                &GrantSignResponse {
                    protocol_version: GRANT_SIGNER_PROTOCOL_VERSION,
                    token: None,
                    error_code: Some(error.safe_error_code().to_owned()),
                },
            )
            .unwrap();
        });
        assert!(matches!(
            bind_unix_grant_signer_listener(&live_socket),
            Err(GrantSignerError::Unavailable)
        ));
        legacy.join().unwrap();
        assert!(live_socket.exists());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn signer_policy_is_an_independent_exact_capability_ceiling() {
        let authority = GrantAuthority::generate();
        let policy = policy("dev-test");
        let denied = sign_request(
            &authority,
            &policy,
            GrantSignRequest {
                protocol_version: GRANT_SIGNER_PROTOCOL_VERSION,
                device_id: "dev-test".into(),
                device_capability: DeviceCapability::Shell,
                issued_at_ms: 1_000,
                ttl_ms: 10_000,
            },
            1_000,
        );
        assert!(denied.token.is_none());
        assert_eq!(denied.error_code.as_deref(), Some("policy_denied"));

        let future = sign_request(
            &authority,
            &policy,
            GrantSignRequest {
                protocol_version: GRANT_SIGNER_PROTOCOL_VERSION,
                device_id: "dev-test".into(),
                device_capability: DeviceCapability::Screenshot,
                issued_at_ms: 100_000,
                ttl_ms: 10_000,
            },
            1_000,
        );
        assert!(future.token.is_none());
        assert_eq!(future.error_code.as_deref(), Some("policy_denied"));
    }
}
