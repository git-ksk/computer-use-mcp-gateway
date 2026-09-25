use anyhow::{Context, Result, bail, ensure};
use clap::Parser;
#[cfg(unix)]
use computer_use_mcp_gateway::v2_handoff_control::UnixHandoffControlServer;
use computer_use_mcp_gateway::{
    v2_grant_signer::HubGrantSigner,
    v2_handoff_coordinator::HandoffCoordinator,
    v2_hosted_handoff_control::{HostedHandoffControlService, HostedHandoffPolicyDocument},
    v2_hosted_handoff_http::{HostedHandoffHttpConfig, build_hosted_handoff_router},
    v2_hosted_ingress::{HostedIngressClassifier, apply_hosted_ingress_classifier},
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
        AccessTokenVerifier, NorthboundMcpConfig, NorthboundPolicyDocument,
        OAuthIntrospectionConfig, OAuthIntrospectionVerifier, TrustedProxyConfig, V2NorthboundMcp,
        build_northbound_router, build_trusted_proxy_router,
    },
    v2_oidc_jwt::{OidcJwtAlgorithm, OidcJwtConfig, OidcJwtVerifier},
    v2_operator_handoff::UnixOperatorHandoffAuthority,
    v2_postgres_hub_state_store::{PostgresHubStateStore, PostgresHubStateStoreConfig},
    v2_semantic_constraints::SemanticConstraintPolicy,
    v2_status_collector::{CollectedOperatorStatusProvider, OperatorStatusCollectionConfig},
};
use std::{net::SocketAddr, path::PathBuf, sync::Arc, time::Duration};
use tokio::sync::{oneshot, watch};
use tonic::{
    service::Routes,
    transport::{Identity, Server, ServerTlsConfig},
};
use tracing::{info, warn};

const MAX_OAUTH_SECRET_BYTES: u64 = 16 * 1024;
const MAX_TRUSTED_PROXY_SECRET_BYTES: u64 = 256;
const MAX_AUDIT_FINGERPRINT_SECRET_BYTES: u64 = 4 * 1024;
const MIN_AUDIT_FINGERPRINT_SECRET_BYTES: usize = 32;
const MAX_NORTHBOUND_POLICY_BYTES: u64 = 64 * 1024;
const MAX_HOSTED_HANDOFF_POLICY_BYTES: u64 = 64 * 1024;
const MAX_SEMANTIC_CONSTRAINT_POLICY_BYTES: u64 = 64 * 1024;
const MAX_POSTGRES_PASSWORD_BYTES: u64 = 4 * 1024;

