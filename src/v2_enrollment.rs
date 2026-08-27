use crate::v2_m0::DeviceRegistry;
use crate::v2_m1_keys::{
    KeyMaterialError, create_new_device_identity, load_tls_root_der, load_verifying_key,
    write_new_tls_root_der, write_new_trusted_text, write_new_verifying_key,
};
use crate::v2_tls_lifecycle::{
    CertificateFormat, CertificateHealth, TlsLifecycleError, inspect_certificate_bytes,
};
use serde::{Deserialize, Serialize};
use std::fmt;
use std::fs;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

pub const ENROLLMENT_MANIFEST_SCHEMA_VERSION: u16 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentEnrollmentManifest {
    pub schema_version: u16,
    pub device_id: String,
    pub agent_device_secret_file: String,
    pub agent_hub_public_key_file: String,
    pub agent_grant_public_key_file: String,
    pub agent_tls_root_der_file: String,
    pub hub_device_public_key_file: String,
}

pub fn prepare_agent_enrollment(
    output_dir: &Path,
    hub_public_key_file: &Path,
    grant_public_key_file: &Path,
    tls_root_der_file: &Path,
) -> Result<AgentEnrollmentManifest, EnrollmentError> {
    let hub_verifier = load_verifying_key(hub_public_key_file).map_err(EnrollmentError::Key)?;
    let grant_verifier = load_verifying_key(grant_public_key_file).map_err(EnrollmentError::Key)?;
    let tls_root_der = load_tls_root_der(tls_root_der_file).map_err(EnrollmentError::Key)?;
    let now = current_unix_secs()?;
    let tls = inspect_certificate_bytes(&tls_root_der, CertificateFormat::Der, 0, now)
        .map_err(EnrollmentError::Tls)?;
    if !matches!(
        tls.health,
        CertificateHealth::Healthy | CertificateHealth::Expiring
    ) {
        return Err(EnrollmentError::InactiveTlsRoot);
    }

    ensure_safe_parent(output_dir)?;
    create_private_dir(output_dir)?;
    let result =
        prepare_created_directory(output_dir, &hub_verifier, &grant_verifier, &tls_root_der);
    if result.is_err() {
        let _ = fs::remove_dir_all(output_dir);
    }
    result
}

fn prepare_created_directory(
    output_dir: &Path,
    hub_verifier: &ed25519_dalek::VerifyingKey,
    grant_verifier: &ed25519_dalek::VerifyingKey,
    tls_root_der: &[u8],
) -> Result<AgentEnrollmentManifest, EnrollmentError> {
    let agent_dir = output_dir.join("agent");
    let agent_secrets = agent_dir.join("secrets");
    let agent_trust = agent_dir.join("trust");
    let hub_dir = output_dir.join("hub");
    for directory in [&agent_dir, &agent_secrets, &agent_trust, &hub_dir] {
        create_private_dir(directory)?;
    }

    let device_secret = agent_secrets.join("device.key");
    let device_identity =
        create_new_device_identity(&device_secret).map_err(EnrollmentError::Key)?;
    let hub_device_public = hub_dir.join("device.pub");
    write_new_verifying_key(&hub_device_public, &device_identity.verifying_key())
        .map_err(EnrollmentError::Key)?;
    write_new_verifying_key(&agent_trust.join("hub.pub"), hub_verifier)
        .map_err(EnrollmentError::Key)?;
    write_new_verifying_key(&agent_trust.join("grant.pub"), grant_verifier)
        .map_err(EnrollmentError::Key)?;
    write_new_tls_root_der(&agent_trust.join("tls-root.der"), tls_root_der)
        .map_err(EnrollmentError::Key)?;

    let mut registry = DeviceRegistry::default();
    let device_id = registry.provision_trusted_device(device_identity.verifying_key());
    let manifest = AgentEnrollmentManifest {
        schema_version: ENROLLMENT_MANIFEST_SCHEMA_VERSION,
        device_id,
        agent_device_secret_file: "agent/secrets/device.key".into(),
        agent_hub_public_key_file: "agent/trust/hub.pub".into(),
        agent_grant_public_key_file: "agent/trust/grant.pub".into(),
        agent_tls_root_der_file: "agent/trust/tls-root.der".into(),
        hub_device_public_key_file: "hub/device.pub".into(),
    };
    let manifest_json = serde_json::to_string_pretty(&manifest).map_err(EnrollmentError::Json)?;
    write_new_trusted_text(&output_dir.join("enrollment.json"), &manifest_json)
        .map_err(EnrollmentError::Key)?;
    Ok(manifest)
}

