use anyhow::{Context, Result, bail, ensure};
use clap::Parser;
#[cfg(unix)]
use computer_use_mcp_gateway::v2_handoff_control::UnixHandoffControlServer;
use computer_use_mcp_gateway::{
    v2_grant_signer::HubGrantSigner,
    v2_handoff_coordinator::HandoffCoordinator,
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
    v2_operator_handoff::{
        ManagedHandoffRuntimeConfig, ManagedOperatorHandoffAuthority, UnixOperatorHandoffAuthority,
    },
};
use std::{net::SocketAddr, path::PathBuf, sync::Arc, time::Duration};
use tokio::sync::{oneshot, watch};
use tonic::transport::{Identity, Server, ServerTlsConfig};
use tracing::{info, warn};

const MAX_OAUTH_SECRET_BYTES: u64 = 16 * 1024;
const MAX_TRUSTED_PROXY_SECRET_BYTES: u64 = 256;
const MAX_AUDIT_FINGERPRINT_SECRET_BYTES: u64 = 4 * 1024;
const MIN_AUDIT_FINGERPRINT_SECRET_BYTES: usize = 32;
const MAX_NORTHBOUND_POLICY_BYTES: u64 = 64 * 1024;

#[derive(Debug, Parser)]
#[command(name = "v2_hub")]
#[command(about = "Single-device V2 Hub over gRPC/TLS for an always-on VM")]
struct Args {
    #[arg(long, env = "CUMG_V2_HUB_BIND", default_value = "0.0.0.0:7443")]
    bind: SocketAddr,
    #[arg(long, env = "CUMG_V2_HUB_SECRET_FILE")]
    hub_secret_file: PathBuf,
    /// Legacy/single-host in-process grant signer. Mutually exclusive with the
    /// external signer socket/public-key pair.
    #[arg(long, env = "CUMG_V2_GRANT_SECRET_FILE")]
    grant_secret_file: Option<PathBuf>,
    /// Production external grant-signing service Unix socket.
    #[arg(long, env = "CUMG_V2_GRANT_SIGNER_SOCKET")]
    grant_signer_socket: Option<PathBuf>,
    /// Public verifier pinned by the Hub for responses from the external signer.
    #[arg(long, env = "CUMG_V2_GRANT_PUBLIC_KEY_FILE")]
    grant_public_key_file: Option<PathBuf>,
    #[arg(long, env = "CUMG_V2_GRANT_SIGNER_TIMEOUT_SECS", default_value_t = 2)]
    grant_signer_timeout_secs: u64,
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
    /// Hard maximum authenticated Agent session lifetime. A fresh handshake is
    /// requested before this deadline and the transport is closed at the deadline.
    #[arg(
        long,
        env = "CUMG_V2_MAX_AGENT_SESSION_LIFETIME_SECS",
        default_value_t = 3600
    )]
    max_agent_session_lifetime_secs: u64,
    /// Headroom before the hard session lifetime used to drain already-admitted work.
    #[arg(
        long,
        env = "CUMG_V2_AGENT_SESSION_REAUTH_DRAIN_SECS",
        default_value_t = 30
    )]
    agent_session_reauth_drain_secs: u64,
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
    /// Compatibility-only local Unix socket for the acceptance/regression Handoff bridge.
    /// Mutually exclusive with the first-class managed Handoff runtime.
    #[arg(long, env = "CUMG_V2_OPERATOR_HANDOFF_SOCKET")]
    operator_handoff_socket: Option<PathBuf>,
    /// Absolute Node.js executable used for the first-class Hub-owned Handoff runtime.
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
    /// Private local operator socket for typed Handoff lifecycle control. This is never MCP.
    /// It is supported only with the first-class managed Handoff runtime.
    #[arg(long, env = "CUMG_V2_HANDOFF_CONTROL_SOCKET")]
    handoff_control_socket: Option<PathBuf>,
    /// Optional private key material used only to HMAC canonical shell/process requests for
    /// privacy-preserving same/different reconciliation. The raw key and fingerprint are never
    /// emitted by normal audit surfaces.
    #[arg(long, env = "CUMG_V2_AUDIT_FINGERPRINT_SECRET_FILE")]
    audit_fingerprint_secret_file: Option<PathBuf>,
    /// Fixed authenticated principal for an explicitly single-principal trusted-proxy deployment.
    /// Must be used only with a loopback listener reachable through the reviewed proxy/tunnel.
    #[arg(long, env = "CUMG_V2_TRUSTED_PROXY_ISSUER")]
    trusted_proxy_issuer: Option<String>,
    #[arg(long, env = "CUMG_V2_TRUSTED_PROXY_SUBJECT")]
    trusted_proxy_subject: Option<String>,
    /// Secret file shared only with the reviewed local proxy/tunnel. The proxy must
    /// overwrite X-CUMG-Trusted-Proxy-Token on every request before loopback forwarding.
    #[arg(long, env = "CUMG_V2_TRUSTED_PROXY_SECRET_FILE")]
    trusted_proxy_secret_file: Option<PathBuf>,
    #[arg(
        long,
        env = "CUMG_V2_TRUSTED_PROXY_MAX_PEER_CONCURRENCY",
        default_value_t = 4
    )]
    trusted_proxy_max_peer_concurrency: usize,
    #[arg(
        long,
        env = "CUMG_V2_TRUSTED_PROXY_MAX_PEER_REQUESTS_PER_MINUTE",
        default_value_t = 60
    )]
    trusted_proxy_max_peer_requests_per_minute: usize,
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
        args.grant_signer_timeout_secs > 0,
        "CUMG_V2_GRANT_SIGNER_TIMEOUT_SECS must be greater than zero"
    );
    let in_process_grant_signer = args.grant_secret_file.is_some();
    let external_grant_signer =
        args.grant_signer_socket.is_some() || args.grant_public_key_file.is_some();
    ensure!(
        (in_process_grant_signer
            && !external_grant_signer
            && args.grant_signer_socket.is_none()
            && args.grant_public_key_file.is_none())
            || (!in_process_grant_signer
                && args.grant_signer_socket.is_some()
                && args.grant_public_key_file.is_some()),
        "configure exactly one grant signer: CUMG_V2_GRANT_SECRET_FILE, or both CUMG_V2_GRANT_SIGNER_SOCKET and CUMG_V2_GRANT_PUBLIC_KEY_FILE"
    );
    ensure!(
        args.max_agent_session_lifetime_secs > 0
            && args.agent_session_reauth_drain_secs > 0
            && args.agent_session_reauth_drain_secs < args.max_agent_session_lifetime_secs,
        "CUMG_V2_MAX_AGENT_SESSION_LIFETIME_SECS must exceed the non-zero CUMG_V2_AGENT_SESSION_REAUTH_DRAIN_SECS"
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
        args.handoff_runtime_timeout_secs > 0,
        "CUMG_V2_HANDOFF_RUNTIME_TIMEOUT_SECS must be greater than zero"
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
    let grant_signer = if let Some(path) = &args.grant_secret_file {
        HubGrantSigner::in_process(
            load_grant_authority(path)
                .context("failed to load in-process grant-signing identity")?,
        )
    } else {
        #[cfg(unix)]
        {
            HubGrantSigner::external_unix(
                args.grant_signer_socket
                    .clone()
                    .expect("validated external signer socket"),
                load_verifying_key(
                    args.grant_public_key_file
                        .as_ref()
                        .expect("validated external signer public key"),
                )
                .context("failed to load external grant signer verifier")?,
                Duration::from_secs(args.grant_signer_timeout_secs),
            )
            .context("invalid external grant signer configuration")?
        }
        #[cfg(not(unix))]
        {
            bail!("external Unix grant signer mode is unavailable on this platform")
        }
    };
    let grant_signer_mode = match &grant_signer {
        HubGrantSigner::InProcess(_) => "in_process",
        #[cfg(unix)]
        HubGrantSigner::ExternalUnix(_) => "external_unix",
    };
    info!(
        event = "v2_grant_signer_configured",
        mode = grant_signer_mode,
        signer_key_id = %computer_use_mcp_gateway::v2_m0::verifying_key_id(&grant_signer.verifier()),
        "grant-signing backend configured"
    );
    let material = HubProvisionedMaterial {
        hub_identity: load_hub_identity(&args.hub_secret_file)
            .context("failed to load Hub Ed25519 identity")?,
        grant_signer,
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
            max_agent_session_lifetime: Duration::from_secs(args.max_agent_session_lifetime_secs),
            agent_session_reauth_drain: Duration::from_secs(args.agent_session_reauth_drain_secs),
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
    let handoff_coordinator = build_handoff_coordinator(&args).await?;
    let northbound = build_northbound_runtime(
        &args,
        handle.clone(),
        &device_id,
        handoff_coordinator.clone(),
    )?;

    #[cfg(unix)]
    let handoff_control_server = if let Some(path) = args.handoff_control_socket.as_ref() {
        Some(
            UnixHandoffControlServer::bind(path)
                .context("failed to bind private Handoff control socket")?,
        )
    } else {
        None
    };
    #[cfg(not(unix))]
    ensure!(
        args.handoff_control_socket.is_none(),
        "CUMG_V2_HANDOFF_CONTROL_SOCKET is supported only on Unix hosts"
    );

    info!(
        event = "v2_hub_start",
        bind = %args.bind,
        device_id = %device_id,
        northbound_mcp_enabled = northbound.is_some(),
        "starting single-device V2 Hub"
    );

    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    #[cfg(unix)]
    let handoff_control_task = if let Some(server) = handoff_control_server {
        let coordinator = handoff_coordinator
            .as_ref()
            .expect("validated Handoff control socket requires managed coordinator")
            .clone();
        let control_hub = handle.clone();
        let control_shutdown = shutdown_rx.clone();
        let task = tokio::spawn(async move {
            match server
                .serve(coordinator, control_hub, control_shutdown)
                .await
            {
                Ok(()) => info!(
                    event = "v2_handoff_control_stopped",
                    outcome = "shutdown",
                    "private Handoff operator control socket stopped"
                ),
                Err(error) => tracing::error!(
                    event = "v2_handoff_control_failed",
                    outcome = "unavailable",
                    error = %error,
                    "private Handoff operator control socket failed; authority semantics remain fail-closed in the managed runtime"
                ),
            }
        });
        info!(
            event = "v2_handoff_control_started",
            mode = "local_unix",
            outcome = "ready",
            "private Handoff operator control socket started outside northbound MCP"
        );
        Some(task)
    } else {
        None
    };
    let drain_timeout = Duration::from_secs(args.drain_timeout_secs);
    let handoff_shutdown = handoff_coordinator.clone();
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
        let session_close_requested = shutdown_handle.close_live_session_for_shutdown().await;
        info!(
            event = "v2_hub_shutdown_agent_session_close",
            signal,
            session_close_requested,
            outcome = "shutdown",
            "Hub shutdown requested closure of the current Agent stream after bounded drain"
        );
        if let Some(coordinator) = handoff_shutdown.as_ref() {
            coordinator.shutdown().await;
            info!(
                event = "v2_handoff_runtime_shutdown",
                signal,
                outcome = "fenced",
                "Handoff runtime stopped after admitted Agent work drained"
            );
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
        let http = axum::serve(
            listener,
            northbound
                .router
                .into_make_service_with_connect_info::<SocketAddr>(),
        )
        .with_graceful_shutdown(wait_for_shutdown(http_shutdown));
        tokio::try_join!(
            async { grpc.await.context("V2 Hub gRPC server failed") },
            async { http.await.context("V2 Hub northbound MCP server failed") },
        )?;
    } else {
        grpc.await.context("V2 Hub gRPC server failed")?;
    }
    #[cfg(unix)]
    if let Some(task) = handoff_control_task {
        task.await
            .context("private Handoff control task failed during shutdown")?;
    }
    Ok(())
}

async fn build_handoff_coordinator(args: &Args) -> Result<Option<Arc<HandoffCoordinator>>> {
    let managed_fields = [
        args.handoff_runtime_command.is_some(),
        args.handoff_runtime_script.is_some(),
        args.handoff_runtime_env_file.is_some(),
    ];
    let managed_configured = managed_fields.into_iter().any(|configured| configured);
    ensure!(
        args.handoff_control_socket.is_none()
            || managed_fields.into_iter().all(|configured| configured),
        "CUMG_V2_HANDOFF_CONTROL_SOCKET requires the complete managed Handoff runtime configuration"
    );
    ensure!(
        !(managed_configured && args.operator_handoff_socket.is_some()),
        "CUMG managed Handoff runtime and CUMG_V2_OPERATOR_HANDOFF_SOCKET are mutually exclusive"
    );

    if managed_configured {
        ensure!(
            managed_fields.into_iter().all(|configured| configured),
            "CUMG_V2_HANDOFF_RUNTIME_COMMAND, CUMG_V2_HANDOFF_RUNTIME_SCRIPT, and CUMG_V2_HANDOFF_RUNTIME_ENV_FILE must be configured together"
        );
        let config = ManagedHandoffRuntimeConfig::new(
            args.handoff_runtime_command
                .clone()
                .expect("validated managed Handoff runtime command"),
            args.handoff_runtime_script
                .clone()
                .expect("validated managed Handoff runtime script"),
            args.handoff_runtime_env_file
                .clone()
                .expect("validated managed Handoff runtime env file"),
            Duration::from_secs(args.handoff_runtime_timeout_secs),
        )
        .context("invalid managed Handoff runtime configuration")?;
        let runtime = Arc::new(
            ManagedOperatorHandoffAuthority::spawn(config)
                .await
                .context("failed to start managed Handoff runtime")?,
        );
        info!(
            event = "v2_handoff_runtime_configured",
            mode = "managed_stdio",
            outcome = "ready",
            "first-class Handoff runtime configured"
        );
        return Ok(Some(Arc::new(HandoffCoordinator::managed(runtime))));
    }

    if let Some(path) = args.operator_handoff_socket.as_ref() {
        warn!(
            event = "v2_handoff_runtime_configured",
            mode = "legacy_unix_bridge",
            outcome = "compatibility_only",
            "acceptance-only Unix Handoff bridge remains enabled as a compatibility backend"
        );
        let authority = UnixOperatorHandoffAuthority::new(path.clone())
            .context("invalid CUMG_V2_OPERATOR_HANDOFF_SOCKET")?;
        return Ok(Some(Arc::new(HandoffCoordinator::new(Arc::new(authority)))));
    }

    Ok(None)
}

fn build_northbound_runtime(
    args: &Args,
    handle: computer_use_mcp_gateway::v2_m1_hub::HubHandle,
    device_id: &str,
    handoff_coordinator: Option<Arc<HandoffCoordinator>>,
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
    let trusted_proxy_configured = args.trusted_proxy_issuer.is_some()
        || args.trusted_proxy_subject.is_some()
        || args.trusted_proxy_secret_file.is_some();
    ensure!(
        !(oauth_configured && trusted_proxy_configured),
        "OAuth introspection and trusted-proxy authentication modes are mutually exclusive"
    );
    let configured = [
        args.mcp_resource.is_some(),
        args.northbound_policy_file.is_some(),
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
    let overload = computer_use_mcp_gateway::v2_limits::HttpOverloadGuard::new(
        args.max_northbound_concurrency,
        args.max_northbound_requests_per_minute,
    )
    .context("invalid V2 northbound connection/rate limits")?;
    let audit_fingerprint_secret: Option<Arc<[u8]>> = args
        .audit_fingerprint_secret_file
        .as_ref()
        .map(|path| {
            let secret = load_secret_text(path, MAX_AUDIT_FINGERPRINT_SECRET_BYTES)
                .context("failed to load audit fingerprint secret")?;
            ensure!(
                secret.len() >= MIN_AUDIT_FINGERPRINT_SECRET_BYTES,
                "CUMG_V2_AUDIT_FINGERPRINT_SECRET_FILE must contain at least 32 bytes"
            );
            Ok::<Arc<[u8]>, anyhow::Error>(Arc::from(secret.into_bytes()))
        })
        .transpose()?;

    let (router, resource, metadata_url, auth_mode) = if trusted_proxy_configured {
        let issuer = required(&args.trusted_proxy_issuer, "CUMG_V2_TRUSTED_PROXY_ISSUER")?;
        let subject = required(&args.trusted_proxy_subject, "CUMG_V2_TRUSTED_PROXY_SUBJECT")?;
        let proxy_config = TrustedProxyConfig::new(resource, issuer, subject)
            .context("invalid trusted-proxy fixed-principal configuration")?;
        let secret_file = args
            .trusted_proxy_secret_file
            .as_ref()
            .context("CUMG_V2_TRUSTED_PROXY_SECRET_FILE is required in trusted-proxy mode")?;
        ensure!(
            args.trusted_proxy_max_peer_concurrency < args.max_northbound_concurrency,
            "CUMG_V2_TRUSTED_PROXY_MAX_PEER_CONCURRENCY must be lower than CUMG_V2_MAX_NORTHBOUND_CONCURRENCY to preserve headroom"
        );
        ensure!(
            args.trusted_proxy_max_peer_requests_per_minute
                < args.max_northbound_requests_per_minute,
            "CUMG_V2_TRUSTED_PROXY_MAX_PEER_REQUESTS_PER_MINUTE must be lower than CUMG_V2_MAX_NORTHBOUND_REQUESTS_PER_MINUTE to preserve headroom"
        );
        let proxy_secret = load_secret_text(secret_file, MAX_TRUSTED_PROXY_SECRET_BYTES)
            .context("failed to load trusted-proxy loopback secret")?;
        let peer_guard = computer_use_mcp_gateway::v2_limits::TrustedProxyLoopbackGuard::new(
            proxy_secret,
            args.trusted_proxy_max_peer_concurrency,
            args.trusted_proxy_max_peer_requests_per_minute,
        )
        .context("invalid trusted-proxy loopback trust/rate configuration")?;
        let policy = NorthboundPolicyDocument::from_json(&policy_text)
            .context("failed to parse northbound authorization policy")?
            .build_policy(proxy_config.issuer(), device_id)
            .context("invalid northbound principal/device/capability policy")?;
        let resource = proxy_config.resource().to_owned();
        let mut service = V2NorthboundMcp::new(handle, policy);
        if let Some(secret) = audit_fingerprint_secret.clone() {
            service = service.with_request_fingerprint_secret(secret);
        }
        if let Some(coordinator) = handoff_coordinator.as_ref() {
            service = service.with_handoff_coordinator(coordinator.clone());
        }
        let router = build_trusted_proxy_router(service, proxy_config)
            .layer(axum::middleware::from_fn_with_state(
                overload,
                computer_use_mcp_gateway::v2_limits::enforce_http_limits,
            ))
            .layer(axum::middleware::from_fn_with_state(
                peer_guard,
                computer_use_mcp_gateway::v2_limits::enforce_trusted_proxy_loopback,
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
        let mut service = V2NorthboundMcp::new(handle, policy);
        if let Some(secret) = audit_fingerprint_secret.clone() {
            service = service.with_request_fingerprint_secret(secret);
        }
        if let Some(coordinator) = handoff_coordinator.as_ref() {
            service = service.with_handoff_coordinator(coordinator.clone());
        }
        let router = build_northbound_router(service, mcp_config, Arc::new(verifier)).layer(
            axum::middleware::from_fn_with_state(
                overload,
                computer_use_mcp_gateway::v2_limits::enforce_http_limits,
            ),
        );
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
