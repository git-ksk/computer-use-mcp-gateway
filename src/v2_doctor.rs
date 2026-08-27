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
    pub handoff_control_socket: Option<PathBuf>,
    pub maintenance_job_exclude_label: Option<String>,
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
    pub recovery_mode: String,
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
    inspect_storage_capacity(&config.agent_state_dir, "agent_state_capacity", &mut checks);
    inspect_storage_capacity(&std::env::temp_dir(), "temp_capacity", &mut checks);

    let agent_pid =
        inspect_launchd_service(&config.agent_launchd_label, "agent_service", &mut checks);
    let transport_established = inspect_agent_hub_transport(agent_pid, &mut checks);
    // Process ancestry is local OS evidence that this doctor was actually spawned by the
    // configured Agent. A caller-provided operation ID or environment marker is never trusted.
    let in_band_live_agent_path =
        transport_established && agent_pid.is_some_and(current_process_descends_from);
    let (hub, hub_device_id) = inspect_hub(config, in_band_live_agent_path, &mut checks);
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
    if manifest.package_version != env!("CARGO_PKG_VERSION") {
        push(
            checks,
            "runtime_manifest",
            CheckStatus::Warning,
            "package_version_differs_from_doctor",
        );
    }
    let mut required = vec!["v2_hub", "v2_agent", "v2_maint", "v2_doctor"];
    #[cfg(target_os = "macos")]
    required.extend(["v2_recover", "v2_recovery_enclave_helper"]);
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
    in_band_agent_descendant: bool,
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
    (summary, device_id)
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
            handoff_control_socket: None,
            maintenance_job_exclude_label: None,
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
