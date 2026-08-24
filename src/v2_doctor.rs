use crate::v2_handoff_control::{LocalHandoffControlRequest, exchange_unix_handoff_control};
use crate::v2_m0_transport::HUB_AGENT_SCHEMA_VERSION;
use crate::v2_m1_persistence::{AgentPersistentState, CheckpointStore, HubPersistentState};
use crate::v2_maintenance::inspect_quarantines_read_only;
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
    pub handoff_control_socket: Option<PathBuf>,
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RuntimeSummary {
    pub package_version: String,
    pub source_commit: Option<String>,
    pub manifest_verified: bool,
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
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AgentSummary {
    pub state_schema: Option<u16>,
    pub replay_generation: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DoctorReport {
    pub schema_version: u16,
    pub overall: String,
    pub runtime: RuntimeSummary,
    pub hub: HubSummary,
    pub agent: AgentSummary,
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
    };
    verify_runtime_manifest(config, &mut runtime, &mut checks);

    let (hub, hub_device_id) = inspect_hub(config, &mut checks);
    let agent = inspect_agent(
        config,
        hub_device_id.as_deref(),
        hub.generation,
        &mut checks,
    );

    let _hub_pid = inspect_launchd_service(&config.hub_launchd_label, "hub_service", &mut checks);
    let agent_pid =
        inspect_launchd_service(&config.agent_launchd_label, "agent_service", &mut checks);
    inspect_agent_hub_transport(agent_pid, &mut checks);
    if let Some(label) = &config.grant_signer_launchd_label {
        inspect_launchd_service(label, "grant_signer_service", &mut checks);
    }
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
    if let Some(socket) = &config.handoff_control_socket {
        inspect_handoff_recovery(socket, &mut checks);
    }

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
        runtime,
        hub,
        agent,
        checks,
    }
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
    if manifest.schema_version != 2
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
    if manifest.package_version != env!("CARGO_PKG_VERSION") {
        push(
            checks,
            "runtime_manifest",
            CheckStatus::Warning,
            "package_version_differs_from_doctor",
        );
    }
    let mut required = vec!["v2_hub", "v2_agent", "v2_maint", "v2_doctor"];
    if config.grant_signer_launchd_label.is_some() || config.grant_signer_socket.is_some() {
        required.push("v2_grant_signer");
    }
    for name in required {
        let Some(entry) = manifest.binaries.iter().find(|entry| entry.name == name) else {
            push(
                checks,
                "runtime_manifest",
                CheckStatus::Error,
                "missing_required_binary",
            );
            return;
        };
        if entry.sha256.len() != 64 || !entry.sha256.bytes().all(|byte| byte.is_ascii_hexdigit()) {
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
                push(
                    checks,
                    "runtime_manifest",
                    CheckStatus::Error,
                    "binary_digest_mismatch",
                );
                return;
            }
            Err(_) => {
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
    runtime.manifest_verified = true;
    push(checks, "runtime_manifest", CheckStatus::Ok, "verified");
}

fn inspect_hub(
    config: &DoctorConfig,
    checks: &mut Vec<DoctorCheck>,
) -> (HubSummary, Option<String>) {
    let mut summary = HubSummary {
        state_schema: None,
        registry_schema: None,
        device_count: 0,
        generation: None,
        capability_schema: None,
        capability_revision: None,
        live_quarantine_count: None,
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
            return (summary, None);
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
            return (summary, None);
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
            summary.live_quarantine_count = Some(report.quarantines.len());
            if report.quarantines.is_empty() {
                push(checks, "live_quarantine", CheckStatus::Ok, "none");
            } else {
                push(checks, "live_quarantine", CheckStatus::Error, "present");
            }
        }
        Err(_) => push(
            checks,
            "live_quarantine",
            CheckStatus::Error,
            "inspection_failed",
        ),
    }
    (summary, device_id)
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

fn inspect_agent_hub_transport(agent_pid: Option<u32>, checks: &mut Vec<DoctorCheck>) {
    #[cfg(target_os = "macos")]
    {
        let Some(agent_pid) = agent_pid else {
            push(
                checks,
                "agent_hub_transport",
                CheckStatus::Error,
                "agent_not_running",
            );
            return;
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
            }
            Ok(_) => push(
                checks,
                "agent_hub_transport",
                CheckStatus::Error,
                "loopback_not_established",
            ),
            Err(_) => push(
                checks,
                "agent_hub_transport",
                CheckStatus::Error,
                "lsof_unavailable",
            ),
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
                "schema_version": 2,
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
            handoff_control_socket: None,
        };
        let mut runtime = RuntimeSummary {
            package_version: env!("CARGO_PKG_VERSION").into(),
            source_commit: None,
            manifest_verified: false,
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