#[derive(Debug, Parser)]
#[command(name = "v2_hub")]
#[command(about = "Single-device V2 Hub for VM or explicit hosted one-port deployment")]
#[command(version = env!("CUMG_BUILD_VERSION"))]
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
    tls_cert_pem_file: Option<PathBuf>,
    #[arg(long, env = "CUMG_V2_TLS_KEY_PEM_FILE")]
    tls_key_pem_file: Option<PathBuf>,
    /// Explicit hosted/Cloud Run profile. Public TLS is terminated before the container and the
    /// Hub serves one internal HTTP/2 cleartext (h2c) listener on `PORT`.
    #[arg(long, env = "CUMG_V2_HOSTED_PROFILE", default_value_t = false)]
    hosted_profile: bool,
    /// Cloud Run supplies this variable. It is consumed only when the explicit hosted profile is enabled.
    #[arg(long, env = "PORT")]
    hosted_port: Option<u16>,
    #[arg(long, env = "CUMG_V2_HUB_STATE_DIR")]
    state_dir: PathBuf,
    /// PostgreSQL host for the hosted authoritative state store. A leading slash selects a Unix
    /// socket directory, including the Cloud SQL /cloudsql/PROJECT:REGION:INSTANCE mount.
    #[arg(long, env = "CUMG_V2_POSTGRES_HOST")]
    postgres_host: Option<String>,
    #[arg(long, env = "CUMG_V2_POSTGRES_PORT", default_value_t = 5432)]
    postgres_port: u16,
    #[arg(long, env = "CUMG_V2_POSTGRES_DATABASE")]
    postgres_database: Option<String>,
    #[arg(long, env = "CUMG_V2_POSTGRES_USER")]
    postgres_user: Option<String>,
    /// Optional private file containing the PostgreSQL password.
    #[arg(long, env = "CUMG_V2_POSTGRES_PASSWORD_FILE")]
    postgres_password_file: Option<PathBuf>,
    /// Stable deployment-owned key for the single authoritative Hub row.
    #[arg(long, env = "CUMG_V2_POSTGRES_STATE_KEY")]
    postgres_state_key: Option<String>,
    #[arg(
        long,
        env = "CUMG_V2_POSTGRES_CONNECT_TIMEOUT_SECS",
        default_value_t = 5
    )]
    postgres_connect_timeout_secs: u64,
    #[arg(long, env = "CUMG_V2_POSTGRES_QUERY_TIMEOUT_SECS", default_value_t = 5)]
    postgres_query_timeout_secs: u64,
    /// Install root used only by the read-only unified MCP status collector.
    #[arg(long, env = "CUMG_V2_STATUS_INSTALL_ROOT")]
    status_install_root: Option<PathBuf>,
    /// Runtime/cache root used only by the read-only unified MCP status collector.
    #[arg(long, env = "CUMG_V2_STATUS_RUN_ROOT")]
    status_run_root: Option<PathBuf>,
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
    /// Canonical hosted Handoff operator resource root, for example
    /// https://hub.example/operator/v1/handoff. Hosted only.
    #[arg(long, env = "CUMG_V2_HOSTED_HANDOFF_RESOURCE")]
    hosted_handoff_resource: Option<String>,
    /// Space-separated scopes required by the hosted Handoff operator resource. Hosted only.
    #[arg(long, env = "CUMG_V2_HOSTED_HANDOFF_REQUIRED_SCOPES")]
    hosted_handoff_required_scopes: Option<String>,
    /// Exact principal -> device -> Handoff-action authorization policy. Hosted only.
    #[arg(long, env = "CUMG_V2_HOSTED_HANDOFF_POLICY_FILE")]
    hosted_handoff_policy_file: Option<PathBuf>,
    /// Separate OIDC/JWT audience for the hosted Handoff resource. Hosted OIDC mode only.
    #[arg(long, env = "CUMG_V2_HOSTED_HANDOFF_OIDC_AUDIENCE")]
    hosted_handoff_oidc_audience: Option<String>,
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
    /// Exact OIDC/JWT audience accepted from the signed `aud` claim.
    #[arg(long, env = "CUMG_V2_OIDC_AUDIENCE")]
    oidc_audience: Option<String>,
    /// Explicit HTTPS JWKS endpoint for provider-neutral signed OIDC/JWT verification.
    #[arg(long, env = "CUMG_V2_OIDC_JWKS_URI")]
    oidc_jwks_uri: Option<String>,
    /// Comma-separated asymmetric JWT algorithms: RS*, PS*, ES256/384, or EdDSA.
    #[arg(long, env = "CUMG_V2_OIDC_ALLOWED_ALGORITHMS")]
    oidc_allowed_algorithms: Option<String>,
    #[arg(long, env = "CUMG_V2_OIDC_CLOCK_SKEW_SECS", default_value_t = 30)]
    oidc_clock_skew_secs: u64,
    #[arg(long, env = "CUMG_V2_OIDC_JWKS_CACHE_SECS", default_value_t = 300)]
    oidc_jwks_cache_secs: u64,
    #[arg(
        long,
        env = "CUMG_V2_OIDC_UNKNOWN_KID_REFRESH_SECS",
        default_value_t = 30
    )]
    oidc_unknown_kid_refresh_secs: u64,
    #[arg(long, env = "CUMG_V2_OIDC_HTTP_TIMEOUT_SECS", default_value_t = 5)]
    oidc_http_timeout_secs: u64,
    /// Space-separated OAuth scopes required to enter the MCP resource boundary.
    #[arg(long, env = "CUMG_V2_OAUTH_REQUIRED_SCOPES")]
    oauth_required_scopes: Option<String>,
    /// Integrity-protected JSON principal -> device -> exact-capability mapping.
    #[arg(long, env = "CUMG_V2_NORTHBOUND_POLICY_FILE")]
    northbound_policy_file: Option<PathBuf>,
    /// Optional immutable operator semantic-constraint snapshot. It can only narrow
    /// already-authorized exact capabilities and is loaded once at Hub startup.
    #[arg(long, env = "CUMG_V2_SEMANTIC_CONSTRAINT_POLICY_FILE")]
    semantic_constraint_policy_file: Option<PathBuf>,
    /// Compatibility-only local Unix socket for the acceptance/regression Handoff bridge.
    /// Mutually exclusive with the first-class managed Handoff runtime.
    #[arg(long, env = "CUMG_V2_OPERATOR_HANDOFF_SOCKET")]
    operator_handoff_socket: Option<PathBuf>,
    /// Legacy Hub-owned Handoff runtime setting. First-class Handoff now runs on the Agent;
    /// configuring any of these Hub runtime fields is refused rather than silently spawning it here.
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
    /// The Hub owns only this local relay; the canonical Handoff runtime/FSM runs on the Agent.
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
    bind: Option<SocketAddr>,
    router: axum::Router,
    resource: String,
    mcp_path: String,
    metadata_path: String,
    metadata_url: Option<String>,
    auth_mode: &'static str,
}

struct HostedHandoffRuntime {
    router: axum::Router,
    resource: String,
    context_path: String,
    control_path: String,
    metadata_path: String,
    auth_mode: &'static str,
}