fn ensure_safe_parent(path: &Path) -> Result<(), EnrollmentError> {
    let parent = path
        .parent()
        .ok_or(EnrollmentError::UnsafeParentDirectory)?;
    let metadata = fs::symlink_metadata(parent).map_err(EnrollmentError::Io)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(EnrollmentError::UnsafeParentDirectory);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o022 != 0 {
            return Err(EnrollmentError::UnsafeParentDirectory);
        }
    }
    Ok(())
}

fn create_private_dir(path: &Path) -> Result<(), EnrollmentError> {
    let mut builder = fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(path).map_err(EnrollmentError::Io)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))
            .map_err(EnrollmentError::Io)?;
    }
    Ok(())
}

fn current_unix_secs() -> Result<i64, EnrollmentError> {
    let duration = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| EnrollmentError::InvalidSystemClock)?;
    i64::try_from(duration.as_secs()).map_err(|_| EnrollmentError::InvalidSystemClock)
}

#[derive(Debug)]
pub enum EnrollmentError {
    Io(std::io::Error),
    Key(KeyMaterialError),
    Tls(TlsLifecycleError),
    Json(serde_json::Error),
    InactiveTlsRoot,
    InvalidSystemClock,
    UnsafeParentDirectory,
}

impl fmt::Display for EnrollmentError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(_) => f.write_str("enrollment filesystem operation failed"),
            Self::Key(error) => write!(f, "enrollment key/trust material failed: {error}"),
            Self::Tls(error) => write!(f, "enrollment TLS root failed: {error}"),
            Self::Json(_) => f.write_str("enrollment manifest serialization failed"),
            Self::InactiveTlsRoot => f.write_str("enrollment TLS root is not currently valid"),
            Self::InvalidSystemClock => f.write_str("system clock is invalid"),
            Self::UnsafeParentDirectory => {
                f.write_str("enrollment output parent must be a private non-symlink directory")
            }
        }
    }
}

impl std::error::Error for EnrollmentError {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::v2_m0::GrantAuthority;
    use crate::v2_m0_transport::HubIdentity;
    use crate::v2_m1_keys::{
        load_agent_material, load_device_identity, load_verifying_key, write_new_verifying_key,
    };
    use rcgen::{CertifiedKey, generate_simple_self_signed};
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[cfg(windows)]
    use std::process::Command;

