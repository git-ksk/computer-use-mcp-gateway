use anyhow::{Context, Result, bail, ensure};
use clap::Parser;
use computer_use_mcp_gateway::{
    v2_m0_trust::DeviceKeyRotation,
    v2_m1_grpc::{
        MAX_GRPC_TRANSPORT_MESSAGE_BYTES, proto::agent_control_server::AgentControlServer,
    },
    v2_m1_hub::{
        DEFAULT_CHECKPOINT_GENERATION_ROLLOVER_BYTES, HubProvisionedMaterial, HubServiceConfig,
        SingleDeviceHub,
    },
    v2_m1_keys::{
        load_grant_authority, load_hub_identity, load_secret_text, load_tls_server_identity,
        load_trusted_text, load_verifying_key,
    },
    v2_m1_northbound::{
        NorthboundMcpConfig, NorthboundPolicyDocument, OAuthIntrospectionConfig,
        OAuthIntrospectionVerifier, TrustedProxyConfig, V2NorthboundMcp, build_northbound_router,
        build_trusted_proxy_router,
    },
    v2_usage::{McpUsageController, UsageManager},
};
use std::{net::SocketAddr, path::PathBuf, sync::Arc, time::Duration};
use tokio::sync::{oneshot, watch};
use tonic::transport::{Identity, Server, ServerTlsConfig};
use tracing::{info, warn};

const MAX_OAUTH_SECRET_BYTES: u64 = 16 * 1024;
const MAX_NORTHBOUND_POLICY_BYTES: u64 = 64 * 1024;

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
    /// Signed device-key continuity document used only when enrolled key changes.
    #[arg(long, env = "CUMG_V2_DEVICE_ROTATION_FILE")]
    device_rotation_file: Option<PathBuf>,
    #[arg(long, env = "CUMG_V2_TLS_CERT_PEM_FILE")]
    tls_cert_pem_file: PathBuf,
    #[arg(long, env = "CUMG_V2_TLS_KEY_PEM_FILE")]
    tls_key_pem_file: PathBuf,
    #[arg(long, env = "CUMG_V2_HUB_STATE_DIR")]
    state_dir: PathBuf,
    #[arg(long, env = "CUMG_V2_HEARTBEAT_TIMEOUT_SECS", default_value_t = 45)]
    heartbeat_timeout_secs: u64,
    /// Maximum time to keep Agent transport alive after a planned shutdown signal
    /// so already-admitted operations can reach a durable terminal state.
    #[arg(long, env = "CUMG_V2_DRAIN_TIMEOUT_SECS", default_value_t = 30)]
    drain_timeout_secs: u64,
    #[arg(
        long,
        env = "CUMG_V2_CHECKPOINT_GENERATION_ROLLOVER_BYTES",
        default_value_t = DEFAULT_CHECKPOINT_GENERATION_ROLLOVER_BYTES
    )]
    checkpoint_generation_rollover_bytes: usize,
    #[arg(long, env = "CUMG_V2_MAX_QUEUED_PER_DEVICE", default_value_t = 8)]
    max_queued_per_device: usize,
    #[arg(long, env = "CUMG_V2_MAX_AGENT_SESSIONS", default_value_t = 2)]
    max_agent_sessions: usize,
    #[arg(
        long,
        env = "CUMG_V2_MAX_AGENT_SESSION_STARTS_PER_MINUTE",
        default_value_t = 30
    )]
    max_agent_session_starts_per_minute: usize,

    /// Optional loopback listener for the protected northbound MCP HTTP endpoint.
    /// Keep this loopback-only and terminate public HTTPS in a reviewed reverse proxy.
    #[arg(long, env = "CUMG_V2_MCP_BIND")]
    mcp_bind: Option<SocketAddr>,
    /// Canonical public HTTPS URI of the MCP resource, including its endpoint path.
    #[arg(long, env = "CUMG_V2_MCP_RESOURCE")]
    mcp_resource: Option<String>,
    /// OAuth authorization-server issuer advertised through RFC 9728 metadata.
    #[arg(long, env = "CUMG_V2_OAUTH_AUTHORIZATION_SERVER")]
    oauth_authorization_server: Option<String>,
    /// RFC 7662 token-introspection endpoint used by the Hub resource server.
    #[arg(long, env = "CUMG_V2_OAUTH_INTROSPECTION_ENDPOINT")]
    oauth_introspection_endpoint: Option<String>,
    #[arg(long, env = "CUMG_V2_OAUTH_INTROSPECTION_CLIENT_ID")]
    oauth_introspection_client_id: Option<String>,
    /// File containing the introspection client secret. The file is required to be private.
    #[arg(long, env = "CUMG_V2_OAUTH_INTROSPECTION_CLIENT_SECRET_FILE")]
    oauth_introspection_client_secret_file: Option<PathBuf>,
    /// Space-separated OAuth scopes required to enter the MCP resource boundary.
    #[arg(long, env = "CUMG_V2_OAUTH_REQUIRED_SCOPES")]
    oauth_required_scopes: Option<String>,
    /// Integrity-protected JSON principal -> device -> exact-capability mapping.
    #[arg(long, env = "CUMG_V2_NORTHBOUND_POLICY_FILE")]
    northbound_policy_file: Option<PathBuf>,
    /// Fixed authenticated principal for an explicitly single-principal trusted-proxy deployment.
    /// Must be used only with a loopback listener reachable through the reviewed proxy/tunnel.
    #[arg(long, env = "CUMG_V2_TRUSTED_PROXY_ISSUER")]
    trusted_proxy_issuer: Option<String>,
    #[arg(long, env = "CUMG_V2_TRUSTED_PROXY_SUBJECT")]
    trusted_proxy_subject: Option<String>,
    #[arg(
        long,
        env = "CUMG_V2_OAUTH_INTROSPECTION_TIMEOUT_SECS",
        default_value_t = 5
    )]
    oauth_introspection_timeout_secs: u64,
    #[arg(long, env = "CUMG_V2_MAX_NORTHBOUND_CONCURRENCY", default_value_t = 16)]
    max_northbound_concurrency: usize,
    #[arg(
        long,
        env = "CUMG_V2_MAX_NORTHBOUND_REQUESTS_PER_MINUTE",
        default_value_t = 120
    )]
    max_northbound_requests_per_minute: usize,
    /// Optional loopback-only mcp-usage-control sidecar endpoint. When omitted,
    /// V2 uses the no-op accounting controller and preserves pre-integration behavior.
    #[arg(long, env = "CUMG_V2_USAGE_ENDPOINT")]
    usage_endpoint: Option<String>,
    #[arg(long, env = "CUMG_V2_USAGE_TIMEOUT_SECS", default_value_t = 2)]
    usage_timeout_secs: u64,
}

