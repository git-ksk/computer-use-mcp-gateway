//! Optional Linux FIDO2/CTAP2 recovery provider.
//!
//! The provider delegates authenticator I/O to a pinned, root-owned libfido2
//! toolchain while keeping the Hub-facing proof contract provider-neutral.
//! PINs are never accepted through configuration, environment, argv, or CUMG
//! stdin; libfido2 obtains them directly from the controlling tty.

#![cfg_attr(not(target_os = "linux"), allow(dead_code, unused_imports))]

use crate::v2_online_recovery::{
    RecoveryAuthorization, RecoveryError, RecoveryVerifier, WEBAUTHN_FLAG_UP, WEBAUTHN_FLAG_UV,
    WEBAUTHN_RECOVERY_RP_ID, WebAuthnRecoveryVerifierDocument, attach_webauthn_assertion_proof,
    webauthn_client_data_json,
};
use base64::{
    Engine as _,
    engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD},
};
use rand::{RngCore as _, rngs::OsRng};
use ring::digest;
use std::path::{Path, PathBuf};
use x509_parser::{
    oid_registry::OID_EC_P256, pem::parse_x509_pem, prelude::FromDer, public_key::PublicKey,
    x509::SubjectPublicKeyInfo,
};

const MIN_LIBFIDO2_VERSION: (u64, u64, u64) = (1, 17, 0);
const MAX_TOOL_OUTPUT_BYTES: usize = 64 * 1024;
#[cfg(target_os = "linux")]
const TOOL_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);
const MAX_CREDENTIAL_ID_BYTES: usize = 1024;
const MAX_AUTHENTICATOR_DATA_BYTES: usize = 4096;
const MAX_ASSERTION_SIGNATURE_BYTES: usize = 256;
const MAX_PUBLIC_KEY_PEM_BYTES: usize = 4096;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinuxFido2UvMode {
    /// CTAP2 Client PIN / PIN-UV Auth token. The PIN is entered only on the
    /// controlling tty owned by libfido2.
    Pin,
    /// Authenticator-integrated user verification such as a biometric sensor.
    Builtin,
}

