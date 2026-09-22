use clap::Parser;
use computer_use_mcp_gateway::{
    v2_operator_status::render_operator_status_text,
    v2_status_collector::{OperatorStatusCollectionConfig, collect_operator_status},
};
use std::path::PathBuf;
use std::process::ExitCode;

#[derive(Debug, Parser)]
#[command(name = "v2_status")]
#[command(about = "Unified privacy-bounded operator status for a single-Mac CUMG V2 deployment")]
#[command(version = env!("CUMG_BUILD_VERSION"))]
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
    #[arg(long)]
    cua_tool_timeout_secs: Option<u64>,
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
    let mut config = OperatorStatusCollectionConfig::installed_defaults(home, root, run_root);
    config.hub_state_dir = args.hub_state_dir;
    config.agent_state_dir = args.agent_state_dir;
    config.runtime_manifest = args.runtime_manifest;
    config.binary_dir = args.binary_dir;
    config.hub_launchd_label = args.hub_launchd_label;
    config.agent_launchd_label = args.agent_launchd_label;
    config.grant_signer_launchd_label = args.grant_signer_launchd_label;
    config.grant_signer_socket = args.grant_signer_socket;
    config.tls_server_certificate = args.tls_server_certificate;
    config.tls_root_certificate = args.tls_root_certificate;
    config.cua_command = args.cua_command;
    config.expected_cua_version = args.expected_cua_version;
    config.cua_tool_timeout_secs = args.cua_tool_timeout_secs;
    config.mutation_authority_dir = args.mutation_authority_dir;
    config.handoff_control_socket = args.handoff_control_socket;
    config.recovery_key_file = args.recovery_key_file;
    config.recovery_helper = args.recovery_helper;

    let report = collect_operator_status(&config);
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
