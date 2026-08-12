use anyhow::{Context, Result, ensure};
use clap::Parser;
use computer_use_mcp_gateway::{
    v2_m1_grpc::{
        MAX_GRPC_TRANSPORT_MESSAGE_BYTES, proto::agent_control_server::AgentControlServer,
    },
    v2_m1_hub::{HubProvisionedMaterial, HubServiceConfig, SingleDeviceHub},
    v2_m1_keys::{
        load_grant_authority, load_hub_identity, load_tls_server_identity, load_verifying_key,
    },
};
use std::{net::SocketAddr, path::PathBuf, time::Duration};
use tonic::transport::{Identity, Server, ServerTlsConfig};
use tracing::info;
use tracing_subscriber::EnvFilter;

#[derive(Debug, Parser)]
#[command(name = "v2_hub")]
#[command(about = "Single-device V2 Hub over gRPC/TLS for an always-on VM")]
struct Args {
    #[arg(long, env = "CUMG_V2_HUB_BIND", default_value = "0.0.0.0:7443")]
    bind: SocketAddr,
    #[arg(long, env = "CUMG_V2_HUB_SECRET_FILE")]
    hub_secret_file: PathBuf,
    #[arg(long, env = "CUMG_V2_GRANT_SECRET_FILE")]
    grant_secret_file: PathBuf,
    #[arg(long, env = "CUMG_V2_DEVICE_PUBLIC_KEY_FILE")]
    device_public_key_file: PathBuf,
    #[arg(long, env = "CUMG_V2_TLS_CERT_PEM_FILE")]
    tls_cert_pem_file: PathBuf,
    #[arg(long, env = "CUMG_V2_TLS_KEY_PEM_FILE")]
    tls_key_pem_file: PathBuf,
    #[arg(long, env = "CUMG_V2_HUB_STATE_DIR")]
    state_dir: PathBuf,
    #[arg(long, env = "CUMG_V2_HEARTBEAT_TIMEOUT_SECS", default_value_t = 45)]
    heartbeat_timeout_secs: u64,
    #[arg(long, env = "CUMG_V2_MAX_QUEUED_PER_DEVICE", default_value_t = 8)]
    max_queued_per_device: usize,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();
    let args = Args::parse();
    ensure!(
        args.heartbeat_timeout_secs > 0,
        "CUMG_V2_HEARTBEAT_TIMEOUT_SECS must be greater than zero"
    );

    let material = HubProvisionedMaterial {
        hub_identity: load_hub_identity(&args.hub_secret_file)
            .context("failed to load Hub Ed25519 identity")?,
        grant_authority: load_grant_authority(&args.grant_secret_file)
            .context("failed to load grant-signing identity")?,
        device_verifier: load_verifying_key(&args.device_public_key_file)
            .context("failed to load enrolled Agent public key")?,
    };
    let (cert_pem, key_pem) =
        load_tls_server_identity(&args.tls_cert_pem_file, &args.tls_key_pem_file)
            .context("failed to load TLS server identity")?;
    let (hub, _handle) = SingleDeviceHub::new(
        HubServiceConfig {
            state_dir: args.state_dir,
            heartbeat_timeout: Duration::from_secs(args.heartbeat_timeout_secs),
            max_queued_per_device: args.max_queued_per_device,
        },
        material,
    )
    .context("failed to initialize V2 Hub state")?;
    let device_id = hub.device_id().to_owned();

    info!(
        event = "v2_hub_start",
        bind = %args.bind,
        device_id = %device_id,
        "starting single-device V2 Hub"
    );
    Server::builder()
        .tls_config(ServerTlsConfig::new().identity(Identity::from_pem(cert_pem, key_pem)))?
        .add_service(
            AgentControlServer::new(hub)
                .max_decoding_message_size(MAX_GRPC_TRANSPORT_MESSAGE_BYTES)
                .max_encoding_message_size(MAX_GRPC_TRANSPORT_MESSAGE_BYTES),
        )
        .serve_with_shutdown(args.bind, async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await
        .context("V2 Hub gRPC server failed")?;
    Ok(())
}
