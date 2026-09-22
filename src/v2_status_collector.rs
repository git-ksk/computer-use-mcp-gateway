//! Shared collection path for the unified privacy-bounded V2 operator status.
//!
//! `v2_status` and the northbound MCP status tool use this module so status
//! semantics stay in one place. Collection is strictly observational.

#[cfg(target_os = "macos")]
use crate::v2_doctor::default_single_mac_recovery_key_file;
use crate::{
    v2_doctor::{DoctorConfig, run_doctor},
    v2_handoff_control::{LocalHandoffControlRequest, exchange_unix_handoff_control},
    v2_operator_status::{
        HandoffStatusInput, OperatorStatusReport, UpgradeStatusInput, build_operator_status,
    },
    v2_upgrade_transaction::{
        UpgradeTransactionError, read_upgrade_transaction, upgrade_transaction_path,
    },
};
use async_trait::async_trait;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct OperatorStatusCollectionConfig {
    pub home: PathBuf,
    pub install_root: PathBuf,
    pub run_root: PathBuf,
    pub hub_state_dir: Option<PathBuf>,
    pub agent_state_dir: Option<PathBuf>,
    pub runtime_manifest: Option<PathBuf>,
    pub binary_dir: Option<PathBuf>,
    pub hub_launchd_label: String,
    pub agent_launchd_label: String,
    pub grant_signer_launchd_label: String,
    pub grant_signer_socket: Option<PathBuf>,
    pub tls_server_certificate: Option<PathBuf>,
    pub tls_root_certificate: Option<PathBuf>,
    pub cua_command: Option<PathBuf>,
    pub expected_cua_version: Option<String>,
    pub cua_tool_timeout_secs: Option<u64>,
    pub mutation_authority_dir: Option<PathBuf>,
    pub handoff_control_socket: Option<PathBuf>,
    pub recovery_key_file: Option<PathBuf>,
    pub recovery_helper: Option<PathBuf>,
}

#[async_trait]
pub trait OperatorStatusProvider: Send + Sync {
    async fn status(&self) -> Result<OperatorStatusReport, ()>;
}

#[derive(Debug, Clone)]
pub struct CollectedOperatorStatusProvider {
    config: OperatorStatusCollectionConfig,
}

impl CollectedOperatorStatusProvider {
    pub fn new(config: OperatorStatusCollectionConfig) -> Self {
        Self { config }
    }
}

#[async_trait]
impl OperatorStatusProvider for CollectedOperatorStatusProvider {
    async fn status(&self) -> Result<OperatorStatusReport, ()> {
        let config = self.config.clone();
        tokio::task::spawn_blocking(move || collect_operator_status(&config))
            .await
            .map_err(|_| ())
    }
}

impl OperatorStatusCollectionConfig {
    pub fn installed_defaults(
        home: impl Into<PathBuf>,
        install_root: impl Into<PathBuf>,
        run_root: impl Into<PathBuf>,
    ) -> Self {
        Self {
            home: home.into(),
            install_root: install_root.into(),
            run_root: run_root.into(),
            hub_state_dir: None,
            agent_state_dir: None,
            runtime_manifest: None,
            binary_dir: None,
            hub_launchd_label: "com.github.git-ksk.cumg-v2-hub".into(),
            agent_launchd_label: "com.github.git-ksk.cumg-v2-agent".into(),
            grant_signer_launchd_label: "com.github.git-ksk.cumg-v2-grant-signer".into(),
            grant_signer_socket: None,
            tls_server_certificate: None,
            tls_root_certificate: None,
            cua_command: None,
            expected_cua_version: None,
            cua_tool_timeout_secs: None,
            mutation_authority_dir: None,
            handoff_control_socket: None,
            recovery_key_file: None,
            recovery_helper: None,
        }
    }
}

