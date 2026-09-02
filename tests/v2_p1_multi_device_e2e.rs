#![cfg(unix)]

use anyhow::{Context, Result, anyhow};
use computer_use_mcp_gateway::{
    v2_execution_safety::{ExecutionEvidence, OperationOwner},
    v2_m0::{
        DeviceCommand, DeviceIdentity, DeviceRegistry, DeviceResult, GrantAuthority, ShellRequest,
    },
    v2_m0_execution::IndeterminateResolution,
    v2_m0_transport::{CancellationDisposition, HubIdentity},
    v2_m1::ReconnectPolicy,
    v2_m1_agent::{AgentService, AgentServiceConfig, CuaAgentConfig},
    v2_m1_grpc::{
        MAX_GRPC_TRANSPORT_MESSAGE_BYTES, proto::agent_control_server::AgentControlServer,
    },
    v2_m1_hub::{HubCommandError, HubHandle, HubProvisionedMaterial, HubServiceConfig},
    v2_m1_keys::AgentProvisionedMaterial,
    v2_multi_device::FixedMultiDeviceHub,
};
use rcgen::{CertifiedKey, generate_simple_self_signed};
use std::{path::PathBuf, time::Duration};
use tokio::sync::watch;
use tonic::transport::{Identity, Server, ServerTlsConfig};

// This E2E proves ownership/quarantine/restart isolation, not heartbeat timeout
// precision. Keep scheduling slack well above the 50 ms Agent heartbeat so a
// briefly descheduled hosted runner cannot turn the fixture into SessionClosed.
const E2E_EVENTUALLY: Duration = Duration::from_secs(10);
const E2E_HEARTBEAT_TIMEOUT: Duration = Duration::from_secs(5);

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

fn ensure_private_dir(path: &std::path::Path) -> Result<()> {
    std::fs::create_dir_all(path)?;
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?;
    Ok(())
}

fn stable_device_id(identity: &DeviceIdentity) -> String {
    let mut registry = DeviceRegistry::default();
    registry.provision_trusted_device(identity.verifying_key())
}

