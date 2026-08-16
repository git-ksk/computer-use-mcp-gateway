#![cfg(unix)]

use anyhow::{Context, Result};
use computer_use_mcp_gateway::{
    v2_execution_safety::{IndeterminateReason, OperationOwner},
    v2_m0::{DeviceCommand, DeviceIdentity, DeviceResult, GrantAuthority, ShellRequest},
    v2_m0_execution::IndeterminateResolution,
    v2_m0_transport::HubIdentity,
    v2_m1::ReconnectPolicy,
    v2_m1_agent::{AgentService, AgentServiceConfig},
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

// Partition is created by explicitly aborting the transport. Do not make this
// recovery/quarantine test depend on hosted-runner sub-second scheduling.
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

async fn wait_generation(
    handle: &computer_use_mcp_gateway::v2_m1_hub::HubHandle,
    greater_than: u64,
) -> Result<u64> {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Some(generation) = handle.current_generation().await
                && generation > greater_than
            {
                return generation;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .context("Agent did not connect with a newer generation")
}

fn agent_config(
    endpoint: String,
    device_id: String,
    cwd: PathBuf,
    state_dir: PathBuf,
) -> AgentServiceConfig {
    AgentServiceConfig {
        hub_endpoint: endpoint,
        hub_domain: "localhost".into(),
        device_id,
        allowed_cwd_roots: vec![cwd],
        state_dir,
        heartbeat_interval: Duration::from_millis(50),
        reconnect: ReconnectPolicy {
            initial_delay: Duration::from_millis(10),
            max_delay: Duration::from_millis(100),
            max_attempts: 10,
        },
        cua: None,
    }
}

fn agent_material(
    device_identity: DeviceIdentity,
    hub_identity: &HubIdentity,
    grant_authority: &GrantAuthority,
    cert_der: Vec<u8>,
) -> AgentProvisionedMaterial {
    AgentProvisionedMaterial {
        device_identity,
        trusted_hub: hub_identity.verifier(),
        grant_verifier: grant_authority.verifier(),
        additional_grant_verifiers: vec![],
        hub_rotation: None,
        tls_root_der: cert_der,
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn partition_after_dispatch_quarantines_across_agent_restart_and_fences_competing_principal()
-> Result<()> {
    let cwd = std::env::current_dir()?;
    let root = temp_dir("partition-recovery");
    let hub_state = root.join("hub");
    let agent_state = root.join("agent");
    std::fs::create_dir_all(&hub_state)?;
    std::fs::create_dir_all(&agent_state)?;
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&hub_state, std::fs::Permissions::from_mode(0o700))?;
    std::fs::set_permissions(&agent_state, std::fs::Permissions::from_mode(0o700))?;
    let effect_marker = root.join("ambiguous-effect");
    let started_marker = root.join("worker-started");

    let CertifiedKey { cert, signing_key } = generate_simple_self_signed(vec!["localhost".into()])?;
    let cert_pem = cert.pem();
    let cert_der = cert.der().to_vec();
    let key_pem = signing_key.serialize_pem();
    let device_identity = DeviceIdentity::generate();
    let hub_identity = HubIdentity::generate();
    let grant_authority = GrantAuthority::generate();

    let (hub, handle) = SingleDeviceHub::new(
        HubServiceConfig {
            state_dir: hub_state.clone(),
            heartbeat_timeout: E2E_HEARTBEAT_TIMEOUT,
            max_agent_session_lifetime: Duration::from_secs(60 * 60),
            agent_session_reauth_drain: Duration::from_secs(30),
            checkpoint_generation_rollover_bytes: 512 * 1024,
            max_queued_per_device: 4,
            max_agent_sessions: 2,
            max_agent_session_starts_per_minute: 60,
        },
        HubProvisionedMaterial {
            hub_identity: hub_identity.clone(),
            grant_signer: grant_authority.clone().into(),
            device_verifier: device_identity.verifying_key(),
            device_rotation: None,
        },
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
            endpoint.clone(),
            device_id.clone(),
            cwd.clone(),
            agent_state.clone(),
        ),
        agent_material(
            device_identity.clone(),
            &hub_identity,
            &grant_authority,
            cert_der.clone(),
        ),
    )?;
    let (_first_shutdown_tx, first_shutdown_rx) = watch::channel(false);
    let first_task = tokio::spawn(async move { first_agent.run(first_shutdown_rx).await });
    let initial_generation = wait_generation(&handle, 0).await?;

    let alice = OperationOwner::new("https://issuer.example", "alice")?;
    let bob = OperationOwner::new("https://issuer.example", "bob")?;
    let command = format!(
        "printf worker-started > '{}'; sleep 1; printf ambiguous-effect > '{}'",
        started_marker.display(),
        effect_marker.display()
    );
    let pending = handle
        .start_command_as(
            alice.clone(),
            DeviceCommand::Shell {
                request: ShellRequest {
                    command,
                    cwd: cwd.to_string_lossy().into_owned(),
                    env: vec![],
                    timeout_ms: 5_000,
                },
            },
        )
        .await?;
    let operation_id = pending.operation_id.clone();
    tokio::time::timeout(Duration::from_secs(3), async {
        while !started_marker.is_file() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .context("Agent local worker did not start before partition")?;

    // Drop the Agent network/runtime future only after the exact local worker has
    // started. Hub dispatch admission alone does not prove spawn has occurred.
    // The already-spawned local worker can still complete, modeling a
    // partition where the side effect and result delivery race independently.
    first_task.abort();
    let _ = first_task.await;

    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if handle
                .desktop_quarantine()
                .await
                .is_some_and(|q| q.operation_id == operation_id)
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .context("Hub did not quarantine partitioned operation")?;
    let pending_result = tokio::time::timeout(Duration::from_secs(2), pending.wait())
        .await
        .context("partitioned pending operation did not close")?;
    assert_eq!(pending_result, Err(HubCommandError::SessionClosed));
    let quarantine = handle.desktop_quarantine().await.unwrap();
    assert_eq!(quarantine.operation_id, operation_id);
    assert_eq!(quarantine.owner, alice);
    assert_eq!(quarantine.reason, IndeterminateReason::ConnectionLost);

    // The local effect can finish after transport loss. This is precisely why
    // reconnect cannot infer success/failure from liveness.
    tokio::time::timeout(Duration::from_secs(6), async {
        while !effect_marker.is_file() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .context("modeled side effect did not complete after partition")?;

    // Crash/restart the Hub itself after quarantine is durable. The replacement
    // process must reconstruct the exact owner/generation quarantine from the
    // checkpoint before any Agent reconnects.
    server.abort();
    let _ = server.await;
    drop(handle);

    let (restarted_hub, handle) = SingleDeviceHub::new(
        HubServiceConfig {
            state_dir: hub_state.clone(),
            heartbeat_timeout: E2E_HEARTBEAT_TIMEOUT,
            max_agent_session_lifetime: Duration::from_secs(60 * 60),
            agent_session_reauth_drain: Duration::from_secs(30),
            checkpoint_generation_rollover_bytes: 512 * 1024,
            max_queued_per_device: 4,
            max_agent_sessions: 2,
            max_agent_session_starts_per_minute: 60,
        },
        HubProvisionedMaterial {
            hub_identity: hub_identity.clone(),
            grant_signer: grant_authority.clone().into(),
            device_verifier: device_identity.verifying_key(),
            device_rotation: None,
        },
    )?;
    assert_eq!(restarted_hub.device_id(), device_id);
    let restored_quarantine = handle
        .desktop_quarantine()
        .await
        .ok_or_else(|| anyhow::anyhow!("Hub restart forgot desktop quarantine"))?;
    assert_eq!(restored_quarantine.operation_id, operation_id);
    assert_eq!(restored_quarantine.owner, alice);
    assert_eq!(restored_quarantine.device_generation, initial_generation);

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
        agent_config(restarted_endpoint, device_id, cwd.clone(), agent_state),
        agent_material(device_identity, &hub_identity, &grant_authority, cert_der),
    )?;
    let (second_shutdown_tx, second_shutdown_rx) = watch::channel(false);
    let second_task = tokio::spawn(async move { second_agent.run(second_shutdown_rx).await });
    let new_generation = wait_generation(&handle, initial_generation).await?;
    assert!(new_generation > initial_generation);
    let still_quarantined = handle.desktop_quarantine().await.unwrap();
    assert_eq!(still_quarantined.operation_id, operation_id);
    assert_eq!(still_quarantined.owner, alice);

    let blocked = handle
        .start_command_as(
            bob.clone(),
            DeviceCommand::Shell {
                request: ShellRequest {
                    command: "printf should-not-run".into(),
                    cwd: cwd.to_string_lossy().into_owned(),
                    env: vec![],
                    timeout_ms: 2_000,
                },
            },
        )
        .await?
        .wait()
        .await;
    assert!(matches!(
        blocked,
        Err(HubCommandError::DeviceIndeterminate { operation_id: ref blocked_id }) if blocked_id == &operation_id
    ));

    handle
        .resolve_indeterminate(
            &operation_id,
            alice,
            IndeterminateResolution::ConfirmedCompleted,
            "integration fixture observed the post-partition side-effect marker",
        )
        .await?;
    let reused_shell = handle
        .start_command_as(
            bob,
            DeviceCommand::Shell {
                request: ShellRequest {
                    command: "printf reused".into(),
                    cwd: cwd.to_string_lossy().into_owned(),
                    env: vec![],
                    timeout_ms: 2_000,
                },
            },
        )
        .await?
        .wait()
        .await?;
    assert!(matches!(reused_shell.result, DeviceResult::Shell { .. }));

    let _ = second_shutdown_tx.send(true);
    let _ = tokio::time::timeout(Duration::from_secs(3), second_task).await;
    restarted_server.abort();
    let _ = restarted_server.await;
    let _ = std::fs::remove_dir_all(root);
    Ok(())
}