impl LinuxFido2UvMode {
    pub const fn name(self) -> &'static str {
        match self {
            Self::Pin => "pin",
            Self::Builtin => "builtin",
        }
    }

    fn tool_toggle(self) -> &'static str {
        match self {
            Self::Pin => "pin=true",
            Self::Builtin => "uv=true",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct DeviceCapabilities {
    fido2: bool,
    es256: bool,
    up: bool,
    client_pin: bool,
    builtin_uv: bool,
}

#[derive(Debug, Clone)]
pub struct LinuxFido2Recovery {
    tool_dir: PathBuf,
    device: PathBuf,
    uv_mode: LinuxFido2UvMode,
    credential_id: Vec<u8>,
}

impl LinuxFido2Recovery {
    pub fn create_new(
        tool_dir: &Path,
        device: &Path,
        uv_mode: LinuxFido2UvMode,
    ) -> Result<(Self, WebAuthnRecoveryVerifierDocument), RecoveryError> {
        #[cfg(target_os = "linux")]
        {
            let provider = Self::preflight(tool_dir, device, uv_mode, Vec::new())?;
            let mut client_hash = [0_u8; 32];
            let mut user_id = [0_u8; 32];
            OsRng.fill_bytes(&mut client_hash);
            OsRng.fill_bytes(&mut user_id);
            let input = format!(
                "{}\n{}\n{}\n{}\n",
                STANDARD.encode(client_hash),
                WEBAUTHN_RECOVERY_RP_ID,
                "cumg-recovery",
                STANDARD.encode(user_id),
            );
            let input_file = PrivateTempFile::with_bytes("cred-input", input.as_bytes())?;
            let creation_output = run_tool(
                &provider.tool_dir.join("fido2-cred"),
                provider.make_credential_args(input_file.path()),
            )?;
            validate_creation_echo(&creation_output, &client_hash)?;
            let creation_file = PrivateTempFile::with_bytes("cred-created", &creation_output)?;
            let verifier_file = PrivateTempFile::empty("cred-verifier")?;
            run_tool_discard_stdout(
                &provider.tool_dir.join("fido2-cred"),
                vec![
                    "-V".into(),
                    "-v".into(),
                    "-i".into(),
                    creation_file.path().as_os_str().to_owned(),
                    "-o".into(),
                    verifier_file.path().as_os_str().to_owned(),
                    "es256".into(),
                ],
            )?;
            let verified = std::fs::read(verifier_file.path())
                .map_err(|_| RecoveryError::PlatformAuthenticatorFailure)?;
            let (credential_id, public_key) = parse_verified_credential(&verified)?;
            let verifier = RecoveryVerifier::from_webauthn_es256(&credential_id, &public_key)?;
            let document = verifier
                .webauthn_document()
                .ok_or(RecoveryError::InvalidPublicKey)?;
            let provider = Self {
                credential_id,
                ..provider
            };
            Ok((provider, document))
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = (tool_dir, device, uv_mode);
            Err(RecoveryError::UnsupportedPlatform)
        }
    }

    pub fn from_verifier_document(
        tool_dir: &Path,
        device: &Path,
        uv_mode: LinuxFido2UvMode,
        verifier_bytes: &[u8],
    ) -> Result<Self, RecoveryError> {
        let verifier = RecoveryVerifier::from_webauthn_document(verifier_bytes)?;
        let document = verifier
            .webauthn_document()
            .ok_or(RecoveryError::InvalidPublicKey)?;
        let credential_id = URL_SAFE_NO_PAD
            .decode(document.credential_id_base64url)
            .map_err(|_| RecoveryError::InvalidPublicKey)?;
        if credential_id.is_empty() || credential_id.len() > MAX_CREDENTIAL_ID_BYTES {
            return Err(RecoveryError::InvalidPublicKey);
        }
        #[cfg(target_os = "linux")]
        {
            Self::preflight(tool_dir, device, uv_mode, credential_id)
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = (tool_dir, device, uv_mode, credential_id);
            Err(RecoveryError::UnsupportedPlatform)
        }
    }

    pub fn sign_authorization(
        &self,
        authorization: RecoveryAuthorization,
    ) -> Result<RecoveryAuthorization, RecoveryError> {
        #[cfg(target_os = "linux")]
        {
            if !authorization.signature.is_empty() {
                return Err(RecoveryError::InvalidMessage);
            }
            self.ensure_current_preflight()?;
            let client_data = webauthn_client_data_json(&authorization)?;
            let client_hash = digest::digest(&digest::SHA256, &client_data);
            let input = format!(
                "{}\n{}\n{}\n",
                STANDARD.encode(client_hash.as_ref()),
                WEBAUTHN_RECOVERY_RP_ID,
                STANDARD.encode(&self.credential_id),
            );
            let input_file = PrivateTempFile::with_bytes("assert-input", input.as_bytes())?;
            let output = run_tool(
                &self.tool_dir.join("fido2-assert"),
                self.assertion_args(input_file.path()),
            )?;
            let assertion = parse_assertion_output(&output, client_hash.as_ref())?;
            attach_webauthn_assertion_proof(
                authorization,
                &self.credential_id,
                &assertion.authenticator_data,
                &assertion.signature,
            )
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = authorization;
            Err(RecoveryError::UnsupportedPlatform)
        }
    }

    #[cfg(target_os = "linux")]
    fn preflight(
        tool_dir: &Path,
        device: &Path,
        uv_mode: LinuxFido2UvMode,
        credential_id: Vec<u8>,
    ) -> Result<Self, RecoveryError> {
        validate_toolchain(tool_dir)?;
        validate_device(device)?;
        if uv_mode == LinuxFido2UvMode::Pin {
            validate_pin_tty()?;
        }
        let version = run_tool(&tool_dir.join("fido2-token"), vec!["-V".into()])?;
        let parsed = parse_semver(&version).ok_or(RecoveryError::PlatformAuthenticatorFailure)?;
        if parsed < MIN_LIBFIDO2_VERSION {
            return Err(RecoveryError::PlatformUserVerificationUnavailable);
        }
        let info = run_tool(
            &tool_dir.join("fido2-token"),
            vec!["-I".into(), device.as_os_str().to_owned()],
        )?;
        let capabilities = parse_device_capabilities(&info)?;
        if !capabilities.fido2 || !capabilities.es256 || !capabilities.up {
            return Err(RecoveryError::PlatformUserVerificationUnavailable);
        }
        match uv_mode {
            LinuxFido2UvMode::Pin if !capabilities.client_pin => {
                return Err(RecoveryError::PlatformUserVerificationUnavailable);
            }
            LinuxFido2UvMode::Builtin if !capabilities.builtin_uv => {
                return Err(RecoveryError::PlatformUserVerificationUnavailable);
            }
            _ => {}
        }
        Ok(Self {
            tool_dir: tool_dir.to_path_buf(),
            device: device.to_path_buf(),
            uv_mode,
            credential_id,
        })
    }

    #[cfg(target_os = "linux")]
    fn ensure_current_preflight(&self) -> Result<(), RecoveryError> {
        let current = Self::preflight(
            &self.tool_dir,
            &self.device,
            self.uv_mode,
            self.credential_id.clone(),
        )?;
        if current.device != self.device || current.uv_mode != self.uv_mode {
            return Err(RecoveryError::PlatformAuthenticatorFailure);
        }
        Ok(())
    }

    fn make_credential_args(&self, input_file: &Path) -> Vec<std::ffi::OsString> {
        let mut args = vec![
            "-M".into(),
            // A discoverable credential makes libfido2 reject any accidental
            // CTAP1/U2F fallback during creation rather than registering a U2F key.
            "-r".into(),
            "-t".into(),
            self.uv_mode.tool_toggle().into(),
            "-i".into(),
            input_file.as_os_str().to_owned(),
        ];
        if self.uv_mode == LinuxFido2UvMode::Builtin {
            // Do not let a built-in-UV request silently turn into a PIN flow.
            args.push("-q".into());
        }
        args.push(self.device.as_os_str().to_owned());
        args.push("es256".into());
        args
    }

    fn assertion_args(&self, input_file: &Path) -> Vec<std::ffi::OsString> {
        vec![
            "-G".into(),
            "-p".into(),
            "-t".into(),
            self.uv_mode.tool_toggle().into(),
            "-i".into(),
            input_file.as_os_str().to_owned(),
            self.device.as_os_str().to_owned(),
        ]
    }
}

#[cfg(target_os = "linux")]
fn validate_toolchain(tool_dir: &Path) -> Result<(), RecoveryError> {
    use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
    if !tool_dir.is_absolute() {
        return Err(RecoveryError::InvalidPath);
    }
    let metadata = std::fs::symlink_metadata(tool_dir)
        .map_err(|_| RecoveryError::PlatformAuthenticatorFailure)?;
    if metadata.file_type().is_symlink()
        || !metadata.is_dir()
        || metadata.uid() != 0
        || metadata.permissions().mode() & 0o022 != 0
    {
        return Err(RecoveryError::PlatformAuthenticatorFailure);
    }
    for name in ["fido2-token", "fido2-cred", "fido2-assert"] {
        let path = tool_dir.join(name);
        let metadata = std::fs::symlink_metadata(&path)
            .map_err(|_| RecoveryError::PlatformAuthenticatorFailure)?;
        if metadata.file_type().is_symlink()
            || !metadata.is_file()
            || metadata.uid() != 0
            || metadata.permissions().mode() & 0o111 == 0
            || metadata.permissions().mode() & 0o022 != 0
        {
            return Err(RecoveryError::PlatformAuthenticatorFailure);
        }
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn validate_device(device: &Path) -> Result<(), RecoveryError> {
    use std::os::unix::fs::FileTypeExt as _;
    if !device.is_absolute() || !device.starts_with("/dev") {
        return Err(RecoveryError::InvalidPath);
    }
    let metadata = std::fs::symlink_metadata(device)
        .map_err(|_| RecoveryError::PlatformUserVerificationUnavailable)?;
    if metadata.file_type().is_symlink() || !metadata.file_type().is_char_device() {
        return Err(RecoveryError::PlatformUserVerificationUnavailable);
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn validate_pin_tty() -> Result<(), RecoveryError> {
    std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/tty")
        .map(|_| ())
        .map_err(|_| RecoveryError::PlatformUserVerificationUnavailable)
}

#[cfg(target_os = "linux")]
fn run_tool(path: &Path, args: Vec<std::ffi::OsString>) -> Result<Vec<u8>, RecoveryError> {
    use std::{
        io::Read as _,
        process::{Command, Stdio},
        thread,
        time::{Duration, Instant},
    };

    let mut child = Command::new(path)
        .args(args)
        .env_clear()
        .env("LANG", "C")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        // Provider-private diagnostics may contain device-specific information;
        // never surface them through CUMG errors or logs.
        .stderr(Stdio::null())
        .spawn()
        .map_err(|_| RecoveryError::PlatformAuthenticatorFailure)?;
    let mut stdout = child
        .stdout
        .take()
        .ok_or(RecoveryError::PlatformAuthenticatorFailure)?;
    let reader = thread::spawn(move || {
        let mut kept = Vec::new();
        let mut oversized = false;
        let mut chunk = [0_u8; 4096];
        loop {
            match stdout.read(&mut chunk) {
                Ok(0) => return Ok::<_, std::io::Error>((kept, oversized)),
                Ok(count) => {
                    let remaining = MAX_TOOL_OUTPUT_BYTES
                        .saturating_add(1)
                        .saturating_sub(kept.len());
                    let copy = count.min(remaining);
                    kept.extend_from_slice(&chunk[..copy]);
                    oversized |= kept.len() > MAX_TOOL_OUTPUT_BYTES || count > copy;
                }
                Err(error) => return Err(error),
            }
        }
    });

    let deadline = Instant::now() + TOOL_TIMEOUT;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(25)),
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                let _ = reader.join();
                return Err(RecoveryError::PlatformAuthenticatorFailure);
            }
            Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                let _ = reader.join();
                return Err(RecoveryError::PlatformAuthenticatorFailure);
            }
        }
    };
    let (output, oversized) = reader
        .join()
        .map_err(|_| RecoveryError::PlatformAuthenticatorFailure)?
        .map_err(|_| RecoveryError::PlatformAuthenticatorFailure)?;
    if !status.success() || oversized {
        return Err(RecoveryError::PlatformAuthenticatorFailure);
    }
    Ok(output)
}

