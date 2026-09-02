use crate::mutation_authority::inspect_mutation_authority;
use crate::v2_execution_safety::EXECUTION_SAFETY_SCHEMA_VERSION;
use crate::v2_handoff_control::{LocalHandoffControlRequest, exchange_unix_handoff_control};
use crate::v2_m0::{CapabilityClass, DeviceCapability};
use crate::v2_m0_transport::HUB_AGENT_SCHEMA_VERSION;
use crate::v2_m1_persistence::{
    AgentPersistentState, CheckpointStore, HubPersistentState, M1_STATE_SCHEMA_VERSION,
};
use crate::v2_maintenance::inspect_quarantines_read_only;
#[cfg(target_os = "macos")]
use crate::v2_online_recovery::RecoveryVerifier;
use crate::v2_operator_handoff::HandoffRuntimeStatus;
use crate::v2_tls_lifecycle::{CertificateFormat, CertificateHealth, inspect_certificate_file};
use ring::digest::{Context as DigestContext, SHA256};
use serde::{Deserialize, Serialize};
use std::fmt::Write as _;
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;

const MAX_HASHED_BINARY_BYTES: u64 = 256 * 1024 * 1024;
const MAX_MANIFEST_BYTES: u64 = 64 * 1024;
const DEFAULT_TLS_WARN_BEFORE_SECS: u64 = 30 * 24 * 60 * 60;
const CRITICAL_AVAILABLE_STORAGE_BYTES: u64 = 64 * 1024 * 1024;
#[cfg(any(target_os = "macos", test))]
const MAINTENANCE_LABEL_PREFIX: &str = "com.github.git-ksk.cumg-v2-maintenance.";
#[cfg(any(target_os = "macos", test))]
const LEGACY_MAINTENANCE_LABEL_PREFIXES: [&str; 2] = [
    "com.git-ksk.cumg-v2-upgrade-",
    "com.github.git-ksk.cumg-v2-upgrade-",
];
#[cfg(target_os = "macos")]
const MAX_MAINTENANCE_JOBS: usize = 64;

