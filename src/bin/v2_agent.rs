use anyhow::{Context, Result};
use clap::Parser;
use computer_use_mcp_gateway::{
    v2_m0_trust::HubKeyRotation,
    v2_m1::ReconnectPolicy,
    v2_m1_agent::{AgentService, AgentServiceConfig, CuaAgentConfig},
    v2_m1_keys::{load_agent_material, load_trusted_text, load_verifying_key},
    v2_operator_handoff::{ManagedHandoffRuntimeConfig, ManagedOperatorHandoffAuthority},
};
use std::path::PathBuf;
use std::time::Duration;
use tokio::sync::watch;
use tracing::info;

#[derive(Debug, Parser)]
#[command(name = "cumg-v2-agent")]
#[command(about = "Outbound V2 secure Agent for computer-use-mcp-gateway")]
struct Config {
    #[arg(long, env = "CUMG_V2_HUB_ENDPOINT")]
    hub_endpoint: String,
    #[arg(long, env = "CUMG_V2_HUB_DOMAIN")]
    hub_domain: String,
    #[arg(long, env = "CUMG_V2_DEVICE_ID")]
    device_id: String,
    #[arg(long, env = "CUMG_V2_DEVICE_SECRET_FILE")]
    device_secret_file: PathBuf,
    #[arg(long, env = "CUMG_V2_HUB_PUBLIC_KEY_FILE")]
    hub_public_key_file: PathBuf,
    #[arg(long, env = "CUMG_V2_GRANT_PUBLIC_KEY_FILE")]
    grant_public_key_file: PathBuf,
    /// Additional grant verifiers used during old/new signing-key overlap.
    #[arg(
        long = "additional-grant-public-key-file",
        env = "CUMG_V2_ADDITIONAL_GRANT_PUBLIC_KEY_FILES",
        value_delimiter = ','
    )]
    additional_grant_public_key_files: Vec<PathBuf>,
    /// Signed Hub-key continuity document used only when the persisted Hub key changes.
    #[arg(long, env = "CUMG_V2_HUB_ROTATION_FILE")]
    hub_rotation_file: Option<PathBuf>,
    #[arg(long, env = "CUMG_V2_TLS_ROOT_DER_FILE")]
    tls_root_der_file: PathBuf,
    #[arg(long, env = "CUMG_V2_STATE_DIR")]
    state_dir: PathBuf,
    #[arg(
        long = "allowed-cwd-root",
        env = "CUMG_V2_ALLOWED_CWD_ROOTS",
        value_delimiter = ',',
        required = true
    )]
    allowed_cwd_roots: Vec<PathBuf>,
    #[arg(long, env = "CUMG_V2_HEARTBEAT_SECS", default_value_t = 15)]
    heartbeat_secs: u64,
    #[arg(long, env = "CUMG_V2_RECONNECT_INITIAL_MS", default_value_t = 250)]
    reconnect_initial_ms: u64,
    #[arg(long, env = "CUMG_V2_RECONNECT_MAX_MS", default_value_t = 4_000)]
    reconnect_max_ms: u64,
    #[arg(long, env = "CUMG_V2_RECONNECT_ATTEMPTS", default_value_t = 8)]
    reconnect_attempts: u32,
    /// Enable the external Cua MCP GUI backend by supplying its executable.
    #[arg(long, env = "CUMG_V2_CUA_COMMAND")]
    cua_command: Option<String>,
    #[arg(long = "cua-arg", env = "CUMG_V2_CUA_ARGS", value_delimiter = ',')]
    cua_args: Vec<String>,
    /// Exact MCP serverInfo.version compatibility target for Cua.
    /// The default `external` disables the runtime version pin for custom deployments.
    #[arg(long, env = "CUMG_V2_CUA_BACKEND_VERSION", default_value = "external")]
    cua_backend_version: String,
    #[arg(long, env = "CUMG_V2_CUA_CONNECT_TIMEOUT_SECS", default_value_t = 10)]
    cua_connect_timeout_secs: u64,
    #[arg(long, env = "CUMG_V2_CUA_TOOL_TIMEOUT_SECS", default_value_t = 30)]
    cua_tool_timeout_secs: u64,
    /// Absolute Node.js executable for the Agent-local canonical Handoff runtime.
    #[arg(long, env = "CUMG_V2_HANDOFF_RUNTIME_COMMAND")]
    handoff_runtime_command: Option<PathBuf>,
    /// Absolute CUMG Handoff runtime host script. The normal target is scripts/v2_handoff_runtime.mjs.
    #[arg(long, env = "CUMG_V2_HANDOFF_RUNTIME_SCRIPT")]
    handoff_runtime_script: Option<PathBuf>,
    /// Private Node --env-file containing only Handoff/runtime configuration and transport secrets.
    #[arg(long, env = "CUMG_V2_HANDOFF_RUNTIME_ENV_FILE")]
    handoff_runtime_env_file: Option<PathBuf>,
    #[arg(
        long,
        env = "CUMG_V2_HANDOFF_RUNTIME_TIMEOUT_SECS",
        default_value_t = 2
    )]
    handoff_runtime_timeout_secs: u64,
}

