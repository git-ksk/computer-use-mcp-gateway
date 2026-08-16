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
use std::{
    path::{Path, PathBuf},
    time::Duration,
};
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

fn prepare_textedit_fixture(path: &Path) -> Result<()> {
    std::fs::write(path, b"V2 P1 real-Cua restart fixture\n")?;
    Ok(())
}

fn hub_config(state_dir: PathBuf) -> HubServiceConfig {
    HubServiceConfig {
        state_dir,
        heartbeat_timeout: Duration::from_secs(2),
        max_agent_session_lifetime: Duration::from_secs(60 * 60),
        agent_session_reauth_drain: Duration::from_secs(30),
        checkpoint_generation_rollover_bytes: 512 * 1024,
        max_queued_per_device: 1,
        max_agent_sessions: 2,
        max_agent_session_starts_per_minute: 30,
    }
}

fn hub_material(
    hub_identity: &HubIdentity,
    grant_authority: &GrantAuthority,
    device_identity: &DeviceIdentity,
) -> HubProvisionedMaterial {
    HubProvisionedMaterial {
        hub_identity: hub_identity.clone(),
        grant_signer: grant_authority.clone().into(),
        device_verifier: device_identity.verifying_key(),
        device_rotation: None,
    }
}

fn agent_config(
    endpoint: String,
    device_id: String,
    cwd: PathBuf,
    state_dir: PathBuf,
    cua: &Path,
) -> AgentServiceConfig {
    AgentServiceConfig {
        hub_endpoint: endpoint,
        hub_domain: "localhost".into(),
        device_id,
        allowed_cwd_roots: vec![cwd],
        state_dir,
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
    }
}

fn agent_material(
    device_identity: &DeviceIdentity,
    hub_identity: &HubIdentity,
    grant_authority: &GrantAuthority,
    cert_der: Vec<u8>,
) -> AgentProvisionedMaterial {
    AgentProvisionedMaterial {
        device_identity: device_identity.clone(),
        trusted_hub: hub_identity.verifier(),
        grant_verifier: grant_authority.verifier(),
        additional_grant_verifiers: vec![],
        hub_rotation: None,
        tls_root_der: cert_der,
    }
}

