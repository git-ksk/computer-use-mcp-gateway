//! V2-M1 key/trust-anchor file boundary.
//!
//! Replay/trust checkpoints intentionally do not contain private signing keys.
//! This module loads or creates those keys from separate files and applies
//! fail-closed filesystem checks before any key material is accepted.

use crate::v2_m0::{DeviceIdentity, GrantAuthority};
use crate::v2_m0_transport::HubIdentity;
use ed25519_dalek::VerifyingKey;
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::Path;

#[cfg(unix)]
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

pub const MAX_TLS_ROOT_BYTES: u64 = 1024 * 1024;

#[derive(Clone)]
pub struct AgentProvisionedMaterial {
    pub device_identity: DeviceIdentity,
    pub trusted_hub: VerifyingKey,
    pub grant_verifier: VerifyingKey,
    pub tls_root_der: Vec<u8>,
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
        tls_root_der: read_public_file(tls_root_der_file, MAX_TLS_ROOT_BYTES)?,
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

pub fn write_new_verifying_key(
    path: &Path,
    verifier: &VerifyingKey,
) -> Result<(), KeyMaterialError> {
    write_new_public_text(path, &hex(&verifier.to_bytes()))
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
    Ok(())
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
        directory
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
}
