from pathlib import Path


def replace_once(path: str, old: str, new: str) -> None:
    p = Path(path)
    text = p.read_text()
    count = text.count(old)
    if count != 1:
        raise SystemExit(f"{path}: expected exactly one match, got {count}: {old[:120]!r}")
    p.write_text(text.replace(old, new, 1))

# Reuse the existing fail-closed trust-anchor file validation rather than
# accepting a symlink or writable recovery verifier file.
replace_once(
    "src/v2_m1_keys.rs",
    "pub fn load_tls_root_der(path: &Path) -> Result<Vec<u8>, KeyMaterialError> {\n    read_public_file(path, MAX_TLS_ROOT_BYTES)\n}\n",
    "pub fn load_tls_root_der(path: &Path) -> Result<Vec<u8>, KeyMaterialError> {\n    read_public_file(path, MAX_TLS_ROOT_BYTES)\n}\n\n/// Load bounded binary public trust material with the same symlink and\n/// writable-permission rejection used by Hub/grant/TLS trust anchors.\npub fn load_public_trust_bytes(\n    path: &Path,\n    max_bytes: u64,\n) -> Result<Vec<u8>, KeyMaterialError> {\n    read_public_file(path, max_bytes)\n}\n",
)

replace_once(
    "src/v2_online_recovery.rs",
    "use crate::v2_m0_transport::HubIdentity;\n",
    "use crate::v2_m0_transport::HubIdentity;\nuse crate::v2_m1_keys::{KeyMaterialError, load_public_trust_bytes};\n",
)
replace_once(
    "src/v2_online_recovery.rs",
    '''    pub fn load_optional(state_dir: &Path) -> Result<Option<Self>, RecoveryError> {
        let path = state_dir.join(RECOVERY_PUBLIC_KEY_FILENAME);
        let bytes = match fs::read(path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(_) => return Err(RecoveryError::Io),
        };
        Self::from_x963_bytes(&bytes).map(Some)
    }
''',
    '''    pub fn load_optional(state_dir: &Path) -> Result<Option<Self>, RecoveryError> {
        let path = state_dir.join(RECOVERY_PUBLIC_KEY_FILENAME);
        let bytes = match load_public_trust_bytes(&path, 65) {
            Ok(bytes) => bytes,
            Err(KeyMaterialError::Io(error))
                if error.kind() == std::io::ErrorKind::NotFound =>
            {
                return Ok(None);
            }
            Err(_) => return Err(RecoveryError::UnsafeTrustAnchor),
        };
        Self::from_x963_bytes(&bytes).map(Some)
    }
''',
)
replace_once(
    "src/v2_online_recovery.rs",
    "    atomic_write_json_private(&authorization_path(state_dir), authorization)\n",
    "    atomic_write_json_private_no_replace(&authorization_path(state_dir), authorization)\n",
)
replace_once(
    "src/v2_online_recovery.rs",
    "fn read_json_optional<T: DeserializeOwned>(path: &Path) -> Result<Option<T>, RecoveryError> {",
    '''fn atomic_write_json_private_no_replace<T: Serialize>(
    path: &Path,
    value: &T,
) -> Result<(), RecoveryError> {
    if path.exists() {
        return Err(RecoveryError::AuthorizationAlreadyPending);
    }
    let bytes = serde_json::to_vec(value).map_err(|_| RecoveryError::InvalidMessage)?;
    if bytes.len() > MAX_RECOVERY_FILE_BYTES {
        return Err(RecoveryError::InvalidMessage);
    }
    let parent = path.parent().ok_or(RecoveryError::InvalidPath)?;
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or(RecoveryError::InvalidPath)?;
    let mut random = [0_u8; 8];
    OsRng.fill_bytes(&mut random);
    let mut suffix = String::new();
    for byte in random {
        use std::fmt::Write as _;
        let _ = write!(&mut suffix, "{byte:02x}");
    }
    let pending = parent.join(format!(".{file_name}.pending-{suffix}.tmp"));
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    let mut file = options.open(&pending).map_err(|_| RecoveryError::Io)?;
    let write_result = (|| {
        file.write_all(&bytes).map_err(|_| RecoveryError::Io)?;
        file.flush().map_err(|_| RecoveryError::Io)?;
        file.sync_all().map_err(|_| RecoveryError::Io)?;
        fs::hard_link(&pending, path).map_err(|error| {
            if error.kind() == std::io::ErrorKind::AlreadyExists {
                RecoveryError::AuthorizationAlreadyPending
            } else {
                RecoveryError::Io
            }
        })?;
        fs::remove_file(&pending).map_err(|_| RecoveryError::Io)?;
        File::open(parent)
            .and_then(|directory| directory.sync_all())
            .map_err(|_| RecoveryError::Io)?;
        Ok(())
    })();
    if write_result.is_err() {
        let _ = fs::remove_file(&pending);
    }
    write_result
}

fn read_json_optional<T: DeserializeOwned>(path: &Path) -> Result<Option<T>, RecoveryError> {''',
)
replace_once(
    "src/v2_online_recovery.rs",
    '''    UnsafeTrustAnchor,
    UnsupportedPlatform,
    KeyUnavailable,
    UserPresenceDenied,
}''',
    '''    UnsafeTrustAnchor,
    UnsupportedPlatform,
    KeyUnavailable,
    KeyAlreadyExists,
    AuthorizationAlreadyPending,
    UserPresenceDenied,
}''',
) if "    UnsafeTrustAnchor,\n" in Path("src/v2_online_recovery.rs").read_text() else replace_once(
    "src/v2_online_recovery.rs",
    '''    InvalidPath,
    Io,
    UnsupportedPlatform,
    KeyUnavailable,
    UserPresenceDenied,
}''',
    '''    InvalidPath,
    Io,
    UnsafeTrustAnchor,
    UnsupportedPlatform,
    KeyUnavailable,
    KeyAlreadyExists,
    AuthorizationAlreadyPending,
    UserPresenceDenied,
}''',
)
replace_once(
    "src/v2_online_recovery.rs",
    '''            Self::InvalidPath => "recovery_invalid_path",
            Self::Io => "recovery_io",
            Self::UnsupportedPlatform => "recovery_unsupported_platform",
            Self::KeyUnavailable => "recovery_key_unavailable",
            Self::UserPresenceDenied => "recovery_user_presence_denied",''',
    '''            Self::InvalidPath => "recovery_invalid_path",
            Self::Io => "recovery_io",
            Self::UnsafeTrustAnchor => "recovery_unsafe_trust_anchor",
            Self::UnsupportedPlatform => "recovery_unsupported_platform",
            Self::KeyUnavailable => "recovery_key_unavailable",
            Self::KeyAlreadyExists => "recovery_key_already_exists",
            Self::AuthorizationAlreadyPending => "recovery_authorization_pending",
            Self::UserPresenceDenied => "recovery_user_presence_denied",''',
)
replace_once(
    "src/v2_online_recovery.rs",
    '''        pub fn load_or_create(label: &str) -> Result<Self, RecoveryError> {
            if label.trim().is_empty() {
                return Err(RecoveryError::KeyUnavailable);
            }
            if let Some(private_key) = load_private_key(label)? {
                return Ok(Self { private_key });
            }
            let flags = kSecAccessControlUserPresence | kSecAccessControlPrivateKeyUsage;''',
    '''        pub fn create_new(label: &str) -> Result<Self, RecoveryError> {
            if label.trim().is_empty() {
                return Err(RecoveryError::KeyUnavailable);
            }
            // Initial provisioning never trusts a pre-existing label. This
            // prevents a software key planted under the default label from
            // being exported and pinned as recovery authority.
            if load_private_key(label)?.is_some() {
                return Err(RecoveryError::KeyAlreadyExists);
            }
            let flags = kSecAccessControlUserPresence | kSecAccessControlPrivateKeyUsage;''',
)
replace_once(
    "src/v2_online_recovery.rs",
    ".set_key_type(KeyType::ec_sec_prime_random())\n",
    ".set_key_type(KeyType::ec())\n",
)
replace_once(
    "src/bin/v2_recover.rs",
    "    let key = MacRecoveryKey::load_or_create(&key_label)\n",
    "    let key = MacRecoveryKey::create_new(&key_label)\n",
)

print("online recovery hardening patch applied")