#[derive(Debug, Clone)]
pub struct DoctorConfig {
    pub hub_state_dir: PathBuf,
    pub agent_state_dir: PathBuf,
    pub runtime_manifest: PathBuf,
    pub binary_dir: PathBuf,
    pub hub_launchd_label: String,
    pub agent_launchd_label: String,
    pub grant_signer_launchd_label: Option<String>,
    pub grant_signer_socket: Option<PathBuf>,
    pub tls_server_certificate: Option<PathBuf>,
    pub tls_root_certificate: Option<PathBuf>,
    pub cua_command: Option<PathBuf>,
    pub expected_cua_version: Option<String>,
    pub mutation_authority_dir: Option<PathBuf>,
    pub handoff_control_socket: Option<PathBuf>,
    pub maintenance_job_exclude_label: Option<String>,
    pub recovery_key_file: Option<PathBuf>,
    pub recovery_helper: Option<PathBuf>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckStatus {
    Ok,
    Warning,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DoctorCheck {
    pub name: String,
    pub status: CheckStatus,
    pub detail: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimePairingStatus {
    Compatible,
    Skewed,
    Unknown,
}

impl RuntimePairingStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Compatible => "compatible",
            Self::Skewed => "skewed",
            Self::Unknown => "unknown",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OperatorToolingStatus {
    Compatible,
    Stale,
    Unavailable,
    Unknown,
}

impl OperatorToolingStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Compatible => "compatible",
            Self::Stale => "stale",
            Self::Unavailable => "unavailable",
            Self::Unknown => "unknown",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckpointReaderCompatibility {
    Compatible,
    Incompatible,
    Unknown,
}

impl CheckpointReaderCompatibility {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Compatible => "compatible",
            Self::Incompatible => "incompatible",
            Self::Unknown => "unknown",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RuntimeSummary {
    pub package_version: String,
    pub source_commit: Option<String>,
    pub manifest_verified: bool,
    pub runtime_pairing: RuntimePairingStatus,
    pub operator_tooling: OperatorToolingStatus,
    pub checkpoint_reader_compatibility: CheckpointReaderCompatibility,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HubSummary {
    pub state_schema: Option<u16>,
    pub registry_schema: Option<u16>,
    pub device_count: usize,
    pub generation: Option<u64>,
    pub capability_schema: Option<u16>,
    pub capability_revision: Option<u64>,
    pub live_quarantine_count: Option<usize>,
    pub recovery_mode: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AgentSummary {
    pub state_schema: Option<u16>,
    pub replay_generation: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MutationAuthoritySummary {
    pub owner: String,
    pub epoch: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LaneReadiness {
    Ready,
    IndeterminateFenced,
    Unavailable,
    Unsupported,
    Unknown,
}

impl LaneReadiness {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ready => "ready",
            Self::IndeterminateFenced => "indeterminate_fenced",
            Self::Unavailable => "unavailable",
            Self::Unsupported => "unsupported",
            Self::Unknown => "unknown",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ReadinessLanes {
    pub control_plane: LaneReadiness,
    pub computer_use_observation: LaneReadiness,
    pub filesystem_observation: LaneReadiness,
    pub effectful_execution: LaneReadiness,
    pub browser_effectful_execution: LaneReadiness,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ReadinessSummary {
    pub device: String,
    pub lanes: ReadinessLanes,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub blocking_operation_present: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub blocking_operation_retry_safe: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub operator_action: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RecoveryKeyReadinessStatus {
    Ready,
    Unprovisioned,
    SealedKeyMissing,
    HubVerifierMissing,
    PublicKeyMismatch,
    HelperUnavailable,
    ReadinessUnknown,
    Unsupported,
}

impl RecoveryKeyReadinessStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Ready => "ready",
            Self::Unprovisioned => "unprovisioned",
            Self::SealedKeyMissing => "sealed_key_missing",
            Self::HubVerifierMissing => "hub_verifier_missing",
            Self::PublicKeyMismatch => "public_key_mismatch",
            Self::HelperUnavailable => "helper_unavailable",
            Self::ReadinessUnknown => "readiness_unknown",
            Self::Unsupported => "unsupported",
        }
    }

    pub const fn needs_operator_action(self) -> bool {
        matches!(
            self,
            Self::SealedKeyMissing
                | Self::HubVerifierMissing
                | Self::PublicKeyMismatch
                | Self::HelperUnavailable
                | Self::ReadinessUnknown
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RecoveryKeyReadinessSummary {
    pub status: RecoveryKeyReadinessStatus,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DoctorReport {
    pub schema_version: u16,
    pub overall: String,
    pub readiness: ReadinessSummary,
    pub runtime: RuntimeSummary,
    pub recovery_key_readiness: RecoveryKeyReadinessSummary,
    pub hub: HubSummary,
    pub agent: AgentSummary,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mutation_authority: Option<MutationAuthoritySummary>,
    pub checks: Vec<DoctorCheck>,
}

impl DoctorReport {
    pub fn exit_code(&self) -> u8 {
        if self
            .checks
            .iter()
            .any(|check| check.status == CheckStatus::Error)
        {
            2
        } else if self
            .checks
            .iter()
            .any(|check| check.status == CheckStatus::Warning)
        {
            1
        } else {
            0
        }
    }
}

#[derive(Debug, Deserialize)]
struct RuntimeManifest {
    schema_version: u16,
    hub_agent_schema_version: u16,
    source_commit: String,
    package_version: String,
    binaries: Vec<RuntimeManifestBinary>,
}

#[derive(Debug, Deserialize)]
struct RuntimeManifestBinary {
    name: String,
    sha256: String,
}

pub fn run_doctor(config: &DoctorConfig) -> DoctorReport {
    let mut checks = Vec::new();
    let mut runtime = RuntimeSummary {
        package_version: env!("CARGO_PKG_VERSION").to_owned(),
        source_commit: None,
        manifest_verified: false,
        runtime_pairing: RuntimePairingStatus::Unknown,
        operator_tooling: OperatorToolingStatus::Unknown,
        checkpoint_reader_compatibility: CheckpointReaderCompatibility::Unknown,
    };
    verify_runtime_manifest(config, &mut runtime, &mut checks);
    runtime.checkpoint_reader_compatibility = inspect_checkpoint_reader_compatibility(
        &config.hub_state_dir,
        runtime.operator_tooling,
        &mut checks,
    );
    let recovery_key_readiness =
        inspect_recovery_key_readiness(config, runtime.manifest_verified, &mut checks);
    inspect_storage_capacity(&config.agent_state_dir, "agent_state_capacity", &mut checks);
    inspect_storage_capacity(&std::env::temp_dir(), "temp_capacity", &mut checks);

    let agent_pid =
        inspect_launchd_service(&config.agent_launchd_label, "agent_service", &mut checks);
    let transport_established = inspect_agent_hub_transport(agent_pid, &mut checks);
    // Process ancestry is local OS evidence that this doctor was actually spawned by the
    // configured Agent. A caller-provided operation ID or environment marker is never trusted.
    let in_band_live_agent_path =
        transport_established && agent_pid.is_some_and(current_process_descends_from);
    let (hub, hub_device_id, supported_capabilities) =
        inspect_hub(config, in_band_live_agent_path, &mut checks);
    let agent = inspect_agent(
        config,
        hub_device_id.as_deref(),
        hub.generation,
        &mut checks,
    );

    let _hub_pid = inspect_launchd_service(&config.hub_launchd_label, "hub_service", &mut checks);
    if let Some(label) = &config.grant_signer_launchd_label {
        inspect_launchd_service(label, "grant_signer_service", &mut checks);
    }
    inspect_launchd_maintenance_jobs(config.maintenance_job_exclude_label.as_deref(), &mut checks);
    if let Some(socket) = &config.grant_signer_socket {
        inspect_unix_socket(socket, &mut checks);
    }
    if let Some(path) = &config.tls_server_certificate {
        inspect_tls(
            path,
            CertificateFormat::Pem,
            "tls_server_certificate",
            &mut checks,
        );
    }
    if let Some(path) = &config.tls_root_certificate {
        inspect_tls(
            path,
            CertificateFormat::Der,
            "tls_root_certificate",
            &mut checks,
        );
    }
    if let Some(command) = &config.cua_command {
        inspect_cua(command, config.expected_cua_version.as_deref(), &mut checks);
    }
    let mutation_authority = inspect_mutation_authority_for_doctor(
        config.mutation_authority_dir.as_deref(),
        config.cua_command.is_some(),
        &mut checks,
    );
    if let Some(socket) = &config.handoff_control_socket {
        inspect_handoff_recovery(socket, &mut checks);
    }

    let readiness = summarize_readiness(
        &checks,
        hub.live_quarantine_count,
        supported_capabilities.as_deref(),
        config.cua_command.is_some(),
        mutation_authority.as_ref(),
    );

    let overall = if checks
        .iter()
        .any(|check| check.status == CheckStatus::Error)
    {
        "unsafe"
    } else if checks
        .iter()
        .any(|check| check.status == CheckStatus::Warning)
    {
        "degraded"
    } else {
        "healthy"
    }
    .to_owned();

    DoctorReport {
        schema_version: 1,
        overall,
        readiness,
        runtime,
        recovery_key_readiness,
        hub,
        agent,
        mutation_authority,
        checks,
    }
}

fn inspect_mutation_authority_for_doctor(
    directory: Option<&Path>,
    cua_enabled: bool,
    checks: &mut Vec<DoctorCheck>,
) -> Option<MutationAuthoritySummary> {
    let Some(directory) = directory else {
        if cua_enabled {
            push(
                checks,
                "mutation_authority",
                CheckStatus::Error,
                "not_configured",
            );
        }
        return None;
    };
    match inspect_mutation_authority(directory) {
        Ok(status) => {
            push(
                checks,
                "mutation_authority",
                CheckStatus::Ok,
                match status.owner.as_str() {
                    "v1" => "owner_v1",
                    "v2" => "owner_v2",
                    _ => "owner_invalid",
                },
            );
            Some(MutationAuthoritySummary {
                owner: status.owner.as_str().to_owned(),
                epoch: status.epoch,
            })
        }
        Err(error) => {
            push(
                checks,
                "mutation_authority",
                CheckStatus::Error,
                error.safe_code(),
            );
            None
        }
    }
}

fn is_operator_runtime_binary(name: &str) -> bool {
    matches!(
        name,
        "v2_maint" | "v2_doctor" | "v2_status" | "v2_recover" | "v2_recovery_enclave_helper"
    )
}

fn verify_runtime_manifest(
    config: &DoctorConfig,
    runtime: &mut RuntimeSummary,
    checks: &mut Vec<DoctorCheck>,
) {
    let manifest =
        match read_bounded_json::<RuntimeManifest>(&config.runtime_manifest, MAX_MANIFEST_BYTES) {
            Ok(manifest) => manifest,
            Err(_) => {
                push(
                    checks,
                    "runtime_manifest",
                    CheckStatus::Error,
                    "unreadable_or_invalid",
                );
                return;
            }
        };
    if manifest.schema_version != 3
        || manifest.hub_agent_schema_version != HUB_AGENT_SCHEMA_VERSION
        || manifest.source_commit.len() != 40
        || !manifest
            .source_commit
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit())
        || manifest.package_version.trim().is_empty()
    {
        push(
            checks,
            "runtime_manifest",
            CheckStatus::Error,
            "invalid_schema_or_identity",
        );
        return;
    }
    runtime.source_commit = Some(manifest.source_commit.clone());
    runtime.runtime_pairing = RuntimePairingStatus::Compatible;
    runtime.operator_tooling = OperatorToolingStatus::Compatible;
    if manifest.package_version != env!("CARGO_PKG_VERSION") {
        runtime.runtime_pairing = RuntimePairingStatus::Skewed;
        runtime.operator_tooling = OperatorToolingStatus::Stale;
        push(
            checks,
            "runtime_manifest",
            CheckStatus::Warning,
            "package_version_differs_from_operator",
        );
    }
    let mut required = vec!["v2_hub", "v2_agent", "v2_maint", "v2_doctor", "v2_status"];
    #[cfg(target_os = "macos")]
    required.extend(["v2_recover", "v2_recovery_enclave_helper"]);
    if config.grant_signer_launchd_label.is_some() || config.grant_signer_socket.is_some() {
        required.push("v2_grant_signer");
    }
    for name in required {
        let Some(entry) = manifest.binaries.iter().find(|entry| entry.name == name) else {
            runtime.runtime_pairing = RuntimePairingStatus::Skewed;
            if is_operator_runtime_binary(name) {
                runtime.operator_tooling = OperatorToolingStatus::Unavailable;
            }
            push(
                checks,
                "runtime_manifest",
                CheckStatus::Error,
                "missing_required_binary",
            );
            return;
        };
        if entry.sha256.len() != 64 || !entry.sha256.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            runtime.runtime_pairing = RuntimePairingStatus::Unknown;
            runtime.operator_tooling = OperatorToolingStatus::Unknown;
            push(
                checks,
                "runtime_manifest",
                CheckStatus::Error,
                "invalid_binary_digest",
            );
            return;
        }
        let path = config.binary_dir.join(name);
        match sha256_file(&path) {
            Ok(actual) if actual.eq_ignore_ascii_case(&entry.sha256) => {}
            Ok(_) => {
                runtime.runtime_pairing = RuntimePairingStatus::Skewed;
                if is_operator_runtime_binary(name) {
                    runtime.operator_tooling = OperatorToolingStatus::Stale;
                }
                push(
                    checks,
                    "runtime_manifest",
                    CheckStatus::Error,
                    "binary_digest_mismatch",
                );
                return;
            }
            Err(_) => {
                runtime.runtime_pairing = RuntimePairingStatus::Skewed;
                if is_operator_runtime_binary(name) {
                    runtime.operator_tooling = OperatorToolingStatus::Unavailable;
                }
                push(
                    checks,
                    "runtime_manifest",
                    CheckStatus::Error,
                    "binary_unreadable",
                );
                return;
            }
        }
    }

    // `v2_status` and `v2_doctor` are often invoked directly by an operator. Verify
    // the executable that is actually running, not only another file with the same
    // basename under --binary-dir. Test harness executables deliberately skip this
    // check because they are not production operator tools.
    if let Ok(current) = std::env::current_exe()
        && let Some(name) = current.file_name().and_then(|value| value.to_str())
        && matches!(name, "v2_status" | "v2_doctor")
        && let Some(entry) = manifest.binaries.iter().find(|entry| entry.name == name)
    {
        match sha256_file(&current) {
            Ok(actual) if actual.eq_ignore_ascii_case(&entry.sha256) => {}
            Ok(_) => {
                runtime.runtime_pairing = RuntimePairingStatus::Skewed;
                runtime.operator_tooling = OperatorToolingStatus::Stale;
                push(
                    checks,
                    "runtime_manifest",
                    CheckStatus::Error,
                    "running_operator_digest_mismatch",
                );
                return;
            }
            Err(_) => {
                runtime.runtime_pairing = RuntimePairingStatus::Unknown;
                runtime.operator_tooling = OperatorToolingStatus::Unknown;
                push(
                    checks,
                    "runtime_manifest",
                    CheckStatus::Error,
                    "running_operator_unreadable",
                );
                return;
            }
        }
    }

    runtime.manifest_verified = true;
    push(checks, "runtime_manifest", CheckStatus::Ok, "verified");
}

fn inspect_checkpoint_reader_compatibility(
    hub_state_dir: &Path,
    operator_tooling: OperatorToolingStatus,
    checks: &mut Vec<DoctorCheck>,
) -> CheckpointReaderCompatibility {
    let store = match CheckpointStore::new(hub_state_dir.to_path_buf(), "hub") {
        Ok(store) => store,
        Err(_) => {
            push(
                checks,
                "checkpoint_reader_compatibility",
                CheckStatus::Warning,
                "state_identity_unavailable",
            );
            return CheckpointReaderCompatibility::Unknown;
        }
    };
    let raw = match store.load_latest::<serde_json::Value>() {
        Ok(raw) => raw,
        Err(_) => {
            push(
                checks,
                "checkpoint_reader_compatibility",
                CheckStatus::Warning,
                "checkpoint_schema_unavailable",
            );
            return CheckpointReaderCompatibility::Unknown;
        }
    };
    let state_schema = raw
        .get("schema_version")
        .and_then(serde_json::Value::as_u64)
        .and_then(|value| u16::try_from(value).ok());
    let execution_schema = raw
        .get("execution")
        .and_then(|value| value.get("schema_version"))
        .and_then(serde_json::Value::as_u64)
        .and_then(|value| u16::try_from(value).ok());
    if state_schema != Some(M1_STATE_SCHEMA_VERSION)
        || execution_schema.is_some_and(|value| value > EXECUTION_SAFETY_SCHEMA_VERSION)
    {
        push(
            checks,
            "checkpoint_reader_compatibility",
            CheckStatus::Error,
            "checkpoint_newer_than_reader",
        );
        return CheckpointReaderCompatibility::Incompatible;
    }
    if state_schema.is_none() || execution_schema.is_none() {
        push(
            checks,
            "checkpoint_reader_compatibility",
            CheckStatus::Warning,
            "checkpoint_schema_unknown",
        );
        return CheckpointReaderCompatibility::Unknown;
    }
    if operator_tooling != OperatorToolingStatus::Compatible {
        push(
            checks,
            "checkpoint_reader_compatibility",
            CheckStatus::Warning,
            "operator_reader_identity_unverified",
        );
        return CheckpointReaderCompatibility::Unknown;
    }
    push(
        checks,
        "checkpoint_reader_compatibility",
        CheckStatus::Ok,
        "compatible",
    );
    CheckpointReaderCompatibility::Compatible
}

#[cfg(any(target_os = "macos", test))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RecoveryPublicKeyInput {
    Missing,
    Available([u8; 65]),
    HelperUnavailable,
    Unknown,
}

#[cfg(any(target_os = "macos", test))]
fn classify_recovery_key_readiness(
    hub: RecoveryPublicKeyInput,
    local: RecoveryPublicKeyInput,
) -> RecoveryKeyReadinessStatus {
    match (hub, local) {
        (RecoveryPublicKeyInput::Missing, RecoveryPublicKeyInput::Missing) => {
            RecoveryKeyReadinessStatus::Unprovisioned
        }
        (RecoveryPublicKeyInput::Available(_), RecoveryPublicKeyInput::Missing) => {
            RecoveryKeyReadinessStatus::SealedKeyMissing
        }
        (RecoveryPublicKeyInput::Missing, RecoveryPublicKeyInput::Available(_)) => {
            RecoveryKeyReadinessStatus::HubVerifierMissing
        }
        (RecoveryPublicKeyInput::Available(hub), RecoveryPublicKeyInput::Available(local)) => {
            if hub == local {
                RecoveryKeyReadinessStatus::Ready
            } else {
                RecoveryKeyReadinessStatus::PublicKeyMismatch
            }
        }
        (_, RecoveryPublicKeyInput::HelperUnavailable) => {
            RecoveryKeyReadinessStatus::HelperUnavailable
        }
        (RecoveryPublicKeyInput::HelperUnavailable, _) => {
            RecoveryKeyReadinessStatus::ReadinessUnknown
        }
        _ => RecoveryKeyReadinessStatus::ReadinessUnknown,
    }
}

#[cfg(target_os = "macos")]
fn inspect_hub_recovery_public_key(state_dir: &Path) -> RecoveryPublicKeyInput {
    match RecoveryVerifier::load_optional(state_dir) {
        Ok(Some(verifier)) if verifier.webauthn_document().is_none() => {
            RecoveryPublicKeyInput::Available(verifier.public_key_bytes())
        }
        Ok(Some(_)) => RecoveryPublicKeyInput::Unknown,
        Ok(None) => RecoveryPublicKeyInput::Missing,
        Err(_) => RecoveryPublicKeyInput::Unknown,
    }
}

#[cfg(target_os = "macos")]
fn inspect_local_recovery_public_key(
    config: &DoctorConfig,
    runtime_manifest_verified: bool,
) -> RecoveryPublicKeyInput {
    use crate::v2_online_recovery::macos::MacRecoveryKey;
    let Some(key_file) = config.recovery_key_file.as_deref() else {
        return RecoveryPublicKeyInput::Missing;
    };
    match std::fs::symlink_metadata(key_file) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return RecoveryPublicKeyInput::Missing;
        }
        Err(_) => return RecoveryPublicKeyInput::Unknown,
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => {
            return RecoveryPublicKeyInput::Unknown;
        }
        Ok(_) => {}
    }
    let Some(helper) = config.recovery_helper.as_deref() else {
        return RecoveryPublicKeyInput::HelperUnavailable;
    };
    let expected_helper = config.binary_dir.join("v2_recovery_enclave_helper");
    if !runtime_manifest_verified || helper != expected_helper {
        return RecoveryPublicKeyInput::HelperUnavailable;
    }
    let key = match MacRecoveryKey::load(helper, key_file) {
        Ok(key) => key,
        Err(crate::v2_online_recovery::RecoveryError::RecoveryHelperUnavailable)
        | Err(crate::v2_online_recovery::RecoveryError::InvalidPath) => {
            return RecoveryPublicKeyInput::HelperUnavailable;
        }
        Err(_) => return RecoveryPublicKeyInput::Unknown,
    };
    match key.public_key_bytes() {
        Ok(public) => RecoveryPublicKeyInput::Available(public),
        Err(crate::v2_online_recovery::RecoveryError::RecoveryHelperUnavailable)
        | Err(crate::v2_online_recovery::RecoveryError::RecoveryHelperProtocol)
        | Err(crate::v2_online_recovery::RecoveryError::RecoveryHelperTimeout)
        | Err(crate::v2_online_recovery::RecoveryError::RecoveryHelperAbnormalExit) => {
            RecoveryPublicKeyInput::HelperUnavailable
        }
        Err(_) => RecoveryPublicKeyInput::Unknown,
    }
}

#[cfg(target_os = "macos")]
fn inspect_recovery_key_readiness(
    config: &DoctorConfig,
    runtime_manifest_verified: bool,
    checks: &mut Vec<DoctorCheck>,
) -> RecoveryKeyReadinessSummary {
    let hub = inspect_hub_recovery_public_key(&config.hub_state_dir);
    let local = inspect_local_recovery_public_key(config, runtime_manifest_verified);
    let status = classify_recovery_key_readiness(hub, local);
    let (check_status, detail) = match status {
        RecoveryKeyReadinessStatus::Ready => (CheckStatus::Ok, "ready"),
        RecoveryKeyReadinessStatus::Unprovisioned => (CheckStatus::Ok, "unprovisioned"),
        RecoveryKeyReadinessStatus::SealedKeyMissing => {
            (CheckStatus::Warning, "sealed_key_missing")
        }
        RecoveryKeyReadinessStatus::HubVerifierMissing => {
            (CheckStatus::Warning, "hub_verifier_missing")
        }
        RecoveryKeyReadinessStatus::PublicKeyMismatch => {
            (CheckStatus::Error, "public_key_mismatch")
        }
        RecoveryKeyReadinessStatus::HelperUnavailable => {
            (CheckStatus::Warning, "helper_unavailable")
        }
        RecoveryKeyReadinessStatus::ReadinessUnknown => (CheckStatus::Warning, "readiness_unknown"),
        RecoveryKeyReadinessStatus::Unsupported => unreachable!(),
    };
    push(checks, "recovery_key_readiness", check_status, detail);
    RecoveryKeyReadinessSummary { status }
}

#[cfg(not(target_os = "macos"))]
fn inspect_recovery_key_readiness(
    _config: &DoctorConfig,
    _runtime_manifest_verified: bool,
    _checks: &mut [DoctorCheck],
) -> RecoveryKeyReadinessSummary {
    RecoveryKeyReadinessSummary {
        status: RecoveryKeyReadinessStatus::Unsupported,
    }
}

fn inspect_hub(
    config: &DoctorConfig,
    in_band_agent_descendant: bool,
    checks: &mut Vec<DoctorCheck>,
) -> (HubSummary, Option<String>, Option<Vec<DeviceCapability>>) {
    let mut summary = HubSummary {
        state_schema: None,
        registry_schema: None,
        device_count: 0,
        generation: None,
        capability_schema: None,
        capability_revision: None,
        live_quarantine_count: None,
        recovery_mode: "normal".to_owned(),
    };
    let store = match CheckpointStore::new(config.hub_state_dir.clone(), "hub") {
        Ok(store) => store,
        Err(_) => {
            push(
                checks,
                "hub_state",
                CheckStatus::Error,
                "invalid_state_directory",
            );
            return (summary, None, None);
        }
    };
    let state = match store.load_latest::<HubPersistentState>() {
        Ok(state) => state,
        Err(_) => {
            push(
                checks,
                "hub_state",
                CheckStatus::Error,
                "checkpoint_unreadable_or_unsafe",
            );
            return (summary, None, None);
        }
    };
    summary.state_schema = Some(state.schema_version);
    summary.registry_schema = Some(state.registry.schema_version);
    summary.device_count = state.registry.devices.len();
    let device = if state.registry.devices.len() == 1 {
        state.registry.devices.first()
    } else {
        None
    };
    let device_id = device.map(|device| device.device_id.clone());
    let supported_capabilities = device
        .and_then(|device| device.capabilities.as_ref())
        .map(|capabilities| capabilities.supported.clone());
    if let Some(device) = device {
        summary.generation = Some(device.generation);
        if let Some(capabilities) = &device.capabilities {
            summary.capability_schema = Some(capabilities.capability_schema_version);
            summary.capability_revision = Some(capabilities.revision);
        }
    }
    let restored = state.restore(crate::v2_m0_execution::AdmissionLimits {
        max_global_active: 1,
        max_queued_per_device: 1,
    });
    if restored.is_err() || summary.device_count != 1 || summary.capability_schema.is_none() {
        push(
            checks,
            "hub_state",
            CheckStatus::Error,
            "state_or_schema_incompatible",
        );
    } else {
        push(
            checks,
            "hub_state",
            CheckStatus::Ok,
            "restored_and_compatible",
        );
    }
    match inspect_quarantines_read_only(&config.hub_state_dir, None) {
        Ok(report) => {
            let (persistent_count, diagnostic_caller_count) =
                classify_quarantine_report(&report, summary.device_count, in_band_agent_descendant);
            summary.live_quarantine_count = Some(persistent_count);
            let (recovery_mode, recovery_mode_status) =
                recovery_mode_for_quarantine_count(persistent_count);
            summary.recovery_mode = recovery_mode.to_owned();
            if persistent_count == 0 {
                push(checks, "live_quarantine", CheckStatus::Ok, "none");
            } else {
                push(checks, "live_quarantine", CheckStatus::Error, "present");
            }
            push(checks, "recovery_mode", recovery_mode_status, recovery_mode);
            if diagnostic_caller_count == 1 {
                push(
                    checks,
                    "diagnostic_self_observation",
                    CheckStatus::Ok,
                    "restart_safe_active_caller",
                );
            }
        }
        Err(_) => push(
            checks,
            "live_quarantine",
            CheckStatus::Error,
            "inspection_failed",
        ),
    }
    (summary, device_id, supported_capabilities)
}

fn check_status(checks: &[DoctorCheck], name: &str) -> Option<CheckStatus> {
    checks
        .iter()
        .rev()
        .find(|check| check.name == name)
        .map(|check| check.status)
}

fn checks_ready(checks: &[DoctorCheck], names: &[&str]) -> Option<bool> {
    let mut saw_unknown = false;
    for name in names {
        match check_status(checks, name) {
            Some(CheckStatus::Ok) => {}
            Some(CheckStatus::Error) => return Some(false),
            Some(CheckStatus::Warning) | None => saw_unknown = true,
        }
    }
    if saw_unknown { None } else { Some(true) }
}

fn capability_group_supported(
    capabilities: Option<&[DeviceCapability]>,
    predicate: impl Fn(DeviceCapability) -> bool,
) -> Option<bool> {
    capabilities.map(|capabilities| capabilities.iter().copied().any(predicate))
}

fn summarize_readiness(
    checks: &[DoctorCheck],
    live_quarantine_count: Option<usize>,
    capabilities: Option<&[DeviceCapability]>,
    cua_configured: bool,
    mutation_authority: Option<&MutationAuthoritySummary>,
) -> ReadinessSummary {
    let control_plane = match checks_ready(
        checks,
        &[
            "hub_state",
            "agent_state",
            "hub_service",
            "agent_service",
            "agent_hub_transport",
        ],
    ) {
        Some(true) => LaneReadiness::Ready,
        Some(false) => LaneReadiness::Unavailable,
        None => LaneReadiness::Unknown,
    };
    let control_ready = control_plane == LaneReadiness::Ready;
    let cua_ready = !cua_configured
        || !matches!(
            check_status(checks, "cua_version"),
            Some(CheckStatus::Error) | None
        );
    let mutation_ready =
        !cua_configured || mutation_authority.is_some_and(|authority| authority.owner == "v2");

    let computer_observation_supported = capability_group_supported(capabilities, |capability| {
        capability.class() == CapabilityClass::Observe
            && !matches!(
                capability,
                DeviceCapability::ReadFile | DeviceCapability::ListDirectory
            )
    });
    let filesystem_observation_supported = capability_group_supported(capabilities, |capability| {
        matches!(
            capability,
            DeviceCapability::ReadFile | DeviceCapability::ListDirectory
        )
    });
    let effectful_supported = capability_group_supported(capabilities, |capability| {
        capability.class() != CapabilityClass::Observe
    });
    let computer_use_effectful_supported = capability_group_supported(capabilities, |capability| {
        capability.class() != CapabilityClass::Observe
            && !matches!(
                capability,
                DeviceCapability::ExecuteProcess | DeviceCapability::Shell
            )
    });
    let browser_effectful_supported = capability_group_supported(capabilities, |capability| {
        matches!(
            capability,
            DeviceCapability::BrowserPrepare
                | DeviceCapability::BrowserNavigate
                | DeviceCapability::BrowserClick
                | DeviceCapability::BrowserType
                | DeviceCapability::BrowserDialog
                | DeviceCapability::BrowserPointer
                | DeviceCapability::BrowserUploadFile
                | DeviceCapability::BrowserDownload
        )
    });

    let observation_lane = |supported: Option<bool>, backend_required: bool| match supported {
        Some(false) => LaneReadiness::Unsupported,
        Some(true) if !control_ready => LaneReadiness::Unavailable,
        Some(true) if backend_required && !cua_ready => LaneReadiness::Unavailable,
        Some(true) => LaneReadiness::Ready,
        None => LaneReadiness::Unknown,
    };
    let blocking_operation_present = live_quarantine_count.map(|count| count > 0);
    let effectful_lane = |supported: Option<bool>, backend_required: bool| match supported {
        Some(false) => LaneReadiness::Unsupported,
        Some(true) if blocking_operation_present == Some(true) => {
            LaneReadiness::IndeterminateFenced
        }
        Some(true) if blocking_operation_present.is_none() => LaneReadiness::Unknown,
        Some(true) if !control_ready => LaneReadiness::Unavailable,
        Some(true) if backend_required && (!cua_ready || !mutation_ready) => {
            LaneReadiness::Unavailable
        }
        Some(true) => LaneReadiness::Ready,
        None => LaneReadiness::Unknown,
    };

    let lanes = ReadinessLanes {
        control_plane,
        computer_use_observation: observation_lane(computer_observation_supported, cua_configured),
        filesystem_observation: observation_lane(filesystem_observation_supported, false),
        effectful_execution: effectful_lane(
            effectful_supported,
            cua_configured && computer_use_effectful_supported == Some(true),
        ),
        browser_effectful_execution: effectful_lane(browser_effectful_supported, cua_configured),
    };
    let device = if blocking_operation_present == Some(true) {
        "degraded_operator_action_required"
    } else if lanes.control_plane == LaneReadiness::Unavailable {
        "unavailable"
    } else if [
        lanes.control_plane,
        lanes.computer_use_observation,
        lanes.filesystem_observation,
        lanes.effectful_execution,
        lanes.browser_effectful_execution,
    ]
    .iter()
    .any(|lane| matches!(lane, LaneReadiness::Unavailable | LaneReadiness::Unknown))
    {
        "degraded"
    } else {
        "healthy"
    }
    .to_owned();

    let diagnostic_attention = [
        lanes.control_plane,
        lanes.computer_use_observation,
        lanes.filesystem_observation,
        lanes.effectful_execution,
        lanes.browser_effectful_execution,
    ]
    .iter()
    .any(|lane| matches!(lane, LaneReadiness::Unavailable | LaneReadiness::Unknown));

    ReadinessSummary {
        device,
        lanes,
        blocking_operation_present,
        blocking_operation_retry_safe: (blocking_operation_present == Some(true)).then_some(false),
        operator_action: if blocking_operation_present == Some(true) {
            Some("inspect_reconciliation_status".to_owned())
        } else if blocking_operation_present.is_none() || diagnostic_attention {
            Some("inspect_doctor_failures".to_owned())
        } else {
            None
        },
    }
}

fn recovery_mode_for_quarantine_count(persistent_count: usize) -> (&'static str, CheckStatus) {
    if persistent_count == 0 {
        ("normal", CheckStatus::Ok)
    } else {
        ("restricted_read_only", CheckStatus::Warning)
    }
}

fn classify_quarantine_report(
    report: &crate::v2_maintenance::QuarantineInspectionReport,
    device_count: usize,
    in_band_agent_descendant: bool,
) -> (usize, usize) {
    // The single-Mac Agent executes only one operation at a time. Therefore an in-band
    // doctor can suppress only the one exact restart-safe entry for its current process/shell
    // generation. Multiple devices/entries or an out-of-band caller remain fail-closed.
    if !in_band_agent_descendant || device_count != 1 || report.quarantines.len() != 1 {
        return (report.quarantines.len(), 0);
    }
    let quarantine = &report.quarantines[0];
    // A real Hub restart tears down the Agent transport; reconnecting necessarily advances the
    // registry generation. With a currently established transport, a same-generation
    // HubRestartAfterDispatch entry is the serialization of live dispatched work, not a
    // persisted post-restart quarantine. This changes only the diagnostic taxonomy.
    let is_current_generation_restart_snapshot = quarantine.indeterminate_reason
        == "hub_restart_after_dispatch"
        && quarantine.current_device_generation == Some(quarantine.device_generation)
        && matches!(quarantine.capability.as_str(), "execute_process" | "shell")
        && quarantine.dispatch_recorded
        && quarantine.dispatch_binding_present
        && quarantine.reconciliation_status == "auto_reconciling";
    if is_current_generation_restart_snapshot {
        (0, 1)
    } else {
        (1, 0)
    }
}

fn inspect_agent(
    config: &DoctorConfig,
    expected_device_id: Option<&str>,
    hub_generation: Option<u64>,
    checks: &mut Vec<DoctorCheck>,
) -> AgentSummary {
    let mut summary = AgentSummary {
        state_schema: None,
        replay_generation: None,
    };
    let store = match CheckpointStore::new(config.agent_state_dir.clone(), "agent") {
        Ok(store) => store,
        Err(_) => {
            push(
                checks,
                "agent_state",
                CheckStatus::Error,
                "invalid_state_directory",
            );
            return summary;
        }
    };
    let state = match store.load_latest::<AgentPersistentState>() {
        Ok(state) => state,
        Err(_) => {
            push(
                checks,
                "agent_state",
                CheckStatus::Error,
                "checkpoint_unreadable_or_unsafe",
            );
            return summary;
        }
    };
    summary.state_schema = Some(state.schema_version);
    summary.replay_generation = state.execution.replay_generation;
    let identity_matches = expected_device_id.is_some_and(|expected| expected == state.device_id);
    let generation_matches = hub_generation == state.execution.replay_generation;
    if state.clone().restore_with_terminal_evidence().is_err()
        || !identity_matches
        || !generation_matches
    {
        push(
            checks,
            "agent_state",
            CheckStatus::Error,
            "hub_agent_state_mismatch",
        );
    } else {
        push(
            checks,
            "agent_state",
            CheckStatus::Ok,
            "paired_generation_matches",
        );
    }
    summary
}

#[cfg(any(target_os = "macos", test))]
fn is_launchd_maintenance_label(label: &str) -> bool {
    !label.is_empty()
        && label.len() <= 180
        && label
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
        && (label.starts_with(MAINTENANCE_LABEL_PREFIX)
            || LEGACY_MAINTENANCE_LABEL_PREFIXES
                .iter()
                .any(|prefix| label.starts_with(prefix)))
}

#[cfg(any(target_os = "macos", test))]
fn launchctl_domain_maintenance_labels(output: &str) -> Vec<String> {
    let mut labels = output
        .split_whitespace()
        .filter_map(|token| {
            let token = token.trim_matches(|character: char| {
                !character.is_ascii_alphanumeric() && !matches!(character, '.' | '_' | '-')
            });
            is_launchd_maintenance_label(token).then(|| token.to_owned())
        })
        .collect::<Vec<_>>();
    labels.sort();
    labels.dedup();
    labels
}

fn inspect_launchd_maintenance_jobs(exclude_label: Option<&str>, checks: &mut Vec<DoctorCheck>) {
    #[cfg(target_os = "macos")]
    {
        if exclude_label.is_some_and(|label| {
            !is_launchd_maintenance_label(label) || !label.starts_with(MAINTENANCE_LABEL_PREFIX)
        }) {
            push(
                checks,
                "maintenance_jobs",
                CheckStatus::Error,
                "invalid_current_maintenance_job_label",
            );
            return;
        }
        let uid = unsafe { libc_getuid() };
        let domain = format!("gui/{uid}");
        if let Some(label) = exclude_label {
            let target = format!("{domain}/{label}");
            let current = match Command::new("launchctl").arg("print").arg(target).output() {
                Ok(current) if current.status.success() => current,
                _ => {
                    push(
                        checks,
                        "maintenance_jobs",
                        CheckStatus::Error,
                        "current_maintenance_job_not_loaded",
                    );
                    return;
                }
            };
            let text = String::from_utf8_lossy(&current.stdout);
            if launchctl_output_running_pid(&text).is_none()
                || launchctl_output_runs(&text) != Some(1)
            {
                push(
                    checks,
                    "maintenance_jobs",
                    CheckStatus::Error,
                    "current_maintenance_job_not_one_shot_running",
                );
                return;
            }
        }
        let output = match Command::new("launchctl").arg("print").arg(&domain).output() {
            Ok(output) if output.status.success() => output,
            _ => {
                push(
                    checks,
                    "maintenance_jobs",
                    CheckStatus::Warning,
                    "launchd_domain_unreadable",
                );
                return;
            }
        };
        let text = String::from_utf8_lossy(&output.stdout);
        let labels = launchctl_domain_maintenance_labels(&text)
            .into_iter()
            .filter(|label| Some(label.as_str()) != exclude_label)
            .collect::<Vec<_>>();
        if labels.len() > MAX_MAINTENANCE_JOBS {
            push(
                checks,
                "maintenance_jobs",
                CheckStatus::Error,
                "maintenance_job_count_exceeded",
            );
            return;
        }
        let mut loaded = 0usize;
        let mut active = 0usize;
        for label in labels {
            let target = format!("{domain}/{label}");
            let result = match Command::new("launchctl").arg("print").arg(target).output() {
                Ok(result) if result.status.success() => result,
                _ => continue,
            };
            loaded += 1;
            let text = String::from_utf8_lossy(&result.stdout);
            if launchctl_output_running_pid(&text).is_some() {
                active += 1;
            }
        }
        if loaded == 0 {
            push(checks, "maintenance_jobs", CheckStatus::Ok, "none");
        } else if active > 0 {
            push(
                checks,
                "maintenance_jobs",
                CheckStatus::Warning,
                &format!("active_stale_jobs count={active} loaded={loaded}"),
            );
        } else {
            push(
                checks,
                "maintenance_jobs",
                CheckStatus::Warning,
                &format!("stale_jobs_loaded count={loaded}"),
            );
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = exclude_label;
        push(
            checks,
            "maintenance_jobs",
            CheckStatus::Warning,
            "launchd_check_not_supported",
        );
    }
}

fn inspect_launchd_service(label: &str, name: &str, checks: &mut Vec<DoctorCheck>) -> Option<u32> {
    #[cfg(target_os = "macos")]
    {
        let uid = unsafe { libc_getuid() };
        let target = format!("gui/{uid}/{label}");
        match Command::new("launchctl").arg("print").arg(target).output() {
            Ok(output) if output.status.success() => {
                let text = String::from_utf8_lossy(&output.stdout);
                if let Some(pid) = launchctl_output_running_pid(&text) {
                    push(checks, name, CheckStatus::Ok, "running");
                    Some(pid)
                } else {
                    push(checks, name, CheckStatus::Error, "loaded_but_not_running");
                    None
                }
            }
            _ => {
                push(checks, name, CheckStatus::Error, "not_loaded_or_unreadable");
                None
            }
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = label;
        push(
            checks,
            name,
            CheckStatus::Warning,
            "launchd_check_not_supported",
        );
        None
    }
}

fn current_process_descends_from(ancestor_pid: u32) -> bool {
    #[cfg(target_os = "macos")]
    {
        if ancestor_pid <= 1 {
            return false;
        }
        let mut current = std::process::id();
        for _ in 0..32 {
            let Some(parent) = macos_parent_pid(current) else {
                return false;
            };
            if parent == ancestor_pid {
                return true;
            }
            if parent <= 1 || parent == current {
                return false;
            }
            current = parent;
        }
        false
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = ancestor_pid;
        false
    }
}

#[cfg(target_os = "macos")]
fn macos_parent_pid(pid: u32) -> Option<u32> {
    let output = Command::new("/bin/ps")
        .args(["-o", "ppid=", "-p", &pid.to_string()])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let parent = String::from_utf8_lossy(&output.stdout)
        .trim()
        .parse()
        .ok()?;
    (parent != 0).then_some(parent)
}

#[cfg(target_os = "macos")]
unsafe fn libc_getuid() -> u32 {
    unsafe extern "C" {
        fn getuid() -> u32;
    }
    unsafe { getuid() }
}

#[cfg(any(target_os = "macos", test))]
fn launchctl_output_running_pid(output: &str) -> Option<u32> {
    if !output.lines().any(|line| line.trim() == "state = running") {
        return None;
    }
    output.lines().find_map(|line| {
        let line = line.trim();
        line.strip_prefix("pid = ")?.parse::<u32>().ok()
    })
}

#[cfg(any(target_os = "macos", test))]
fn launchctl_output_runs(output: &str) -> Option<u64> {
    output.lines().find_map(|line| {
        let line = line.trim();
        line.strip_prefix("runs = ")?.parse::<u64>().ok()
    })
}

fn inspect_agent_hub_transport(agent_pid: Option<u32>, checks: &mut Vec<DoctorCheck>) -> bool {
    #[cfg(target_os = "macos")]
    {
        let Some(agent_pid) = agent_pid else {
            push(
                checks,
                "agent_hub_transport",
                CheckStatus::Error,
                "agent_not_running",
            );
            return false;
        };
        let output = Command::new("lsof")
            .args([
                "-nP",
                "-a",
                "-p",
                &agent_pid.to_string(),
                "-iTCP@127.0.0.1:7443",
                "-sTCP:ESTABLISHED",
            ])
            .output();
        match output {
            Ok(output)
                if output.status.success()
                    && String::from_utf8_lossy(&output.stdout)
                        .lines()
                        .skip(1)
                        .any(|line| !line.trim().is_empty()) =>
            {
                push(
                    checks,
                    "agent_hub_transport",
                    CheckStatus::Ok,
                    "loopback_established",
                );
                true
            }
            Ok(_) => {
                push(
                    checks,
                    "agent_hub_transport",
                    CheckStatus::Error,
                    "loopback_not_established",
                );
                false
            }
            Err(_) => {
                push(
                    checks,
                    "agent_hub_transport",
                    CheckStatus::Error,
                    "lsof_unavailable",
                );
                false
            }
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = agent_pid;
        push(
            checks,
            "agent_hub_transport",
            CheckStatus::Warning,
            "macos_transport_check_not_supported",
        );
        false
    }
}

fn inspect_unix_socket(path: &Path, checks: &mut Vec<DoctorCheck>) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{FileTypeExt, PermissionsExt};
        let Ok(metadata) = std::fs::symlink_metadata(path) else {
            push(checks, "grant_signer_socket", CheckStatus::Error, "missing");
            return;
        };
        let parent_safe = path
            .parent()
            .and_then(|parent| std::fs::symlink_metadata(parent).ok())
            .is_some_and(|parent| {
                !parent.file_type().is_symlink()
                    && parent.is_dir()
                    && parent.permissions().mode() & 0o022 == 0
            });
        if metadata.file_type().is_socket()
            && parent_safe
            && metadata.permissions().mode() & 0o007 == 0
        {
            push(
                checks,
                "grant_signer_socket",
                CheckStatus::Ok,
                "private_socket_present",
            );
        } else {
            push(
                checks,
                "grant_signer_socket",
                CheckStatus::Error,
                "unsafe_socket_or_parent",
            );
        }
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        push(
            checks,
            "grant_signer_socket",
            CheckStatus::Warning,
            "unix_socket_check_not_supported",
        );
    }
}

fn inspect_tls(path: &Path, format: CertificateFormat, name: &str, checks: &mut Vec<DoctorCheck>) {
    match inspect_certificate_file(path, format, DEFAULT_TLS_WARN_BEFORE_SECS) {
        Ok(inspection) => match inspection.health {
            CertificateHealth::Healthy => push(checks, name, CheckStatus::Ok, "healthy"),
            CertificateHealth::Expiring => push(checks, name, CheckStatus::Warning, "expiring"),
            CertificateHealth::Expired | CertificateHealth::NotYetValid => {
                push(checks, name, CheckStatus::Error, inspection.health.as_str())
            }
        },
        Err(error) => push(checks, name, CheckStatus::Error, error.safe_error_code()),
    }
}

fn handoff_recovery_guidance(status: &HandoffRuntimeStatus) -> (CheckStatus, &'static str) {
    if status.faulted {
        return (CheckStatus::Error, "runtime_faulted_fail_closed");
    }
    if status.recovery_required {
        if status.recovery_expired {
            return (
                CheckStatus::Warning,
                "expired_recovery_exact_recover_rebind_or_abandon_if_prior_surface_absent",
            );
        }
        return (
            CheckStatus::Warning,
            "non_expired_recovery_use_exact_recover_reissue",
        );
    }
    if status.active.is_some() || status.resume_requested {
        return (
            CheckStatus::Warning,
            "active_handoff_finish_or_cancel_before_runtime_upgrade",
        );
    }
    (CheckStatus::Ok, "idle_no_recovery")
}

fn inspect_handoff_recovery(socket: &Path, checks: &mut Vec<DoctorCheck>) {
    match exchange_unix_handoff_control(socket, &LocalHandoffControlRequest::Status) {
        Ok(response) if response.ok => match response.status {
            Some(status) => {
                let (check_status, detail) = handoff_recovery_guidance(&status);
                push(checks, "handoff_recovery", check_status, detail);
            }
            None => push(
                checks,
                "handoff_recovery",
                CheckStatus::Error,
                "status_missing_fail_closed",
            ),
        },
        _ => push(
            checks,
            "handoff_recovery",
            CheckStatus::Error,
            "status_unavailable_fail_closed",
        ),
    }
}

fn inspect_cua(command: &Path, expected: Option<&str>, checks: &mut Vec<DoctorCheck>) {
    let output = Command::new(command).arg("--version").output();
    let Ok(output) = output else {
        push(
            checks,
            "cua_version",
            CheckStatus::Error,
            "command_unavailable",
        );
        return;
    };
    if !output.status.success() {
        push(
            checks,
            "cua_version",
            CheckStatus::Error,
            "version_probe_failed",
        );
        return;
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let combined = format!("{stdout}\n{stderr}");
    if let Some(expected) = expected {
        if combined.contains(expected) {
            push(
                checks,
                "cua_version",
                CheckStatus::Ok,
                "expected_version_present",
            );
        } else {
            push(
                checks,
                "cua_version",
                CheckStatus::Error,
                "version_pin_mismatch",
            );
        }
    } else {
        push(
            checks,
            "cua_version",
            CheckStatus::Warning,
            "version_not_pinned_for_doctor",
        );
    }
}

fn inspect_storage_capacity(path: &Path, name: &str, checks: &mut Vec<DoctorCheck>) {
    let Some(existing) = nearest_existing_ancestor(path) else {
        push(checks, name, CheckStatus::Warning, "capacity_unavailable");
        return;
    };
    let result = fs2::available_space(existing).map_err(|_| ());
    let (status, detail) = storage_capacity_status(result);
    push(checks, name, status, detail);
}

fn nearest_existing_ancestor(path: &Path) -> Option<&Path> {
    let mut candidate = Some(path);
    while let Some(path) = candidate {
        if path.exists() {
            return Some(path);
        }
        candidate = path.parent();
    }
    None
}

fn storage_capacity_status(available: Result<u64, ()>) -> (CheckStatus, &'static str) {
    match available {
        Ok(bytes) if bytes < CRITICAL_AVAILABLE_STORAGE_BYTES => {
            (CheckStatus::Warning, "critical_lt_64_mib")
        }
        Ok(_) => (CheckStatus::Ok, "available_ge_64_mib"),
        Err(()) => (CheckStatus::Warning, "capacity_unavailable"),
    }
}

fn push(checks: &mut Vec<DoctorCheck>, name: &str, status: CheckStatus, detail: &str) {
    checks.push(DoctorCheck {
        name: name.to_owned(),
        status,
        detail: detail.to_owned(),
    });
}

fn read_bounded_json<T: for<'de> Deserialize<'de>>(path: &Path, max_bytes: u64) -> Result<T, ()> {
    let metadata = std::fs::symlink_metadata(path).map_err(|_| ())?;
    if metadata.file_type().is_symlink()
        || !metadata.is_file()
        || metadata.len() == 0
        || metadata.len() > max_bytes
    {
        return Err(());
    }
    let file = File::open(path).map_err(|_| ())?;
    let mut bytes = Vec::with_capacity(usize::try_from(metadata.len()).unwrap_or(0));
    file.take(max_bytes.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|_| ())?;
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > max_bytes {
        return Err(());
    }
    serde_json::from_slice(&bytes).map_err(|_| ())
}

fn sha256_file(path: &Path) -> Result<String, ()> {
    let metadata = std::fs::symlink_metadata(path).map_err(|_| ())?;
    if metadata.file_type().is_symlink()
        || !metadata.is_file()
        || metadata.len() == 0
        || metadata.len() > MAX_HASHED_BINARY_BYTES
    {
        return Err(());
    }
    let mut file = File::open(path).map_err(|_| ())?;
    let mut context = DigestContext::new(&SHA256);
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer).map_err(|_| ())?;
        if read == 0 {
            break;
        }
        context.update(&buffer[..read]);
    }
    let digest = context.finish();
    let mut output = String::with_capacity(64);
    for byte in digest.as_ref() {
        let _ = write!(&mut output, "{byte:02x}");
    }
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_dir(label: &str) -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("cumg-doctor-{label}-{unique}"))
    }

    fn quarantine_inspection(
        reason: &str,
        capability: &str,
        device_generation: u64,
        current_device_generation: Option<u64>,
        dispatch_binding_present: bool,
    ) -> crate::v2_maintenance::QuarantineInspection {
        crate::v2_maintenance::QuarantineInspection {
            blocking_operation_id: "op_test".into(),
            device_id: "dev_test".into(),
            device_generation,
            current_device_generation,
            capability: capability.into(),
            workflow_id: None,
            workflow_step_id: None,
            client_correlation_id: None,
            request_fingerprint_present: false,
            evidence_envelope: None,
            dispatch_binding_present,
            semantic_operation_class: capability.into(),
            effect_class: "effectful".into(),
            target_class: "process".into(),
            effect_kind: "process".into(),
            verification_kind: "terminal_result".into(),
            dispatch_recorded: true,
            prepared_at_ms: 10,
            dispatched_at_ms: Some(11),
            indeterminate_at_ms: 11,
            indeterminate_reason: reason.into(),
            evidence_class: None,
            evidence_status: "missing".into(),
            reconciliation_status: "auto_reconciling".into(),
            recovery_disposition: "await_authoritative_evidence".into(),
            manual_audit_required: false,
            retry_safe: false,
            execution_outcome: "indeterminate".into(),
            retirement_eligibility: "ineligible_policy".into(),
            retirement_policy: None,
            recommended_action: "keep_quarantine".into(),
        }
    }

    fn quarantine_report(
        quarantines: Vec<crate::v2_maintenance::QuarantineInspection>,
    ) -> crate::v2_maintenance::QuarantineInspectionReport {
        crate::v2_maintenance::QuarantineInspectionReport {
            quarantines,
            recovery_guidance: crate::v2_maintenance::QuarantineRecoveryGuidance {
                confirmed_not_executed: "independent evidence".into(),
                confirmed_effect_applied_uncommitted: "independent evidence".into(),
                confirmed_completed: "independent evidence".into(),
                otherwise: "keep quarantine".into(),
                replay_old_operation: false,
            },
        }
    }

    fn readiness_checks(cua_status: CheckStatus) -> Vec<DoctorCheck> {
        vec![
            DoctorCheck {
                name: "hub_state".into(),
                status: CheckStatus::Ok,
                detail: "ok".into(),
            },
            DoctorCheck {
                name: "agent_state".into(),
                status: CheckStatus::Ok,
                detail: "ok".into(),
            },
            DoctorCheck {
                name: "hub_service".into(),
                status: CheckStatus::Ok,
                detail: "ok".into(),
            },
            DoctorCheck {
                name: "agent_service".into(),
                status: CheckStatus::Ok,
                detail: "ok".into(),
            },
            DoctorCheck {
                name: "agent_hub_transport".into(),
                status: CheckStatus::Ok,
                detail: "ok".into(),
            },
            DoctorCheck {
                name: "cua_version".into(),
                status: cua_status,
                detail: "bounded".into(),
            },
        ]
    }

    fn readiness_capabilities() -> Vec<DeviceCapability> {
        vec![
            DeviceCapability::ListApplications,
            DeviceCapability::ListWindows,
            DeviceCapability::ReadFile,
            DeviceCapability::ListDirectory,
            DeviceCapability::ExecuteProcess,
            DeviceCapability::Shell,
            DeviceCapability::BrowserInspect,
            DeviceCapability::BrowserPrepare,
            DeviceCapability::BrowserNavigate,
        ]
    }

    fn v2_mutation_authority() -> MutationAuthoritySummary {
        MutationAuthoritySummary {
            owner: "v2".into(),
            epoch: 7,
        }
    }

    #[test]
    fn readiness_preserves_observation_while_quarantine_fences_effectful_lanes() {
        let checks = readiness_checks(CheckStatus::Ok);
        let capabilities = readiness_capabilities();
        let authority = v2_mutation_authority();
        let readiness = summarize_readiness(
            &checks,
            Some(1),
            Some(&capabilities),
            true,
            Some(&authority),
        );

        assert_eq!(readiness.device, "degraded_operator_action_required");
        assert_eq!(readiness.lanes.control_plane, LaneReadiness::Ready);
        assert_eq!(
            readiness.lanes.computer_use_observation,
            LaneReadiness::Ready
        );
        assert_eq!(readiness.lanes.filesystem_observation, LaneReadiness::Ready);
        assert_eq!(
            readiness.lanes.effectful_execution,
            LaneReadiness::IndeterminateFenced
        );
        assert_eq!(
            readiness.lanes.browser_effectful_execution,
            LaneReadiness::IndeterminateFenced
        );
        assert_eq!(readiness.blocking_operation_present, Some(true));
        assert_eq!(readiness.blocking_operation_retry_safe, Some(false));
        assert_eq!(
            readiness.operator_action.as_deref(),
            Some("inspect_reconciliation_status")
        );
    }

    #[test]
    fn readiness_reports_supported_lanes_ready_without_quarantine() {
        let checks = readiness_checks(CheckStatus::Ok);
        let capabilities = readiness_capabilities();
        let authority = v2_mutation_authority();
        let readiness = summarize_readiness(
            &checks,
            Some(0),
            Some(&capabilities),
            true,
            Some(&authority),
        );

        assert_eq!(readiness.device, "healthy");
        assert_eq!(
            readiness.lanes.computer_use_observation,
            LaneReadiness::Ready
        );
        assert_eq!(readiness.lanes.filesystem_observation, LaneReadiness::Ready);
        assert_eq!(readiness.lanes.effectful_execution, LaneReadiness::Ready);
        assert_eq!(
            readiness.lanes.browser_effectful_execution,
            LaneReadiness::Ready
        );
        assert_eq!(readiness.blocking_operation_present, Some(false));
        assert_eq!(readiness.blocking_operation_retry_safe, None);
        assert_eq!(readiness.operator_action, None);
    }

    #[test]
    fn readiness_distinguishes_backend_unavailable_from_quarantine_fencing() {
        let checks = readiness_checks(CheckStatus::Error);
        let capabilities = readiness_capabilities();
        let authority = v2_mutation_authority();
        let readiness = summarize_readiness(
            &checks,
            Some(0),
            Some(&capabilities),
            true,
            Some(&authority),
        );

        assert_eq!(
            readiness.lanes.computer_use_observation,
            LaneReadiness::Unavailable
        );
        assert_eq!(
            readiness.lanes.browser_effectful_execution,
            LaneReadiness::Unavailable
        );
        assert_eq!(
            readiness.lanes.effectful_execution,
            LaneReadiness::Unavailable
        );
        assert_eq!(readiness.lanes.filesystem_observation, LaneReadiness::Ready);
        assert_ne!(
            readiness.lanes.browser_effectful_execution,
            LaneReadiness::IndeterminateFenced
        );
    }

    #[test]
    fn readiness_fails_closed_when_quarantine_state_is_unknown() {
        let checks = readiness_checks(CheckStatus::Ok);
        let capabilities = readiness_capabilities();
        let authority = v2_mutation_authority();
        let readiness =
            summarize_readiness(&checks, None, Some(&capabilities), true, Some(&authority));

        assert_eq!(readiness.device, "degraded");
        assert_eq!(readiness.lanes.effectful_execution, LaneReadiness::Unknown);
        assert_eq!(
            readiness.lanes.browser_effectful_execution,
            LaneReadiness::Unknown
        );
        assert_eq!(readiness.blocking_operation_present, None);
        assert_eq!(
            readiness.operator_action.as_deref(),
            Some("inspect_doctor_failures")
        );
    }

    #[test]
    fn dispatched_checkpoint_regression_preserves_restart_quarantine_but_classifies_live_caller() {
        use crate::v2_execution_safety::{
            AuthoritativeOperationController, OperationDispatchBinding, OperationOwner,
        };
        use crate::v2_m0::{DeviceCapability, DeviceIdentity, DeviceRegistry};
        use crate::v2_m0_execution::{AdmissionLimits, OperationRef};
        use crate::v2_m1_persistence::M1_STATE_SCHEMA_VERSION;

        let root = temp_dir("self-observation-checkpoint");
        std::fs::create_dir_all(&root).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        let identity = DeviceIdentity::generate();
        let mut registry = DeviceRegistry::default();
        let device_id = registry.provision_trusted_device(identity.verifying_key());
        let mut registry_snapshot = registry.snapshot();
        registry_snapshot.devices[0].generation = 7;

        let owner = OperationOwner::new("https://issuer.example", "doctor").unwrap();
        let operation_id = "op_doctor_self_observation";
        let mut execution = AuthoritativeOperationController::new(AdmissionLimits {
            max_global_active: 1,
            max_queued_per_device: 1,
        })
        .unwrap();
        execution
            .prepare(
                OperationRef {
                    device_id,
                    device_generation: 7,
                    operation_id: operation_id.into(),
                },
                owner.clone(),
                DeviceCapability::ExecuteProcess,
                10,
            )
            .unwrap();
        execution
            .mark_dispatched_with_binding(
                operation_id,
                &owner,
                7,
                Some(OperationDispatchBinding::new(3, "grant_self_observation").unwrap()),
                11,
            )
            .unwrap();
        let state = HubPersistentState {
            schema_version: M1_STATE_SCHEMA_VERSION,
            registry: registry_snapshot,
            execution: execution.snapshot_for_restart(),
        };
        CheckpointStore::new(root.clone(), "hub")
            .unwrap()
            .save(&state)
            .unwrap();

        let report = inspect_quarantines_read_only(&root, None).unwrap();
        assert_eq!(report.quarantines.len(), 1);
        assert_eq!(
            report.quarantines[0].indeterminate_reason,
            "hub_restart_after_dispatch"
        );
        assert_eq!(report.quarantines[0].current_device_generation, Some(7));
        assert_eq!(classify_quarantine_report(&report, 1, true), (0, 1));

        let (_, restored) = state
            .restore(AdmissionLimits {
                max_global_active: 1,
                max_queued_per_device: 1,
            })
            .unwrap();
        assert!(
            restored
                .quarantine(&report.quarantines[0].device_id)
                .is_some()
        );

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn doctor_recovery_mode_is_explicitly_restricted_when_quarantine_exists() {
        assert_eq!(
            recovery_mode_for_quarantine_count(0),
            ("normal", CheckStatus::Ok)
        );
        assert_eq!(
            recovery_mode_for_quarantine_count(1),
            ("restricted_read_only", CheckStatus::Warning)
        );
        assert_eq!(
            recovery_mode_for_quarantine_count(3),
            ("restricted_read_only", CheckStatus::Warning)
        );
    }

    #[test]
    fn in_band_current_generation_restart_snapshot_is_not_persistent_quarantine() {
        let report = quarantine_report(vec![quarantine_inspection(
            "hub_restart_after_dispatch",
            "execute_process",
            7,
            Some(7),
            true,
        )]);
        assert_eq!(classify_quarantine_report(&report, 1, true), (0, 1));

        let shell = quarantine_report(vec![quarantine_inspection(
            "hub_restart_after_dispatch",
            "shell",
            7,
            Some(7),
            true,
        )]);
        assert_eq!(classify_quarantine_report(&shell, 1, true), (0, 1));
    }

    #[test]
    fn quarantine_self_observation_classification_fails_closed() {
        let exact = quarantine_inspection(
            "hub_restart_after_dispatch",
            "execute_process",
            7,
            Some(7),
            true,
        );
        assert_eq!(
            classify_quarantine_report(&quarantine_report(vec![exact.clone()]), 1, false),
            (1, 0)
        );

        let real = quarantine_inspection("connection_lost", "execute_process", 7, Some(7), true);
        assert_eq!(
            classify_quarantine_report(&quarantine_report(vec![real]), 1, true),
            (1, 0)
        );

        let stale_generation = quarantine_inspection(
            "hub_restart_after_dispatch",
            "execute_process",
            6,
            Some(7),
            true,
        );
        assert_eq!(
            classify_quarantine_report(&quarantine_report(vec![stale_generation]), 1, true),
            (1, 0)
        );

        let no_binding = quarantine_inspection(
            "hub_restart_after_dispatch",
            "execute_process",
            7,
            Some(7),
            false,
        );
        assert_eq!(
            classify_quarantine_report(&quarantine_report(vec![no_binding]), 1, true),
            (1, 0)
        );

        let unrelated = quarantine_inspection("connection_lost", "pointer_click", 5, Some(7), true);
        assert_eq!(
            classify_quarantine_report(&quarantine_report(vec![exact, unrelated]), 1, true,),
            (2, 0)
        );
    }

    #[test]
    fn launchctl_parser_requires_explicit_running_state() {
        assert_eq!(
            launchctl_output_running_pid("path = /tmp/x\nstate = running\npid = 42\n"),
            Some(42)
        );
        assert_eq!(
            launchctl_output_running_pid("state = waiting\npid = 42\nlast exit code = 1\n"),
            None
        );
        assert_eq!(launchctl_output_running_pid("state = running\n"), None);
        assert_eq!(
            launchctl_output_runs("state = running\nruns = 1\n"),
            Some(1)
        );
        assert_eq!(
            launchctl_output_runs("state = running\nruns = nope\n"),
            None
        );
    }

    #[test]
    fn maintenance_label_parser_accepts_only_bounded_known_job_families() {
        let current = "com.github.git-ksk.cumg-v2-maintenance.upgrade.1.2.deadbeef";
        let legacy = "com.git-ksk.cumg-v2-upgrade-once.1787651265";
        let labels = launchctl_domain_maintenance_labels(&format!(
            "0 0 {legacy}\n0 0 {current}\n0 0 com.github.git-ksk.cumg-v2-hub\n"
        ));
        assert_eq!(labels, vec![legacy.to_owned(), current.to_owned()]);
        assert!(is_launchd_maintenance_label(current));
        assert!(is_launchd_maintenance_label(legacy));
        assert!(!is_launchd_maintenance_label(
            "com.github.git-ksk.cumg-v2-hub"
        ));
        assert!(!is_launchd_maintenance_label(
            "com.github.git-ksk.cumg-v2-maintenance.bad/secret"
        ));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn recovery_key_readiness_uses_only_verified_public_helper_without_user_presence() {
        use base64::{Engine as _, engine::general_purpose::STANDARD};
        use ring::{
            rand::SystemRandom,
            signature::{ECDSA_P256_SHA256_ASN1_SIGNING, EcdsaKeyPair, KeyPair as _},
        };
        use std::os::unix::fs::PermissionsExt as _;

        let root = temp_dir("recovery-readiness-public-only");
        let hub = root.join("hub");
        let bin = root.join("bin");
        let recovery = root.join("recovery");
        for directory in [&root, &hub, &bin, &recovery] {
            std::fs::create_dir_all(directory).unwrap();
            std::fs::set_permissions(directory, std::fs::Permissions::from_mode(0o700)).unwrap();
        }

        let rng = SystemRandom::new();
        let pkcs8 = EcdsaKeyPair::generate_pkcs8(&ECDSA_P256_SHA256_ASN1_SIGNING, &rng).unwrap();
        let key = EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_ASN1_SIGNING, pkcs8.as_ref(), &rng)
            .unwrap();
        let public = key.public_key().as_ref();
        std::fs::write(
            hub.join(crate::v2_online_recovery::RECOVERY_PUBLIC_KEY_FILENAME),
            public,
        )
        .unwrap();

        let sealed = recovery.join("recovery-key.sealed");
        std::fs::write(&sealed, b"sealed-key-fixture").unwrap();
        std::fs::set_permissions(&sealed, std::fs::Permissions::from_mode(0o600)).unwrap();

        let helper = bin.join("v2_recovery_enclave_helper");
        let marker_file = root.join("helper-operation");
        let response = serde_json::json!({
            "schema_version": 1,
            "sealed_key_base64": null,
            "public_key_base64": STANDARD.encode(public),
            "signature_base64": null
        });
        let helper_body = format!(
            "#!/bin/sh\n[ \"$1\" = \"public\" ] || exit 23\nprintf '%s\\n' \"$1\" > '{}'\ncat >/dev/null\nprintf '%s\\n' '{}'\n",
            marker_file.display(),
            response
        );
        std::fs::write(&helper, helper_body).unwrap();
        std::fs::set_permissions(&helper, std::fs::Permissions::from_mode(0o700)).unwrap();

        let config = DoctorConfig {
            hub_state_dir: hub,
            agent_state_dir: root.join("agent"),
            runtime_manifest: root.join("runtime-manifest.json"),
            binary_dir: bin,
            hub_launchd_label: "hub".into(),
            agent_launchd_label: "agent".into(),
            grant_signer_launchd_label: None,
            grant_signer_socket: None,
            tls_server_certificate: None,
            tls_root_certificate: None,
            cua_command: None,
            expected_cua_version: None,
            mutation_authority_dir: None,
            handoff_control_socket: None,
            maintenance_job_exclude_label: None,
            recovery_key_file: Some(sealed),
            recovery_helper: Some(helper),
        };
        let mut checks = Vec::new();
        let summary = inspect_recovery_key_readiness(&config, true, &mut checks);
        assert_eq!(summary.status, RecoveryKeyReadinessStatus::Ready);
        assert_eq!(
            std::fs::read_to_string(marker_file).unwrap().trim(),
            "public"
        );
        assert_eq!(
            checks,
            vec![DoctorCheck {
                name: "recovery_key_readiness".into(),
                status: CheckStatus::Ok,
                detail: "ready".into(),
            }]
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn recovery_key_readiness_never_executes_unverified_helper() {
        use std::os::unix::fs::PermissionsExt as _;
        let root = temp_dir("recovery-readiness-unverified-helper");
        let bin = root.join("bin");
        let recovery = root.join("recovery");
        for directory in [&root, &bin, &recovery] {
            std::fs::create_dir_all(directory).unwrap();
            std::fs::set_permissions(directory, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        let sealed = recovery.join("recovery-key.sealed");
        std::fs::write(&sealed, b"sealed-key-fixture").unwrap();
        std::fs::set_permissions(&sealed, std::fs::Permissions::from_mode(0o600)).unwrap();
        let helper = bin.join("v2_recovery_enclave_helper");
        let marker_file = root.join("executed");
        std::fs::write(
            &helper,
            format!("#!/bin/sh\ntouch '{}'\nexit 0\n", marker_file.display()),
        )
        .unwrap();
        std::fs::set_permissions(&helper, std::fs::Permissions::from_mode(0o700)).unwrap();
        let config = DoctorConfig {
            hub_state_dir: root.join("hub"),
            agent_state_dir: root.join("agent"),
            runtime_manifest: root.join("runtime-manifest.json"),
            binary_dir: bin,
            hub_launchd_label: "hub".into(),
            agent_launchd_label: "agent".into(),
            grant_signer_launchd_label: None,
            grant_signer_socket: None,
            tls_server_certificate: None,
            tls_root_certificate: None,
            cua_command: None,
            expected_cua_version: None,
            mutation_authority_dir: None,
            handoff_control_socket: None,
            maintenance_job_exclude_label: None,
            recovery_key_file: Some(sealed),
            recovery_helper: Some(helper),
        };
        assert_eq!(
            inspect_local_recovery_public_key(&config, false),
            RecoveryPublicKeyInput::HelperUnavailable
        );
        assert!(!marker_file.exists());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn recovery_key_readiness_distinguishes_ready_missing_mismatch_and_helper_failure() {
        let a = [4_u8; 65];
        let b = [5_u8; 65];
        assert_eq!(
            classify_recovery_key_readiness(
                RecoveryPublicKeyInput::Available(a),
                RecoveryPublicKeyInput::Available(a)
            ),
            RecoveryKeyReadinessStatus::Ready
        );
        assert_eq!(
            classify_recovery_key_readiness(
                RecoveryPublicKeyInput::Missing,
                RecoveryPublicKeyInput::Missing
            ),
            RecoveryKeyReadinessStatus::Unprovisioned
        );
        assert_eq!(
            classify_recovery_key_readiness(
                RecoveryPublicKeyInput::Available(a),
                RecoveryPublicKeyInput::Missing
            ),
            RecoveryKeyReadinessStatus::SealedKeyMissing
        );
        assert_eq!(
            classify_recovery_key_readiness(
                RecoveryPublicKeyInput::Missing,
                RecoveryPublicKeyInput::Available(a)
            ),
            RecoveryKeyReadinessStatus::HubVerifierMissing
        );
        assert_eq!(
            classify_recovery_key_readiness(
                RecoveryPublicKeyInput::Available(a),
                RecoveryPublicKeyInput::Available(b)
            ),
            RecoveryKeyReadinessStatus::PublicKeyMismatch
        );
        assert_eq!(
            classify_recovery_key_readiness(
                RecoveryPublicKeyInput::Available(a),
                RecoveryPublicKeyInput::HelperUnavailable
            ),
            RecoveryKeyReadinessStatus::HelperUnavailable
        );
        assert_eq!(
            classify_recovery_key_readiness(
                RecoveryPublicKeyInput::Unknown,
                RecoveryPublicKeyInput::Available(a)
            ),
            RecoveryKeyReadinessStatus::ReadinessUnknown
        );
    }

    #[test]
    fn runtime_manifest_verifies_exact_required_binary_hashes() {
        let root = temp_dir("manifest");
        let bin = root.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let names = [
            "v2_hub",
            "v2_agent",
            "v2_maint",
            "v2_doctor",
            "v2_status",
            "v2_recover",
            "v2_recovery_enclave_helper",
            "v2_grant_signer",
        ];
        let mut binaries = Vec::new();
        for name in names {
            let path = bin.join(name);
            std::fs::write(&path, format!("binary-{name}")).unwrap();
            binaries.push(serde_json::json!({"name": name, "sha256": sha256_file(&path).unwrap()}));
        }
        let manifest = root.join("runtime-manifest.json");
        std::fs::write(
            &manifest,
            serde_json::to_vec_pretty(&serde_json::json!({
                "schema_version": 3,
                "hub_agent_schema_version": HUB_AGENT_SCHEMA_VERSION,
                "source_commit": "0123456789abcdef0123456789abcdef01234567",
                "package_version": env!("CARGO_PKG_VERSION"),
                "binaries": binaries
            }))
            .unwrap(),
        )
        .unwrap();
        let config = DoctorConfig {
            hub_state_dir: root.join("hub"),
            agent_state_dir: root.join("agent"),
            runtime_manifest: manifest,
            binary_dir: bin,
            hub_launchd_label: "hub".into(),
            agent_launchd_label: "agent".into(),
            grant_signer_launchd_label: None,
            grant_signer_socket: None,
            tls_server_certificate: None,
            tls_root_certificate: None,
            cua_command: None,
            expected_cua_version: None,
            mutation_authority_dir: None,
            handoff_control_socket: None,
            maintenance_job_exclude_label: None,
            recovery_key_file: None,
            recovery_helper: None,
        };
        let mut runtime = RuntimeSummary {
            package_version: env!("CARGO_PKG_VERSION").into(),
            source_commit: None,
            manifest_verified: false,
            runtime_pairing: RuntimePairingStatus::Unknown,
            operator_tooling: OperatorToolingStatus::Unknown,
            checkpoint_reader_compatibility: CheckpointReaderCompatibility::Unknown,
        };
        let mut checks = Vec::new();
        verify_runtime_manifest(&config, &mut runtime, &mut checks);
        assert!(runtime.manifest_verified);
        assert_eq!(
            checks,
            vec![DoctorCheck {
                name: "runtime_manifest".into(),
                status: CheckStatus::Ok,
                detail: "verified".into(),
            }]
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn runtime_manifest_detects_stale_operator_binary_before_incident() {
        let root = temp_dir("manifest-stale-operator");
        let bin = root.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let names = [
            "v2_hub",
            "v2_agent",
            "v2_maint",
            "v2_doctor",
            "v2_status",
            "v2_recover",
            "v2_recovery_enclave_helper",
        ];
        let mut binaries = Vec::new();
        for name in names {
            let path = bin.join(name);
            std::fs::write(&path, format!("paired-{name}")).unwrap();
            binaries.push(serde_json::json!({"name": name, "sha256": sha256_file(&path).unwrap()}));
        }
        std::fs::write(bin.join("v2_maint"), b"older-maintenance-binary").unwrap();
        let manifest = root.join("runtime-manifest.json");
        std::fs::write(
            &manifest,
            serde_json::to_vec_pretty(&serde_json::json!({
                "schema_version": 3,
                "hub_agent_schema_version": HUB_AGENT_SCHEMA_VERSION,
                "source_commit": "0123456789abcdef0123456789abcdef01234567",
                "package_version": env!("CARGO_PKG_VERSION"),
                "binaries": binaries
            }))
            .unwrap(),
        )
        .unwrap();
        let config = DoctorConfig {
            hub_state_dir: root.join("hub"),
            agent_state_dir: root.join("agent"),
            runtime_manifest: manifest,
            binary_dir: bin,
            hub_launchd_label: "hub".into(),
            agent_launchd_label: "agent".into(),
            grant_signer_launchd_label: None,
            grant_signer_socket: None,
            tls_server_certificate: None,
            tls_root_certificate: None,
            cua_command: None,
            expected_cua_version: None,
            mutation_authority_dir: None,
            handoff_control_socket: None,
            maintenance_job_exclude_label: None,
            recovery_key_file: None,
            recovery_helper: None,
        };
        let mut runtime = RuntimeSummary {
            package_version: env!("CARGO_PKG_VERSION").into(),
            source_commit: None,
            manifest_verified: false,
            runtime_pairing: RuntimePairingStatus::Unknown,
            operator_tooling: OperatorToolingStatus::Unknown,
            checkpoint_reader_compatibility: CheckpointReaderCompatibility::Unknown,
        };
        let mut checks = Vec::new();
        verify_runtime_manifest(&config, &mut runtime, &mut checks);
        assert!(!runtime.manifest_verified);
        assert_eq!(runtime.runtime_pairing, RuntimePairingStatus::Skewed);
        assert_eq!(runtime.operator_tooling, OperatorToolingStatus::Stale);
        assert!(checks.iter().any(|check| {
            check.name == "runtime_manifest"
                && check.status == CheckStatus::Error
                && check.detail == "binary_digest_mismatch"
        }));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn checkpoint_reader_reports_future_schema_incompatibility_directly() {
        let root = temp_dir("future-checkpoint-reader");
        let hub = root.join("hub");
        CheckpointStore::new(&hub, "hub")
            .unwrap()
            .save(&serde_json::json!({
                "schema_version": M1_STATE_SCHEMA_VERSION,
                "execution": {
                    "schema_version": EXECUTION_SAFETY_SCHEMA_VERSION + 1
                }
            }))
            .unwrap();
        let mut checks = Vec::new();
        let compatibility = inspect_checkpoint_reader_compatibility(
            &hub,
            OperatorToolingStatus::Compatible,
            &mut checks,
        );
        assert_eq!(compatibility, CheckpointReaderCompatibility::Incompatible);
        assert_eq!(
            checks,
            vec![DoctorCheck {
                name: "checkpoint_reader_compatibility".into(),
                status: CheckStatus::Error,
                detail: "checkpoint_newer_than_reader".into(),
            }]
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn checkpoint_reader_requires_paired_operator_identity_before_green() {
        let root = temp_dir("checkpoint-reader-identity");
        let hub = root.join("hub");
        CheckpointStore::new(&hub, "hub")
            .unwrap()
            .save(&serde_json::json!({
                "schema_version": M1_STATE_SCHEMA_VERSION,
                "execution": {
                    "schema_version": EXECUTION_SAFETY_SCHEMA_VERSION
                }
            }))
            .unwrap();
        let mut checks = Vec::new();
        assert_eq!(
            inspect_checkpoint_reader_compatibility(
                &hub,
                OperatorToolingStatus::Stale,
                &mut checks,
            ),
            CheckpointReaderCompatibility::Unknown
        );
        assert_eq!(checks[0].detail, "operator_reader_identity_unverified");
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn storage_capacity_signal_is_read_only_bounded_and_low_cardinality() {
        assert_eq!(
            storage_capacity_status(Ok(CRITICAL_AVAILABLE_STORAGE_BYTES - 1)),
            (CheckStatus::Warning, "critical_lt_64_mib")
        );
        assert_eq!(
            storage_capacity_status(Ok(CRITICAL_AVAILABLE_STORAGE_BYTES)),
            (CheckStatus::Ok, "available_ge_64_mib")
        );
        assert_eq!(
            storage_capacity_status(Err(())),
            (CheckStatus::Warning, "capacity_unavailable")
        );
    }

    #[test]
    fn storage_capacity_uses_existing_ancestor_without_creating_state_directory() {
        let root = temp_dir("capacity-ancestor");
        std::fs::create_dir_all(&root).unwrap();
        let missing = root.join("agent").join("future");
        let ancestor = nearest_existing_ancestor(&missing).unwrap();
        assert_eq!(ancestor, root.as_path());
        let mut checks = Vec::new();
        inspect_storage_capacity(&missing, "agent_state_capacity", &mut checks);
        assert!(
            !missing.exists(),
            "doctor capacity inspection must stay read-only"
        );
        assert_eq!(checks.len(), 1);
        assert_eq!(checks[0].name, "agent_state_capacity");
        assert!(matches!(
            checks[0].status,
            CheckStatus::Ok | CheckStatus::Warning
        ));
        assert!(matches!(
            checks[0].detail.as_str(),
            "available_ge_64_mib" | "critical_lt_64_mib" | "capacity_unavailable"
        ));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn handoff_recovery_guidance_is_bounded_and_never_contains_locator_or_owner_ids() {
        use crate::v2_operator_handoff::{
            HandoffActiveStatus, HandoffExecutionAuthority, HandoffInterventionStatus,
            HandoffSurfaceKind,
        };
        let mut status = HandoffRuntimeStatus {
            active: None,
            recovery_required: false,
            recovery_status: None,
            recovery_epoch: None,
            recovery_expired: false,
            resume_requested: false,
            faulted: false,
            human_surface: Some(HandoffSurfaceKind::Webrtc),
            locator: Some("sensitive-locator-must-not-appear".into()),
        };
        assert_eq!(
            handoff_recovery_guidance(&status),
            (CheckStatus::Ok, "idle_no_recovery")
        );
        status.recovery_required = true;
        status.recovery_status = Some(HandoffInterventionStatus::HumanActive);
        status.recovery_epoch = Some(7);
        assert_eq!(
            handoff_recovery_guidance(&status),
            (
                CheckStatus::Warning,
                "non_expired_recovery_use_exact_recover_reissue"
            )
        );
        status.recovery_expired = true;
        let (_, detail) = handoff_recovery_guidance(&status);
        assert_eq!(
            detail,
            "expired_recovery_exact_recover_rebind_or_abandon_if_prior_surface_absent"
        );
        assert!(!detail.contains("sensitive-locator"));
        status.recovery_required = false;
        status.recovery_expired = false;
        status.active = Some(HandoffActiveStatus {
            intervention_id: "private-intervention-id".into(),
            status: HandoffInterventionStatus::HumanActive,
            epoch: 9,
            authority: HandoffExecutionAuthority::Human,
        });
        let (_, detail) = handoff_recovery_guidance(&status);
        assert_eq!(
            detail,
            "active_handoff_finish_or_cancel_before_runtime_upgrade"
        );
        assert!(!detail.contains("private-intervention-id"));
        status.faulted = true;
        assert_eq!(
            handoff_recovery_guidance(&status),
            (CheckStatus::Error, "runtime_faulted_fail_closed")
        );
    }

    #[test]
    fn bounded_hash_rejects_symlink_and_hashes_regular_file() {
        let root = temp_dir("hash");
        std::fs::create_dir(&root).unwrap();
        let file = root.join("bin");
        std::fs::write(&file, b"abc").unwrap();
        assert_eq!(
            sha256_file(&file).unwrap(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(&file, root.join("link")).unwrap();
            assert!(sha256_file(&root.join("link")).is_err());
        }
        let _ = std::fs::remove_dir_all(root);
    }
}