struct NorthboundRuntime {
    bind: SocketAddr,
    router: axum::Router,
    resource: String,
    metadata_url: Option<String>,
    auth_mode: &'static str,
}

#[tokio::main]
async fn main() -> Result<()> {
    let _observability = computer_use_mcp_gateway::v2_observability::init("cumg-v2-hub")?;
    let args = Args::parse();
    ensure!(
        args.heartbeat_timeout_secs > 0,
        "CUMG_V2_HEARTBEAT_TIMEOUT_SECS must be greater than zero"
    );
    ensure!(
        args.drain_timeout_secs > 0,
        "CUMG_V2_DRAIN_TIMEOUT_SECS must be greater than zero"
    );
    ensure!(
        args.oauth_introspection_timeout_secs > 0,
        "CUMG_V2_OAUTH_INTROSPECTION_TIMEOUT_SECS must be greater than zero"
    );
    ensure!(
        args.usage_timeout_secs > 0,
        "CUMG_V2_USAGE_TIMEOUT_SECS must be greater than zero"
    );

    // Install the OS signal handlers before secret/checkpoint loading so an
    // operator stop immediately after exec cannot hit the process-wide default
    // SIGTERM action before the Hub reaches its serving loop. The received
    // signal is retained until the runtime has a Hub handle to drain safely.
    let (signal_tx, signal_rx) = oneshot::channel();
    tokio::spawn(async move {
        let _ = signal_tx.send(shutdown_signal().await);
    });
    tokio::task::yield_now().await;

    let device_rotation = if let Some(path) = &args.device_rotation_file {
        let document = load_trusted_text(path, 64 * 1024)
            .context("failed to load device rotation document")?;
        Some(
            serde_json::from_str::<DeviceKeyRotation>(&document)
                .context("invalid device rotation document")?,
        )
    } else {
        None
    };
    let material = HubProvisionedMaterial {
        hub_identity: load_hub_identity(&args.hub_secret_file)
            .context("failed to load Hub Ed25519 identity")?,
        grant_authority: load_grant_authority(&args.grant_secret_file)
            .context("failed to load grant-signing identity")?,
        device_verifier: load_verifying_key(&args.device_public_key_file)
            .context("failed to load enrolled Agent public key")?,
        device_rotation,
    };
    let (cert_pem, key_pem) =
        load_tls_server_identity(&args.tls_cert_pem_file, &args.tls_key_pem_file)
            .context("failed to load TLS server identity")?;
    let (hub, handle) = SingleDeviceHub::new(
        HubServiceConfig {
            state_dir: args.state_dir.clone(),
            heartbeat_timeout: Duration::from_secs(args.heartbeat_timeout_secs),
            checkpoint_generation_rollover_bytes: args.checkpoint_generation_rollover_bytes,
            max_queued_per_device: args.max_queued_per_device,
            max_agent_sessions: args.max_agent_sessions,
            max_agent_session_starts_per_minute: args.max_agent_session_starts_per_minute,
        },
        material,
    )
    .context("failed to initialize V2 Hub state")?;
    let device_id = hub.device_id().to_owned();
    let shutdown_handle = handle.clone();
    let northbound = build_northbound_runtime(&args, handle, &device_id)?;

    info!(
        event = "v2_hub_start",
        bind = %args.bind,
        device_id = %device_id,
        northbound_mcp_enabled = northbound.is_some(),
        usage_accounting_enabled = args.usage_endpoint.is_some(),
        "starting single-device V2 Hub"
    );

    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let drain_timeout = Duration::from_secs(args.drain_timeout_secs);
    tokio::spawn(async move {
        let signal = signal_rx.await.unwrap_or("signal_listener_closed");
        let first = shutdown_handle.begin_shutdown_drain();
        info!(
            event = "v2_hub_shutdown_drain_start",
            signal,
            drain_timeout_ms = drain_timeout.as_millis() as u64,
            outcome = if first { "started" } else { "already_draining" },
            "Hub shutdown signal received; admission closed while admitted operations drain"
        );
        match tokio::time::timeout(drain_timeout, shutdown_handle.wait_for_shutdown_drain()).await {
            Ok(()) => {
                info!(
                    event = "v2_hub_shutdown_drain_complete",
                    signal,
                    outcome = "drained",
                    "Hub shutdown drain completed"
                );
            }
            Err(_) => {
                warn!(
                    event = "v2_hub_shutdown_drain_timeout",
                    signal,
                    drain_timeout_ms = drain_timeout.as_millis() as u64,
                    outcome = "timeout_fail_closed",
                    "Hub shutdown drain timed out; remaining dispatched work will retain fail-closed restart semantics"
                );
            }
        }
        let _ = shutdown_tx.send(true);
    });

    let grpc_shutdown = shutdown_rx.clone();
    let grpc = Server::builder()
        .tls_config(ServerTlsConfig::new().identity(Identity::from_pem(cert_pem, key_pem)))?
        .add_service(
            AgentControlServer::new(hub)
                .max_decoding_message_size(MAX_GRPC_TRANSPORT_MESSAGE_BYTES)
                .max_encoding_message_size(MAX_GRPC_TRANSPORT_MESSAGE_BYTES),
        )
        .serve_with_shutdown(args.bind, wait_for_shutdown(grpc_shutdown));

    if let Some(northbound) = northbound {
        let listener = tokio::net::TcpListener::bind(northbound.bind)
            .await
            .with_context(|| {
                format!(
                    "failed to bind northbound MCP listener at {}",
                    northbound.bind
                )
            })?;
        info!(
            event = "v2_northbound_mcp_start",
            bind = %northbound.bind,
            resource = %northbound.resource,
            metadata_url = northbound.metadata_url.as_deref().unwrap_or("none"),
            auth_mode = northbound.auth_mode,
            "starting protected northbound MCP resource server"
        );
        let http_shutdown = shutdown_rx;
        let http = axum::serve(listener, northbound.router)
            .with_graceful_shutdown(wait_for_shutdown(http_shutdown));
        tokio::try_join!(
            async { grpc.await.context("V2 Hub gRPC server failed") },
            async { http.await.context("V2 Hub northbound MCP server failed") },
        )?;
    } else {
        grpc.await.context("V2 Hub gRPC server failed")?;
    }
    Ok(())
}