fn hub_config(state_dir: PathBuf) -> HubServiceConfig {
    HubServiceConfig {
        state_dir,
        heartbeat_timeout: E2E_HEARTBEAT_TIMEOUT,
        max_agent_session_lifetime: Duration::from_secs(60 * 60),
        agent_session_reauth_drain: Duration::from_secs(30),
        checkpoint_generation_rollover_bytes: 512 * 1024,
        max_queued_per_device: 4,
        max_agent_sessions: 2,
        max_agent_session_starts_per_minute: 120,
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

fn agent_material(
    identity: DeviceIdentity,
    hub_identity: &HubIdentity,
    grants: &GrantAuthority,
    cert_der: Vec<u8>,
) -> AgentProvisionedMaterial {
    AgentProvisionedMaterial {
        device_identity: identity,
        trusted_hub: hub_identity.verifier(),
        grant_verifier: grants.verifier(),
        additional_grant_verifiers: vec![],
        hub_rotation: None,
        tls_root_der: cert_der,
    }
}

fn agent_config(
    endpoint: String,
    device_id: String,
    roots: Vec<PathBuf>,
    state_dir: PathBuf,
    cua: Option<CuaAgentConfig>,
) -> AgentServiceConfig {
    AgentServiceConfig {
        hub_endpoint: endpoint,
        hub_domain: "localhost".into(),
        device_id,
        allowed_file_roots: roots.clone(),
        allowed_cwd_roots: roots,
        state_dir,
        // Recovery semantics are the subject of this E2E, not a 150 ms ACK deadline.
        // Keep the Agent cadence above hosted-runner/fsync scheduling jitter without
        // changing production heartbeat semantics.
        heartbeat_interval: Duration::from_millis(500),
        reconnect: ReconnectPolicy {
            initial_delay: Duration::from_millis(10),
            max_delay: Duration::from_millis(100),
            max_attempts: 100,
        },
        cua,
    }
}

async fn wait_online(handle: &HubHandle) -> Result<u64> {
    tokio::time::timeout(E2E_EVENTUALLY, async {
        loop {
            if let Some(generation) = handle.current_generation().await {
                return generation;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .context("Agent did not become online")
}

async fn wait_new_generation(handle: &HubHandle, old: u64) -> Result<u64> {
    tokio::time::timeout(E2E_EVENTUALLY, async {
        loop {
            if let Some(generation) = handle.current_generation().await
                && generation > old
            {
                return generation;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .context("Agent did not advance generation")
}

async fn wait_file(path: &std::path::Path) -> Result<()> {
    tokio::time::timeout(E2E_EVENTUALLY, async {
        while !path.is_file() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .with_context(|| format!("marker was not created: {}", path.display()))?;
    Ok(())
}

fn line_count(path: &std::path::Path) -> Result<usize> {
    Ok(std::fs::read_to_string(path)?.lines().count())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
async fn multi_device_quarantine_partition_restart_and_no_replay() -> Result<()> {
    let cwd = std::env::current_dir()?;
    let root = temp_dir("p1-multi-device");
    let hub_a_state = root.join("hub-a");
    let hub_b_state = root.join("hub-b");
    let agent_a_state = root.join("agent-a");
    let agent_b_state = root.join("agent-b");
    for path in [&hub_a_state, &hub_b_state, &agent_a_state, &agent_b_state] {
        ensure_private_dir(path)?;
    }

    let drag_marker = root.join("a-drag-calls");
    let cancel_marker = root.join("a-drag-cancels");
    let b_during_a_marker = root.join("b-during-a");
    let b_after_restart_marker = root.join("b-after-restart");
    let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("scripts/mock_cua_mcp_backend.py");

    let CertifiedKey { cert, signing_key } = generate_simple_self_signed(vec!["localhost".into()])?;
    let cert_pem = cert.pem();
    let cert_der = cert.der().to_vec();
    let key_pem = signing_key.serialize_pem();

    let device_a_identity = DeviceIdentity::generate();
    let device_b_identity = DeviceIdentity::generate();
    let device_a_id = stable_device_id(&device_a_identity);
    let device_b_id = stable_device_id(&device_b_identity);
    assert_ne!(device_a_id, device_b_id);
    let hub_a_identity = HubIdentity::generate();
    let hub_b_identity = HubIdentity::generate();
    let grants_a = GrantAuthority::generate();
    let grants_b = GrantAuthority::generate();

    let make_hub = || {
        FixedMultiDeviceHub::new(vec![
            (
                hub_config(hub_a_state.clone()),
                hub_material(&hub_a_identity, &grants_a, &device_a_identity),
            ),
            (
                hub_config(hub_b_state.clone()),
                hub_material(&hub_b_identity, &grants_b, &device_b_identity),
            ),
        ])
    };

    let mut hub = make_hub()?;
    assert_eq!(hub.provisioned_device_count(), 2);
    assert!(hub.handle_for_device("unknown-device").is_none());
    let mut handle_a = hub
        .handle_for_device(&device_a_id)
        .ok_or_else(|| anyhow!("Device A route missing"))?;
    let mut handle_b = hub
        .handle_for_device(&device_b_id)
        .ok_or_else(|| anyhow!("Device B route missing"))?;
    let service_a = hub
        .service_for_device(&device_a_id)
        .ok_or_else(|| anyhow!("Device A service missing"))?;
    let service_b = hub
        .service_for_device(&device_b_id)
        .ok_or_else(|| anyhow!("Device B service missing"))?;
    assert_eq!(service_a.device_id(), device_a_id);
    assert_eq!(service_b.device_id(), device_b_id);

    let listener_a = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let address_a = listener_a.local_addr()?;
    let endpoint_a = format!("https://localhost:{}", address_a.port());
    let incoming_a = tokio_stream::wrappers::TcpListenerStream::new(listener_a);
    let a_cert = cert_pem.clone();
    let a_key = key_pem.clone();
    let mut server_a = tokio::spawn(async move {
        Server::builder()
            .tls_config(ServerTlsConfig::new().identity(Identity::from_pem(a_cert, a_key)))?
            .add_service(
                AgentControlServer::new(service_a)
                    .max_decoding_message_size(MAX_GRPC_TRANSPORT_MESSAGE_BYTES)
                    .max_encoding_message_size(MAX_GRPC_TRANSPORT_MESSAGE_BYTES),
            )
            .serve_with_incoming(incoming_a)
            .await
    });

    let listener_b = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let address_b = listener_b.local_addr()?;
    let endpoint_b = format!("https://localhost:{}", address_b.port());
    let incoming_b = tokio_stream::wrappers::TcpListenerStream::new(listener_b);
    let b_cert = cert_pem.clone();
    let b_key = key_pem.clone();
    let mut server_b = tokio::spawn(async move {
        Server::builder()
            .tls_config(ServerTlsConfig::new().identity(Identity::from_pem(b_cert, b_key)))?
            .add_service(
                AgentControlServer::new(service_b)
                    .max_decoding_message_size(MAX_GRPC_TRANSPORT_MESSAGE_BYTES)
                    .max_encoding_message_size(MAX_GRPC_TRANSPORT_MESSAGE_BYTES),
            )
            .serve_with_incoming(incoming_b)
            .await
    });

    let cua_config = CuaAgentConfig {
        command: "python3".into(),
        args: vec![
            fixture.to_string_lossy().into_owned(),
            "--drag-marker".into(),
            drag_marker.to_string_lossy().into_owned(),
            "--cancel-marker".into(),
            cancel_marker.to_string_lossy().into_owned(),
        ],
        backend_version: "1.0.0".into(),
        platform: "test".into(),
        revision: 1,
        connect_timeout: Duration::from_secs(3),
        tool_timeout: Duration::from_secs(30),
        reconnect_attempts: 3,
        reconnect_backoff: Duration::from_millis(20),
        mutation_authority_dir: None,
    };
    let config_a = agent_config(
        endpoint_a,
        device_a_id.clone(),
        vec![cwd.clone(), root.clone()],
        agent_a_state,
        Some(cua_config),
    );
    let config_b = agent_config(
        endpoint_b,
        device_b_id.clone(),
        vec![cwd.clone(), root.clone()],
        agent_b_state,
        None,
    );
    let material_a = agent_material(
        device_a_identity.clone(),
        &hub_a_identity,
        &grants_a,
        cert_der.clone(),
    );
    let material_b = agent_material(
        device_b_identity.clone(),
        &hub_b_identity,
        &grants_b,
        cert_der,
    );

    let mut agent_a = AgentService::new(config_a.clone(), material_a.clone())?;
    let mut agent_b = AgentService::new(config_b.clone(), material_b.clone())?;
    let (a_shutdown_tx, a_shutdown_rx) = watch::channel(false);
    let (b_shutdown_tx, b_shutdown_rx) = watch::channel(false);
    let mut a_task = tokio::spawn(async move { agent_a.run(a_shutdown_rx).await });
    let b_task = tokio::spawn(async move { agent_b.run(b_shutdown_rx).await });
    let a_generation = wait_online(&handle_a).await?;
    let b_generation = wait_online(&handle_b).await?;

    let alice = OperationOwner::new("https://issuer.example", "alice")?;
    let bob = OperationOwner::new("https://issuer.example", "bob")?;

    let pending_a = handle_a
        .start_command_as(
            alice.clone(),
            DeviceCommand::PointerDrag {
                from_x: 10,
                from_y: 10,
                to_x: 30,
                to_y: 30,
                duration_ms: 5_000,
            },
        )
        .await?;
    let ambiguous_a = pending_a.operation_id.clone();
    wait_file(&drag_marker).await?;

    let pending_b = handle_b
        .start_command_as(
            bob.clone(),
            DeviceCommand::Shell {
                request: ShellRequest {
                    command: format!(
                        "sleep 0.25; printf b-ok > '{}'",
                        b_during_a_marker.display()
                    ),
                    cwd: cwd.to_string_lossy().into_owned(),
                    env: vec![],
                    timeout_ms: 3_000,
                },
            },
        )
        .await?;

    assert!(matches!(
        handle_a.cancel_as(bob.clone(), ambiguous_a.clone()).await,
        Err(HubCommandError::Rejected)
    ));
    assert_eq!(
        handle_a
            .cancel_as(alice.clone(), ambiguous_a.clone())
            .await?,
        CancellationDisposition::IndeterminateAfterPropagation
    );
    assert!(matches!(
        pending_a.wait().await,
        Err(HubCommandError::DeviceIndeterminate { operation_id }) if operation_id == ambiguous_a
    ));
    wait_file(&cancel_marker).await?;
    let b_result = pending_b.wait().await?;
    assert!(matches!(b_result.result, DeviceResult::Shell { .. }));
    assert!(b_during_a_marker.is_file());

    let quarantine_a = handle_a
        .desktop_quarantine()
        .await
        .ok_or_else(|| anyhow!("Device A quarantine missing"))?;
    assert_eq!(quarantine_a.operation_id, ambiguous_a);
    assert_eq!(quarantine_a.owner, alice);
    assert_eq!(quarantine_a.device_generation, a_generation);
    assert!(handle_b.desktop_quarantine().await.is_none());
    assert_eq!(line_count(&drag_marker)?, 1);
    assert_eq!(line_count(&cancel_marker)?, 1);

    let recovery_read = handle_a
        .start_command_as(bob.clone(), DeviceCommand::ScreenGeometry)
        .await?
        .wait()
        .await?;
    assert!(matches!(
        recovery_read.result,
        DeviceResult::ScreenGeometry { .. }
    ));
    assert_eq!(
        handle_a
            .desktop_quarantine()
            .await
            .expect("recovery read must preserve Device A quarantine")
            .operation_id,
        ambiguous_a
    );
    assert!(matches!(
        handle_a
            .start_command_as(
                bob.clone(),
                DeviceCommand::Shell {
                    request: ShellRequest {
                        command: "printf must-not-dispatch".into(),
                        cwd: cwd.to_string_lossy().into_owned(),
                        env: vec![],
                        timeout_ms: 2_000,
                    },
                },
            )
            .await?
            .wait()
            .await,
        Err(HubCommandError::DeviceIndeterminate { operation_id }) if operation_id == ambiguous_a
    ));
    handle_b
        .start_command_as(
            bob.clone(),
            DeviceCommand::Shell {
                request: ShellRequest {
                    command: "printf b-still-usable".into(),
                    cwd: cwd.to_string_lossy().into_owned(),
                    env: vec![],
                    timeout_ms: 2_000,
                },
            },
        )
        .await?
        .wait()
        .await?;

    // Reconnect only A. B keeps its exact generation and remains usable.
    a_shutdown_tx.send(true)?;
    a_task.await??;
    let mut replacement_a = AgentService::new(config_a.clone(), material_a.clone())?;
    let (a2_shutdown_tx, a2_shutdown_rx) = watch::channel(false);
    a_task = tokio::spawn(async move { replacement_a.run(a2_shutdown_rx).await });
    let a_generation_after_reconnect = wait_new_generation(&handle_a, a_generation)
        .await
        .context("Device A isolated reconnect did not advance generation")?;
    assert_eq!(handle_b.current_generation().await, Some(b_generation));
    assert_eq!(line_count(&drag_marker)?, 1);
    assert_eq!(
        handle_a.desktop_quarantine().await.map(|q| q.operation_id),
        Some(ambiguous_a.clone())
    );

    // Partition A's route while B's service remains up. A's Agent retries only
    // its own fixed endpoint; B must remain normal.
    server_a.abort();
    let _ = server_a.await;
    tokio::time::sleep(Duration::from_millis(150)).await;
    handle_b
        .start_command_as(
            bob.clone(),
            DeviceCommand::Shell {
                request: ShellRequest {
                    command: "printf b-during-a-partition".into(),
                    cwd: cwd.to_string_lossy().into_owned(),
                    env: vec![],
                    timeout_ms: 2_000,
                },
            },
        )
        .await?
        .wait()
        .await?;
    assert_eq!(
        handle_a
            .desktop_quarantine()
            .await
            .map(|q| (q.operation_id, q.owner, q.device_generation)),
        Some((ambiguous_a.clone(), alice.clone(), a_generation))
    );

    // The partition invariant is now proven. Stop this Agent process cleanly
    // before the Hub crash. The restart phase then proves recovery from the
    // same durable Agent checkpoint independently of tonic abort classification.
    let _ = a2_shutdown_tx.send(true);
    let _ = a_task.await;

    // Crash the remaining Hub route, then reconstruct the fixed set from the two
    // independent P0 checkpoints before allowing either Agent to reconnect.
    server_b.abort();
    let _ = server_b.await;
    let _ = b_shutdown_tx.send(true);
    let _ = b_task.await;
    drop(handle_a);
    drop(handle_b);
    drop(hub);
    tokio::time::sleep(Duration::from_millis(80)).await;

    hub = make_hub()?;
    handle_a = hub
        .handle_for_device(&device_a_id)
        .ok_or_else(|| anyhow!("restored Device A route missing"))?;
    handle_b = hub
        .handle_for_device(&device_b_id)
        .ok_or_else(|| anyhow!("restored Device B route missing"))?;
    let restored_a = handle_a
        .desktop_quarantine()
        .await
        .ok_or_else(|| anyhow!("Hub restart forgot Device A quarantine"))?;
    assert_eq!(restored_a.operation_id, ambiguous_a);
    assert_eq!(restored_a.owner, alice);
    assert_eq!(restored_a.device_generation, a_generation);
    assert!(handle_b.desktop_quarantine().await.is_none());

    let service_a = hub
        .service_for_device(&device_a_id)
        .ok_or_else(|| anyhow!("restored Device A service missing"))?;
    let service_b = hub
        .service_for_device(&device_b_id)
        .ok_or_else(|| anyhow!("restored Device B service missing"))?;

    let listener_a = tokio::net::TcpListener::bind(address_a).await?;
    let incoming_a = tokio_stream::wrappers::TcpListenerStream::new(listener_a);
    let a_cert = cert_pem.clone();
    let a_key = key_pem.clone();
    server_a = tokio::spawn(async move {
        Server::builder()
            .tls_config(ServerTlsConfig::new().identity(Identity::from_pem(a_cert, a_key)))?
            .add_service(
                AgentControlServer::new(service_a)
                    .max_decoding_message_size(MAX_GRPC_TRANSPORT_MESSAGE_BYTES)
                    .max_encoding_message_size(MAX_GRPC_TRANSPORT_MESSAGE_BYTES),
            )
            .serve_with_incoming(incoming_a)
            .await
    });

    let listener_b = tokio::net::TcpListener::bind(address_b).await?;
    let incoming_b = tokio_stream::wrappers::TcpListenerStream::new(listener_b);
    let b_cert = cert_pem;
    let b_key = key_pem;
    server_b = tokio::spawn(async move {
        Server::builder()
            .tls_config(ServerTlsConfig::new().identity(Identity::from_pem(b_cert, b_key)))?
            .add_service(
                AgentControlServer::new(service_b)
                    .max_decoding_message_size(MAX_GRPC_TRANSPORT_MESSAGE_BYTES)
                    .max_encoding_message_size(MAX_GRPC_TRANSPORT_MESSAGE_BYTES),
            )
            .serve_with_incoming(incoming_b)
            .await
    });

    // Recreate A from its durable Agent checkpoint after the Hub is back. The
    // earlier phase already proved isolated reconnect while B kept its generation.
    let mut restarted_agent_a = AgentService::new(config_a.clone(), material_a.clone())?;
    let (a3_shutdown_tx, a3_shutdown_rx) = watch::channel(false);
    a_task = tokio::spawn(async move { restarted_agent_a.run(a3_shutdown_rx).await });
    let mut restarted_agent_b = AgentService::new(config_b.clone(), material_b.clone())?;
    let (b2_shutdown_tx, b2_shutdown_rx) = watch::channel(false);
    let b_task = tokio::spawn(async move { restarted_agent_b.run(b2_shutdown_rx).await });

    let a_generation_after_hub_restart =
        wait_new_generation(&handle_a, a_generation_after_reconnect)
            .await
            .context("Device A did not reconnect after Hub restart")?;
    let b_generation_after_hub_restart = wait_new_generation(&handle_b, b_generation)
        .await
        .context("Device B did not reconnect after Hub restart")?;
    assert!(a_generation_after_hub_restart > a_generation_after_reconnect);
    assert!(b_generation_after_hub_restart > b_generation);
    assert_eq!(line_count(&drag_marker)?, 1);
    assert_eq!(line_count(&cancel_marker)?, 1);
    assert_eq!(
        handle_a.desktop_quarantine().await.map(|q| q.operation_id),
        Some(ambiguous_a.clone())
    );
    assert!(handle_b.desktop_quarantine().await.is_none());

    handle_b
        .start_command_as(
            bob,
            DeviceCommand::Shell {
                request: ShellRequest {
                    command: format!(
                        "printf b-after-restart > '{}'",
                        b_after_restart_marker.display()
                    ),
                    cwd: cwd.to_string_lossy().into_owned(),
                    env: vec![],
                    timeout_ms: 2_000,
                },
            },
        )
        .await?
        .wait()
        .await?;
    assert!(b_after_restart_marker.is_file());

    handle_a
        .resolve_indeterminate(
            &ambiguous_a,
            alice.clone(),
            IndeterminateResolution::ConfirmedCompleted,
            "P1 E2E reconciled the exact ambiguous Cua action after restart",
        )
        .await?;
    assert!(handle_a.desktop_quarantine().await.is_none());
    let old_receipt = handle_a
        .operation_receipt(&ambiguous_a)
        .await
        .ok_or_else(|| anyhow!("resolved old operation receipt missing"))?;
    assert_eq!(old_receipt.evidence, ExecutionEvidence::OperatorResolution);
    let reused_a = handle_a
        .start_command_as(alice, DeviceCommand::ScreenGeometry)
        .await?
        .wait()
        .await?;
    assert!(matches!(
        reused_a.result,
        DeviceResult::ScreenGeometry { .. }
    ));
    assert_ne!(reused_a.operation_id, ambiguous_a);
    assert_eq!(line_count(&drag_marker)?, 1);
    assert_eq!(line_count(&cancel_marker)?, 1);
    assert_eq!(handle_a.resolution_records().await.len(), 1);

    a3_shutdown_tx.send(true)?;
    b2_shutdown_tx.send(true)?;
    let _ = a_task.await?;
    let _ = b_task.await?;
    server_a.abort();
    server_b.abort();
    let _ = server_a.await;
    let _ = server_b.await;
    let _ = std::fs::remove_dir_all(root);
    Ok(())
}