pub fn collect_operator_status(config: &OperatorStatusCollectionConfig) -> OperatorStatusReport {
    let root = &config.install_root;
    let run_root = &config.run_root;
    let binary_dir = config
        .binary_dir
        .clone()
        .unwrap_or_else(|| root.join("bin"));

    let hub_handoff = read_launchd_environment(
        &config.home,
        &config.hub_launchd_label,
        "CUMG_V2_HANDOFF_CONTROL_SOCKET",
    )
    .and_then(absolute_path);
    let hub_signer = read_launchd_environment(
        &config.home,
        &config.hub_launchd_label,
        "CUMG_V2_GRANT_SIGNER_SOCKET",
    )
    .and_then(absolute_path);
    let agent_cua = read_launchd_environment(
        &config.home,
        &config.agent_launchd_label,
        "CUMG_V2_CUA_COMMAND",
    )
    .and_then(absolute_path);
    let agent_cua_version = read_launchd_environment(
        &config.home,
        &config.agent_launchd_label,
        "CUMG_V2_CUA_BACKEND_VERSION",
    )
    .filter(|value| safe_version(value));
    let agent_cua_tool_timeout_secs = read_launchd_environment(
        &config.home,
        &config.agent_launchd_label,
        "CUMG_V2_CUA_TOOL_TIMEOUT_SECS",
    )
    .and_then(|value| value.parse::<u64>().ok())
    .filter(|value| *value > 0);
    let agent_mutation = read_launchd_environment(
        &config.home,
        &config.agent_launchd_label,
        "CUMG_MUTATION_AUTHORITY_DIR",
    )
    .and_then(absolute_path);

    let fallback_cua = config.home.join(".local/bin/cua-driver");
    let cua_command = config
        .cua_command
        .clone()
        .or(agent_cua)
        .or_else(|| fallback_cua.is_file().then_some(fallback_cua));
    let expected_cua_version = config.expected_cua_version.clone().or(agent_cua_version);
    let cua_tool_timeout_secs = config
        .cua_tool_timeout_secs
        .or(agent_cua_tool_timeout_secs)
        .unwrap_or(30);
    let mutation_authority_dir = config
        .mutation_authority_dir
        .clone()
        .or(agent_mutation)
        .or_else(|| default_mutation_authority_dir(root));
    let handoff_control_socket = config.handoff_control_socket.clone().or(hub_handoff);
    let grant_signer_socket = config
        .grant_signer_socket
        .clone()
        .or(hub_signer)
        .or_else(|| default_grant_signer_socket(run_root));

    let doctor_config = DoctorConfig {
        hub_state_dir: config
            .hub_state_dir
            .clone()
            .unwrap_or_else(|| default_hub_state_dir(root)),
        agent_state_dir: config
            .agent_state_dir
            .clone()
            .unwrap_or_else(|| default_agent_state_dir(root)),
        runtime_manifest: config
            .runtime_manifest
            .clone()
            .unwrap_or_else(|| root.join("runtime-manifest.json")),
        binary_dir,
        hub_launchd_label: config.hub_launchd_label.clone(),
        agent_launchd_label: config.agent_launchd_label.clone(),
        grant_signer_launchd_label: Some(config.grant_signer_launchd_label.clone()),
        grant_signer_socket,
        tls_server_certificate: config
            .tls_server_certificate
            .clone()
            .or_else(|| default_tls_server_certificate(root)),
        tls_root_certificate: config
            .tls_root_certificate
            .clone()
            .or_else(|| default_tls_root_certificate(root)),
        cua_command,
        expected_cua_version,
        cua_tool_timeout_secs,
        mutation_authority_dir,
        handoff_control_socket: handoff_control_socket.clone(),
        maintenance_job_exclude_label: None,
        recovery_key_file: config
            .recovery_key_file
            .clone()
            .or_else(|| default_recovery_key_file(root, &config.home)),
        recovery_helper: config
            .recovery_helper
            .clone()
            .or_else(|| default_recovery_helper(root)),
    };
    let doctor = run_doctor(&doctor_config);

    let handoff_status = handoff_control_socket
        .as_deref()
        .map(|socket| exchange_unix_handoff_control(socket, &LocalHandoffControlRequest::Status));
    let handoff_input = match handoff_status.as_ref() {
        None => HandoffStatusInput::NotConfigured,
        Some(Ok(response)) if response.ok => match response.status.as_ref() {
            Some(status) => HandoffStatusInput::Available(status),
            None => HandoffStatusInput::Unavailable,
        },
        Some(_) => HandoffStatusInput::Unavailable,
    };

    let transaction_path = upgrade_transaction_path(root);
    let transaction = match std::fs::symlink_metadata(&transaction_path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(_) => Some(Err(())),
        Ok(_) => Some(
            read_upgrade_transaction(&transaction_path).map_err(|error| match error {
                UpgradeTransactionError::Io(_) => (),
                UpgradeTransactionError::UnsafeRecord
                | UpgradeTransactionError::InvalidJson
                | UpgradeTransactionError::UnsupportedSchema(_)
                | UpgradeTransactionError::InvalidRecord(_)
                | UpgradeTransactionError::InvalidCompletedContract => (),
            }),
        ),
    };
    let upgrade_input = match transaction.as_ref() {
        None => UpgradeStatusInput::None,
        Some(Ok(record)) => UpgradeStatusInput::Available(record),
        Some(Err(())) => UpgradeStatusInput::Unavailable,
    };

    build_operator_status(&doctor, handoff_input, upgrade_input)
}

