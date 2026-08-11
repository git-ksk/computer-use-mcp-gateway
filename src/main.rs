mod backend;
mod config;
mod gateway;
mod policy;

use anyhow::{Context, Result, ensure};
use axum::{
    Json, Router,
    extract::{Request, State},
    http::StatusCode,
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::get,
};
use backend::{BackendHealth, BackendResourceMetrics, ComputerUseBackend, cua::CuaBackend};
use clap::Parser;
use config::Config;
use gateway::Gateway;
use policy::ToolPolicy;
use rmcp::transport::streamable_http_server::{
    StreamableHttpServerConfig, StreamableHttpService, session::local::LocalSessionManager,
};
use serde_json::{Value, json};
use std::{sync::Arc, time::Duration};
use tokio::{
    net::TcpListener,
    sync::{OwnedSemaphorePermit, Semaphore},
};
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
        config.max_http_concurrency > 0,
        "CUMG_MAX_HTTP_CONCURRENCY must be greater than zero"
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

    let mcp_path = config.mcp_path.clone();
    let max_http_concurrency = config.max_http_concurrency;
    let mcp_router = Router::new()
        .nest_service(&mcp_path, service)
        .layer(middleware::from_fn_with_state(
            Arc::new(Semaphore::new(max_http_concurrency)),
            mcp_concurrency_guard,
        ));

    let health_backend = backend.clone();
    let health_details = config.health_details;
    let app = Router::new()
        .route(
            "/healthz",
            get(move || {
                let backend = health_backend.clone();
                async move { health_response(backend, health_details).await }
            }),
        )
        .merge(mcp_router)
        .layer(TraceLayer::new_for_http());

    let listener = TcpListener::bind(config.bind)
        .await
        .with_context(|| format!("failed to bind gateway listener at {}", config.bind))?;

    info!(
        event = "gateway_ready",
        bind = %config.bind,
        mcp_path = %mcp_path,
        health_path = "/healthz",
        max_http_concurrency,
        health_details,
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

async fn mcp_concurrency_guard(
    State(semaphore): State<Arc<Semaphore>>,
    request: Request,
    next: Next,
) -> Response {
    let _permit = match try_acquire_mcp_slot(&semaphore) {
        Ok(permit) => permit,
        Err(status) => {
            return (
                status,
                Json(json!({
                    "error": "gateway_overloaded",
                    "message": "MCP HTTP concurrency limit reached"
                })),
            )
                .into_response();
        }
    };

    next.run(request).await
}

fn try_acquire_mcp_slot(
    semaphore: &Arc<Semaphore>,
) -> std::result::Result<OwnedSemaphorePermit, StatusCode> {
    semaphore
        .clone()
        .try_acquire_owned()
        .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)
}

async fn health_response(
    backend: Arc<dyn ComputerUseBackend>,
    include_details: bool,
) -> (StatusCode, Json<Value>) {
    let health = backend.health().await;
    let resources = if include_details {
        backend.resource_metrics().await
    } else {
        None
    };
    let (status, body) = health_payload(health, include_details, resources);
    (status, Json(body))
}

fn health_payload(
    health: BackendHealth,
    include_details: bool,
    resources: Option<BackendResourceMetrics>,
) -> (StatusCode, Value) {
    let (status, state, backend_state) = match health {
        BackendHealth::Ready => (StatusCode::OK, "ok", "ready"),
        BackendHealth::Unhealthy => (StatusCode::SERVICE_UNAVAILABLE, "degraded", "unhealthy"),
        BackendHealth::Stopped => (StatusCode::SERVICE_UNAVAILABLE, "degraded", "stopped"),
    };

    let mut body = json!({
        "status": state,
        "backend": backend_state
    });
    if include_details {
        body["backend_resources"] = json!(resources);
    }

    (status, body)
}

async fn shutdown_signal() {
    if let Err(error) = tokio::signal::ctrl_c().await {
        warn!(%error, "failed to install Ctrl-C shutdown handler");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn concurrency_gate_sheds_excess_requests_without_waiting() {
        let semaphore = Arc::new(Semaphore::new(2));
        let first = try_acquire_mcp_slot(&semaphore).expect("first slot should be available");
        let second = try_acquire_mcp_slot(&semaphore).expect("second slot should be available");

        assert!(matches!(
            try_acquire_mcp_slot(&semaphore),
            Err(StatusCode::SERVICE_UNAVAILABLE)
        ));

        drop(first);
        assert!(try_acquire_mcp_slot(&semaphore).is_ok());
        drop(second);
    }

    #[test]
    fn health_payload_is_coarse_by_default() {
        let resources = BackendResourceMetrics {
            pid: 4242,
            cpu_seconds: Some(12.5),
            rss_bytes: Some(4096),
        };
        let (status, body) = health_payload(BackendHealth::Ready, false, Some(resources));

        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, json!({"status": "ok", "backend": "ready"}));
        assert!(body.get("backend_resources").is_none());
    }

    #[test]
    fn health_payload_includes_resources_only_when_opted_in() {
        let resources = BackendResourceMetrics {
            pid: 4242,
            cpu_seconds: Some(12.5),
            rss_bytes: Some(4096),
        };
        let (status, body) = health_payload(BackendHealth::Ready, true, Some(resources));

        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["backend_resources"]["pid"], 4242);
        assert_eq!(body["backend_resources"]["cpu_seconds"], 12.5);
        assert_eq!(body["backend_resources"]["rss_bytes"], 4096);
    }
}