#[cfg(target_os = "linux")]
fn run_tool_discard_stdout(
    path: &Path,
    args: Vec<std::ffi::OsString>,
) -> Result<(), RecoveryError> {
    let output = run_tool(path, args)?;
    if !output.is_empty() {
        return Err(RecoveryError::PlatformAuthenticatorFailure);
    }
    Ok(())
}

#[cfg(target_os = "linux")]
struct PrivateTempFile {
    path: PathBuf,
}

#[cfg(target_os = "linux")]
impl PrivateTempFile {
    fn with_bytes(label: &str, bytes: &[u8]) -> Result<Self, RecoveryError> {
        use std::{fs::OpenOptions, io::Write as _, os::unix::fs::OpenOptionsExt as _};
        for _ in 0..16 {
            let mut nonce = [0_u8; 16];
            OsRng.fill_bytes(&mut nonce);
            let path = std::env::temp_dir().join(format!(
                "cumg-{label}-{}",
                nonce.iter().map(|b| format!("{b:02x}")).collect::<String>()
            ));
            let mut options = OpenOptions::new();
            options.write(true).create_new(true).mode(0o600);
            match options.open(&path) {
                Ok(mut file) => {
                    file.write_all(bytes).map_err(|_| RecoveryError::Io)?;
                    file.sync_all().map_err(|_| RecoveryError::Io)?;
                    return Ok(Self { path });
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(_) => return Err(RecoveryError::Io),
            }
        }
        Err(RecoveryError::Io)
    }

    fn empty(label: &str) -> Result<Self, RecoveryError> {
        Self::with_bytes(label, &[])
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

#[cfg(target_os = "linux")]
impl Drop for PrivateTempFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

fn parse_semver(bytes: &[u8]) -> Option<(u64, u64, u64)> {
    let text = std::str::from_utf8(bytes).ok()?;
    for token in text.split(|c: char| !(c.is_ascii_digit() || c == '.')) {
        let mut parts = token.split('.');
        let Some(major) = parts.next().and_then(|v| v.parse().ok()) else {
            continue;
        };
        let Some(minor) = parts.next().and_then(|v| v.parse().ok()) else {
            continue;
        };
        let Some(patch) = parts.next().and_then(|v| v.parse().ok()) else {
            continue;
        };
        if parts.next().is_none() {
            return Some((major, minor, patch));
        }
    }
    None
}

fn parse_device_capabilities(bytes: &[u8]) -> Result<DeviceCapabilities, RecoveryError> {
    if bytes.len() > MAX_TOOL_OUTPUT_BYTES {
        return Err(RecoveryError::PlatformAuthenticatorFailure);
    }
    let text =
        std::str::from_utf8(bytes).map_err(|_| RecoveryError::PlatformAuthenticatorFailure)?;
    let mut fido2 = false;
    let mut es256 = false;
    let mut up = false;
    let mut client_pin = false;
    let mut builtin_uv = false;
    for line in text.lines() {
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let key = key.trim();
        let values = value.split(',').map(str::trim).filter(|v| !v.is_empty());
        match key {
            "version strings" | "versions" => {
                fido2 |= values.clone().any(|v| v.starts_with("FIDO_2_"));
            }
            "algorithms" => {
                es256 |= values
                    .clone()
                    .any(|v| v == "es256" || v.starts_with("es256 "));
            }
            "options" => {
                for option in values {
                    match option {
                        "up" => up = true,
                        "clientPin" => client_pin = true,
                        "uv" => builtin_uv = true,
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }
    Ok(DeviceCapabilities {
        fido2,
        es256,
        up,
        client_pin,
        builtin_uv,
    })
}

fn validate_creation_echo(bytes: &[u8], expected_hash: &[u8; 32]) -> Result<(), RecoveryError> {
    if bytes.len() > MAX_TOOL_OUTPUT_BYTES {
        return Err(RecoveryError::PlatformAuthenticatorFailure);
    }
    let text =
        std::str::from_utf8(bytes).map_err(|_| RecoveryError::PlatformAuthenticatorFailure)?;
    let mut lines = text.lines();
    let hash = decode_standard_bounded(lines.next(), 32)?;
    if hash.as_slice() != expected_hash {
        return Err(RecoveryError::PlatformAuthenticatorFailure);
    }
    if lines.next() != Some(WEBAUTHN_RECOVERY_RP_ID) {
        return Err(RecoveryError::WebAuthnRpMismatch);
    }
    Ok(())
}

fn parse_verified_credential(bytes: &[u8]) -> Result<(Vec<u8>, [u8; 65]), RecoveryError> {
    if bytes.is_empty() || bytes.len() > MAX_TOOL_OUTPUT_BYTES {
        return Err(RecoveryError::PlatformAuthenticatorFailure);
    }
    let newline = bytes
        .iter()
        .position(|b| *b == b'\n')
        .ok_or(RecoveryError::PlatformAuthenticatorFailure)?;
    let credential_id = decode_standard_bounded(
        std::str::from_utf8(&bytes[..newline]).ok(),
        MAX_CREDENTIAL_ID_BYTES,
    )?;
    let public_key = parse_es256_public_key_pem(&bytes[newline + 1..])?;
    Ok((credential_id, public_key))
}

struct ParsedAssertion {
    authenticator_data: Vec<u8>,
    signature: Vec<u8>,
}

fn parse_assertion_output(
    bytes: &[u8],
    expected_hash: &[u8],
) -> Result<ParsedAssertion, RecoveryError> {
    if bytes.len() > MAX_TOOL_OUTPUT_BYTES {
        return Err(RecoveryError::PlatformAuthenticatorFailure);
    }
    let text =
        std::str::from_utf8(bytes).map_err(|_| RecoveryError::PlatformAuthenticatorFailure)?;
    let lines: Vec<&str> = text.lines().collect();
    if lines.len() != 4 {
        return Err(RecoveryError::PlatformAuthenticatorFailure);
    }
    let echoed_hash = decode_standard_bounded(Some(lines[0]), 32)?;
    if echoed_hash != expected_hash || lines[1] != WEBAUTHN_RECOVERY_RP_ID {
        return Err(RecoveryError::WebAuthnRpMismatch);
    }
    let cbor = decode_standard_bounded(Some(lines[2]), MAX_AUTHENTICATOR_DATA_BYTES + 16)?;
    let authenticator_data = decode_cbor_byte_string(&cbor, MAX_AUTHENTICATOR_DATA_BYTES)?;
    if authenticator_data.len() < 37 {
        return Err(RecoveryError::InvalidWebAuthnProof);
    }
    let flags = authenticator_data[32];
    if flags & WEBAUTHN_FLAG_UP == 0 || flags & WEBAUTHN_FLAG_UV == 0 {
        return Err(RecoveryError::WebAuthnUserVerificationRequired);
    }
    let signature = decode_standard_bounded(Some(lines[3]), MAX_ASSERTION_SIGNATURE_BYTES)?;
    Ok(ParsedAssertion {
        authenticator_data,
        signature,
    })
}

fn decode_standard_bounded(value: Option<&str>, max: usize) -> Result<Vec<u8>, RecoveryError> {
    let value = value.ok_or(RecoveryError::PlatformAuthenticatorFailure)?;
    if value.is_empty() || value.len() > max.saturating_mul(2).saturating_add(16) {
        return Err(RecoveryError::PlatformAuthenticatorFailure);
    }
    let decoded = STANDARD
        .decode(value)
        .map_err(|_| RecoveryError::PlatformAuthenticatorFailure)?;
    if decoded.is_empty() || decoded.len() > max {
        return Err(RecoveryError::PlatformAuthenticatorFailure);
    }
    Ok(decoded)
}

fn decode_cbor_byte_string(bytes: &[u8], max: usize) -> Result<Vec<u8>, RecoveryError> {
    let first = *bytes.first().ok_or(RecoveryError::InvalidWebAuthnProof)?;
    if first >> 5 != 2 {
        return Err(RecoveryError::InvalidWebAuthnProof);
    }
    let (length, header) = match first & 0x1f {
        n @ 0..=23 => (usize::from(n), 1),
        24 if bytes.len() >= 2 => (usize::from(bytes[1]), 2),
        25 if bytes.len() >= 3 => (usize::from(u16::from_be_bytes([bytes[1], bytes[2]])), 3),
        26 if bytes.len() >= 5 => (
            usize::try_from(u32::from_be_bytes([bytes[1], bytes[2], bytes[3], bytes[4]]))
                .map_err(|_| RecoveryError::InvalidWebAuthnProof)?,
            5,
        ),
        _ => return Err(RecoveryError::InvalidWebAuthnProof),
    };
    if length == 0 || length > max || bytes.len() != header + length {
        return Err(RecoveryError::InvalidWebAuthnProof);
    }
    Ok(bytes[header..].to_vec())
}

fn parse_es256_public_key_pem(bytes: &[u8]) -> Result<[u8; 65], RecoveryError> {
    if bytes.is_empty() || bytes.len() > MAX_PUBLIC_KEY_PEM_BYTES {
        return Err(RecoveryError::InvalidPublicKey);
    }
    let (remainder, pem) = parse_x509_pem(bytes).map_err(|_| RecoveryError::InvalidPublicKey)?;
    if !remainder.iter().all(u8::is_ascii_whitespace) || pem.label != "PUBLIC KEY" {
        return Err(RecoveryError::InvalidPublicKey);
    }
    let (remainder, spki) = SubjectPublicKeyInfo::from_der(&pem.contents)
        .map_err(|_| RecoveryError::InvalidPublicKey)?;
    if !remainder.is_empty() {
        return Err(RecoveryError::InvalidPublicKey);
    }
    let curve = spki
        .algorithm
        .parameters()
        .ok_or(RecoveryError::InvalidPublicKey)?
        .as_oid()
        .map_err(|_| RecoveryError::InvalidPublicKey)?;
    if curve != OID_EC_P256 {
        return Err(RecoveryError::InvalidPublicKey);
    }
    let point = match spki.parsed().map_err(|_| RecoveryError::InvalidPublicKey)? {
        PublicKey::EC(point) => point,
        _ => return Err(RecoveryError::InvalidPublicKey),
    };
    let public_key: [u8; 65] = point
        .data()
        .try_into()
        .map_err(|_| RecoveryError::InvalidPublicKey)?;
    if public_key[0] != 0x04 {
        return Err(RecoveryError::InvalidPublicKey);
    }
    Ok(public_key)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ring::{
        rand::SystemRandom,
        signature::{ECDSA_P256_SHA256_ASN1_SIGNING, EcdsaKeyPair, KeyPair as _},
    };

    #[test]
    fn parses_current_libfido2_version() {
        assert_eq!(parse_semver(b"fido2-token 1.17.0\n"), Some((1, 17, 0)));
        assert_eq!(parse_semver(b"libfido2-1.18.2"), Some((1, 18, 2)));
    }

    #[test]
    fn command_contract_never_requests_u2f_and_keeps_uv_mode_explicit() {
        let provider = |uv_mode| LinuxFido2Recovery {
            tool_dir: PathBuf::from("/usr/bin"),
            device: PathBuf::from("/dev/hidraw7"),
            uv_mode,
            credential_id: vec![1, 2, 3],
        };
        let input = Path::new("/tmp/input");
        let pin_make: Vec<String> = provider(LinuxFido2UvMode::Pin)
            .make_credential_args(input)
            .into_iter()
            .map(|v| v.to_string_lossy().into_owned())
            .collect();
        assert!(pin_make.windows(2).any(|w| w == ["-t", "pin=true"]));
        assert!(pin_make.iter().any(|v| v == "-r"));
        assert!(!pin_make.iter().any(|v| v == "-u"));
        assert!(!pin_make.iter().any(|v| v == "-q"));

        let builtin_make: Vec<String> = provider(LinuxFido2UvMode::Builtin)
            .make_credential_args(input)
            .into_iter()
            .map(|v| v.to_string_lossy().into_owned())
            .collect();
        assert!(builtin_make.windows(2).any(|w| w == ["-t", "uv=true"]));
        assert!(builtin_make.iter().any(|v| v == "-r"));
        assert!(builtin_make.iter().any(|v| v == "-q"));
        assert!(!builtin_make.iter().any(|v| v == "-u"));

        let pin_assert: Vec<String> = provider(LinuxFido2UvMode::Pin)
            .assertion_args(input)
            .into_iter()
            .map(|v| v.to_string_lossy().into_owned())
            .collect();
        assert!(pin_assert.iter().any(|v| v == "-p"));
        assert!(pin_assert.windows(2).any(|w| w == ["-t", "pin=true"]));
        assert!(!pin_assert.iter().any(|v| v == "-u"));
    }

    #[test]
    fn device_preflight_distinguishes_pin_and_builtin_uv() {
        let pin = parse_device_capabilities(b"version strings: U2F_V2, FIDO_2_0\nalgorithms: es256 (public-key)\noptions: rk, up, noplat, clientPin\n").unwrap();
        assert!(pin.fido2 && pin.es256 && pin.up && pin.client_pin && !pin.builtin_uv);
        let bio = parse_device_capabilities(
            b"versions: FIDO_2_1\nalgorithms: es256 (public-key)\noptions: up, uv, noclientPin\n",
        )
        .unwrap();
        assert!(bio.fido2 && bio.es256 && bio.up && !bio.client_pin && bio.builtin_uv);
    }

    #[test]
    fn cbor_authenticator_data_unwrap_is_exact_and_bounded() {
        let raw = vec![7_u8; 37];
        let mut cbor = vec![0x58, 37];
        cbor.extend_from_slice(&raw);
        assert_eq!(decode_cbor_byte_string(&cbor, 4096).unwrap(), raw);
        let mut trailing = cbor.clone();
        trailing.push(0);
        assert_eq!(
            decode_cbor_byte_string(&trailing, 4096),
            Err(RecoveryError::InvalidWebAuthnProof)
        );
        assert_eq!(
            decode_cbor_byte_string(&[0x9f, 0xff], 4096),
            Err(RecoveryError::InvalidWebAuthnProof)
        );
    }

    #[test]
    fn parses_only_p256_subject_public_key_info() {
        let rng = SystemRandom::new();
        let pkcs8 = EcdsaKeyPair::generate_pkcs8(&ECDSA_P256_SHA256_ASN1_SIGNING, &rng).unwrap();
        let key = EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_ASN1_SIGNING, pkcs8.as_ref(), &rng)
            .unwrap();
        let point = key.public_key().as_ref();
        let mut der = vec![
            0x30, 0x59, 0x30, 0x13, 0x06, 0x07, 0x2a, 0x86, 0x48, 0xce, 0x3d, 0x02, 0x01, 0x06,
            0x08, 0x2a, 0x86, 0x48, 0xce, 0x3d, 0x03, 0x01, 0x07, 0x03, 0x42, 0x00,
        ];
        der.extend_from_slice(point);
        let encoded = STANDARD.encode(der);
        let pem = format!("-----BEGIN PUBLIC KEY-----\n{encoded}\n-----END PUBLIC KEY-----\n");
        assert_eq!(
            parse_es256_public_key_pem(pem.as_bytes())
                .unwrap()
                .as_slice(),
            point
        );
    }

    #[test]
    fn assertion_parser_rejects_echo_or_cbor_shape_changes() {
        let hash = [3_u8; 32];
        let mut raw = vec![9_u8; 37];
        raw[32] = WEBAUTHN_FLAG_UP | WEBAUTHN_FLAG_UV;
        let mut cbor = vec![0x58, 37];
        cbor.extend_from_slice(&raw);
        let output = format!(
            "{}\n{}\n{}\n{}\n",
            STANDARD.encode(hash),
            WEBAUTHN_RECOVERY_RP_ID,
            STANDARD.encode(cbor),
            STANDARD.encode([0x30, 0x00])
        );
        let parsed = parse_assertion_output(output.as_bytes(), &hash).unwrap();
        assert_eq!(parsed.authenticator_data, raw);
        let wrong = output.replace(WEBAUTHN_RECOVERY_RP_ID, "wrong.invalid");
        assert_eq!(
            parse_assertion_output(wrong.as_bytes(), &hash).err(),
            Some(RecoveryError::WebAuthnRpMismatch)
        );

        let mut no_uv_raw = raw.clone();
        no_uv_raw[32] = WEBAUTHN_FLAG_UP;
        let mut no_uv_cbor = vec![0x58, 37];
        no_uv_cbor.extend_from_slice(&no_uv_raw);
        let no_uv = format!(
            "{}\n{}\n{}\n{}\n",
            STANDARD.encode(hash),
            WEBAUTHN_RECOVERY_RP_ID,
            STANDARD.encode(no_uv_cbor),
            STANDARD.encode([0x30, 0x00])
        );
        assert_eq!(
            parse_assertion_output(no_uv.as_bytes(), &hash).err(),
            Some(RecoveryError::WebAuthnUserVerificationRequired)
        );
    }
}