#[tokio::main]
async fn main() -> Result<()> {
    let _observability = computer_use_mcp_gateway::v2_observability::init("cumg-v2-hub")?;
    let args = Args::parse();
    validate_profile_configuration(&args)?;
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
    let tls_identity = if args.hosted_profile {
        None
    } else {
        let cert = args
            .tls_cert_pem_file
            .as_ref()
            .expect("validated non-hosted TLS certificate");
        let key = args
            .tls_key_pem_file
            .as_ref()
            .expect("validated non-hosted TLS key");
        Some(load_tls_server_identity(cert, key).context("failed to load TLS server identity")?)
    };
    let hub_config = HubServiceConfig {
        state_dir: args.state_dir.clone(),
        heartbeat_timeout: Duration::from_secs(args.heartbeat_timeout_secs),
        max_agent_session_lifetime: Duration::from_secs(args.max_agent_session_lifetime_secs),
        agent_session_reauth_drain: Duration::from_secs(args.agent_session_reauth_drain_secs),
        checkpoint_generation_rollover_bytes: args.checkpoint_generation_rollover_bytes,
        max_queued_per_device: args.max_queued_per_device,
        max_agent_sessions: args.max_agent_sessions,
        max_agent_session_starts_per_minute: args.max_agent_session_starts_per_minute,
    };
    let (hub, handle) = if args.hosted_profile {
        let store_config = build_hosted_postgres_state_config(&args)?;
        let state_store = Arc::new(
            PostgresHubStateStore::connect(store_config)
                .await
                .context("failed to connect hosted PostgreSQL Hub state")?,
        );
        SingleDeviceHub::new_with_async_state_store(hub_config, material, state_store)
            .await
            .context("failed to initialize hosted V2 Hub state")?
    } else {
        SingleDeviceHub::new(hub_config, material).context("failed to initialize V2 Hub state")?
    };
    let device_id = hub.device_id().to_owned();
    let shutdown_handle = handle.clone();
    let handoff_coordinator = build_handoff_coordinator(&args, handle.clone()).await?;
    let northbound = build_northbound_runtime(
        &args,
        handle.clone(),
        &device_id,
        handoff_coordinator.clone(),
    )?;
    let hosted_handoff = build_hosted_handoff_runtime(
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

    let startup_bind = if args.hosted_profile {
        SocketAddr::from((
            [0, 0, 0, 0],
            args.hosted_port.expect("validated hosted PORT"),
        ))
    } else {
        args.bind
    };
    info!(
        event = "v2_hub_start",
        profile = if args.hosted_profile { "hosted_one_port" } else { "single_host" },
        bind = %startup_bind,
        device_id = %device_id,
        northbound_mcp_enabled = northbound.is_some(),
        hosted_handoff_enabled = hosted_handoff.is_some(),
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

    let agent_service = AgentControlServer::new(hub)
        .max_decoding_message_size(MAX_GRPC_TRANSPORT_MESSAGE_BYTES)
        .max_encoding_message_size(MAX_GRPC_TRANSPORT_MESSAGE_BYTES);

    if args.hosted_profile {
        let port = args.hosted_port.expect("validated hosted PORT");
        let bind = SocketAddr::from(([0, 0, 0, 0], port));
        let northbound =
            northbound.context("hosted profile requires protected northbound MCP runtime")?;
        let hosted_handoff =
            hosted_handoff.context("hosted profile requires protected Handoff operator runtime")?;
        let classifier = HostedIngressClassifier::new(
            northbound.mcp_path.clone(),
            northbound.metadata_path.clone(),
            hosted_handoff.context_path.clone(),
            hosted_handoff.control_path.clone(),
            hosted_handoff.metadata_path.clone(),
        )
        .context("invalid hosted one-port route composition")?;

        // Tonic 0.14 can add the Agent gRPC NamedService directly to an Axum router.
        // Google terminates public TLS; this single container listener intentionally
        // serves HTTP/2 cleartext (h2c). Agent application-level Ed25519 identity
        // remains independent of transport TLS.
        let http_router = northbound.router.merge(hosted_handoff.router);
        let router = Routes::from(http_router)
            .add_service(agent_service)
            .into_axum_router();
        let router = apply_hosted_ingress_classifier(router, classifier);
        let listener = tokio::net::TcpListener::bind(bind)
            .await
            .with_context(|| format!("failed to bind hosted one-port listener at {bind}"))?;
        info!(
            event = "v2_hosted_one_port_start",
            bind = %bind,
            transport = "h2c",
            mcp_resource = %northbound.resource,
            mcp_metadata_url = northbound.metadata_url.as_deref().unwrap_or("none"),
            mcp_auth_mode = northbound.auth_mode,
            handoff_resource = %hosted_handoff.resource,
            handoff_auth_mode = hosted_handoff.auth_mode,
            agent_identity = "ed25519_application_level",
            "starting closed hosted Agent gRPC + MCP + Handoff one-port ingress"
        );
        axum::serve(
            listener,
            router.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .with_graceful_shutdown(wait_for_shutdown(shutdown_rx))
        .await
        .context("V2 Hub hosted one-port server failed")?;
    } else {
        let (cert_pem, key_pem) = tls_identity.expect("validated non-hosted TLS identity");
        let grpc_shutdown = shutdown_rx.clone();
        let grpc = Server::builder()
            .tls_config(ServerTlsConfig::new().identity(Identity::from_pem(cert_pem, key_pem)))?
            .add_service(agent_service)
            .serve_with_shutdown(args.bind, wait_for_shutdown(grpc_shutdown));

        if let Some(northbound) = northbound {
            let bind = northbound
                .bind
                .expect("non-hosted northbound runtime must carry loopback bind");
            let listener = tokio::net::TcpListener::bind(bind)
                .await
                .with_context(|| format!("failed to bind northbound MCP listener at {bind}"))?;
            info!(
                event = "v2_northbound_mcp_start",
                bind = %bind,
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
    }

    #[cfg(unix)]
    if let Some(task) = handoff_control_task {
        task.await
            .context("private Handoff control task failed during shutdown")?;
    }
    Ok(())
}

async fn build_handoff_coordinator(
    args: &Args,
    hub: computer_use_mcp_gateway::v2_m1_hub::HubHandle,
) -> Result<Option<Arc<HandoffCoordinator>>> {
    let legacy_managed_fields = [
        args.handoff_runtime_command.is_some(),
        args.handoff_runtime_script.is_some(),
        args.handoff_runtime_env_file.is_some(),
    ];
    ensure!(
        !legacy_managed_fields
            .into_iter()
            .any(|configured| configured),
        "Hub-owned CUMG_V2_HANDOFF_RUNTIME_* configuration is no longer supported; configure the managed Handoff runtime on v2_agent"
    );
    ensure!(
        !(args.handoff_control_socket.is_some() && args.operator_handoff_socket.is_some()),
        "first-class Agent-owned Handoff and CUMG_V2_OPERATOR_HANDOFF_SOCKET are mutually exclusive"
    );

    if args.hosted_profile {
        ensure!(
            args.handoff_control_socket.is_none() && args.operator_handoff_socket.is_none(),
            "hosted profile uses the OAuth Handoff operator resource and must not configure local Handoff operator sockets"
        );
        info!(
            event = "v2_handoff_runtime_configured",
            mode = "agent_owned_hosted",
            outcome = "ready",
            "hosted Handoff coordination routes to the controlled Agent without a Hub-local operator socket"
        );
        return Ok(Some(Arc::new(HandoffCoordinator::agent_owned(hub))));
    }

    if args.handoff_control_socket.is_some() {
        info!(
            event = "v2_handoff_runtime_configured",
            mode = "agent_owned_remote",
            outcome = "ready",
            "first-class Handoff coordination routes to the controlled Agent"
        );
        return Ok(Some(Arc::new(HandoffCoordinator::agent_owned(hub))));
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
    let shared_oauth_configured =
        args.oauth_authorization_server.is_some() || args.oauth_required_scopes.is_some();
    let introspection_configured = args.oauth_introspection_endpoint.is_some()
        || args.oauth_introspection_client_id.is_some()
        || args.oauth_introspection_client_secret_file.is_some();
    let oidc_configured = args.oidc_audience.is_some()
        || args.oidc_jwks_uri.is_some()
        || args.oidc_allowed_algorithms.is_some();
    let trusted_proxy_configured = args.trusted_proxy_issuer.is_some()
        || args.trusted_proxy_subject.is_some()
        || args.trusted_proxy_secret_file.is_some();
    validate_northbound_auth_mode_selection(
        introspection_configured,
        oidc_configured,
        trusted_proxy_configured,
        shared_oauth_configured,
    )?;
    let configured = [
        args.mcp_resource.is_some(),
        args.northbound_policy_file.is_some(),
        args.semantic_constraint_policy_file.is_some(),
        shared_oauth_configured,
        introspection_configured,
        oidc_configured,
        trusted_proxy_configured,
    ]
    .into_iter()
    .any(|value| value);

    let bind = if args.hosted_profile {
        ensure!(
            args.mcp_bind.is_none(),
            "hosted profile serves MCP on the shared PORT listener; CUMG_V2_MCP_BIND must be unset"
        );
        if !configured {
            bail!("hosted profile requires northbound MCP configuration");
        }
        None
    } else {
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
        Some(bind)
    };

    let resource = required(&args.mcp_resource, "CUMG_V2_MCP_RESOURCE")?;
    let policy_file = args
        .northbound_policy_file
        .as_ref()
        .context("CUMG_V2_NORTHBOUND_POLICY_FILE is required")?;
    let policy_text = load_trusted_text(policy_file, MAX_NORTHBOUND_POLICY_BYTES)
        .context("failed to load northbound authorization policy")?;
    let semantic_constraints = args
        .semantic_constraint_policy_file
        .as_ref()
        .map(|path| {
            let text = load_trusted_text(path, MAX_SEMANTIC_CONSTRAINT_POLICY_BYTES)
                .context("failed to load semantic constraint policy")?;
            SemanticConstraintPolicy::from_json(&text).context("invalid semantic constraint policy")
        })
        .transpose()?;
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
    let status_provider = build_northbound_status_provider(args)?;

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
        if let Some(constraints) = semantic_constraints.clone() {
            service = service
                .with_semantic_constraints(constraints)
                .context("failed to install immutable semantic constraint revision")?;
        }
        if let Some(secret) = audit_fingerprint_secret.clone() {
            service = service.with_request_fingerprint_secret(secret);
        }
        if let Some(coordinator) = handoff_coordinator.as_ref() {
            service = service.with_handoff_coordinator(coordinator.clone());
        }
        if let Some(provider) = status_provider.as_ref() {
            service = service.with_status_provider(provider.clone());
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
    } else if oidc_configured {
        let authorization_server = required(
            &args.oauth_authorization_server,
            "CUMG_V2_OAUTH_AUTHORIZATION_SERVER",
        )?;
        let scopes = required(&args.oauth_required_scopes, "CUMG_V2_OAUTH_REQUIRED_SCOPES")?
            .split_ascii_whitespace()
            .map(ToOwned::to_owned)
            .collect::<Vec<_>>();
        let audience = required(&args.oidc_audience, "CUMG_V2_OIDC_AUDIENCE")?;
        let jwks_uri = required(&args.oidc_jwks_uri, "CUMG_V2_OIDC_JWKS_URI")?;
        let algorithms = parse_oidc_algorithms(required(
            &args.oidc_allowed_algorithms,
            "CUMG_V2_OIDC_ALLOWED_ALGORITHMS",
        )?)?;
        let mcp_config = NorthboundMcpConfig::new(resource, authorization_server, scopes)
            .context("invalid V2 northbound OIDC/JWT Authorization configuration")?;
        let policy = NorthboundPolicyDocument::from_json(&policy_text)
            .context("failed to parse northbound authorization policy")?
            .build_policy(mcp_config.authorization_server(), device_id)
            .context("invalid northbound principal/device/capability policy")?;
        let mut verifier_config = OidcJwtConfig::new(
            mcp_config.authorization_server(),
            audience,
            jwks_uri,
            algorithms,
        );
        verifier_config.clock_skew = Duration::from_secs(args.oidc_clock_skew_secs);
        verifier_config.jwks_cache_ttl = Duration::from_secs(args.oidc_jwks_cache_secs);
        verifier_config.unknown_kid_refresh_interval =
            Duration::from_secs(args.oidc_unknown_kid_refresh_secs);
        verifier_config.http_timeout = Duration::from_secs(args.oidc_http_timeout_secs);
        let verifier = OidcJwtVerifier::new(verifier_config)
            .context("invalid OIDC/JWT token verification configuration")?;
        let metadata_url = mcp_config.metadata_url().to_owned();
        let resource = mcp_config.resource().to_owned();
        let mut service = V2NorthboundMcp::new(handle, policy);
        if let Some(constraints) = semantic_constraints.clone() {
            service = service
                .with_semantic_constraints(constraints)
                .context("failed to install immutable semantic constraint revision")?;
        }
        if let Some(secret) = audit_fingerprint_secret.clone() {
            service = service.with_request_fingerprint_secret(secret);
        }
        if let Some(coordinator) = handoff_coordinator.as_ref() {
            service = service.with_handoff_coordinator(coordinator.clone());
        }
        if let Some(provider) = status_provider.as_ref() {
            service = service.with_status_provider(provider.clone());
        }
        let router = build_northbound_router(service, mcp_config, Arc::new(verifier)).layer(
            axum::middleware::from_fn_with_state(
                overload,
                computer_use_mcp_gateway::v2_limits::enforce_http_limits,
            ),
        );
        (router, resource, Some(metadata_url), "oidc_jwt")
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
        if let Some(constraints) = semantic_constraints.clone() {
            service = service
                .with_semantic_constraints(constraints)
                .context("failed to install immutable semantic constraint revision")?;
        }
        if let Some(secret) = audit_fingerprint_secret.clone() {
            service = service.with_request_fingerprint_secret(secret);
        }
        if let Some(coordinator) = handoff_coordinator.as_ref() {
            service = service.with_handoff_coordinator(coordinator.clone());
        }
        if let Some(provider) = status_provider.as_ref() {
            service = service.with_status_provider(provider.clone());
        }
        let router = build_northbound_router(service, mcp_config, Arc::new(verifier)).layer(
            axum::middleware::from_fn_with_state(
                overload,
                computer_use_mcp_gateway::v2_limits::enforce_http_limits,
            ),
        );
        (router, resource, Some(metadata_url), "oauth_introspection")
    };

    let (mcp_path, metadata_path) = if args.hosted_profile {
        let mcp_uri = args
            .mcp_resource
            .as_ref()
            .expect("validated hosted MCP resource")
            .parse::<axum::http::Uri>()
            .context("invalid hosted MCP resource URI")?;
        let metadata_uri = metadata_url
            .as_deref()
            .context("hosted MCP OAuth mode must expose protected-resource metadata")?
            .parse::<axum::http::Uri>()
            .context("invalid hosted MCP metadata URI")?;
        (mcp_uri.path().to_owned(), metadata_uri.path().to_owned())
    } else {
        (String::new(), String::new())
    };

    Ok(Some(NorthboundRuntime {
        bind,
        router,
        resource,
        mcp_path,
        metadata_path,
        metadata_url,
        auth_mode,
    }))
}

fn build_hosted_handoff_runtime(
    args: &Args,
    handle: computer_use_mcp_gateway::v2_m1_hub::HubHandle,
    device_id: &str,
    handoff_coordinator: Option<Arc<HandoffCoordinator>>,
) -> Result<Option<HostedHandoffRuntime>> {
    if !args.hosted_profile {
        return Ok(None);
    }

    let coordinator =
        handoff_coordinator.context("hosted profile requires Agent-owned Handoff coordination")?;
    let resource = required(
        &args.hosted_handoff_resource,
        "CUMG_V2_HOSTED_HANDOFF_RESOURCE",
    )?;
    let authorization_server = required(
        &args.oauth_authorization_server,
        "CUMG_V2_OAUTH_AUTHORIZATION_SERVER",
    )?;
    let scopes = required(
        &args.hosted_handoff_required_scopes,
        "CUMG_V2_HOSTED_HANDOFF_REQUIRED_SCOPES",
    )?
    .split_ascii_whitespace()
    .map(ToOwned::to_owned)
    .collect::<Vec<_>>();
    let config = HostedHandoffHttpConfig::new(resource, authorization_server, scopes)
        .context("invalid hosted Handoff OAuth resource configuration")?;
    let policy_file = args
        .hosted_handoff_policy_file
        .as_ref()
        .context("CUMG_V2_HOSTED_HANDOFF_POLICY_FILE is required in hosted profile")?;
    let policy_text = load_trusted_text(policy_file, MAX_HOSTED_HANDOFF_POLICY_BYTES)
        .context("failed to load hosted Handoff authorization policy")?;
    let policy = HostedHandoffPolicyDocument::from_json(&policy_text)
        .context("failed to parse hosted Handoff authorization policy")?
        .build_policy(config.authorization_server(), device_id)
        .context("invalid hosted Handoff principal/device/action policy")?;

    let oidc_configured = args.oidc_audience.is_some()
        || args.oidc_jwks_uri.is_some()
        || args.oidc_allowed_algorithms.is_some();
    let introspection_configured = args.oauth_introspection_endpoint.is_some()
        || args.oauth_introspection_client_id.is_some()
        || args.oauth_introspection_client_secret_file.is_some();

    let (verifier, auth_mode): (Arc<dyn AccessTokenVerifier>, &'static str) = if oidc_configured {
        let audience = required(
            &args.hosted_handoff_oidc_audience,
            "CUMG_V2_HOSTED_HANDOFF_OIDC_AUDIENCE",
        )?;
        let jwks_uri = required(&args.oidc_jwks_uri, "CUMG_V2_OIDC_JWKS_URI")?;
        let algorithms = parse_oidc_algorithms(required(
            &args.oidc_allowed_algorithms,
            "CUMG_V2_OIDC_ALLOWED_ALGORITHMS",
        )?)?;
        let mut verifier_config = OidcJwtConfig::new(
            config.authorization_server(),
            audience,
            jwks_uri,
            algorithms,
        );
        verifier_config.clock_skew = Duration::from_secs(args.oidc_clock_skew_secs);
        verifier_config.jwks_cache_ttl = Duration::from_secs(args.oidc_jwks_cache_secs);
        verifier_config.unknown_kid_refresh_interval =
            Duration::from_secs(args.oidc_unknown_kid_refresh_secs);
        verifier_config.http_timeout = Duration::from_secs(args.oidc_http_timeout_secs);
        (
            Arc::new(
                OidcJwtVerifier::new(verifier_config)
                    .context("invalid hosted Handoff OIDC/JWT verifier configuration")?,
            ),
            "oidc_jwt",
        )
    } else if introspection_configured {
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
        let secret = load_secret_text(secret_file, MAX_OAUTH_SECRET_BYTES)
            .context("failed to load OAuth introspection client secret")?;
        let mut verifier_config = OAuthIntrospectionConfig::new(
            config.authorization_server(),
            config.resource(),
            introspection_endpoint,
            introspection_client_id,
            secret,
        );
        verifier_config.timeout = Duration::from_secs(args.oauth_introspection_timeout_secs);
        (
            Arc::new(
                OAuthIntrospectionVerifier::new(verifier_config)
                    .context("invalid hosted Handoff OAuth introspection configuration")?,
            ),
            "oauth_introspection",
        )
    } else {
        bail!("hosted profile requires OIDC/JWT or OAuth introspection authentication");
    };

    let service = Arc::new(
        HostedHandoffControlService::new(
            device_id.to_owned(),
            Arc::new(policy),
            coordinator,
            handle,
        )
        .context("invalid hosted Handoff control service configuration")?,
    );
    let context_path = config.context_path().to_owned();
    let control_path = config.control_path().to_owned();
    let metadata_path = config.metadata_path().to_owned();
    let resource = config.resource().to_owned();
    let router = build_hosted_handoff_router(service, config, verifier);

    Ok(Some(HostedHandoffRuntime {
        router,
        resource,
        context_path,
        control_path,
        metadata_path,
        auth_mode,
    }))
}

fn build_hosted_postgres_state_config(args: &Args) -> Result<PostgresHubStateStoreConfig> {
    let mut config = PostgresHubStateStoreConfig::new(
        required(&args.postgres_host, "CUMG_V2_POSTGRES_HOST")?,
        required(&args.postgres_database, "CUMG_V2_POSTGRES_DATABASE")?,
        required(&args.postgres_user, "CUMG_V2_POSTGRES_USER")?,
        required(&args.postgres_state_key, "CUMG_V2_POSTGRES_STATE_KEY")?,
    )
    .context("invalid hosted PostgreSQL Hub-state configuration")?
    .with_port(args.postgres_port)
    .context("invalid hosted PostgreSQL port")?
    .with_timeouts(
        Duration::from_secs(args.postgres_connect_timeout_secs),
        Duration::from_secs(args.postgres_query_timeout_secs),
    )
    .context("invalid hosted PostgreSQL timeout configuration")?;

    if let Some(path) = args.postgres_password_file.as_ref() {
        let password = load_secret_text(path, MAX_POSTGRES_PASSWORD_BYTES)
            .context("failed to load hosted PostgreSQL password")?;
        config = config
            .with_password(password)
            .context("invalid hosted PostgreSQL password")?;
    }
    Ok(config)
}

fn validate_profile_configuration(args: &Args) -> Result<()> {
    let hosted_handoff_configured = args.hosted_handoff_resource.is_some()
        || args.hosted_handoff_required_scopes.is_some()
        || args.hosted_handoff_policy_file.is_some()
        || args.hosted_handoff_oidc_audience.is_some();
    let trusted_proxy_configured = args.trusted_proxy_issuer.is_some()
        || args.trusted_proxy_subject.is_some()
        || args.trusted_proxy_secret_file.is_some();
    let oidc_configured = args.oidc_audience.is_some()
        || args.oidc_jwks_uri.is_some()
        || args.oidc_allowed_algorithms.is_some();
    let introspection_configured = args.oauth_introspection_endpoint.is_some()
        || args.oauth_introspection_client_id.is_some()
        || args.oauth_introspection_client_secret_file.is_some();

    if args.hosted_profile {
        ensure!(
            args.hosted_port.is_some_and(|port| port > 0),
            "hosted profile requires Cloud Run PORT"
        );
        ensure!(
            args.postgres_host.is_some()
                && args.postgres_database.is_some()
                && args.postgres_user.is_some()
                && args.postgres_state_key.is_some(),
            "hosted profile requires complete external PostgreSQL Hub-state configuration"
        );
        ensure!(
            args.postgres_port > 0
                && args.postgres_connect_timeout_secs > 0
                && args.postgres_query_timeout_secs > 0,
            "hosted profile requires positive PostgreSQL port/timeouts"
        );
        ensure!(
            args.drain_timeout_secs <= 8,
            "hosted profile requires CUMG_V2_DRAIN_TIMEOUT_SECS <= 8"
        );
        ensure!(
            args.tls_cert_pem_file.is_none() && args.tls_key_pem_file.is_none(),
            "hosted profile terminates public TLS before the container; CUMG_V2_TLS_* must be unset"
        );
        ensure!(
            args.mcp_bind.is_none(),
            "hosted profile serves MCP on shared PORT; CUMG_V2_MCP_BIND must be unset"
        );
        ensure!(
            !trusted_proxy_configured,
            "hosted profile requires OAuth/OIDC bearer authentication; trusted-proxy mode is not accepted on public hosted ingress"
        );
        ensure!(
            oidc_configured ^ introspection_configured,
            "hosted profile requires exactly one of OIDC/JWT or OAuth introspection authentication"
        );
        ensure!(
            args.hosted_handoff_resource.is_some()
                && args.hosted_handoff_required_scopes.is_some()
                && args.hosted_handoff_policy_file.is_some(),
            "hosted profile requires complete hosted Handoff resource/scope/policy configuration"
        );
        ensure!(
            args.mcp_resource.is_some()
                && args.northbound_policy_file.is_some()
                && args.oauth_authorization_server.is_some()
                && args.oauth_required_scopes.is_some(),
            "hosted profile requires the complete protected northbound MCP resource/policy/scope configuration"
        );
        ensure!(
            args.hosted_handoff_resource != args.mcp_resource,
            "hosted Handoff and MCP must use distinct protected resource URIs"
        );
        if oidc_configured {
            let mcp_audience = required(&args.oidc_audience, "CUMG_V2_OIDC_AUDIENCE")?;
            let handoff_audience = required(
                &args.hosted_handoff_oidc_audience,
                "CUMG_V2_HOSTED_HANDOFF_OIDC_AUDIENCE",
            )?;
            ensure!(
                mcp_audience != handoff_audience,
                "hosted Handoff and MCP must use distinct OIDC audiences"
            );
        } else {
            ensure!(
                args.hosted_handoff_oidc_audience.is_none(),
                "CUMG_V2_HOSTED_HANDOFF_OIDC_AUDIENCE is valid only in hosted OIDC/JWT mode"
            );
        }
        ensure!(
            args.handoff_control_socket.is_none() && args.operator_handoff_socket.is_none(),
            "hosted profile must not configure local Handoff operator sockets"
        );
    } else {
        ensure!(
            args.tls_cert_pem_file.is_some() && args.tls_key_pem_file.is_some(),
            "non-hosted profile requires CUMG_V2_TLS_CERT_PEM_FILE and CUMG_V2_TLS_KEY_PEM_FILE"
        );
        ensure!(
            args.postgres_host.is_none()
                && args.postgres_database.is_none()
                && args.postgres_user.is_none()
                && args.postgres_password_file.is_none()
                && args.postgres_state_key.is_none(),
            "PostgreSQL hosted Hub-state settings require CUMG_V2_HOSTED_PROFILE=true"
        );
        ensure!(
            !hosted_handoff_configured,
            "hosted Handoff resource settings require CUMG_V2_HOSTED_PROFILE=true"
        );
    }
    Ok(())
}

fn status_home(install_root: &std::path::Path) -> Result<PathBuf> {
    if let Some(home) = std::env::var_os("HOME") {
        return Ok(PathBuf::from(home));
    }
    #[cfg(target_os = "windows")]
    if let Some(profile) = std::env::var_os("USERPROFILE") {
        return Ok(PathBuf::from(profile));
    }
    #[cfg(target_os = "windows")]
    {
        return Ok(install_root.to_path_buf());
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = install_root;
        bail!("HOME is required when CUMG_V2_STATUS_INSTALL_ROOT is configured")
    }
}

fn build_northbound_status_provider(
    args: &Args,
) -> Result<Option<Arc<CollectedOperatorStatusProvider>>> {
    let Some(install_root) = args.status_install_root.as_ref() else {
        return Ok(None);
    };
    ensure!(
        install_root.is_absolute(),
        "CUMG_V2_STATUS_INSTALL_ROOT must be absolute"
    );
    let home = status_home(install_root)?;
    let run_root = args
        .status_run_root
        .clone()
        .unwrap_or_else(|| home.join("Library/Caches/cumg-v2"));
    ensure!(
        run_root.is_absolute(),
        "CUMG_V2_STATUS_RUN_ROOT must be absolute"
    );
    let mut config =
        OperatorStatusCollectionConfig::installed_defaults(home, install_root.clone(), run_root);
    config.hub_state_dir = Some(args.state_dir.clone());
    config.grant_signer_socket = args.grant_signer_socket.clone();
    config.tls_server_certificate = args.tls_cert_pem_file.clone();
    config.handoff_control_socket = args.handoff_control_socket.clone();
    Ok(Some(Arc::new(CollectedOperatorStatusProvider::new(config))))
}

fn validate_northbound_auth_mode_selection(
    introspection: bool,
    oidc: bool,
    trusted_proxy: bool,
    shared_oauth: bool,
) -> Result<()> {
    let auth_mode_count =
        usize::from(introspection) + usize::from(oidc) + usize::from(trusted_proxy);
    ensure!(
        auth_mode_count <= 1,
        "OAuth introspection, OIDC/JWT, and trusted-proxy authentication modes are mutually exclusive"
    );
    ensure!(
        !(trusted_proxy && shared_oauth),
        "trusted-proxy mode must not configure OAuth/OIDC authorization-server or scope settings"
    );
    Ok(())
}

fn parse_oidc_algorithms(value: &str) -> Result<Vec<OidcJwtAlgorithm>> {
    let algorithms = value
        .split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| {
            value
                .parse::<OidcJwtAlgorithm>()
                .map_err(|_| anyhow::anyhow!("unsupported OIDC/JWT algorithm: {value}"))
        })
        .collect::<Result<Vec<_>>>()?;
    ensure!(
        !algorithms.is_empty(),
        "CUMG_V2_OIDC_ALLOWED_ALGORITHMS must contain at least one asymmetric algorithm"
    );
    Ok(algorithms)
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

#[cfg(test)]
mod auth_mode_tests {
    use super::*;

    #[test]
    fn authentication_modes_are_mutually_exclusive() {
        assert!(validate_northbound_auth_mode_selection(true, false, false, true).is_ok());
        assert!(validate_northbound_auth_mode_selection(false, true, false, true).is_ok());
        assert!(validate_northbound_auth_mode_selection(false, false, true, false).is_ok());
        assert!(validate_northbound_auth_mode_selection(true, true, false, true).is_err());
        assert!(validate_northbound_auth_mode_selection(true, false, true, true).is_err());
        assert!(validate_northbound_auth_mode_selection(false, true, true, true).is_err());
        assert!(validate_northbound_auth_mode_selection(false, false, true, true).is_err());
    }

    fn parsed_args(extra: &[&str]) -> Args {
        let mut argv = vec![
            "v2_hub",
            "--hub-secret-file",
            "/tmp/hub.key",
            "--device-public-key-file",
            "/tmp/device.pub",
            "--state-dir",
            "/tmp/state",
        ];
        argv.extend_from_slice(extra);
        Args::try_parse_from(argv).unwrap()
    }

    fn hosted_oidc_args() -> Args {
        parsed_args(&[
            "--hosted-profile",
            "--hosted-port",
            "8080",
            "--drain-timeout-secs",
            "8",
            "--postgres-host",
            "/cloudsql/project:region:instance",
            "--postgres-database",
            "cumg",
            "--postgres-user",
            "cumg_runtime",
            "--postgres-state-key",
            "hosted-device-a",
            "--mcp-resource",
            "https://hub.example/mcp",
            "--northbound-policy-file",
            "/tmp/mcp-policy.json",
            "--oauth-authorization-server",
            "https://issuer.example",
            "--oauth-required-scopes",
            "cumg:mcp",
            "--oidc-audience",
            "cumg-mcp",
            "--oidc-jwks-uri",
            "https://issuer.example/jwks",
            "--oidc-allowed-algorithms",
            "RS256",
            "--hosted-handoff-resource",
            "https://hub.example/operator/v1/handoff",
            "--hosted-handoff-required-scopes",
            "cumg:handoff",
            "--hosted-handoff-policy-file",
            "/tmp/handoff-policy.json",
            "--hosted-handoff-oidc-audience",
            "cumg-handoff",
        ])
    }

    #[test]
    fn hosted_profile_requires_one_port_h2c_and_distinct_auth_resources() {
        let args = hosted_oidc_args();
        assert!(validate_profile_configuration(&args).is_ok());

        let with_tls = parsed_args(&[
            "--hosted-profile",
            "--hosted-port",
            "8080",
            "--postgres-host",
            "/cloudsql/project:region:instance",
            "--postgres-database",
            "cumg",
            "--postgres-user",
            "cumg_runtime",
            "--postgres-state-key",
            "hosted-device-a",
            "--tls-cert-pem-file",
            "/tmp/cert.pem",
            "--tls-key-pem-file",
            "/tmp/key.pem",
            "--mcp-resource",
            "https://hub.example/mcp",
            "--northbound-policy-file",
            "/tmp/mcp-policy.json",
            "--oauth-authorization-server",
            "https://issuer.example",
            "--oauth-required-scopes",
            "cumg:mcp",
            "--oidc-audience",
            "cumg-mcp",
            "--oidc-jwks-uri",
            "https://issuer.example/jwks",
            "--oidc-allowed-algorithms",
            "RS256",
            "--hosted-handoff-resource",
            "https://hub.example/operator/v1/handoff",
            "--hosted-handoff-required-scopes",
            "cumg:handoff",
            "--hosted-handoff-policy-file",
            "/tmp/handoff-policy.json",
            "--hosted-handoff-oidc-audience",
            "cumg-handoff",
        ]);
        assert!(validate_profile_configuration(&with_tls).is_err());

        let mut same_audience = hosted_oidc_args();
        same_audience.hosted_handoff_oidc_audience = same_audience.oidc_audience.clone();
        assert!(validate_profile_configuration(&same_audience).is_err());

        let mut same_resource = hosted_oidc_args();
        same_resource.hosted_handoff_resource = same_resource.mcp_resource.clone();
        assert!(validate_profile_configuration(&same_resource).is_err());

        let mut loopback_mcp = hosted_oidc_args();
        loopback_mcp.mcp_bind = Some("127.0.0.1:7444".parse().unwrap());
        assert!(validate_profile_configuration(&loopback_mcp).is_err());

        let mut missing_handoff_scope = hosted_oidc_args();
        missing_handoff_scope.hosted_handoff_required_scopes = None;
        assert!(validate_profile_configuration(&missing_handoff_scope).is_err());

        let mut missing_handoff_policy = hosted_oidc_args();
        missing_handoff_policy.hosted_handoff_policy_file = None;
        assert!(validate_profile_configuration(&missing_handoff_policy).is_err());

        let mut excessive_drain = hosted_oidc_args();
        excessive_drain.drain_timeout_secs = 9;
        assert!(validate_profile_configuration(&excessive_drain).is_err());
    }

    #[test]
    fn non_hosted_profile_retains_tls_and_rejects_hosted_only_settings() {
        let args = parsed_args(&[
            "--tls-cert-pem-file",
            "/tmp/cert.pem",
            "--tls-key-pem-file",
            "/tmp/key.pem",
        ]);
        assert!(validate_profile_configuration(&args).is_ok());

        let missing_tls = parsed_args(&[]);
        assert!(validate_profile_configuration(&missing_tls).is_err());

        let with_hosted_resource = parsed_args(&[
            "--tls-cert-pem-file",
            "/tmp/cert.pem",
            "--tls-key-pem-file",
            "/tmp/key.pem",
            "--hosted-handoff-resource",
            "https://hub.example/operator/v1/handoff",
        ]);
        assert!(validate_profile_configuration(&with_hosted_resource).is_err());
    }

    #[test]
    fn oidc_algorithm_parser_accepts_only_asymmetric_allowlist() {
        let parsed = parse_oidc_algorithms("RS256, ES256,EdDSA").unwrap();
        assert_eq!(parsed.len(), 3);
        assert!(parse_oidc_algorithms("HS256").is_err());
        assert!(parse_oidc_algorithms("none").is_err());
        assert!(parse_oidc_algorithms(" , ").is_err());
    }
}
