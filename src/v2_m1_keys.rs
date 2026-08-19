//! V2-M1 key/trust-anchor file boundary.
//!
//! Replay/trust checkpoints intentionally do not contain private signing keys.
//! This module loads or creates those keys from separate files and applies
//! fail-closed filesystem checks before any key material is accepted.

use crate::v2_m0::{DeviceIdentity, GrantAuthority};
use crate::v2_m0_transport::HubIdentity;
use crate::v2_m0_trust::HubKeyRotation;
use ed25519_dalek::VerifyingKey;
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::Path;

#[cfg(unix)]
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

#[cfg(windows)]
use std::os::windows::fs::MetadataExt;
#[cfg(windows)]
use windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT;

pub const MAX_TLS_ROOT_BYTES: u64 = 1024 * 1024;

#[derive(Clone)]
pub struct AgentProvisionedMaterial {
    pub device_identity: DeviceIdentity,
    pub trusted_hub: VerifyingKey,
    pub grant_verifier: VerifyingKey,
    pub additional_grant_verifiers: Vec<VerifyingKey>,
    pub hub_rotation: Option<HubKeyRotation>,
    pub tls_root_der: Vec<u8>,
}

pub const MAX_TLS_IDENTITY_BYTES: u64 = 1024 * 1024;

pub fn load_tls_server_identity(
    certificate_pem_file: &Path,
    private_key_pem_file: &Path,
) -> Result<(Vec<u8>, Vec<u8>), KeyMaterialError> {
    validate_regular_file(certificate_pem_file, FileSensitivity::PublicTrustAnchor)?;
    validate_regular_file(private_key_pem_file, FileSensitivity::Secret)?;
    Ok((
        read_bounded(certificate_pem_file, MAX_TLS_IDENTITY_BYTES)?,
        read_bounded(private_key_pem_file, MAX_TLS_IDENTITY_BYTES)?,
    ))
}

pub fn load_agent_material(
    device_secret_file: &Path,
    hub_public_key_file: &Path,
    grant_public_key_file: &Path,
    tls_root_der_file: &Path,
) -> Result<AgentProvisionedMaterial, KeyMaterialError> {
    Ok(AgentProvisionedMaterial {
        device_identity: load_device_identity(device_secret_file)?,
        trusted_hub: load_verifying_key(hub_public_key_file)?,
        grant_verifier: load_verifying_key(grant_public_key_file)?,
        additional_grant_verifiers: Vec::new(),
        hub_rotation: None,
        tls_root_der: load_tls_root_der(tls_root_der_file)?,
    })
}

pub fn create_new_device_identity(path: &Path) -> Result<DeviceIdentity, KeyMaterialError> {
    let identity = DeviceIdentity::generate();
    write_new_secret(path, &identity.secret_key_bytes())?;
    Ok(identity)
}

pub fn create_new_hub_identity(path: &Path) -> Result<HubIdentity, KeyMaterialError> {
    let identity = HubIdentity::generate();
    write_new_secret(path, &identity.secret_key_bytes())?;
    Ok(identity)
}

pub fn create_new_grant_authority(path: &Path) -> Result<GrantAuthority, KeyMaterialError> {
    let authority = GrantAuthority::generate();
    write_new_secret(path, &authority.secret_key_bytes())?;
    Ok(authority)
}

pub fn load_device_identity(path: &Path) -> Result<DeviceIdentity, KeyMaterialError> {
    Ok(DeviceIdentity::from_secret_key_bytes(read_secret_32(path)?))
}

pub fn load_hub_identity(path: &Path) -> Result<HubIdentity, KeyMaterialError> {
    Ok(HubIdentity::from_secret_key_bytes(read_secret_32(path)?))
}

pub fn load_grant_authority(path: &Path) -> Result<GrantAuthority, KeyMaterialError> {
    Ok(GrantAuthority::from_secret_key_bytes(read_secret_32(path)?))
}

pub fn load_secret_text(path: &Path, max_bytes: u64) -> Result<String, KeyMaterialError> {
    validate_regular_file(path, FileSensitivity::Secret)?;
    let bytes = read_bounded(path, max_bytes)?;
    let text = String::from_utf8(bytes).map_err(|_| KeyMaterialError::InvalidUtf8)?;
    let value = text.trim();
    if value.is_empty() {
        return Err(KeyMaterialError::EmptySecret);
    }
    Ok(value.to_owned())
}