async fn wait_generation(
    handle: &computer_use_mcp_gateway::v2_m1_hub::HubHandle,
    greater_than: u64,
) -> Result<u64> {
    tokio::time::timeout(Duration::from_secs(8), async {
        loop {
            if let Some(generation) = handle.current_generation().await
                && generation > greater_than
            {
                return generation;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .context("real-Cua Agent did not connect with a newer generation")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "main-only trusted macOS/TCC P1 acceptance; run explicitly with CUMG_V2_CUA_CANCEL_E2E_ACK=1"]
async fn real_cua_indeterminate_survives_restart_without_replay_and_requires_resolution()
-> Result<()> {
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

    let root = temp_dir("v2-p1-real-cua");
    let hub_state = root.join("hub");
    let agent_state = root.join("agent");
    std::fs::create_dir_all(&hub_state)?;
    std::fs::create_dir_all(&agent_state)?;
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&hub_state, std::fs::Permissions::from_mode(0o700))?;
    std::fs::set_permissions(&agent_state, std::fs::Permissions::from_mode(0o700))?;
    let fixture = root.join("V2-P1-Cua-Restart-Fixture.txt");
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
        hub_config(hub_state.clone()),
        hub_material(&hub_identity, &grant_authority, &device_identity),
    )?;
    let device_id = hub.device_id().to_owned();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let endpoint = format!("https://localhost:{}", address.port());
    let incoming = tokio_stream::wrappers::TcpListenerStream::new(listener);
    let first_cert_pem = cert_pem.clone();
    let first_key_pem = key_pem.clone();
    let server = tokio::spawn(async move {
        Server::builder()
            .tls_config(
                ServerTlsConfig::new().identity(Identity::from_pem(first_cert_pem, first_key_pem)),
            )?
            .add_service(
                AgentControlServer::new(hub)
                    .max_decoding_message_size(MAX_GRPC_TRANSPORT_MESSAGE_BYTES)
                    .max_encoding_message_size(MAX_GRPC_TRANSPORT_MESSAGE_BYTES),
            )
            .serve_with_incoming(incoming)
            .await
    });

    let mut first_agent = AgentService::new(
        agent_config(
            endpoint,
            device_id.clone(),
            cwd.clone(),
            agent_state.clone(),
            &cua,
        ),
        agent_material(
            &device_identity,
            &hub_identity,
            &grant_authority,
            cert_der.clone(),
        ),
    )?;
    let (first_shutdown_tx, first_shutdown_rx) = watch::channel(false);
    let first_agent_task = tokio::spawn(async move { first_agent.run(first_shutdown_rx).await });
    let initial_generation = wait_generation(&handle, 0).await?;

    let alice = OperationOwner::new("https://issuer.example", "alice")?;
    let bob = OperationOwner::new("https://issuer.example", "bob")?;

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
    assert!(matches!(
        geometry.result,
        DeviceResult::ScreenGeometry { .. }
    ));

    // A long same-point drag gives the physical Cua backend a live state-changing
    // operation with a large cancellation window without materially moving the cursor.
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
    .context("real-Cua cancellation timed out")??;
    assert_eq!(
        disposition,
        CancellationDisposition::IndeterminateAfterPropagation
    );
    let pending_result = tokio::time::timeout(Duration::from_secs(5), pending.wait())
        .await
        .context("indeterminate operation did not close")?;
    assert!(matches!(
        pending_result,
        Err(HubCommandError::DeviceIndeterminate { operation_id: ref blocked })
            if blocked == &operation_id
    ));

    let quarantine = handle
        .desktop_quarantine()
        .await
        .ok_or_else(|| anyhow!("real-Cua desktop quarantine missing"))?;
    assert_eq!(quarantine.operation_id, operation_id);
    assert_eq!(quarantine.owner, alice);
    assert_eq!(quarantine.device_generation, initial_generation);
    assert!(handle.operation_receipt(&operation_id).await.is_none());

    // Stop both ends only after the ambiguity is durable. The replacement Hub
    // must restore the exact quarantine before a replacement Agent can reconnect.
    let _ = first_shutdown_tx.send(true);
    let first_agent_result = tokio::time::timeout(Duration::from_secs(8), first_agent_task)
        .await
        .context("first real-Cua Agent did not shut down")?
        .context("first real-Cua Agent task join failed")?;
    first_agent_result?;
    server.abort();
    let _ = server.await;
    drop(handle);

    let (restarted_hub, handle) = SingleDeviceHub::new(
        hub_config(hub_state),
        hub_material(&hub_identity, &grant_authority, &device_identity),
    )?;
    assert_eq!(restarted_hub.device_id(), device_id);
    let restored = handle
        .desktop_quarantine()
        .await
        .ok_or_else(|| anyhow!("Hub restart forgot real-Cua quarantine"))?;
    assert_eq!(restored.operation_id, operation_id);
    assert_eq!(restored.owner, alice);
    assert_eq!(restored.device_generation, initial_generation);
    assert!(handle.operation_receipt(&operation_id).await.is_none());

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let restarted_endpoint = format!("https://localhost:{}", address.port());
    let incoming = tokio_stream::wrappers::TcpListenerStream::new(listener);
    let restarted_server = tokio::spawn(async move {
        Server::builder()
            .tls_config(ServerTlsConfig::new().identity(Identity::from_pem(cert_pem, key_pem)))?
            .add_service(
                AgentControlServer::new(restarted_hub)
                    .max_decoding_message_size(MAX_GRPC_TRANSPORT_MESSAGE_BYTES)
                    .max_encoding_message_size(MAX_GRPC_TRANSPORT_MESSAGE_BYTES),
            )
            .serve_with_incoming(incoming)
            .await
    });

    let mut second_agent = AgentService::new(
        agent_config(
            restarted_endpoint,
            device_id,
            cwd.clone(),
            agent_state,
            &cua,
        ),
        agent_material(&device_identity, &hub_identity, &grant_authority, cert_der),
    )?;
    let (second_shutdown_tx, second_shutdown_rx) = watch::channel(false);
    let second_agent_task = tokio::spawn(async move { second_agent.run(second_shutdown_rx).await });
    let new_generation = wait_generation(&handle, initial_generation).await?;
    assert!(new_generation > initial_generation);

    // Reconnect is liveness only: it cannot settle, clear, or dispatch the old
    // ambiguous operation. The unchanged exact quarantine and absent terminal
    // receipt are the authoritative no-auto-replay evidence on this physical lane.
    tokio::time::sleep(Duration::from_millis(500)).await;
    let after_reconnect = handle
        .desktop_quarantine()
        .await
        .ok_or_else(|| anyhow!("Agent reconnect cleared real-Cua quarantine"))?;
    assert_eq!(after_reconnect.operation_id, operation_id);
    assert_eq!(after_reconnect.owner, alice);
    assert_eq!(after_reconnect.device_generation, initial_generation);
    assert!(handle.operation_receipt(&operation_id).await.is_none());

    let blocked = handle
        .start_command_as(bob.clone(), DeviceCommand::ScreenGeometry)
        .await?
        .wait()
        .await;
    assert!(matches!(
        blocked,
        Err(HubCommandError::DeviceIndeterminate { operation_id: ref blocked_id })
            if blocked_id == &operation_id
    ));

    let resolution = handle
        .resolve_indeterminate(
            &operation_id,
            alice.clone(),
            IndeterminateResolution::ConfirmedCompleted,
            "P1 real-Cua acceptance explicitly reconciled the TextEdit desktop after Hub/Agent restart",
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
    assert_ne!(reused.operation_id, operation_id);

    let audit = handle.resolution_records().await;
    assert_eq!(
        audit.last().map(|record| record.operation_id.as_str()),
        Some(operation_id.as_str())
    );

    let _ = second_shutdown_tx.send(true);
    let _ = tokio::time::timeout(Duration::from_secs(8), second_agent_task).await;
    restarted_server.abort();
    let _ = restarted_server.await;
    let _ = std::fs::remove_dir_all(root);
    Ok(())
}
