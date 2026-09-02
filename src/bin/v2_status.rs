use clap::Parser;
use computer_use_mcp_gateway::{
    v2_doctor::{DoctorConfig, run_doctor},
    v2_handoff_control::{LocalHandoffControlRequest, exchange_unix_handoff_control},
    v2_operator_status::{
        HandoffStatusInput, UpgradeStatusInput, build_operator_status, render_operator_status_text,
    },
    v2_upgrade_transaction::{
        UpgradeTransactionError, read_upgrade_transaction, upgrade_transaction_path,
    },
};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

#[derive(Debug, Parser)]
#[command(name = "v2_status")]
#[command(about = "Unified privacy-bounded operator status for a single-Mac CUMG V2 deployment")]
struct Args {
    #[arg(long, env = "CUMG_V2_INSTALL_ROOT")]
    install_root: Option<PathBuf>,
    #[arg(long, env = "CUMG_V2_RUN_ROOT")]
    run_root: Option<PathBuf>,
    #[arg(long)]
    hub_state_dir: Option<PathBuf>,
    #[arg(long)]
    agent_state_dir: Option<PathBuf>,
    #[arg(long)]
    runtime_manifest: Option<PathBuf>,
    #[arg(long)]
    binary_dir: Option<PathBuf>,
    #[arg(long, default_value = "com.github.git-ksk.cumg-v2-hub")]
    hub_launchd_label: String,
    #[arg(long, default_value = "com.github.git-ksk.cumg-v2-agent")]
    agent_launchd_label: String,
    #[arg(long, default_value = "com.github.git-ksk.cumg-v2-grant-signer")]
    grant_signer_launchd_label: String,
    #[arg(long)]
    grant_signer_socket: Option<PathBuf>,
    #[arg(long)]
    tls_server_certificate: Option<PathBuf>,
    #[arg(long)]
    tls_root_certificate: Option<PathBuf>,
    #[arg(long, env = "CUMG_V2_CUA_COMMAND")]
    cua_command: Option<PathBuf>,
    #[arg(long, env = "CUMG_V2_CUA_BACKEND_VERSION")]
    expected_cua_version: Option<String>,
    #[arg(long, env = "CUMG_MUTATION_AUTHORITY_DIR")]
    mutation_authority_dir: Option<PathBuf>,
    #[arg(long, env = "CUMG_V2_HANDOFF_CONTROL_SOCKET")]
    handoff_control_socket: Option<PathBuf>,
    #[arg(long, env = "CUMG_V2_RECOVERY_KEY_FILE")]
    recovery_key_file: Option<PathBuf>,
    #[arg(long, env = "CUMG_V2_RECOVERY_HELPER")]
    recovery_helper: Option<PathBuf>,
    #[arg(long)]
    json: bool,
}

fn main() -> ExitCode {
    let args = Args::parse();
    let home = match std::env::var_os("HOME") {
        Some(home) => PathBuf::from(home),
        None => {
            eprintln!("v2_status: HOME is unavailable");
            return ExitCode::from(2);
        }
    };
    let root = args
        .install_root
        .unwrap_or_else(|| home.join("Library/Application Support/computer-use-mcp-gateway"));
    let run_root = args
        .run_root
        .unwrap_or_else(|| home.join("Library/Caches/cumg-v2"));
    let binary_dir = args.binary_dir.unwrap_or_else(|| root.join("bin"));

    let hub_handoff = read_launchd_environment(
        &home,
        &args.hub_launchd_label,
        "CUMG_V2_HANDOFF_CONTROL_SOCKET",
    )
    .and_then(absolute_path);
    let hub_signer = read_launchd_environment(
        &home,
        &args.hub_launchd_label,
        "CUMG_V2_GRANT_SIGNER_SOCKET",
    )
    .and_then(absolute_path);
    let agent_cua =
        read_launchd_environment(&home, &args.agent_launchd_label, "CUMG_V2_CUA_COMMAND")
            .and_then(absolute_path);
    let agent_cua_version = read_launchd_environment(
        &home,
        &args.agent_launchd_label,
        "CUMG_V2_CUA_BACKEND_VERSION",
    )
    .filter(|value| safe_version(value));
    let agent_mutation = read_launchd_environment(
        &home,
        &args.agent_launchd_label,
        "CUMG_MUTATION_AUTHORITY_DIR",
    )
    .and_then(absolute_path);

    let fallback_cua = home.join(".local/bin/cua-driver");
    let cua_command = args
        .cua_command
        .or(agent_cua)
        .or_else(|| fallback_cua.is_file().then_some(fallback_cua));
    let expected_cua_version = args.expected_cua_version.or(agent_cua_version);
    let mutation_authority_dir = args
        .mutation_authority_dir
        .or(agent_mutation)
        .or_else(|| Some(root.join("mutation-authority")));
    let handoff_control_socket = args.handoff_control_socket.or(hub_handoff);
    let grant_signer_socket = args
        .grant_signer_socket
        .or(hub_signer)
        .or_else(|| Some(run_root.join("grant-signer.sock")));

    let doctor_config = DoctorConfig {
        hub_state_dir: args
            .hub_state_dir
            .unwrap_or_else(|| root.join("v2/state/hub")),
        agent_state_dir: args
            .agent_state_dir
            .unwrap_or_else(|| root.join("v2/state/agent")),
        runtime_manifest: args
            .runtime_manifest
            .unwrap_or_else(|| root.join("runtime-manifest.json")),
        binary_dir,
        hub_launchd_label: args.hub_launchd_label,
        agent_launchd_label: args.agent_launchd_label,
        grant_signer_launchd_label: Some(args.grant_signer_launchd_label),
        grant_signer_socket,
        tls_server_certificate: args
            .tls_server_certificate
            .or_else(|| Some(root.join("v2/trust/tls-server.pem"))),
        tls_root_certificate: args
            .tls_root_certificate
            .or_else(|| Some(root.join("v2/trust/tls-root.der"))),
        cua_command,
        expected_cua_version,
        mutation_authority_dir,
        handoff_control_socket: handoff_control_socket.clone(),
        maintenance_job_exclude_label: None,
        recovery_key_file: args.recovery_key_file.or_else(|| {
            Some(
                home.join("Library/Application Support/cumg-v2-agent/recovery/recovery-key.sealed"),
            )
        }),
        recovery_helper: args
            .recovery_helper
            .or_else(|| Some(root.join("bin/v2_recovery_enclave_helper"))),
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

    let transaction_path = upgrade_transaction_path(&root);
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

    let report = build_operator_status(&doctor, handoff_input, upgrade_input);
    if args.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&report).expect("operator status report serializes")
        );
    } else {
        println!("{}", render_operator_status_text(&report));
    }
    ExitCode::from(report.exit_code())
}

fn absolute_path(value: String) -> Option<PathBuf> {
    let path = PathBuf::from(value);
    (path.is_absolute()).then_some(path)
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
