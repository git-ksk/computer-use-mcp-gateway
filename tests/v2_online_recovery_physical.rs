#![cfg(target_os = "macos")]

use anyhow::{Context, Result, anyhow};
use computer_use_mcp_gateway::{
    v2_execution_safety::{ExecutionEvidence, OperationOwner},
    v2_m0::{DeviceCommand, DeviceIdentity, DeviceResult, GrantAuthority},
    v2_m0_execution::HubOperationState,
    v2_m0_transport::{CancellationDisposition, HubIdentity},
    v2_m1::ReconnectPolicy,
    v2_m1_agent::{AgentService, AgentServiceConfig, CuaAgentConfig},
    v2_m1_grpc::{
        MAX_GRPC_TRANSPORT_MESSAGE_BYTES, proto::agent_control_server::AgentControlServer,
    },
    v2_m1_hub::{HubCommandError, HubProvisionedMaterial, HubServiceConfig, SingleDeviceHub},
    v2_m1_keys::{AgentProvisionedMaterial, write_new_verifying_key},
    v2_online_recovery::{RECOVERY_PUBLIC_KEY_FILENAME, load_challenge},
};
use rcgen::{CertifiedKey, generate_simple_self_signed};
use std::{
    path::{Path, PathBuf},
    time::Duration,
};
use tokio::sync::{oneshot, watch};
use tonic::transport::{Identity, Server, ServerTlsConfig};

fn acceptance_root() -> Result<PathBuf> {
    let root = PathBuf::from(
        std::env::var("CUMG_V2_ONLINE_RECOVERY_ACCEPTANCE_ROOT")
            .context("CUMG_V2_ONLINE_RECOVERY_ACCEPTANCE_ROOT is required")?,
    );
    if !root.is_absolute() || root.exists() {
        return Err(anyhow!(
            "online recovery acceptance root must be a new absolute path"
        ));
    }
    std::fs::create_dir_all(&root)?;
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700))?;
    Ok(root)
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
        heartbeat_interval: Duration::from_millis(250),
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
    .context("physical online-recovery Agent did not connect with a newer generation")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "trusted physical macOS/Cua/Secure Enclave online recovery acceptance; requires explicit ACK, pre-provisioned public key, and external v2_recover resolve"]