fn build_northbound_runtime(
    args: &Args,
    handle: computer_use_mcp_gateway::v2_m1_hub::HubHandle,
    device_id: &str,
) -> Result<Option<NorthboundRuntime>> {
    let oauth_configured = [
        args.oauth_authorization_server.is_some(),
        args.oauth_introspection_endpoint.is_some(),
        args.oauth_introspection_client_id.is_some(),
        args.oauth_introspection_client_secret_file.is_some(),
        args.oauth_required_scopes.is_some(),
    ]
    .into_iter()
    .any(|value| value);
    let trusted_proxy_configured =
        args.trusted_proxy_issuer.is_some() || args.trusted_proxy_subject.is_some();
    ensure!(
        !(oauth_configured && trusted_proxy_configured),
        "OAuth introspection and trusted-proxy authentication modes are mutually exclusive"
    );
    let configured = [
        args.mcp_resource.is_some(),
        args.northbound_policy_file.is_some(),
        args.usage_endpoint.is_some(),
        oauth_configured,
        trusted_proxy_configured,
    ]
    .into_iter()
    .any(|value| value);

    let Some(bind) = args.mcp_bind else {
        if configured {
            bail!("CUMG_V2_MCP_BIND is required when northbound settings are configured");
        }
        return Ok(None);
    };
    ensure!(
        bind.ip().is_loopback(),
        "CUMG_V2_MCP_BIND must remain loopback-only; terminate public HTTPS before the Hub"
    );

    let resource = required(&args.mcp_resource, "CUMG_V2_MCP_RESOURCE")?;
    let policy_file = args
        .northbound_policy_file
        .as_ref()
        .context("CUMG_V2_NORTHBOUND_POLICY_FILE is required")?;
    let policy_text = load_trusted_text(policy_file, MAX_NORTHBOUND_POLICY_BYTES)
        .context("failed to load northbound authorization policy")?;
    let usage = if let Some(endpoint) = args.usage_endpoint.as_deref() {
        UsageManager::new(Arc::new(
            McpUsageController::new(endpoint, Duration::from_secs(args.usage_timeout_secs))
                .context("invalid loopback MCPUsage sidecar configuration")?,
        ))
    } else {
        UsageManager::noop()
    };
    let overload = computer_use_mcp_gateway::v2_limits::HttpOverloadGuard::new(
        args.max_northbound_concurrency,
        args.max_northbound_requests_per_minute,
    )
    .context("invalid V2 northbound connection/rate limits")?;

    let (router, resource, metadata_url, auth_mode) = if trusted_proxy_configured {
        let issuer = required(&args.trusted_proxy_issuer, "CUMG_V2_TRUSTED_PROXY_ISSUER")?;
        let subject = required(&args.trusted_proxy_subject, "CUMG_V2_TRUSTED_PROXY_SUBJECT")?;
        let proxy_config = TrustedProxyConfig::new(resource, issuer, subject)
            .context("invalid trusted-proxy fixed-principal configuration")?;
        let policy = NorthboundPolicyDocument::from_json(&policy_text)
            .context("failed to parse northbound authorization policy")?
            .build_policy(proxy_config.issuer(), device_id)
            .context("invalid northbound principal/device/capability policy")?;
        let resource = proxy_config.resource().to_owned();
        let router = build_trusted_proxy_router(
            V2NorthboundMcp::new_with_usage(handle, policy, usage),
            proxy_config,
        )
        .layer(axum::middleware::from_fn_with_state(
            overload,
            computer_use_mcp_gateway::v2_limits::enforce_http_limits,
        ));
        (router, resource, None, "trusted_proxy_fixed_principal")
    } else {
        let authorization_server = required(
            &args.oauth_authorization_server,
            "CUMG_V2_OAUTH_AUTHORIZATION_SERVER",
        )?;
        let introspection_endpoint = required(
            &args.oauth_introspection_endpoint,
            "CUMG_V2_OAUTH_INTROSPECTION_ENDPOINT",
        )?;
        let introspection_client_id = required(
            &args.oauth_introspection_client_id,
            "CUMG_V2_OAUTH_INTROSPECTION_CLIENT_ID",
        )?;
        let secret_file = args
            .oauth_introspection_client_secret_file
            .as_ref()
            .context("CUMG_V2_OAUTH_INTROSPECTION_CLIENT_SECRET_FILE is required")?;
        let scopes = required(&args.oauth_required_scopes, "CUMG_V2_OAUTH_REQUIRED_SCOPES")?
            .split_ascii_whitespace()
            .map(ToOwned::to_owned)
            .collect::<Vec<_>>();
        let mcp_config = NorthboundMcpConfig::new(resource, authorization_server, scopes)
            .context("invalid V2 northbound MCP Authorization configuration")?;
        let secret = load_secret_text(secret_file, MAX_OAUTH_SECRET_BYTES)
            .context("failed to load OAuth introspection client secret")?;
        let policy = NorthboundPolicyDocument::from_json(&policy_text)
            .context("failed to parse northbound authorization policy")?
            .build_policy(mcp_config.authorization_server(), device_id)
            .context("invalid northbound principal/device/capability policy")?;
        let mut verifier_config = OAuthIntrospectionConfig::new(
            mcp_config.authorization_server(),
            mcp_config.resource(),
            introspection_endpoint,
            introspection_client_id,
            secret,
        );
        verifier_config.timeout = Duration::from_secs(args.oauth_introspection_timeout_secs);
        let verifier = OAuthIntrospectionVerifier::new(verifier_config)
            .context("invalid OAuth token introspection configuration")?;
        let metadata_url = mcp_config.metadata_url().to_owned();
        let resource = mcp_config.resource().to_owned();
        let router = build_northbound_router(
            V2NorthboundMcp::new_with_usage(handle, policy, usage),
            mcp_config,
            Arc::new(verifier),
        )
        .layer(axum::middleware::from_fn_with_state(
            overload,
            computer_use_mcp_gateway::v2_limits::enforce_http_limits,
        ));
        (router, resource, Some(metadata_url), "oauth_introspection")
    };

    Ok(Some(NorthboundRuntime {
        bind,
        router,
        resource,
        metadata_url,
        auth_mode,
    }))
}

fn required<'a>(value: &'a Option<String>, name: &'static str) -> Result<&'a str> {
    value
        .as_deref()
        .filter(|value| !value.trim().is_empty())
        .with_context(|| format!("{name} is required when northbound MCP is enabled"))
}

async fn shutdown_signal() -> &'static str {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};

        let mut terminate = signal(SignalKind::terminate()).expect("install SIGTERM handler");
        let mut hangup = signal(SignalKind::hangup()).expect("install SIGHUP handler");
        tokio::select! {
            result = tokio::signal::ctrl_c() => {
                if result.is_err() { "signal_error" } else { "SIGINT" }
            }
            _ = terminate.recv() => "SIGTERM",
            _ = hangup.recv() => "SIGHUP",
        }
    }

    #[cfg(not(unix))]
    {
        if tokio::signal::ctrl_c().await.is_err() {
            "signal_error"
        } else {
            "CTRL_C"
        }
    }
}

async fn wait_for_shutdown(mut shutdown: watch::Receiver<bool>) {
    if *shutdown.borrow() {
        return;
    }
    while shutdown.changed().await.is_ok() {
        if *shutdown.borrow() {
            return;
        }
    }
}
