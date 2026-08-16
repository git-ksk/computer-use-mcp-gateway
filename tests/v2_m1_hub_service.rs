#![cfg(unix)]

use anyhow::{Result, anyhow};
use computer_use_mcp_gateway::{
    v2_grant_signer::{
        GRANT_SIGNER_POLICY_SCHEMA_VERSION, GrantSigningPolicyDocument, HubGrantSigner,
    },
    v2_m0::{
        DeviceCommand, DeviceIdentity, DeviceRegistry, GrantAuthority, ProcessRequest, ShellRequest,
    },
    v2_m0_transport::HubIdentity,
    v2_m1::ReconnectPolicy,
    v2_m1_agent::{AgentService, AgentServiceConfig},
    v2_m1_grpc::{
        MAX_GRPC_TRANSPORT_MESSAGE_BYTES, proto::agent_control_server::AgentControlServer,
    },
    v2_m1_hub::{HubCommandError, HubProvisionedMaterial, HubServiceConfig, SingleDeviceHub},
    v2_m1_keys::{
        AgentProvisionedMaterial, create_new_grant_authority, load_verifying_key,
        write_new_trusted_text, write_new_verifying_key,
    },
};
use rcgen::{CertifiedKey, generate_simple_self_signed};
use std::{env, process::Stdio, time::Duration};
use tokio::sync::watch;
use tonic::transport::{Identity, Server, ServerTlsConfig};

// This E2E validates deployable Hub/Agent execution, filesystem boundaries and
// cancellation semantics rather than heartbeat timeout precision. The Agent
// treats 3 missed heartbeat intervals as an acknowledgement timeout, so a
// 50 ms fixture interval turns hosted-runner contention into a 150 ms reconnect
// deadline. Keep the fixture comfortably outside that sub-second timing regime.
const E2E_AGENT_HEARTBEAT_INTERVAL: Duration = Duration::from_secs(1);
const E2E_HEARTBEAT_TIMEOUT: Duration = Duration::from_secs(5);

