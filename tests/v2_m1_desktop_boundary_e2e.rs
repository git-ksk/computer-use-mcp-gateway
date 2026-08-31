#![cfg(unix)]

use anyhow::{Context, Result, anyhow};
use computer_use_mcp_gateway::{
    v2_execution_safety::{ExecutionEvidence, IndeterminateReason, OperationOwner},
    v2_m0::{DeviceCommand, DeviceIdentity, DeviceResult, GrantAuthority, ShellRequest},
    v2_m0_execution::{HubOperationState, IndeterminateResolution},
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

// This E2E validates the shared desktop ownership/quarantine boundary rather
// than heartbeat timeout precision. The Agent treats 3 missed heartbeat
// intervals as an acknowledgement timeout, so a 50 ms fixture interval turns
// an ordinary hosted-runner fsync/deschedule into a 150 ms reconnect deadline.
// Keep both sides comfortably outside that sub-second timing regime.
const E2E_EVENTUALLY: Duration = Duration::from_secs(10);
const E2E_AGENT_HEARTBEAT_INTERVAL: Duration = Duration::from_secs(1);
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

async fn wait_online(handle: &computer_use_mcp_gateway::v2_m1_hub::HubHandle) -> Result<()> {
    tokio::time::timeout(E2E_EVENTUALLY, async {
        while !handle.is_online().await {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .context("Agent did not become online")?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn shell_and_cua_share_one_owner_fence_quarantine_and_resolution_boundary() -> Result<()> {
    let cwd = std::env::current_dir()?;
    let state_root = temp_dir("desktop-boundary");
    let hub_state = state_root.join("hub");
    let agent_state = state_root.join("agent");
    std::fs::create_dir_all(&hub_state)?;
    std::fs::create_dir_all(&agent_state)?;
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&hub_state, std::fs::Permissions::from_mode(0o700))?;
    std::fs::set_permissions(&agent_state, std::fs::Permissions::from_mode(0o700))?;
    let app_marker = state_root.join("app-launched");
    let ambiguous_click_marker = state_root.join("ambiguous-click");
    let successful_click_marker = state_root.join("successful-click");
    let drag_marker = state_root.join("drag-calls");
    let cancel_marker = state_root.join("drag-cancels");
    let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("scripts/mock_cua_mcp_backend.py");

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
            allowed_cwd_roots: vec![cwd.clone(), state_root.clone()],
            state_dir: agent_state,
            heartbeat_interval: E2E_AGENT_HEARTBEAT_INTERVAL,
            reconnect: ReconnectPolicy {
                initial_delay: Duration::from_millis(10),
                max_delay: Duration::from_millis(100),
                max_attempts: 10,
            },
            cua: Some(CuaAgentConfig {
                command: "python3".into(),
                args: vec![
                    fixture.to_string_lossy().into_owned(),
                    "--ambiguous-click-marker".into(),
                    ambiguous_click_marker.to_string_lossy().into_owned(),
                    "--successful-click-marker".into(),
                    successful_click_marker.to_string_lossy().into_owned(),
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
    wait_online(&handle).await?;
    let initial_generation = handle
        .current_generation()
        .await
        .ok_or_else(|| anyhow!("missing initial Agent generation"))?;

    let alice = OperationOwner::new("https://issuer.example", "alice")?;
    let bob = OperationOwner::new("https://issuer.example", "bob")?;

    // Native shell and Cua both enter through `start_command_as`, so the same
    // operation owner/fence is used instead of separate shell and GUI ownership.
    let shell = handle
        .start_command_as(
            alice.clone(),
            DeviceCommand::Shell {
                request: ShellRequest {
                    command: format!("printf app-launched > '{}'", app_marker.display()),
                    cwd: cwd.to_string_lossy().into_owned(),
                    env: vec![],
                    timeout_ms: 5_000,
                },
            },
        )
        .await
        .context("shell command admission lost the Agent session")?
        .wait()
        .await
        .context("shell command result lost the Agent session")?;
    assert!(app_marker.is_file());
    assert_eq!(shell.receipt.owner, alice);
    assert_eq!(shell.receipt.terminal_state, HubOperationState::Completed);
    assert_eq!(
        shell.receipt.evidence,
        ExecutionEvidence::VerifiedAgentResult
    );

    // A provider-level error after an effectful request was dispatched is not
    // proof of non-execution. The Agent sends a closed signed indeterminate
    // signal; the Hub quarantines durably without tearing down this generation.
    let ambiguous_click = handle
        .start_command_as(
            alice.clone(),
            DeviceCommand::PointerClick {
                x: 11,
                y: 22,
                button: computer_use_mcp_gateway::v2_m0::PointerButton::Left,
            },
        )
        .await
        .context("ambiguous click admission lost the Agent session")?;
    let ambiguous_click_operation = ambiguous_click.operation_id.clone();
    let click_result = tokio::time::timeout(Duration::from_secs(3), ambiguous_click.wait())
        .await
        .context("post-effect backend error did not settle at Hub")?;
    assert!(matches!(
        click_result,
        Err(HubCommandError::DeviceIndeterminate { operation_id })
            if operation_id == ambiguous_click_operation
    ));
    assert!(ambiguous_click_marker.is_file());
    assert_eq!(handle.current_generation().await, Some(initial_generation));
    let quarantine = handle
        .desktop_quarantine()
        .await
        .ok_or_else(|| anyhow!("post-effect backend error did not quarantine desktop"))?;
    assert_eq!(quarantine.operation_id, ambiguous_click_operation);
    assert_eq!(
        quarantine.reason,
        IndeterminateReason::BackendOutcomeUnproven
    );
    // Quarantine opens only the explicit evidence-read lane. The observation
    // completes normally but cannot settle or replace the original ambiguity.
    let recovery_read = handle
        .start_command_as(bob.clone(), DeviceCommand::ScreenGeometry)
        .await?
        .wait()
        .await?;
    assert_eq!(
        recovery_read.receipt.terminal_state,
        HubOperationState::Completed
    );
    assert_eq!(
        handle
            .desktop_quarantine()
            .await
            .expect("evidence read must not clear quarantine")
            .operation_id,
        ambiguous_click_operation
    );
    assert!(matches!(
        handle
            .start_command_as(
                bob.clone(),
                DeviceCommand::PointerClick {
                    x: 12,
                    y: 23,
                    button: computer_use_mcp_gateway::v2_m0::PointerButton::Left,
                },
            )
            .await?
            .wait()
            .await,
        Err(HubCommandError::DeviceIndeterminate { operation_id })
            if operation_id == ambiguous_click_operation
    ));
    handle
        .resolve_indeterminate(
            &ambiguous_click_operation,
            alice.clone(),
            IndeterminateResolution::ConfirmedCompleted,
            "fixture recorded dispatch before returning a generic backend error",
        )
        .await?;
    assert!(handle.desktop_quarantine().await.is_none());

    // Model a caller losing the response after a successful GUI dispatch. The
    // caller retained the stable operation id before dispatch, so recovery is a
    // read-only ledger lookup and never replays the click.
    let lost_response_operation = "op_22222222222222222222222222222222".to_owned();
    let lost_response = handle
        .start_command_as_with_id(
            alice.clone(),
            lost_response_operation.clone(),
            DeviceCommand::PointerClick {
                x: 31,
                y: 41,
                button: computer_use_mcp_gateway::v2_m0::PointerButton::Left,
            },
        )
        .await?;
    tokio::time::timeout(Duration::from_secs(3), async {
        while !successful_click_marker.is_file() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .context("successful GUI click never crossed the fixture dispatch boundary")?;
    drop(lost_response);

    let recovered = tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            match handle
                .operation_recovery_as(alice.clone(), &lost_response_operation)
                .await
            {
                Ok(recovery) if recovery.state == HubOperationState::Completed => break recovery,
                Ok(_) | Err(HubCommandError::UnknownOperation) => {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
                Err(error) => panic!("unexpected GUI recovery lookup error: {error:?}"),
            }
        }
    })
    .await
    .context("lost GUI response did not become durably recoverable")?;
    assert_eq!(
        recovered.result,
        Some(computer_use_mcp_gateway::v2_execution_safety::RecoverableOperationResult::EffectfulStatus)
    );
    assert_eq!(
        recovered.receipt.unwrap().evidence,
        ExecutionEvidence::VerifiedAgentResult
    );
    assert!(handle.desktop_quarantine().await.is_none());
    assert_eq!(
        handle
            .operation_recovery_as(bob.clone(), &lost_response_operation)
            .await,
        Err(HubCommandError::UnknownOperation)
    );
    let replay = handle
        .start_command_as_with_id(
            alice.clone(),
            lost_response_operation.clone(),
            DeviceCommand::PointerClick {
                x: 31,
                y: 41,
                button: computer_use_mcp_gateway::v2_m0::PointerButton::Left,
            },
        )
        .await?;
    assert_eq!(replay.wait().await, Err(HubCommandError::OperationReplay));
    let click_calls = std::fs::read_to_string(&successful_click_marker)?;
    assert_eq!(click_calls.lines().count(), 1);

    let pending = handle
        .start_command_as(
            alice.clone(),
            DeviceCommand::PointerDrag {
                from_x: 10,
                from_y: 10,
                to_x: 20,
                to_y: 20,
                duration_ms: 10_000,
            },
        )
        .await
        .context("Cua drag admission lost the Agent session")?;
    let ambiguous_operation = pending.operation_id.clone();
    tokio::time::timeout(Duration::from_secs(3), async {
        while !drag_marker.is_file() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .context("Cua fixture never received drag")?;

    // A competing principal cannot steal the in-flight operation, even to cancel it.
    assert_eq!(
        handle
            .cancel_as(bob.clone(), ambiguous_operation.clone())
            .await,
        Err(HubCommandError::Rejected)
    );
    let disposition = handle
        .cancel_as(alice.clone(), ambiguous_operation.clone())
        .await
        .context("Cua cancellation acknowledgement lost the Agent session")?;
    assert_eq!(
        disposition,
        CancellationDisposition::IndeterminateAfterPropagation
    );
    let pending_result = tokio::time::timeout(Duration::from_secs(3), pending.wait())
        .await
        .context("ambiguous Cua operation did not settle at Hub")?;
    assert!(matches!(
        pending_result,
        Err(HubCommandError::DeviceIndeterminate { operation_id }) if operation_id == ambiguous_operation
    ));
    assert!(cancel_marker.is_file());

    // Cancellation ambiguity no longer tears down the transport merely to signal
    // uncertainty. The same live generation remains connected, but liveness
    // still cannot clear the durable quarantine.
    assert_eq!(handle.current_generation().await, Some(initial_generation));
    let quarantine = handle
        .desktop_quarantine()
        .await
        .ok_or_else(|| anyhow!("desktop quarantine missing"))?;
    assert_eq!(quarantine.operation_id, ambiguous_operation);
    assert_eq!(quarantine.owner, alice);

    let recovery_read = handle
        .start_command_as(bob.clone(), DeviceCommand::ScreenGeometry)
        .await?
        .wait()
        .await?;
    assert!(matches!(
        recovery_read.result,
        DeviceResult::ScreenGeometry { .. }
    ));
    assert_eq!(
        handle
            .desktop_quarantine()
            .await
            .expect("recovery evidence must not settle cancellation ambiguity")
            .operation_id,
        ambiguous_operation
    );

    // Re-opening the desktop is persistence-gated. If the checkpoint cannot be
    // made durable, the in-memory resolution is rolled back to quarantine.
    std::fs::set_permissions(&hub_state, std::fs::Permissions::from_mode(0o500))?;
    assert_eq!(
        handle
            .resolve_indeterminate(
                &ambiguous_operation,
                alice.clone(),
                IndeterminateResolution::ConfirmedCompleted,
                "this resolution must fail because persistence is unavailable",
            )
            .await,
        Err(HubCommandError::Rejected)
    );
    assert_eq!(
        handle
            .desktop_quarantine()
            .await
            .map(|quarantine| quarantine.operation_id),
        Some(ambiguous_operation.clone())
    );
    let records_after_failed_resolution = handle.resolution_records().await;
    assert_eq!(records_after_failed_resolution.len(), 1);
    assert_eq!(
        records_after_failed_resolution[0].operation_id,
        ambiguous_click_operation
    );
    std::fs::set_permissions(&hub_state, std::fs::Permissions::from_mode(0o700))?;

    let receipt = handle
        .resolve_indeterminate(
            &ambiguous_operation,
            alice.clone(),
            IndeterminateResolution::ConfirmedCompleted,
            "explicit test resolution after reviewing modeled desktop state",
        )
        .await?;
    assert_eq!(receipt.operation.operation_id, ambiguous_operation);
    assert_eq!(receipt.evidence, ExecutionEvidence::OperatorResolution);
    assert!(handle.desktop_quarantine().await.is_none());
    let records = handle.resolution_records().await;
    let audit = records
        .last()
        .ok_or_else(|| anyhow!("resolution audit missing"))?;
    assert_eq!(audit.operation_id, ambiguous_operation);
    assert_eq!(audit.resolver, alice);

    assert_eq!(handle.current_generation().await, Some(initial_generation));
    let reused = handle
        .start_command_as(bob, DeviceCommand::ScreenGeometry)
        .await
        .context("post-resolution admission lost the Agent session")?
        .wait()
        .await
        .context("post-resolution result lost the Agent session")?;
    assert!(matches!(reused.result, DeviceResult::ScreenGeometry { .. }));

    // Resolution reopens the desktop but never replays the old Cua action.
    let drag_calls = std::fs::read_to_string(&drag_marker)?;
    assert_eq!(drag_calls.lines().count(), 1);

    let _ = shutdown_tx.send(true);
    let _ = tokio::time::timeout(Duration::from_secs(3), agent_task).await;
    server.abort();
    let _ = std::fs::remove_dir_all(state_root);
    Ok(())
}