#[cfg(target_os = "windows")]
fn default_hub_state_dir(root: &Path) -> PathBuf {
    root.join("v2-windows-shell/state/hub")
}

#[cfg(not(target_os = "windows"))]
fn default_hub_state_dir(root: &Path) -> PathBuf {
    root.join("v2/state/hub")
}

#[cfg(target_os = "windows")]
fn default_agent_state_dir(root: &Path) -> PathBuf {
    root.join("v2-windows-shell/state/agent")
}

#[cfg(not(target_os = "windows"))]
fn default_agent_state_dir(root: &Path) -> PathBuf {
    root.join("v2/state/agent")
}

#[cfg(target_os = "windows")]
fn default_mutation_authority_dir(_root: &Path) -> Option<PathBuf> {
    None
}

#[cfg(not(target_os = "windows"))]
fn default_mutation_authority_dir(root: &Path) -> Option<PathBuf> {
    Some(root.join("mutation-authority"))
}

#[cfg(target_os = "windows")]
fn default_grant_signer_socket(_run_root: &Path) -> Option<PathBuf> {
    None
}

#[cfg(not(target_os = "windows"))]
fn default_grant_signer_socket(run_root: &Path) -> Option<PathBuf> {
    Some(run_root.join("grant-signer.sock"))
}

#[cfg(target_os = "windows")]
fn default_tls_server_certificate(root: &Path) -> Option<PathBuf> {
    Some(root.join("v2-windows-shell/tls/tls-server.pem"))
}

#[cfg(not(target_os = "windows"))]
fn default_tls_server_certificate(root: &Path) -> Option<PathBuf> {
    Some(root.join("v2/trust/tls-server.pem"))
}

#[cfg(target_os = "windows")]
fn default_tls_root_certificate(root: &Path) -> Option<PathBuf> {
    Some(root.join("v2-windows-shell/enrollment/agent/trust/tls-root.der"))
}

#[cfg(not(target_os = "windows"))]
fn default_tls_root_certificate(root: &Path) -> Option<PathBuf> {
    Some(root.join("v2/trust/tls-root.der"))
}

#[cfg(target_os = "macos")]
fn default_recovery_key_file(root: &Path, home: &Path) -> Option<PathBuf> {
    Some(default_single_mac_recovery_key_file(root, home))
}

#[cfg(not(target_os = "macos"))]
fn default_recovery_key_file(_root: &Path, _home: &Path) -> Option<PathBuf> {
    None
}

#[cfg(target_os = "macos")]
fn default_recovery_helper(root: &Path) -> Option<PathBuf> {
    Some(root.join("bin/v2_recovery_enclave_helper"))
}

#[cfg(not(target_os = "macos"))]
fn default_recovery_helper(_root: &Path) -> Option<PathBuf> {
    None
}

fn absolute_path(value: String) -> Option<PathBuf> {
    let path = PathBuf::from(value);
    path.is_absolute().then_some(path)
}

fn safe_version(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-' | b'+'))
}

#[cfg(target_os = "macos")]
fn read_launchd_environment(home: &Path, label: &str, key: &str) -> Option<String> {
    if !label
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
        || !key
            .bytes()
            .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte == b'_')
    {
        return None;
    }
    let plist = home
        .join("Library/LaunchAgents")
        .join(format!("{label}.plist"));
    let command = format!("Print :EnvironmentVariables:{key}");
    let output = std::process::Command::new("/usr/libexec/PlistBuddy")
        .arg("-c")
        .arg(command)
        .arg(plist)
        .output()
        .ok()?;
    if !output.status.success() || output.stdout.len() > 4096 {
        return None;
    }
    let value = String::from_utf8(output.stdout).ok()?;
    let value = value.trim();
    (!value.is_empty() && !value.contains(['\n', '\r', '\0'])).then(|| value.to_owned())
}

#[cfg(not(target_os = "macos"))]
fn read_launchd_environment(_home: &Path, _label: &str, _key: &str) -> Option<String> {
    None
}