fn temp_dir(name: &str) -> std::path::PathBuf {
    let path = std::env::temp_dir().join(format!(
        "cumg-{name}-{}-{}",
        std::process::id(),
        rand::random::<u64>()
    ));
    std::fs::create_dir_all(&path).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    path
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn deployable_hub_and_agent_execute_and_cancel_over_grpc_tls() -> Result<()> {
    let cwd = env::current_dir()?;
    let fs_root = temp_dir("hub-runtime-fs");
    let outside_root = temp_dir("hub-runtime-outside");
    std::fs::write(fs_root.join("note.txt"), b"bounded filesystem read")?;
    std::fs::write(outside_root.join("secret.txt"), b"must-not-read")?;
    std::os::unix::fs::symlink(outside_root.join("secret.txt"), fs_root.join("escape"))?;
    let CertifiedKey { cert, signing_key } = generate_simple_self_signed(vec!["localhost".into()])?;
    let cert_pem = cert.pem();
    let cert_der = cert.der().to_vec();
    let key_pem = signing_key.serialize_pem();

    let device_identity = DeviceIdentity::generate();
    let hub_identity = HubIdentity::generate();
    let grant_authority = GrantAuthority::generate();
    let hub_state = temp_dir("hub-runtime-state");
    let agent_state = temp_dir("agent-runtime-state");

    let (hub, handle) = SingleDeviceHub::new(
        HubServiceConfig {
            state_dir: hub_state.clone(),
            heartbeat_timeout: E2E_HEARTBEAT_TIMEOUT,
            max_agent_session_lifetime: Duration::from_secs(60 * 60),
            agent_session_reauth_drain: Duration::from_secs(30),
            checkpoint_generation_rollover_bytes: 512 * 1024,
            max_queued_per_device: 2,
            max_agent_sessions: 2,
            max_agent_session_starts_per_minute: 30,
        },
        HubProvisionedMaterial {
            hub_identity: hub_identity.clone(),
            grant_signer: grant_authority.clone().into(),
            device_verifier: device_identity.verifying_key(),
            device_rotation: None,
        },
    )
    .map_err(|error| anyhow!("Hub init failed: {error:?}"))?;
    let device_id = hub.device_id().to_owned();
    assert_eq!(device_id, handle.device_id());

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
            allowed_cwd_roots: vec![cwd.clone(), fs_root.clone()],
            state_dir: agent_state.clone(),
            heartbeat_interval: E2E_AGENT_HEARTBEAT_INTERVAL,
            reconnect: ReconnectPolicy {
                initial_delay: Duration::from_millis(5),
                max_delay: Duration::from_millis(50),
                max_attempts: 5,
            },
            cua: None,
        },
        AgentProvisionedMaterial {
            device_identity,
            trusted_hub: hub_identity.verifier(),
            grant_verifier: grant_authority.verifier(),
            additional_grant_verifiers: vec![],
            hub_rotation: None,
            tls_root_der: cert_der,
        },
    )
    .map_err(|error| anyhow!("Agent init failed: {error:?}"))?;
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let agent_task = tokio::spawn(async move { agent.run(shutdown_rx).await });

    tokio::time::timeout(Duration::from_secs(3), async {
        while !handle.is_online().await {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .map_err(|_| anyhow!("Agent did not connect to deployable Hub"))?;

    let git = handle
        .execute_process(ProcessRequest {
            program: "git".into(),
            args: vec!["status".into(), "--short".into()],
            cwd: cwd.to_string_lossy().into_owned(),
            env: vec![],
            timeout_ms: 10_000,
        })
        .await
        .map_err(|error| anyhow!("Hub git execution failed: {error:?}"))?;
    assert_eq!(git.output.exit_code, Some(0));
    assert!(!git.output.cancelled && !git.output.timed_out);

    let shell = handle
        .execute_shell(ShellRequest {
            command: "printf 'shell\n' | tr a-z A-Z".into(),
            cwd: cwd.to_string_lossy().into_owned(),
            env: vec![],
            timeout_ms: 10_000,
        })
        .await
        .map_err(|error| anyhow!("Hub shell execution failed: {error:?}"))?;
    assert_eq!(shell.output.exit_code, Some(0));
    assert_eq!(shell.output.stdout, "SHELL\n");
    assert!(!shell.output.cancelled && !shell.output.timed_out);

    let (bytes, truncated) = handle
        .read_file(fs_root.join("note.txt").to_string_lossy().into_owned())
        .await?;
    assert_eq!(bytes, b"bounded filesystem read");
    assert!(!truncated);

    let (entries, truncated) = handle
        .list_directory(fs_root.to_string_lossy().into_owned())
        .await?;
    assert!(!truncated);
    assert!(entries.iter().any(|entry| entry.name == "note.txt"));
    assert!(entries.iter().any(|entry| entry.name == "escape"));

    assert_eq!(
        handle
            .read_file(fs_root.join("escape").to_string_lossy().into_owned())
            .await,
        Err(HubCommandError::Remote(
            computer_use_mcp_gateway::v2_m0::DeviceErrorCode::PermissionDenied
        ))
    );
    // A rejected bounded-filesystem request is command-local; it must not tear
    // down the authenticated Agent session.
    assert!(handle.is_online().await);

    let pending = handle
        .start_process(ProcessRequest {
            program: "/bin/sleep".into(),
            args: vec!["30".into()],
            cwd: cwd.to_string_lossy().into_owned(),
            env: vec![],
            timeout_ms: 60_000,
        })
        .await
        .map_err(|error| anyhow!("Hub sleep start failed: {error:?}"))?;
    let operation_id = pending.operation_id.clone();
    tokio::time::sleep(Duration::from_millis(40)).await;
    let disposition = handle
        .cancel(operation_id)
        .await
        .map_err(|error| anyhow!("Hub cancel failed: {error:?}"))?;
    assert_eq!(
        disposition,
        computer_use_mcp_gateway::v2_m0_transport::CancellationDisposition::CancellationRequested
    );
    let cancelled = tokio::time::timeout(Duration::from_secs(3), pending.wait())
        .await
        .map_err(|_| anyhow!("cancelled process did not complete"))??;
    assert!(cancelled.output.cancelled && !cancelled.output.timed_out);

    let pending_shell = handle
        .start_shell(ShellRequest {
            command: "sleep 30".into(),
            cwd: cwd.to_string_lossy().into_owned(),
            env: vec![],
            timeout_ms: 60_000,
        })
        .await
        .map_err(|error| anyhow!("Hub shell sleep start failed: {error:?}"))?;
    let shell_operation_id = pending_shell.operation_id.clone();
    tokio::time::sleep(Duration::from_millis(40)).await;
    let shell_disposition = handle
        .cancel(shell_operation_id)
        .await
        .map_err(|error| anyhow!("Hub shell cancel failed: {error:?}"))?;
    assert_eq!(
        shell_disposition,
        computer_use_mcp_gateway::v2_m0_transport::CancellationDisposition::CancellationRequested
    );
    let cancelled_shell = tokio::time::timeout(Duration::from_secs(3), pending_shell.wait())
        .await
        .map_err(|_| anyhow!("cancelled shell did not complete"))??;
    assert!(cancelled_shell.output.cancelled && !cancelled_shell.output.timed_out);

    let _ = shutdown_tx.send(true);
    tokio::time::timeout(Duration::from_secs(2), agent_task)
        .await
        .map_err(|_| anyhow!("Agent did not shut down"))???;
    server.abort();
    let _ = std::fs::remove_dir_all(hub_state);
    let _ = std::fs::remove_dir_all(agent_state);
    let _ = std::fs::remove_dir_all(fs_root);
    let _ = std::fs::remove_dir_all(outside_root);
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn planned_shutdown_drain_waits_for_dispatched_work_and_rejects_new_admission() -> Result<()>
{
    let cwd = env::current_dir()?;
    let fs_root = temp_dir("hub-drain-fs");
    let sentinel = fs_root.join("dispatched.marker");
    let CertifiedKey { cert, signing_key } = generate_simple_self_signed(vec!["localhost".into()])?;
    let cert_pem = cert.pem();
    let cert_der = cert.der().to_vec();
    let key_pem = signing_key.serialize_pem();

    let device_identity = DeviceIdentity::generate();
    let hub_identity = HubIdentity::generate();
    let grant_authority = GrantAuthority::generate();
    let hub_state = temp_dir("hub-drain-state");
    let agent_state = temp_dir("agent-drain-state");

    let (hub, handle) = SingleDeviceHub::new(
        HubServiceConfig {
            state_dir: hub_state.clone(),
            heartbeat_timeout: E2E_HEARTBEAT_TIMEOUT,
            max_agent_session_lifetime: Duration::from_secs(60 * 60),
            agent_session_reauth_drain: Duration::from_secs(30),
            checkpoint_generation_rollover_bytes: 512 * 1024,
            max_queued_per_device: 2,
            max_agent_sessions: 2,
            max_agent_session_starts_per_minute: 30,
        },
        HubProvisionedMaterial {
            hub_identity: hub_identity.clone(),
            grant_signer: grant_authority.clone().into(),
            device_verifier: device_identity.verifying_key(),
            device_rotation: None,
        },
    )
    .map_err(|error| anyhow!("Hub init failed: {error:?}"))?;
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
            allowed_cwd_roots: vec![cwd, fs_root.clone()],
            state_dir: agent_state.clone(),
            heartbeat_interval: E2E_AGENT_HEARTBEAT_INTERVAL,
            reconnect: ReconnectPolicy {
                initial_delay: Duration::from_millis(5),
                max_delay: Duration::from_millis(50),
                max_attempts: 5,
            },
            cua: None,
        },
        AgentProvisionedMaterial {
            device_identity,
            trusted_hub: hub_identity.verifier(),
            grant_verifier: grant_authority.verifier(),
            additional_grant_verifiers: vec![],
            hub_rotation: None,
            tls_root_der: cert_der,
        },
    )
    .map_err(|error| anyhow!("Agent init failed: {error:?}"))?;
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let agent_task = tokio::spawn(async move { agent.run(shutdown_rx).await });

    tokio::time::timeout(Duration::from_secs(3), async {
        while !handle.is_online().await {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .map_err(|_| anyhow!("Agent did not connect to drain-test Hub"))?;

    let pending = handle
        .start_shell(ShellRequest {
            command: format!(
                "touch '{}'; sleep 0.3; printf done",
                sentinel.to_string_lossy()
            ),
            cwd: fs_root.to_string_lossy().into_owned(),
            env: vec![],
            timeout_ms: 5_000,
        })
        .await
        .map_err(|error| anyhow!("drain fixture shell start failed: {error:?}"))?;

    tokio::time::timeout(Duration::from_secs(2), async {
        while !sentinel.exists() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .map_err(|_| anyhow!("fixture never crossed the dispatch boundary"))?;

    assert!(handle.begin_shutdown_drain());
    assert!(matches!(
        handle.start_command(DeviceCommand::ScreenGeometry).await,
        Err(HubCommandError::Busy)
    ));
    assert!(
        tokio::time::timeout(Duration::from_millis(50), handle.wait_for_shutdown_drain())
            .await
            .is_err(),
        "drain completed before the already-dispatched command settled"
    );

    tokio::time::timeout(Duration::from_secs(2), handle.wait_for_shutdown_drain())
        .await
        .map_err(|_| anyhow!("drain did not finish after command settlement"))?;
    let completed = pending.wait().await?;
    assert_eq!(completed.output.exit_code, Some(0));
    assert_eq!(completed.output.stdout, "done");
    assert!(handle.desktop_quarantine().await.is_none());

    let _ = shutdown_tx.send(true);
    tokio::time::timeout(Duration::from_secs(2), agent_task)
        .await
        .map_err(|_| anyhow!("Agent did not shut down"))???;
    server.abort();
    let _ = std::fs::remove_dir_all(hub_state);
    let _ = std::fs::remove_dir_all(agent_state);
    let _ = std::fs::remove_dir_all(fs_root);
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn checkpoint_high_water_rolls_generation_without_quarantine() -> Result<()> {
    let cwd = env::current_dir()?;
    let fs_root = temp_dir("hub-rollover-fs");
    let CertifiedKey { cert, signing_key } = generate_simple_self_signed(vec!["localhost".into()])?;
    let cert_pem = cert.pem();
    let cert_der = cert.der().to_vec();
    let key_pem = signing_key.serialize_pem();

    let device_identity = DeviceIdentity::generate();
    let hub_identity = HubIdentity::generate();
    let grant_authority = GrantAuthority::generate();
    let hub_state = temp_dir("hub-rollover-state");
    let agent_state = temp_dir("agent-rollover-state");

    let (hub, handle) = SingleDeviceHub::new(
        HubServiceConfig {
            state_dir: hub_state.clone(),
            heartbeat_timeout: E2E_HEARTBEAT_TIMEOUT,
            max_agent_session_lifetime: Duration::from_secs(60 * 60),
            agent_session_reauth_drain: Duration::from_secs(30),
            checkpoint_generation_rollover_bytes: 8 * 1024,
            max_queued_per_device: 2,
            max_agent_sessions: 2,
            max_agent_session_starts_per_minute: 120,
        },
        HubProvisionedMaterial {
            hub_identity: hub_identity.clone(),
            grant_signer: grant_authority.clone().into(),
            device_verifier: device_identity.verifying_key(),
            device_rotation: None,
        },
    )
    .map_err(|error| anyhow!("Hub init failed: {error:?}"))?;
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
            allowed_cwd_roots: vec![cwd.clone(), fs_root.clone()],
            state_dir: agent_state.clone(),
            heartbeat_interval: E2E_AGENT_HEARTBEAT_INTERVAL,
            reconnect: ReconnectPolicy {
                initial_delay: Duration::from_millis(5),
                max_delay: Duration::from_millis(50),
                max_attempts: 20,
            },
            cua: None,
        },
        AgentProvisionedMaterial {
            device_identity,
            trusted_hub: hub_identity.verifier(),
            grant_verifier: grant_authority.verifier(),
            additional_grant_verifiers: vec![],
            hub_rotation: None,
            tls_root_der: cert_der,
        },
    )
    .map_err(|error| anyhow!("Agent init failed: {error:?}"))?;
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let agent_task = tokio::spawn(async move { agent.run(shutdown_rx).await });

    tokio::time::timeout(Duration::from_secs(3), async {
        while handle.current_generation().await != Some(1) {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .map_err(|_| anyhow!("Agent did not establish generation 1"))?;

    for index in 0..40_u32 {
        if handle.current_generation().await.unwrap_or(0) > 1 {
            break;
        }
        match handle
            .execute_shell(ShellRequest {
                command: format!("printf rollover-{index}"),
                cwd: cwd.to_string_lossy().into_owned(),
                env: vec![],
                timeout_ms: 5_000,
            })
            .await
        {
            Ok(result) => assert_eq!(result.output.exit_code, Some(0)),
            Err(
                HubCommandError::AgentOffline
                | HubCommandError::SessionClosed
                | HubCommandError::Busy,
            ) => {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            Err(error) => return Err(anyhow!("unexpected rollover command error: {error:?}")),
        }
    }

    tokio::time::timeout(Duration::from_secs(5), async {
        while handle.current_generation().await.unwrap_or(0) < 2 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .map_err(|_| anyhow!("checkpoint high-water did not trigger generation rollover"))?;

    assert!(handle.desktop_quarantine().await.is_none());
    let mut checkpoints: Vec<_> = std::fs::read_dir(&hub_state)?
        .filter_map(Result::ok)
        .filter(|entry| entry.file_name().to_string_lossy().starts_with("hub-"))
        .collect();
    checkpoints.sort_by_key(|entry| entry.file_name());
    let latest_size = checkpoints
        .last()
        .and_then(|entry| entry.metadata().ok())
        .map_or(0, |metadata| metadata.len());
    let maximum_size = checkpoints
        .iter()
        .filter_map(|entry| entry.metadata().ok())
        .map(|metadata| metadata.len())
        .max()
        .unwrap_or(0);
    assert!(
        latest_size < 8 * 1024,
        "fresh generation did not compact terminal history"
    );
    assert!(maximum_size < computer_use_mcp_gateway::v2_m1_persistence::MAX_CHECKPOINT_BYTES);

    let after = handle
        .execute_shell(ShellRequest {
            command: "printf after-rollover".into(),
            cwd: cwd.to_string_lossy().into_owned(),
            env: vec![],
            timeout_ms: 5_000,
        })
        .await
        .map_err(|error| anyhow!("post-rollover shell failed: {error:?}"))?;
    assert_eq!(after.output.stdout, "after-rollover");
    assert!(after.receipt.operation.device_generation >= 2);

    let _ = shutdown_tx.send(true);
    tokio::time::timeout(Duration::from_secs(2), agent_task)
        .await
        .map_err(|_| anyhow!("Agent did not shut down"))???;
    server.abort();
    let _ = std::fs::remove_dir_all(hub_state);
    let _ = std::fs::remove_dir_all(agent_state);
    let _ = std::fs::remove_dir_all(fs_root);
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn session_lifetime_reauthenticates_cleanly_without_quarantine() -> Result<()> {
    let cwd = env::current_dir()?;
    let fs_root = temp_dir("hub-session-lifetime-fs");
    let CertifiedKey { cert, signing_key } = generate_simple_self_signed(vec!["localhost".into()])?;
    let cert_pem = cert.pem();
    let cert_der = cert.der().to_vec();
    let key_pem = signing_key.serialize_pem();

    let device_identity = DeviceIdentity::generate();
    let hub_identity = HubIdentity::generate();
    let grant_authority = GrantAuthority::generate();
    let hub_state = temp_dir("hub-session-lifetime-state");
    let agent_state = temp_dir("agent-session-lifetime-state");

    let (hub, handle) = SingleDeviceHub::new(
        HubServiceConfig {
            state_dir: hub_state.clone(),
            heartbeat_timeout: E2E_HEARTBEAT_TIMEOUT,
            max_agent_session_lifetime: Duration::from_secs(2),
            agent_session_reauth_drain: Duration::from_secs(1),
            checkpoint_generation_rollover_bytes: 512 * 1024,
            max_queued_per_device: 2,
            max_agent_sessions: 2,
            max_agent_session_starts_per_minute: 120,
        },
        HubProvisionedMaterial {
            hub_identity: hub_identity.clone(),
            grant_signer: grant_authority.clone().into(),
            device_verifier: device_identity.verifying_key(),
            device_rotation: None,
        },
    )
    .map_err(|error| anyhow!("Hub init failed: {error:?}"))?;
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
            allowed_cwd_roots: vec![cwd.clone(), fs_root.clone()],
            state_dir: agent_state.clone(),
            heartbeat_interval: Duration::from_millis(200),
            reconnect: ReconnectPolicy {
                initial_delay: Duration::from_millis(5),
                max_delay: Duration::from_millis(50),
                max_attempts: 20,
            },
            cua: None,
        },
        AgentProvisionedMaterial {
            device_identity,
            trusted_hub: hub_identity.verifier(),
            grant_verifier: grant_authority.verifier(),
            additional_grant_verifiers: vec![],
            hub_rotation: None,
            tls_root_der: cert_der,
        },
    )
    .map_err(|error| anyhow!("Agent init failed: {error:?}"))?;
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let agent_task = tokio::spawn(async move { agent.run(shutdown_rx).await });

    tokio::time::timeout(Duration::from_secs(3), async {
        while handle.current_generation().await != Some(1) {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .map_err(|_| anyhow!("Agent did not establish generation 1"))?;

    tokio::time::timeout(Duration::from_secs(4), async {
        while handle.current_generation().await.unwrap_or(0) < 2 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .map_err(|_| anyhow!("session lifetime did not trigger a fresh authenticated generation"))?;

    assert!(handle.desktop_quarantine().await.is_none());
    let after = handle
        .execute_shell(ShellRequest {
            command: "printf after-reauth".into(),
            cwd: cwd.to_string_lossy().into_owned(),
            env: vec![],
            timeout_ms: 5_000,
        })
        .await
        .map_err(|error| anyhow!("post-reauth shell failed: {error:?}"))?;
    assert_eq!(after.output.stdout, "after-reauth");
    assert!(after.receipt.operation.device_generation >= 2);

    let _ = shutdown_tx.send(true);
    tokio::time::timeout(Duration::from_secs(2), agent_task)
        .await
        .map_err(|_| anyhow!("Agent did not shut down"))???;
    server.abort();
    let _ = std::fs::remove_dir_all(hub_state);
    let _ = std::fs::remove_dir_all(agent_state);
    let _ = std::fs::remove_dir_all(fs_root);
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn hard_session_lifetime_cuts_off_unsettled_work_fail_closed() -> Result<()> {
    let cwd = env::current_dir()?;
    let fs_root = temp_dir("hub-session-hard-limit-fs");
    let CertifiedKey { cert, signing_key } = generate_simple_self_signed(vec!["localhost".into()])?;
    let cert_pem = cert.pem();
    let cert_der = cert.der().to_vec();
    let key_pem = signing_key.serialize_pem();

    let device_identity = DeviceIdentity::generate();
    let hub_identity = HubIdentity::generate();
    let grant_authority = GrantAuthority::generate();
    let hub_state = temp_dir("hub-session-hard-limit-state");
    let agent_state = temp_dir("agent-session-hard-limit-state");

    let (hub, handle) = SingleDeviceHub::new(
        HubServiceConfig {
            state_dir: hub_state.clone(),
            heartbeat_timeout: E2E_HEARTBEAT_TIMEOUT,
            max_agent_session_lifetime: Duration::from_secs(2),
            agent_session_reauth_drain: Duration::from_secs(1),
            checkpoint_generation_rollover_bytes: 512 * 1024,
            max_queued_per_device: 2,
            max_agent_sessions: 2,
            max_agent_session_starts_per_minute: 120,
        },
        HubProvisionedMaterial {
            hub_identity: hub_identity.clone(),
            grant_signer: grant_authority.clone().into(),
            device_verifier: device_identity.verifying_key(),
            device_rotation: None,
        },
    )
    .map_err(|error| anyhow!("Hub init failed: {error:?}"))?;
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
            allowed_cwd_roots: vec![cwd.clone(), fs_root.clone()],
            state_dir: agent_state.clone(),
            heartbeat_interval: Duration::from_millis(200),
            reconnect: ReconnectPolicy {
                initial_delay: Duration::from_millis(5),
                max_delay: Duration::from_millis(50),
                max_attempts: 20,
            },
            cua: None,
        },
        AgentProvisionedMaterial {
            device_identity,
            trusted_hub: hub_identity.verifier(),
            grant_verifier: grant_authority.verifier(),
            additional_grant_verifiers: vec![],
            hub_rotation: None,
            tls_root_der: cert_der,
        },
    )
    .map_err(|error| anyhow!("Agent init failed: {error:?}"))?;
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let agent_task = tokio::spawn(async move { agent.run(shutdown_rx).await });

    tokio::time::timeout(Duration::from_secs(3), async {
        while handle.current_generation().await != Some(1) {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .map_err(|_| anyhow!("Agent did not establish generation 1"))?;

    let command_handle = handle.clone();
    let cwd_string = cwd.to_string_lossy().into_owned();
    let pending = tokio::spawn(async move {
        command_handle
            .execute_shell(ShellRequest {
                command: "sleep 4; printf must-not-complete".into(),
                cwd: cwd_string,
                env: vec![],
                timeout_ms: 10_000,
            })
            .await
    });

    tokio::time::timeout(Duration::from_secs(4), async {
        while handle.desktop_quarantine().await.is_none() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .map_err(|_| anyhow!("hard session lifetime did not fail closed on unsettled work"))?;

    let quarantine = handle
        .desktop_quarantine()
        .await
        .ok_or_else(|| anyhow!("expected quarantine after hard session cutoff"))?;
    assert_eq!(
        quarantine.reason,
        computer_use_mcp_gateway::v2_execution_safety::IndeterminateReason::ConnectionLost
    );
    assert_eq!(quarantine.device_generation, 1);

    let command_result = tokio::time::timeout(Duration::from_secs(2), pending)
        .await
        .map_err(|_| anyhow!("unsettled command did not return after hard session cutoff"))??;
    assert!(
        command_result.is_err(),
        "hard cutoff unexpectedly returned command success"
    );

    let _ = shutdown_tx.send(true);
    tokio::time::timeout(Duration::from_secs(2), agent_task)
        .await
        .map_err(|_| anyhow!("Agent did not shut down"))???;
    server.abort();
    let _ = std::fs::remove_dir_all(hub_state);
    let _ = std::fs::remove_dir_all(agent_state);
    let _ = std::fs::remove_dir_all(fs_root);
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn external_grant_signer_executes_without_hub_key_custody_and_has_no_fallback() -> Result<()>
{
    let cwd = env::current_dir()?;
    // Keep the Unix socket pathname short enough for macOS while remaining
    // portable to Linux CI. The private child directory is mode 0700 below.
    let root = std::path::PathBuf::from(format!(
        "/tmp/cumg-gs-{}-{}",
        std::process::id(),
        rand::random::<u32>()
    ));
    std::fs::create_dir(&root)?;
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700))?;
    }
    let hub_state = temp_dir("external-grant-hub-state");
    let agent_state = temp_dir("external-grant-agent-state");
    let signer_socket = root.join("grant-signer.sock");
    let signer_secret = root.join("grant.key");
    let signer_public = root.join("grant.pub");
    let signer_policy = root.join("grant-policy.json");

    let grant_authority = create_new_grant_authority(&signer_secret)?;
    write_new_verifying_key(&signer_public, &grant_authority.verifier())?;
    let device_identity = DeviceIdentity::generate();
    let mut registry = DeviceRegistry::default();
    let device_id = registry.provision_trusted_device(device_identity.verifying_key());
    write_new_trusted_text(
        &signer_policy,
        &serde_json::to_string_pretty(&GrantSigningPolicyDocument {
            schema_version: GRANT_SIGNER_POLICY_SCHEMA_VERSION,
            device_id: device_id.clone(),
            allowed_device_capabilities: vec![
                computer_use_mcp_gateway::v2_m0::DeviceCapability::Shell,
            ],
            max_grant_lifetime_ms: 30_000,
            max_clock_skew_ms: 15_000,
        })?,
    )?;

    let mut signer = std::process::Command::new(env!("CARGO_BIN_EXE_v2_grant_signer"))
        .args([
            "--socket",
            signer_socket.to_str().unwrap(),
            "--grant-secret-file",
            signer_secret.to_str().unwrap(),
            "--policy-file",
            signer_policy.to_str().unwrap(),
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    tokio::time::timeout(Duration::from_secs(3), async {
        while !signer_socket.exists() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .map_err(|_| anyhow!("external grant signer did not create its socket"))?;

    let CertifiedKey { cert, signing_key } = generate_simple_self_signed(vec!["localhost".into()])?;
    let cert_pem = cert.pem();
    let cert_der = cert.der().to_vec();
    let key_pem = signing_key.serialize_pem();
    let hub_identity = HubIdentity::generate();
    let external_signer = HubGrantSigner::external_unix(
        signer_socket.clone(),
        load_verifying_key(&signer_public)?,
        Duration::from_secs(1),
    )?;
    let (hub, handle) = SingleDeviceHub::new(
        HubServiceConfig {
            state_dir: hub_state.clone(),
            heartbeat_timeout: E2E_HEARTBEAT_TIMEOUT,
            max_agent_session_lifetime: Duration::from_secs(60 * 60),
            agent_session_reauth_drain: Duration::from_secs(30),
            checkpoint_generation_rollover_bytes: 512 * 1024,
            max_queued_per_device: 2,
            max_agent_sessions: 2,
            max_agent_session_starts_per_minute: 30,
        },
        HubProvisionedMaterial {
            hub_identity: hub_identity.clone(),
            grant_signer: external_signer,
            device_verifier: device_identity.verifying_key(),
            device_rotation: None,
        },
    )?;
    assert_eq!(hub.device_id(), device_id);

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
            state_dir: agent_state.clone(),
            heartbeat_interval: E2E_AGENT_HEARTBEAT_INTERVAL,
            reconnect: ReconnectPolicy {
                initial_delay: Duration::from_millis(5),
                max_delay: Duration::from_millis(50),
                max_attempts: 5,
            },
            cua: None,
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
    tokio::time::timeout(Duration::from_secs(3), async {
        while !handle.is_online().await {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .map_err(|_| anyhow!("Agent did not connect"))?;

    let signed = handle
        .execute_shell(ShellRequest {
            command: "printf external-signer".into(),
            cwd: cwd.to_string_lossy().into_owned(),
            env: vec![],
            timeout_ms: 5_000,
        })
        .await?;
    assert_eq!(signed.output.stdout, "external-signer");

    signer.kill()?;
    let _ = signer.wait()?;
    let denied = handle
        .execute_shell(ShellRequest {
            command: "printf must-not-dispatch".into(),
            cwd: cwd.to_string_lossy().into_owned(),
            env: vec![],
            timeout_ms: 5_000,
        })
        .await;
    assert_eq!(denied, Err(HubCommandError::GrantSigningUnavailable));
    assert!(handle.desktop_quarantine().await.is_none());
    assert!(handle.is_online().await);

    let _ = shutdown_tx.send(true);
    tokio::time::timeout(Duration::from_secs(2), agent_task)
        .await
        .map_err(|_| anyhow!("Agent did not shut down"))???;
    server.abort();
    let _ = std::fs::remove_dir_all(root);
    let _ = std::fs::remove_dir_all(hub_state);
    let _ = std::fs::remove_dir_all(agent_state);
    Ok(())
}