pub fn load_trusted_text(path: &Path, max_bytes: u64) -> Result<String, KeyMaterialError> {
    read_public_text(path, max_bytes)
}

pub fn load_tls_root_der(path: &Path) -> Result<Vec<u8>, KeyMaterialError> {
    read_public_file(path, MAX_TLS_ROOT_BYTES)
}

pub fn write_new_tls_root_der(path: &Path, value: &[u8]) -> Result<(), KeyMaterialError> {
    if value.is_empty() || u64::try_from(value.len()).unwrap_or(u64::MAX) > MAX_TLS_ROOT_BYTES {
        return Err(KeyMaterialError::FileTooLarge);
    }
    write_new_public_bytes(path, value)
}

pub fn write_new_verifying_key(
    path: &Path,
    verifier: &VerifyingKey,
) -> Result<(), KeyMaterialError> {
    write_new_public_text(path, &hex(&verifier.to_bytes()))
}

pub fn write_new_trusted_text(path: &Path, value: &str) -> Result<(), KeyMaterialError> {
    if value.is_empty() || value.len() > 64 * 1024 {
        return Err(KeyMaterialError::FileTooLarge);
    }
    write_new_public_text(path, value)
}

pub fn load_verifying_key(path: &Path) -> Result<VerifyingKey, KeyMaterialError> {
    let text = read_public_text(path, 256)?;
    let bytes = decode_hex_32(text.trim())?;
    VerifyingKey::from_bytes(&bytes).map_err(|_| KeyMaterialError::InvalidEd25519PublicKey)
}

fn read_secret_32(path: &Path) -> Result<[u8; 32], KeyMaterialError> {
    validate_regular_file(path, FileSensitivity::Secret)?;
    let bytes = read_bounded(path, 256)?;
    let text = String::from_utf8(bytes).map_err(|_| KeyMaterialError::InvalidUtf8)?;
    decode_hex_32(text.trim())
}

fn write_new_secret(path: &Path, secret: &[u8; 32]) -> Result<(), KeyMaterialError> {
    ensure_parent_is_safe(path)?;
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    options.mode(0o600);
    let mut file = options.open(path).map_err(KeyMaterialError::Io)?;
    file.write_all(hex(secret).as_bytes())
        .map_err(KeyMaterialError::Io)?;
    file.write_all(b"\n").map_err(KeyMaterialError::Io)?;
    file.flush().map_err(KeyMaterialError::Io)?;
    file.sync_all().map_err(KeyMaterialError::Io)?;
    validate_regular_file(path, FileSensitivity::Secret)?;
    sync_parent(path)?;
    Ok(())
}

fn write_new_public_bytes(path: &Path, value: &[u8]) -> Result<(), KeyMaterialError> {
    ensure_parent_is_safe(path)?;
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    options.mode(0o644);
    let mut file = options.open(path).map_err(KeyMaterialError::Io)?;
    file.write_all(value).map_err(KeyMaterialError::Io)?;
    file.flush().map_err(KeyMaterialError::Io)?;
    file.sync_all().map_err(KeyMaterialError::Io)?;
    validate_regular_file(path, FileSensitivity::PublicTrustAnchor)?;
    sync_parent(path)?;
    Ok(())
}

fn write_new_public_text(path: &Path, value: &str) -> Result<(), KeyMaterialError> {
    ensure_parent_is_safe(path)?;
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    options.mode(0o644);
    let mut file = options.open(path).map_err(KeyMaterialError::Io)?;
    file.write_all(value.as_bytes())
        .map_err(KeyMaterialError::Io)?;
    file.write_all(b"\n").map_err(KeyMaterialError::Io)?;
    file.flush().map_err(KeyMaterialError::Io)?;
    file.sync_all().map_err(KeyMaterialError::Io)?;
    validate_regular_file(path, FileSensitivity::PublicTrustAnchor)?;
    sync_parent(path)?;
    Ok(())
}

fn read_public_text(path: &Path, max_bytes: u64) -> Result<String, KeyMaterialError> {
    validate_regular_file(path, FileSensitivity::PublicTrustAnchor)?;
    let bytes = read_bounded(path, max_bytes)?;
    String::from_utf8(bytes).map_err(|_| KeyMaterialError::InvalidUtf8)
}

fn read_public_file(path: &Path, max_bytes: u64) -> Result<Vec<u8>, KeyMaterialError> {
    validate_regular_file(path, FileSensitivity::PublicTrustAnchor)?;
    read_bounded(path, max_bytes)
}

