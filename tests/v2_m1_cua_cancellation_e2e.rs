#![cfg(target_os = "macos")]

use anyhow::{Context, Result, anyhow};
use computer_use_mcp_gateway::{
    v2_execution_safety::{ExecutionEvidence, OperationOwner},
    v2_m0::{DeviceCommand, DeviceIdentity, DeviceResult, GrantAuthority, ShellRequest},
    v2_m0_execution::IndeterminateResolution,
    v2_m0_transport::{CancellationDisposition, HubIdentity},
    v2_m1::ReconnectPolicy,
    v2_m1_agent::{AgentService, AgentServiceConfig, CuaAgentConfig},
    v2_m1_grpc::{
        MAX_GRPC_TRANSPORT_MESSAGE_BYTES, proto::agent_control_server::AgentControlServer,
    },
    v2_m1_hub::{HubCommandError, HubProvisionedMaterial, HubServiceConfig, SingleDeviceHub},
    v2_m1_keys::AgentProvisionedMaterial,
};
use rcgen::{CertifiedKey, generate_simple_self_signed};
use std::{path::PathBuf, time::Duration};
use tokio::sync::watch;
use tonic::transport::{Identity, Server, ServerTlsConfig};

fn temp_dir(name: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!(
        "cumg-{name}-{}-{}",
        std::process::id(),
        rand::random::<u64>()
    ));
    std::fs::create_dir_all(&path).unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
    path
}

fn prepare_textedit_fixture(path: &std::path::Path) -> Result<()> {
    std::fs::write(path, b"V2 real-Cua cancellation fixture\n")?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "real macOS desktop/Cua acceptance; run explicitly with CUMG_V2_CUA_CANCEL_E2E_ACK=1"]