    fn temp_dir(label: &str) -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("cumg-{label}-{unique}"))
    }

    #[cfg(windows)]
    fn lock_down_windows_test_directory(path: &Path) {
        let identity = Command::new("whoami").output().expect("run whoami");
        assert!(identity.status.success());
        let identity = String::from_utf8(identity.stdout)
            .unwrap()
            .trim()
            .to_owned();
        let output = Command::new("icacls.exe")
            .arg(path)
            .args([
                "/inheritance:r",
                "/grant:r",
                &format!("{identity}:(OI)(CI)F"),
                "*S-1-5-18:(OI)(CI)F",
                "*S-1-5-32-544:(OI)(CI)F",
                "/Q",
            ])
            .output()
            .expect("run icacls");
        assert!(output.status.success());
    }
    #[test]
    fn enrollment_bundle_is_repeatable_safe_and_matches_hub_registration() {
        let root = temp_dir("enrollment");
        fs::create_dir(&root).unwrap();
        #[cfg(windows)]
        lock_down_windows_test_directory(&root);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        }
        let hub = HubIdentity::generate();
        let grants = GrantAuthority::generate();
        let hub_public = root.join("source-hub.pub");
        let grant_public = root.join("source-grant.pub");
        let tls_root = root.join("source-root.der");
        write_new_verifying_key(&hub_public, &hub.verifier()).unwrap();
        write_new_verifying_key(&grant_public, &grants.verifier()).unwrap();
        let CertifiedKey { cert, .. } =
            generate_simple_self_signed(vec!["localhost".into()]).unwrap();
        fs::write(&tls_root, cert.der().as_ref()).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&tls_root, fs::Permissions::from_mode(0o644)).unwrap();
        }

        let output = root.join("bundle");
        let manifest =
            prepare_agent_enrollment(&output, &hub_public, &grant_public, &tls_root).unwrap();
        assert_eq!(manifest.schema_version, ENROLLMENT_MANIFEST_SCHEMA_VERSION);
        assert!(!manifest.device_id.is_empty());

        let agent = load_agent_material(
            &output.join(&manifest.agent_device_secret_file),
            &output.join(&manifest.agent_hub_public_key_file),
            &output.join(&manifest.agent_grant_public_key_file),
            &output.join(&manifest.agent_tls_root_der_file),
        )
        .unwrap();
        assert_eq!(agent.trusted_hub, hub.verifier());
        assert_eq!(agent.grant_verifier, grants.verifier());
        assert_eq!(agent.tls_root_der, cert.der().as_ref());

        let registered =
            load_verifying_key(&output.join(&manifest.hub_device_public_key_file)).unwrap();
        assert_eq!(registered, agent.device_identity.verifying_key());
        let secret_before = fs::read(output.join(&manifest.agent_device_secret_file)).unwrap();
        assert!(prepare_agent_enrollment(&output, &hub_public, &grant_public, &tls_root).is_err());
        let secret_after = fs::read(output.join(&manifest.agent_device_secret_file)).unwrap();
        assert_eq!(secret_before, secret_after);
        assert_eq!(
            load_device_identity(&output.join(&manifest.agent_device_secret_file))
                .unwrap()
                .verifying_key(),
            registered
        );

        let manifest_on_disk: AgentEnrollmentManifest =
            serde_json::from_slice(&fs::read(output.join("enrollment.json")).unwrap()).unwrap();
        assert_eq!(manifest_on_disk, manifest);
        let _ = fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn enrollment_rejects_group_or_world_writable_staging_parent() {
        use std::os::unix::fs::PermissionsExt;

        let root = temp_dir("unsafe-enrollment-parent");
        fs::create_dir(&root).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o777)).unwrap();
        let hub = HubIdentity::generate();
        let grants = GrantAuthority::generate();
        let hub_public = root.join("source-hub.pub");
        let grant_public = root.join("source-grant.pub");
        let tls_root = root.join("source-root.der");
        fs::write(
            &hub_public,
            format!("{}\n", hex_for_test(&hub.verifier().to_bytes())),
        )
        .unwrap();
        fs::write(
            &grant_public,
            format!("{}\n", hex_for_test(&grants.verifier().to_bytes())),
        )
        .unwrap();
        let CertifiedKey { cert, .. } =
            generate_simple_self_signed(vec!["localhost".into()]).unwrap();
        fs::write(&tls_root, cert.der().as_ref()).unwrap();
        for path in [&hub_public, &grant_public, &tls_root] {
            fs::set_permissions(path, fs::Permissions::from_mode(0o644)).unwrap();
        }

        let error =
            prepare_agent_enrollment(&root.join("bundle"), &hub_public, &grant_public, &tls_root)
                .unwrap_err();
        assert!(matches!(error, EnrollmentError::UnsafeParentDirectory));
        assert!(!root.join("bundle").exists());
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        let _ = fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    fn hex_for_test(bytes: &[u8]) -> String {
        const TABLE: &[u8; 16] = b"0123456789abcdef";
        let mut output = String::with_capacity(bytes.len() * 2);
        for byte in bytes {
            output.push(TABLE[(byte >> 4) as usize] as char);
            output.push(TABLE[(byte & 0x0f) as usize] as char);
        }
        output
    }
}