fn read_bounded(path: &Path, max_bytes: u64) -> Result<Vec<u8>, KeyMaterialError> {
    let metadata = fs::symlink_metadata(path).map_err(KeyMaterialError::Io)?;
    if metadata.len() > max_bytes {
        return Err(KeyMaterialError::FileTooLarge);
    }
    let file = File::open(path).map_err(KeyMaterialError::Io)?;
    let mut bytes = Vec::with_capacity(usize::try_from(metadata.len()).unwrap_or(0));
    file.take(max_bytes.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(KeyMaterialError::Io)?;
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > max_bytes {
        return Err(KeyMaterialError::FileTooLarge);
    }
    Ok(bytes)
}

#[derive(Debug, Clone, Copy)]
enum FileSensitivity {
    Secret,
    PublicTrustAnchor,
}

fn validate_regular_file(
    path: &Path,
    sensitivity: FileSensitivity,
) -> Result<(), KeyMaterialError> {
    let metadata = fs::symlink_metadata(path).map_err(KeyMaterialError::Io)?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(KeyMaterialError::UnsafePath);
    }
    #[cfg(unix)]
    {
        let mode = metadata.permissions().mode();
        match sensitivity {
            FileSensitivity::Secret if mode & 0o077 != 0 => {
                return Err(KeyMaterialError::UnsafeSecretPermissions);
            }
            FileSensitivity::PublicTrustAnchor if mode & 0o022 != 0 => {
                return Err(KeyMaterialError::WritableTrustAnchor);
            }
            _ => {}
        }
    }
    #[cfg(windows)]
    {
        if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return Err(KeyMaterialError::UnsafePath);
        }
        let subject = match sensitivity {
            FileSensitivity::Secret => crate::v2_windows_acl::AclSubject::SecretFile,
            FileSensitivity::PublicTrustAnchor => {
                crate::v2_windows_acl::AclSubject::PublicTrustFile
            }
        };
        crate::v2_windows_acl::validate_acl(path, subject)
            .map_err(|error| map_windows_acl_error(error, sensitivity))?;
    }
    Ok(())
}

#[cfg(windows)]
fn map_windows_acl_error(
    error: crate::v2_windows_acl::AclCheckError,
    sensitivity: FileSensitivity,
) -> KeyMaterialError {
    match error {
        crate::v2_windows_acl::AclCheckError::Io(error) => KeyMaterialError::Io(error),
        crate::v2_windows_acl::AclCheckError::UntrustedOwner
        | crate::v2_windows_acl::AclCheckError::UntrustedAccess => match sensitivity {
            FileSensitivity::Secret => KeyMaterialError::UnsafeSecretPermissions,
            FileSensitivity::PublicTrustAnchor => KeyMaterialError::WritableTrustAnchor,
        },
    }
}
fn ensure_parent_is_safe(path: &Path) -> Result<(), KeyMaterialError> {
    let parent = path.parent().ok_or(KeyMaterialError::UnsafePath)?;
    let metadata = fs::symlink_metadata(parent).map_err(KeyMaterialError::Io)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(KeyMaterialError::UnsafePath);
    }
    #[cfg(unix)]
    if metadata.permissions().mode() & 0o022 != 0 {
        return Err(KeyMaterialError::WritableParentDirectory);
    }
    #[cfg(windows)]
    {
        if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return Err(KeyMaterialError::UnsafePath);
        }
        crate::v2_windows_acl::validate_acl(
            parent,
            crate::v2_windows_acl::AclSubject::ParentDirectory,
        )
        .map_err(|error| match error {
            crate::v2_windows_acl::AclCheckError::Io(error) => KeyMaterialError::Io(error),
            crate::v2_windows_acl::AclCheckError::UntrustedOwner
            | crate::v2_windows_acl::AclCheckError::UntrustedAccess => {
                KeyMaterialError::WritableParentDirectory
            }
        })?;
    }
    Ok(())
}

fn sync_parent(path: &Path) -> Result<(), KeyMaterialError> {
    #[cfg(unix)]
    {
        let parent = path.parent().ok_or(KeyMaterialError::UnsafePath)?;
        File::open(parent)
            .and_then(|file| file.sync_all())
            .map_err(KeyMaterialError::Io)?;
    }
    Ok(())
}