#[tokio::main]
async fn main() -> Result<()> {
    let _observability = computer_use_mcp_gateway::v2_observability::init("cumg-v2-agent")?;
    let args = Config::parse();
    let mut material = load_agent_material(
        &args.device_secret_file,
        &args.hub_public_key_file,
        &args.grant_public_key_file,
        &args.tls_root_der_file,
    )
    .context("failed to load V2 Agent key/trust material")?;
    for path in &args.additional_grant_public_key_files {
        material
            .additional_grant_verifiers
            .push(load_verifying_key(path).context("failed to load additional grant verifier")?);
    }
    if let Some(path) = &args.hub_rotation_file {
        let document =
            load_trusted_text(path, 64 * 1024).context("failed to load Hub rotation document")?;
        material.hub_rotation = Some(
            serde_json::from_str::<HubKeyRotation>(&document)
                .context("invalid Hub rotation document")?,
        );
    }
    let cua = args.cua_command.as_ref().map(|command| CuaAgentConfig {
        command: command.clone(),
        args: if args.cua_args.is_empty() {
            vec!["mcp".into()]
        } else {
            args.cua_args.clone()
        },
        backend_version: args.cua_backend_version.clone(),
        platform: format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH),
        revision: 1,
        connect_timeout: Duration::from_secs(args.cua_connect_timeout_secs),
        tool_timeout: Duration::from_secs(args.cua_tool_timeout_secs),
        reconnect_attempts: 3,
        reconnect_backoff: Duration::from_millis(200),
    });
    let managed_handoff_fields = [
        args.handoff_runtime_command.is_some(),
        args.handoff_runtime_script.is_some(),
        args.handoff_runtime_env_file.is_some(),
    ];
    anyhow::ensure!(
        args.handoff_runtime_timeout_secs > 0,
        "CUMG_V2_HANDOFF_RUNTIME_TIMEOUT_SECS must be greater than zero"
    );
    anyhow::ensure!(
        !managed_handoff_fields
            .into_iter()
            .any(|configured| configured)
            || managed_handoff_fields
                .into_iter()
                .all(|configured| configured),
        "CUMG_V2_HANDOFF_RUNTIME_COMMAND, CUMG_V2_HANDOFF_RUNTIME_SCRIPT, and CUMG_V2_HANDOFF_RUNTIME_ENV_FILE must be configured together"
    );
    let handoff_runtime = if managed_handoff_fields
        .into_iter()
        .all(|configured| configured)
    {
        let config = ManagedHandoffRuntimeConfig::new(
            args.handoff_runtime_command
                .clone()
                .expect("validated Agent Handoff runtime command"),
            args.handoff_runtime_script
                .clone()
                .expect("validated Agent Handoff runtime script"),
            args.handoff_runtime_env_file
                .clone()
                .expect("validated Agent Handoff runtime env file"),
            Duration::from_secs(args.handoff_runtime_timeout_secs),
        )
        .context("invalid Agent-local managed Handoff runtime configuration")?;
        let runtime = ManagedOperatorHandoffAuthority::spawn(config)
            .await
            .context("failed to start Agent-local managed Handoff runtime")?;
        info!(
            event = "v2_agent_handoff_runtime_configured",
            mode = "managed_stdio",
            outcome = "ready",
            "Agent-local canonical Handoff runtime configured"
        );
        Some(runtime)
    } else {
        None
    };
    let config = AgentServiceConfig {
        hub_endpoint: args.hub_endpoint,
        hub_domain: args.hub_domain,
        device_id: args.device_id,
        allowed_cwd_roots: args.allowed_cwd_roots,
        state_dir: args.state_dir,
        heartbeat_interval: Duration::from_secs(args.heartbeat_secs),
        reconnect: ReconnectPolicy {
            initial_delay: Duration::from_millis(args.reconnect_initial_ms),
            max_delay: Duration::from_millis(args.reconnect_max_ms),
            max_attempts: args.reconnect_attempts,
        },
        cua,
    };
    let mut agent =
        AgentService::new(config, material).context("invalid V2 Agent configuration")?;
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    tokio::spawn(async move {
        let _ = tokio::signal::ctrl_c().await;
        let _ = shutdown_tx.send(true);
    });
    info!(
        event = "v2_agent_start",
        "starting outbound V2 Agent service"
    );
    let result = agent.run(shutdown_rx).await.context("V2 Agent stopped");
    if let Some(runtime) = handoff_runtime.as_ref() {
        runtime.shutdown().await;
    }
    result
}
