use anyhow::{Context, Result};
use clap::Parser;
use computer_use_mcp_gateway::{
    v2_m1::ReconnectPolicy,
    v2_m1_agent::{AgentService, AgentServiceConfig},
    v2_m1_keys::load_agent_material,
};
use std::path::PathBuf;
use std::time::Duration;
use tokio::sync::watch;
use tracing::info;
use tracing_subscriber::EnvFilter;

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
    #[arg(long, env = "CUMG_V2_TLS_ROOT_DER_FILE")]
    tls_root_der_file: PathBuf,
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
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();
    let args = Config::parse();
    let material = load_agent_material(
        &args.device_secret_file,
        &args.hub_public_key_file,
        &args.grant_public_key_file,
        &args.tls_root_der_file,
    )
    .context("failed to load V2 Agent key/trust material")?;
    let config = AgentServiceConfig {
        hub_endpoint: args.hub_endpoint,
        hub_domain: args.hub_domain,
        device_id: args.device_id,
        allowed_cwd_roots: args.allowed_cwd_roots,
        heartbeat_interval: Duration::from_secs(args.heartbeat_secs),
        reconnect: ReconnectPolicy {
            initial_delay: Duration::from_millis(args.reconnect_initial_ms),
            max_delay: Duration::from_millis(args.reconnect_max_ms),
            max_attempts: args.reconnect_attempts,
        },
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
    agent.run(shutdown_rx).await.context("V2 Agent stopped")
}
