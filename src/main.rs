mod backend;
mod config;
mod gateway;
mod policy;

use anyhow::{Context, Result, ensure};
use axum::{Json, Router, http::StatusCode, routing::get};
use backend::{BackendHealth, ComputerUseBackend, cua::CuaBackend};
use clap::Parser;
use config::Config;
use gateway::Gateway;
use policy::ToolPolicy;
use rmcp::transport::streamable_http_server::{
    StreamableHttpServerConfig, StreamableHttpService, session::local::LocalSessionManager,
};
use serde_json::json;
use std::{sync::Arc, time::Duration};
use tokio::net::TcpListener;
use tower_http::trace::TraceLayer;
use tracing::{info, warn};
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    let config = Config::parse();
    ensure!(
        config.mcp_path.starts_with('/'),
        "CUMG_MCP_PATH must start with '/'"
    );
    ensure!(
        config.connect_timeout_secs > 0,
        "CUMG_CONNECT_TIMEOUT_SECS must be greater than zero"
    );
    ensure!(
        config.tool_timeout_secs > 0,
        "CUMG_TOOL_TIMEOUT_SECS must be greater than zero"
    );

    if !config.bind.ip().is_loopback() {
        warn!(
            bind = %config.bind,
            "gateway is bound to a non-loopback address; terminate TLS and enforce authentication before exposing it"
        );
    }

    let backend_command = config.backend_command.clone();
    let backend_args = config.backend_args();
    let backend_arg_count = backend_args.len();
    let backend: Arc<dyn ComputerUseBackend> = Arc::new(CuaBackend::new(
        backend_command.clone(),
        backend_args,
        Duration::from_secs(config.connect_timeout_secs),
        Duration::from_secs(config.tool_timeout_secs),
        config.reconnect_attempts,
        Duration::from_millis(config.reconnect_backoff_ms),
    ));

    info!(
        event = "backend_connect",
        backend_command = %backend_command,
        backend_arg_count,
        "connecting computer-use MCP backend"
    );
    backend
        .connect()
        .await
        .context("computer-use backend startup failed")?;

    let policy = ToolPolicy::new(config.allow_tools(), config.deny_tools());
    let gateway = Gateway::discover(backend.clone(), policy)
        .await
        .context("gateway tool discovery failed")?;

    let allowed_hosts = config.allowed_hosts();
    let allowed_origins = config.allowed_origins();
    let http_config = StreamableHttpServerConfig::default()
        .with_legacy_session_mode(false)
        .with_json_response(true)
        .with_allowed_hosts(allowed_hosts.iter().cloned())
        .with_allowed_origins(allowed_origins.iter().cloned());
    let service = StreamableHttpService::new(
        {
            let gateway = gateway.clone();
            move || Ok(gateway.clone())
        },
        LocalSessionManager::default().into(),
        http_config,
    );

    let health_backend = backend.clone();
    let app = Router::new()
        .route(
            "/healthz",
            get(move || {
                let backend = health_backend.clone();
                async move { health_response(backend).await }
            }),
        )
        .nest_service(&config.mcp_path, service)
        .layer(TraceLayer::new_for_http());

    let listener = TcpListener::bind(config.bind)
        .await
        .with_context(|| format!("failed to bind gateway listener at {}", config.bind))?;

    info!(
        event = "gateway_ready",
        bind = %config.bind,
        mcp_path = %config.mcp_path,
        health_path = "/healthz",
        allowed_host_count = allowed_hosts.len(),
        allowed_origin_count = allowed_origins.len(),
        "computer-use MCP gateway ready"
    );

    let serve_result = axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await;
    let shutdown_result = backend.shutdown().await;

    serve_result.context("gateway HTTP server failed")?;
    shutdown_result.context("computer-use backend shutdown failed")?;
    Ok(())
}

async fn health_response(
    backend: Arc<dyn ComputerUseBackend>,
) -> (StatusCode, Json<serde_json::Value>) {
    let resources = backend.resource_metrics().await;
    match backend.health().await {
        BackendHealth::Ready => (
            StatusCode::OK,
            Json(json!({
                "status": "ok",
                "backend": "ready",
                "backend_resources": resources
            })),
        ),
        BackendHealth::Unhealthy => (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({
                "status": "degraded",
                "backend": "unhealthy",
                "backend_resources": resources
            })),
        ),
        BackendHealth::Stopped => (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({
                "status": "degraded",
                "backend": "stopped",
                "backend_resources": resources
            })),
        ),
    }
}

async fn shutdown_signal() {
    if let Err(error) = tokio::signal::ctrl_c().await {
        warn!(%error, "failed to install Ctrl-C shutdown handler");
    }
}
