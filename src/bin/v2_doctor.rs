use clap::Parser;
use computer_use_mcp_gateway::v2_doctor::{DoctorConfig, run_doctor};
use std::path::PathBuf;
use std::process::ExitCode;

#[derive(Debug, Parser)]
#[command(name = "v2_doctor")]
#[command(about = "Privacy-bounded diagnostics for a single-Mac CUMG V2 deployment")]
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
    #[arg(long)]
    cua_command: Option<PathBuf>,
    #[arg(long)]
    expected_cua_version: Option<String>,
    #[arg(long, env = "CUMG_MUTATION_AUTHORITY_DIR")]
    mutation_authority_dir: Option<PathBuf>,
    /// Optional private local Handoff control socket. Status is queried read-only and locator/IDs are omitted.
    #[arg(long, env = "CUMG_V2_HANDOFF_CONTROL_SOCKET")]
    handoff_control_socket: Option<PathBuf>,
    /// Current reviewed one-shot maintenance label. Only this exact label is excluded from stale-job diagnostics.
    #[arg(long, env = "CUMG_V2_MAINTENANCE_JOB_LABEL")]
    maintenance_job_exclude_label: Option<String>,
    #[arg(long)]
    json: bool,
}

fn main() -> ExitCode {
    let args = Args::parse();
    let home = match std::env::var_os("HOME") {
        Some(home) => PathBuf::from(home),
        None => {
            eprintln!("v2_doctor: HOME is unavailable");
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
    let config = DoctorConfig {
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
        grant_signer_socket: args
            .grant_signer_socket
            .or_else(|| Some(run_root.join("grant-signer.sock"))),
        tls_server_certificate: args
            .tls_server_certificate
            .or_else(|| Some(root.join("v2/trust/tls-server.pem"))),
        tls_root_certificate: args
            .tls_root_certificate
            .or_else(|| Some(root.join("v2/trust/tls-root.der"))),
        cua_command: args
            .cua_command
            .or_else(|| Some(home.join(".local/bin/cua-driver"))),
        expected_cua_version: args.expected_cua_version,
        mutation_authority_dir: args
            .mutation_authority_dir
            .or_else(|| Some(root.join("mutation-authority"))),
        handoff_control_socket: args.handoff_control_socket,
        maintenance_job_exclude_label: args.maintenance_job_exclude_label,
    };
    let report = run_doctor(&config);
    if args.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&report).expect("doctor report serializes")
        );
    } else {
        println!("CUMG_V2_DOCTOR overall={}", report.overall);
        for check in &report.checks {
            println!("{:?} {} {}", check.status, check.name, check.detail);
        }
    }
    ExitCode::from(report.exit_code())
}