fn decode_hex_32(value: &str) -> Result<[u8; 32], KeyMaterialError> {
    if value.len() != 64 {
        return Err(KeyMaterialError::InvalidHexKey);
    }
    let mut output = [0_u8; 32];
    for (index, chunk) in value.as_bytes().chunks_exact(2).enumerate() {
        let high = decode_nibble(chunk[0])?;
        let low = decode_nibble(chunk[1])?;
        output[index] = (high << 4) | low;
    }
    Ok(output)
}

fn decode_nibble(value: u8) -> Result<u8, KeyMaterialError> {
    match value {
        b'0'..=b'9' => Ok(value - b'0'),
        b'a'..=b'f' => Ok(value - b'a' + 10),
        b'A'..=b'F' => Ok(value - b'A' + 10),
        _ => Err(KeyMaterialError::InvalidHexKey),
    }
}

fn hex(bytes: &[u8]) -> String {
    const TABLE: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(TABLE[(byte >> 4) as usize] as char);
        output.push(TABLE[(byte & 0x0f) as usize] as char);
    }
    output
}

#[derive(Debug)]
pub enum KeyMaterialError {
    Io(std::io::Error),
    UnsafePath,
    UnsafeSecretPermissions,
    WritableTrustAnchor,
    WritableParentDirectory,
    FileTooLarge,
    InvalidUtf8,
    EmptySecret,
    InvalidHexKey,
    InvalidEd25519PublicKey,
}

impl fmt::Display for KeyMaterialError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self:?}")
    }
}

