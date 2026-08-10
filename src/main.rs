mod backend;
mod config;
mod policy;

use backend::cua::CuaBackend;
use clap::Parser;
use config::Config;
use tracing::info;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    let config = Config::parse();
    let backend_args = config
        .backend_args
        .split_ascii_whitespace()
        .map(ToOwned::to_owned)
        .collect();
    let backend = CuaBackend::new(config.backend_command.clone(), backend_args);

    info!(
        bind = %config.bind,
        mcp_path = %config.mcp_path,
        backend = backend.command(),
        args = ?backend.args(),
        "gateway scaffold configured"
    );

    // M1: connect backend via rmcp::transport::TokioChildProcess.
    // M2: mount rmcp::transport::streamable_http_server::StreamableHttpService
    //     at config.mcp_path and forward dynamic tools through policy + audit.
    //
    // We intentionally do not open an HTTP listener until transparent MCP
    // forwarding is wired; a half-working remote-control endpoint is unsafe.
    Ok(())
}