async fn real_cua_cancel_is_propagated_and_quarantined_indeterminate() -> Result<()> {
    if std::env::var("CUMG_V2_CUA_CANCEL_E2E_ACK").as_deref() != Ok("1") {
        return Err(anyhow!(
            "refusing real desktop action without CUMG_V2_CUA_CANCEL_E2E_ACK=1"
        ));
    }
    let cua = std::env::var("CUMG_V2_CUA_COMMAND")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            PathBuf::from(std::env::var("HOME").unwrap()).join(".local/bin/cua-driver")
        });
    if !cua.is_file() {
        return Err(anyhow!("Cua executable not found: {}", cua.display()));
    }

    let fixture_dir = temp_dir("v2-real-cua");
    let fixture = fixture_dir.join("V2-Cua-Cancel-Fixture.txt");
    prepare_textedit_fixture(&fixture)?;

    let CertifiedKey { cert, signing_key } = generate_simple_self_signed(vec!["localhost".into()])?;
    let cert_pem = cert.pem();
    let cert_der = cert.der().to_vec();
    let key_pem = signing_key.serialize_pem();
    let device_identity = DeviceIdentity::generate();
    let hub_identity = HubIdentity::generate();
    let grant_authority = GrantAuthority::generate();
    let cwd = std::env::current_dir()?;

    let (hub, handle) = SingleDeviceHub::new(
        HubServiceConfig {
            state_dir: temp_dir("v2-real-cua-hub-state"),
            heartbeat_timeout: Duration::from_secs(2),
            max_queued_per_device: 1,
            max_agent_sessions: 2,
            max_agent_session_starts_per_minute: 30,
        },
        HubProvisionedMaterial {
            hub_identity: hub_identity.clone(),
            grant_authority: grant_authority.clone(),
            device_verifier: device_identity.verifying_key(),
            device_rotation: None,
        },
    )?;
    let device_id = hub.device_id().to_owned();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let incoming = tokio_stream::wrappers::TcpListenerStream::new(listener);
    let server = tokio::spawn(async move {
        Server::builder()
            .tls_config(ServerTlsConfig::new().identity(Identity::from_pem(cert_pem, key_pem)))?
            .add_service(
                AgentControlServer::new(hub)
                    .max_decoding_message_size(MAX_GRPC_TRANSPORT_MESSAGE_BYTES)
                    .max_encoding_message_size(MAX_GRPC_TRANSPORT_MESSAGE_BYTES),
            )
            .serve_with_incoming(incoming)
            .await
    });

    let mut agent = AgentService::new(
        AgentServiceConfig {
            hub_endpoint: format!("https://localhost:{}", address.port()),
            hub_domain: "localhost".into(),
            device_id,
            allowed_cwd_roots: vec![cwd.clone()],
            state_dir: temp_dir("v2-real-cua-agent-state"),
            heartbeat_interval: Duration::from_millis(100),
            reconnect: ReconnectPolicy {
                initial_delay: Duration::from_millis(50),
                max_delay: Duration::from_millis(250),
                max_attempts: 8,
            },
            cua: Some(CuaAgentConfig {
                command: cua.to_string_lossy().into_owned(),
                args: vec!["mcp".into()],
                backend_version: "0.19.3".into(),
                platform: "macos".into(),
                revision: 1,
                connect_timeout: Duration::from_secs(10),
                tool_timeout: Duration::from_secs(30),
                reconnect_attempts: 3,
                reconnect_backoff: Duration::from_millis(200),
            }),
        },
        AgentProvisionedMaterial {
            device_identity,
            trusted_hub: hub_identity.verifier(),
            grant_verifier: grant_authority.verifier(),
            additional_grant_verifiers: vec![],
            hub_rotation: None,
            tls_root_der: cert_der,
        },
    )?;
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let agent_task = tokio::spawn(async move { agent.run(shutdown_rx).await });

    tokio::time::timeout(Duration::from_secs(8), async {
        while !handle.is_online().await {
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .context("real-Cua Agent did not connect")?;

    let alice = OperationOwner::new("https://issuer.example", "alice")?;
    let bob = OperationOwner::new("https://issuer.example", "bob")?;

    // Launch the real desktop application through the native shell executor,
    // not outside CUMG. The next GUI command therefore crosses the same Hub
    // operation owner/fence boundary as this shell side effect.
    let launch = handle
        .start_command_as(
            alice.clone(),
            DeviceCommand::Shell {
                request: ShellRequest {
                    command: format!("/usr/bin/open -a TextEdit '{}'", fixture.to_string_lossy()),
                    cwd: cwd.to_string_lossy().into_owned(),
                    env: vec![],
                    timeout_ms: 10_000,
                },
            },
        )
        .await?
        .wait()
        .await?;
    assert!(matches!(launch.result, DeviceResult::Shell { .. }));
    assert_eq!(launch.receipt.owner, alice);
    assert_eq!(
        launch.receipt.evidence,
        ExecutionEvidence::VerifiedAgentResult
    );
    tokio::time::sleep(Duration::from_millis(800)).await;

    let geometry = handle
        .start_command_as(alice.clone(), DeviceCommand::ScreenGeometry)
        .await?
        .wait()
        .await?;
    if !matches!(geometry.result, DeviceResult::ScreenGeometry { .. }) {
        return Err(anyhow!(
            "real Cua screen geometry returned unexpected result"
        ));
    }

    // The TextEdit fixture covers this point. from==to makes the physical gesture
    // a long press with no cursor displacement while still leaving a 10s window
    // in which downstream MCP cancellation can race a live desktop operation.
    let pending = handle
        .start_command_as(
            alice.clone(),
            DeviceCommand::PointerDrag {
                from_x: 500,
                from_y: 400,
                to_x: 500,
                to_y: 400,
                duration_ms: 10_000,
            },
        )
        .await?;
    let operation_id = pending.operation_id.clone();
    tokio::time::sleep(Duration::from_millis(500)).await;
    let disposition = tokio::time::timeout(
        Duration::from_secs(5),
        handle.cancel_as(alice.clone(), operation_id.clone()),
    )
    .await
    .context("cancel timed out")??;
    assert_eq!(
        disposition,
        CancellationDisposition::IndeterminateAfterPropagation
    );
    let pending_result = tokio::time::timeout(Duration::from_secs(5), pending.wait())
        .await
        .context("pending wait timed out")?;
    assert!(matches!(
        pending_result,
        Err(HubCommandError::DeviceIndeterminate { operation_id: ref blocked }) if blocked == &operation_id
    ));

    tokio::time::sleep(Duration::from_millis(300)).await;
    let quarantine = handle
        .desktop_quarantine()
        .await
        .ok_or_else(|| anyhow!("real-Cua desktop quarantine missing"))?;
    assert_eq!(quarantine.operation_id, operation_id);
    assert_eq!(quarantine.owner, alice);

    let blocked_pending = handle
        .start_command_as(bob.clone(), DeviceCommand::ScreenGeometry)
        .await?;
    let blocked = tokio::time::timeout(Duration::from_secs(5), blocked_pending.wait())
        .await
        .context("quarantine check timed out")?;
    assert!(matches!(
        blocked,
        Err(HubCommandError::DeviceIndeterminate { operation_id: ref blocked_id }) if blocked_id == &operation_id
    ));

    let resolution = handle
        .resolve_indeterminate(
            &operation_id,
            alice.clone(),
            IndeterminateResolution::ConfirmedCompleted,
            "real-Cua acceptance explicitly reconciled the TextEdit desktop state",
        )
        .await?;
    assert_eq!(resolution.operation.operation_id, operation_id);
    assert_eq!(resolution.evidence, ExecutionEvidence::OperatorResolution);
    assert!(handle.desktop_quarantine().await.is_none());

    let reused = handle
        .start_command_as(bob, DeviceCommand::ScreenGeometry)
        .await?
        .wait()
        .await?;
    assert!(matches!(reused.result, DeviceResult::ScreenGeometry { .. }));

    let audit = handle.resolution_records().await;
    assert_eq!(
        audit.last().map(|record| record.operation_id.as_str()),
        Some(operation_id.as_str())
    );

    let _ = shutdown_tx.send(true);
    let _ = tokio::time::timeout(Duration::from_secs(5), agent_task).await;
    server.abort();
    let _ = std::fs::remove_dir_all(fixture_dir);
    Ok(())
}