impl std::error::Error for KeyMaterialError {}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[cfg(windows)]
    use std::process::Command;

    #[cfg(unix)]
    use std::os::unix::fs::{PermissionsExt, symlink};

    fn temp_directory(name: &str) -> PathBuf {
        let directory = std::env::temp_dir().join(format!(
            "cumg-keys-{name}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir(&directory).unwrap();
        #[cfg(unix)]
        fs::set_permissions(&directory, fs::Permissions::from_mode(0o700)).unwrap();
        #[cfg(windows)]
        lock_down_windows_path(&directory, true);
        directory
    }

    #[cfg(windows)]
    fn windows_identity() -> String {
        let output = Command::new("whoami").output().expect("run whoami");
        assert!(output.status.success());
        String::from_utf8(output.stdout).unwrap().trim().to_owned()
    }

    #[cfg(windows)]
    fn run_icacls(path: &Path, args: &[String]) {
        let mut command = Command::new("icacls.exe");
        command.arg(path);
        for argument in args {
            command.arg(argument);
        }
        let output = command.output().expect("run icacls");
        assert!(
            output.status.success(),
            "icacls failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[cfg(windows)]
    fn lock_down_windows_path(path: &Path, directory: bool) {
        let current = windows_identity();
        let suffix = if directory { "(OI)(CI)F" } else { "F" };
        run_icacls(
            path,
            &[
                "/inheritance:r".into(),
                "/grant:r".into(),
                format!("{current}:{suffix}"),
                format!("*S-1-5-18:{suffix}"),
                format!("*S-1-5-32-544:{suffix}"),
                "/Q".into(),
            ],
        );
    }

    #[cfg(windows)]
    fn grant_builtin_users(path: &Path, rights: &str) {
        run_icacls(
            path,
            &[
                "/grant".into(),
                format!("*S-1-5-32-545:{rights}"),
                "/Q".into(),
            ],
        );
    }
    #[test]
    fn generated_device_secret_round_trips_without_checkpoint_storage() {
        let directory = temp_directory("device");
        let secret_path = directory.join("device.key");
        let identity = create_new_device_identity(&secret_path).unwrap();
        let restored = load_device_identity(&secret_path).unwrap();
        assert_eq!(identity.public_key(), restored.public_key());
        assert!(secret_path.exists());
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn public_trust_anchor_round_trips() {
        let directory = temp_directory("public");
        let path = directory.join("hub.pub");
        let hub = HubIdentity::generate();
        write_new_verifying_key(&path, &hub.verifier()).unwrap();
        assert_eq!(load_verifying_key(&path).unwrap(), hub.verifier());
        fs::remove_dir_all(directory).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn weak_secret_permissions_and_symlink_are_rejected() {
        let directory = temp_directory("permissions");
        let secret_path = directory.join("device.key");
        create_new_device_identity(&secret_path).unwrap();
        fs::set_permissions(&secret_path, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(matches!(
            load_device_identity(&secret_path),
            Err(KeyMaterialError::UnsafeSecretPermissions)
        ));

        fs::set_permissions(&secret_path, fs::Permissions::from_mode(0o600)).unwrap();
        let link = directory.join("device-link.key");
        symlink(&secret_path, &link).unwrap();
        assert!(matches!(
            load_device_identity(&link),
            Err(KeyMaterialError::UnsafePath)
        ));
        fs::remove_dir_all(directory).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn tls_server_identity_requires_secret_key_permissions_and_regular_files() {
        let directory = temp_directory("tls-server");
        let cert = directory.join("server.pem");
        let key = directory.join("server.key");
        fs::write(
            &cert,
            b"-----BEGIN CERTIFICATE-----\nplaceholder\n-----END CERTIFICATE-----\n",
        )
        .unwrap();
        fs::write(
            &key,
            b"-----BEGIN PRIVATE KEY-----\nplaceholder\n-----END PRIVATE KEY-----\n",
        )
        .unwrap();
        fs::set_permissions(&cert, fs::Permissions::from_mode(0o644)).unwrap();
        fs::set_permissions(&key, fs::Permissions::from_mode(0o600)).unwrap();
        assert!(load_tls_server_identity(&cert, &key).is_ok());

        fs::set_permissions(&key, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(matches!(
            load_tls_server_identity(&cert, &key),
            Err(KeyMaterialError::UnsafeSecretPermissions)
        ));
        fs::set_permissions(&key, fs::Permissions::from_mode(0o600)).unwrap();
        let link = directory.join("server-link.key");
        symlink(&key, &link).unwrap();
        assert!(matches!(
            load_tls_server_identity(&cert, &link),
            Err(KeyMaterialError::UnsafePath)
        ));
        fs::remove_dir_all(directory).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn writable_public_trust_anchor_is_rejected() {
        let directory = temp_directory("trust-anchor");
        let path = directory.join("hub.pub");
        let hub = HubIdentity::generate();
        write_new_verifying_key(&path, &hub.verifier()).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o666)).unwrap();
        assert!(matches!(
            load_verifying_key(&path),
            Err(KeyMaterialError::WritableTrustAnchor)
        ));
        fs::remove_dir_all(directory).unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn windows_secret_rejects_unrelated_read_or_write() {
        let directory = temp_directory("windows-secret-acl");
        let path = directory.join("device.key");
        create_new_device_identity(&path).unwrap();

        grant_builtin_users(&path, "R");
        assert!(matches!(
            load_device_identity(&path),
            Err(KeyMaterialError::UnsafeSecretPermissions)
        ));

        lock_down_windows_path(&path, false);
        grant_builtin_users(&path, "W");
        assert!(matches!(
            load_device_identity(&path),
            Err(KeyMaterialError::UnsafeSecretPermissions)
        ));
        fs::remove_dir_all(directory).unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn windows_public_trust_allows_unrelated_read_but_rejects_write() {
        let directory = temp_directory("windows-public-acl");
        let path = directory.join("hub.pub");
        let hub = HubIdentity::generate();
        write_new_verifying_key(&path, &hub.verifier()).unwrap();

        grant_builtin_users(&path, "R");
        assert_eq!(load_verifying_key(&path).unwrap(), hub.verifier());

        grant_builtin_users(&path, "W");
        assert!(matches!(
            load_verifying_key(&path),
            Err(KeyMaterialError::WritableTrustAnchor)
        ));
        fs::remove_dir_all(directory).unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn windows_inherited_writable_parent_acl_is_rejected() {
        let directory = temp_directory("windows-parent-acl");
        grant_builtin_users(&directory, "(OI)(CI)M");
        let child = directory.join("inherited");
        fs::create_dir(&child).unwrap();

        let path = child.join("device.key");
        assert!(matches!(
            create_new_device_identity(&path),
            Err(KeyMaterialError::WritableParentDirectory)
        ));
        fs::remove_dir_all(directory).unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn windows_reparse_point_parent_is_rejected() {
        let directory = temp_directory("windows-reparse");
        let target = directory.join("target");
        fs::create_dir(&target).unwrap();
        let junction = directory.join("junction");
        let status = Command::new("cmd.exe")
            .args(["/D", "/C", "mklink", "/J"])
            .arg(&junction)
            .arg(&target)
            .status()
            .expect("create junction");
        assert!(status.success());

        assert!(matches!(
            create_new_device_identity(&junction.join("device.key")),
            Err(KeyMaterialError::UnsafePath)
        ));
        fs::remove_dir_all(directory).unwrap();
    }
}