async fn physical_secure_enclave_online_recovery_never_replays_ambiguous_cua_operation()
-> Result<()> {
    if std::env::var("CUMG_V2_ONLINE_RECOVERY_E2E_ACK").as_deref() != Ok("1") {
        return Err(anyhow!(
            "refusing physical online recovery without CUMG_V2_ONLINE_RECOVERY_E2E_ACK=1"
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
    let recovery_public_source = PathBuf::from(
        std::env::var("CUMG_V2_ONLINE_RECOVERY_PUBLIC_KEY_FILE")
            .context("CUMG_V2_ONLINE_RECOVERY_PUBLIC_KEY_FILE is required")?,
    );
    let recovery_public = std::fs::read(&recovery_public_source)
        .context("failed to read provisioned recovery public key")?;
    if recovery_public.len() != 65 {
        return Err(anyhow!("recovery public key must be exactly 65 bytes"));
    }

    let root = acceptance_root()?;
    let hub_state = root.join("hub");
    let agent_state = root.join("agent");
    std::fs::create_dir_all(&hub_state)?;
    std::fs::create_dir_all(&agent_state)?;
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&hub_state, std::fs::Permissions::from_mode(0o700))?;
    std::fs::set_permissions(&agent_state, std::fs::Permissions::from_mode(0o700))?;
    let recovery_public_path = hub_state.join(RECOVERY_PUBLIC_KEY_FILENAME);
    std::fs::write(&recovery_public_path, recovery_public)?;
    std::fs::set_permissions(
        &recovery_public_path,
        std::fs::Permissions::from_mode(0o644),
    )?;

    let CertifiedKey { cert, signing_key } = generate_simple_self_signed(vec!["localhost".into()])?;
    let cert_pem = cert.pem();
    let cert_der = cert.der().to_vec();
    let key_pem = signing_key.serialize_pem();
    let device_identity = DeviceIdentity::generate();
    let hub_identity = HubIdentity::generate();
    let grant_authority = GrantAuthority::generate();
    let hub_public_key_file = root.join("hub-public-key.ed25519");
    write_new_verifying_key(&hub_public_key_file, &hub_identity.verifier())?;
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
    let (server_shutdown_tx, server_shutdown_rx) = oneshot::channel::<()>();
    let server = tokio::spawn(async move {
        Server::builder()
            .tls_config(ServerTlsConfig::new().identity(Identity::from_pem(cert_pem, key_pem)))?
            .add_service(
                AgentControlServer::new(hub)
                    .max_decoding_message_size(MAX_GRPC_TRANSPORT_MESSAGE_BYTES)
                    .max_encoding_message_size(MAX_GRPC_TRANSPORT_MESSAGE_BYTES),
            )
            .serve_with_incoming_shutdown(incoming, async move {
                let _ = server_shutdown_rx.await;
            })
            .await
    });

    let mut first_agent = AgentService::new(
        agent_config(
            endpoint.clone(),
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
    let owner = OperationOwner::new("https://issuer.example", "physical-online-recovery")?;

    // A long same-point drag is a real Cua mutation with a large cancellation
    // window while avoiding intentional cursor relocation on the operator desktop.
    let pending = handle
        .start_command_as(
            owner.clone(),
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
        handle.cancel_as(owner.clone(), operation_id.clone()),
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
        .ok_or_else(|| anyhow!("physical online-recovery quarantine missing"))?;
    assert_eq!(quarantine.operation_id, operation_id);
    assert_eq!(quarantine.owner, owner);
    assert_eq!(quarantine.device_generation, initial_generation);

    // Advance only the authenticated Agent generation. The Hub stays running and
    // the durable quarantine must remain authoritative across reconnect.
    let _ = first_shutdown_tx.send(true);
    let first_agent_result = tokio::time::timeout(Duration::from_secs(8), first_agent_task)
        .await
        .context("first physical online-recovery Agent did not shut down")?
        .context("first Agent task join failed")?;
    first_agent_result?;
    let mut second_agent = AgentService::new(
        agent_config(
            endpoint,
            device_id.clone(),
            cwd.clone(),
            agent_state.clone(),
            &cua,
        ),
        agent_material(&device_identity, &hub_identity, &grant_authority, cert_der),
    )?;
    let (second_shutdown_tx, second_shutdown_rx) = watch::channel(false);
    let second_agent_task = tokio::spawn(async move { second_agent.run(second_shutdown_rx).await });
    let current_generation = wait_generation(&handle, initial_generation).await?;
    assert!(current_generation > initial_generation);
    let after_reconnect = handle
        .desktop_quarantine()
        .await
        .ok_or_else(|| anyhow!("Agent reconnect cleared physical quarantine"))?;
    assert_eq!(after_reconnect.operation_id, operation_id);

    let challenge = tokio::time::timeout(Duration::from_secs(8), async {
        loop {
            if let Some(challenge) = load_challenge(&agent_state).map_err(|error| anyhow!(error))?
                && challenge.operation_id == operation_id
                && challenge.current_generation == current_generation
            {
                return Ok::<_, anyhow::Error>(challenge);
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .context("Agent did not publish online recovery challenge")??;
    assert_eq!(challenge.quarantine_generation, initial_generation);

    // This line is intentionally path-only local evidence for the operator. It
    // contains no command/result payload, credential, key, or desktop content.
    println!("ONLINE_RECOVERY_PHYSICAL_READY");
    println!("state_dir={}", agent_state.display());
    println!("hub_public_key_file={}", hub_public_key_file.display());
    println!("operation_id={operation_id}");
    println!("quarantine_generation={initial_generation}");
    println!("current_generation={current_generation}");

    // External `v2_recover resolve` performs the real Secure Enclave
    // user-presence interaction. The isolated stack remains alive while waiting.
    tokio::time::timeout(Duration::from_secs(10 * 60), async {
        loop {
            if handle.desktop_quarantine().await.is_none() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .context("physical online recovery was not approved within ten minutes")?;

    let recovery = handle
        .operation_recovery_as(owner.clone(), &operation_id)
        .await?;
    assert_eq!(recovery.state, HubOperationState::Completed);
    let receipt = handle
        .operation_receipt(&operation_id)
        .await
        .ok_or_else(|| anyhow!("resolved physical operation receipt missing"))?;
    assert_eq!(receipt.evidence, ExecutionEvidence::OperatorResolution);
    assert_eq!(handle.resolution_records().await.len(), 1);

    let fresh = handle
        .start_command_as(owner.clone(), DeviceCommand::ScreenGeometry)
        .await?
        .wait()
        .await?;
    assert!(matches!(fresh.result, DeviceResult::ScreenGeometry { .. }));
    assert_ne!(fresh.operation_id, operation_id);

    let _ = second_shutdown_tx.send(true);
    let second_agent_result = tokio::time::timeout(Duration::from_secs(8), second_agent_task)
        .await
        .context("second physical online-recovery Agent did not shut down")?
        .context("second Agent task join failed")?;
    second_agent_result?;
    let _ = server_shutdown_tx.send(());
    tokio::time::timeout(Duration::from_secs(8), server)
        .await
        .context("physical online-recovery Hub server did not shut down")?
        .context("physical online-recovery Hub server join failed")??;
    drop(handle);

    // Restart the Hub from durable state. Resolution must survive and the old
    // operation remains a terminal tombstone rather than becoming runnable.
    let (_restarted_hub, restarted_handle) = SingleDeviceHub::new(
        hub_config(hub_state),
        hub_material(&hub_identity, &grant_authority, &device_identity),
    )?;
    assert!(restarted_handle.desktop_quarantine().await.is_none());
    let restarted_recovery = restarted_handle
        .operation_recovery_as(owner, &operation_id)
        .await?;
    assert_eq!(restarted_recovery.state, HubOperationState::Completed);
    assert_eq!(restarted_handle.resolution_records().await.len(), 1);

    println!("ONLINE_RECOVERY_PHYSICAL_PASS operation_replayed=false");
    Ok(())
}
