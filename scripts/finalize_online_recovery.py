from pathlib import Path


def replace_once(path: str, old: str, new: str) -> None:
    p = Path(path)
    text = p.read_text()
    count = text.count(old)
    if count != 1:
        raise SystemExit(f"{path}: expected one match, got {count}: {old[:120]!r}")
    p.write_text(text.replace(old, new, 1))


def append_once(path: str, marker: str, text: str) -> None:
    p = Path(path)
    current = p.read_text()
    if marker in current:
        return
    if not current.endswith("\n"):
        current += "\n"
    p.write_text(current + "\n" + text.rstrip() + "\n")

# Once the final no-clobber authorization name is published, cleanup failure is
# not allowed to turn successful publication into a false failure. The handoff
# is ephemeral; Hub durable checkpointing remains the execution authority.
replace_once(
    "src/v2_online_recovery.rs",
    '''        fs::hard_link(&pending, path).map_err(|error| {
            if error.kind() == std::io::ErrorKind::AlreadyExists {
                RecoveryError::AuthorizationAlreadyPending
            } else {
                RecoveryError::Io
            }
        })?;
        fs::remove_file(&pending).map_err(|_| RecoveryError::Io)?;
        File::open(parent)
            .and_then(|directory| directory.sync_all())
            .map_err(|_| RecoveryError::Io)?;
        Ok(())
''',
    '''        fs::hard_link(&pending, path).map_err(|error| {
            if error.kind() == std::io::ErrorKind::AlreadyExists {
                RecoveryError::AuthorizationAlreadyPending
            } else {
                RecoveryError::Io
            }
        })?;
        // Publication already succeeded. Pending-file cleanup and directory
        // durability are best effort because this local handoff is not durable
        // execution authority; reporting failure here would invite a second,
        // conflicting local decision while the first authorization is visible.
        let _ = fs::remove_file(&pending);
        let _ = File::open(parent).and_then(|directory| directory.sync_all());
        Ok(())
''',
)

# An idempotent duplicate must be the same accepted resolution shape. A caller
# cannot reuse an accepted request id to get a conflicting decision treated as
# the same request.
replace_once(
    "src/v2_m1_hub.rs",
    '''        if let Some(ack) = duplicate_ack {
            send_hub(outbound, HubToAgent::RecoveryResolved(ack)).await?;
            return Ok(());
        }
''',
    '''        if let Some(ack) = duplicate_ack {
            if ack.device_id != authorization.device_id
                || ack.operation_id != authorization.operation_id
                || ack.current_generation != authorization.current_generation
                || ack.decision != authorization.decision
            {
                return Err(HubServiceError::OnlineRecovery(
                    RecoveryError::ChallengeMismatch,
                ));
            }
            send_hub(outbound, HubToAgent::RecoveryResolved(ack)).await?;
            return Ok(());
        }
''',
)

# Keep the internal Hub transition test in the same module so it can prove the
# persistence-gated private handler without exposing recovery administration as
# a public/northbound API.
replace_once(
    "src/v2_m1_hub.rs",
    '''}

#[tonic::async_trait]
impl AgentControl for SingleDeviceHub {
''',
    '''}

#[cfg(test)]
#[path = "v2_m1_hub_online_recovery_tests.rs"]
mod online_recovery_tests;

#[tonic::async_trait]
impl AgentControl for SingleDeviceHub {
''',
)

# CLI shows exactly what the OS-backed signature is authorizing before the
# user-presence operation begins; no raw desktop payload is displayed or stored.
replace_once(
    "src/bin/v2_recover.rs",
    '''    let key = MacRecoveryKey::load(&key_label).context("recovery key is not provisioned")?;
    let authorization = key
''',
    '''    println!("approving_local_recovery");
    println!("device_id={}", authorization.device_id);
    println!("operation_id={}", authorization.operation_id);
    println!("quarantine_generation={}", authorization.quarantine_generation);
    println!("current_generation={}", authorization.current_generation);
    println!("audit_assessment=inconclusive");
    println!(
        "decision={}",
        match authorization.decision {
            IndeterminateResolution::ConfirmedCompleted => "confirmed_completed",
            IndeterminateResolution::ConfirmedNotExecuted => "confirmed_not_executed",
        }
    );
    let key = MacRecoveryKey::load(&key_label).context("recovery key is not provisioned")?;
    let authorization = key
''',
)

# Strengthen the internal restart proof: test the authoritative replay ledger
# directly rather than letting AgentOffline short-circuit the public command path.
replace_once(
    "src/v2_m1_hub_online_recovery_tests.rs",
    "use crate::v2_m0_execution::{HubOperationState, OperationRef};\n",
    "use crate::v2_m0_execution::{ExecutionError, HubOperationState, OperationRef};\n",
)
replace_once(
    "src/v2_m1_hub_online_recovery_tests.rs",
    '''    let (_restarted, restarted_handle) =
        SingleDeviceHub::new(config(state_dir.clone()), material).unwrap();
    assert!(restarted_handle.desktop_quarantine().await.is_none());
    let recovery = restarted_handle
        .operation_recovery_as(owner.clone(), &operation_id)
        .await
        .unwrap();
    assert_eq!(recovery.state, HubOperationState::Completed);
    assert_eq!(
        restarted_handle
            .start_command_as_with_id(
                owner,
                operation_id.clone(),
                DeviceCommand::ScreenGeometry,
                None,
            )
            .await,
        Err(HubCommandError::AgentOffline)
    );

    let _ = std::fs::remove_dir_all(state_dir);
''',
    '''    let (restarted, restarted_handle) =
        SingleDeviceHub::new(config(state_dir.clone()), material).unwrap();
    assert!(restarted_handle.desktop_quarantine().await.is_none());
    let recovery = restarted_handle
        .operation_recovery_as(owner.clone(), &operation_id)
        .await
        .unwrap();
    assert_eq!(recovery.state, HubOperationState::Completed);
    {
        let mut persistent = restarted.inner.persistent.lock().await;
        assert!(matches!(
            persistent.execution.prepare(
                OperationRef {
                    device_id: restarted.device_id().to_owned(),
                    device_generation: current_generation + 1,
                    operation_id: operation_id.clone(),
                },
                owner,
                DeviceCapability::Shell,
                200,
            ),
            Err(ExecutionError::OperationReplay)
        ));
    }

    drop(restarted_handle);
    drop(restarted);
    let _ = std::fs::remove_dir_all(state_dir);
''',
)

# Status/index documentation.
replace_once(
    "docs/v2/STATUS.md",
    '''- **Process/shell response-loss recovery:** `execute_process` and `shell` accept a stable caller-retained `operation_id`, and the Hub exposes read-only `get_operation` for owner/capability-scoped recovery without replay or Agent liveness. Proven terminal output is persisted before northbound delivery and survives Agent generation rollover in a bounded recovery archive (8 entries / 256 KiB encoded total). Unknown/evicted references never make the original operation retry-safe.
- **V1 production:** unchanged by the V2 development branch. V1 regression and conformance coverage remains required during V2 work.
''',
    '''- **Process/shell response-loss recovery:** `execute_process` and `shell` accept a stable caller-retained `operation_id`, and the Hub exposes read-only `get_operation` for owner/capability-scoped recovery without replay or Agent liveness. Proven terminal output is persisted before northbound delivery and survives Agent generation rollover in a bounded recovery archive (8 entries / 256 KiB encoded total). Unknown/evicted references never make the original operation retry-safe.
- **Local-user online quarantine recovery:** implemented behind explicit recovery-key provisioning. The Agent device key is not recovery authority; a fresh Hub-signed challenge is resolved only by a separately pinned P-256 endpoint recovery key. The initial macOS signer uses a Secure Enclave key with user-presence access control. Automated protocol/persistence coverage is required, and physical Secure Enclave user-presence acceptance remains a release gate.
- **V1 production:** unchanged by the V2 development branch. V1 regression and conformance coverage remains required during V2 work.
''',
)
replace_once(
    "docs/v2/STATUS.md",
    '''- [`V2_OPERATION_RECOVERY.md`](V2_OPERATION_RECOVERY.md) — durable bounded process/shell result recovery after northbound response loss.
''',
    '''- [`V2_OPERATION_RECOVERY.md`](V2_OPERATION_RECOVERY.md) — durable bounded process/shell result recovery after northbound response loss.
- [`V2_ONLINE_RECOVERY.md`](V2_ONLINE_RECOVERY.md) — local-user-authorized online resolution of durable desktop quarantine.
''',
)
replace_once(
    "docs/v2/STATUS.md",
    '''- [`acceptance/V2_LOCAL_DESKTOP_ACCEPTANCE.md`](acceptance/V2_LOCAL_DESKTOP_ACCEPTANCE.md) — trusted physical Desktop acceptance procedure/evidence.
''',
    '''- [`acceptance/V2_LOCAL_DESKTOP_ACCEPTANCE.md`](acceptance/V2_LOCAL_DESKTOP_ACCEPTANCE.md) — trusted physical Desktop acceptance procedure/evidence.
- [`acceptance/V2_ONLINE_RECOVERY_ACCEPTANCE.md`](acceptance/V2_ONLINE_RECOVERY_ACCEPTANCE.md) — automated and trusted-Mac acceptance gate for local-user online quarantine recovery.
''',
)

append_once(
    "docs/v2/V2_THREAT_MODEL.md",
    "## Local-user online recovery authority",
    '''## Local-user online recovery authority

Online quarantine recovery does not grant the Agent device identity administrative recovery authority. A compromised Agent remains able to lie about local desktop state, so an Agent/device signature alone cannot clear `DesktopQuarantine`.

A deployment may explicitly provision a separate endpoint recovery verifier. The initial macOS provider keeps the corresponding P-256 private key in the Secure Enclave with Keychain user-presence/private-key-use access control. The Hub signs a fresh short-lived challenge bound to the exact durable quarantine, historical operation generation, current authenticated Agent generation, and nonce. The local user's signed decision is accepted only while that challenge and the current quarantine fingerprint still match. The Agent transports the authorization but cannot construct a valid recovery signature itself.

This is user-presence authorization, not cryptographic proof that an arbitrary GUI side effect did or did not occur. Because normal checkpoints intentionally exclude raw GUI payloads and screenshots, the generic initial audit assessment is `inconclusive`; the local user inspects the current desktop and chooses the exact resolution. A future automatic assessment must be capability-specific and must not widen the ordinary audit/privacy boundary.

Resolution remains persistence-gated and never resumes the old operation. If the verifier is absent, the challenge is stale, the signature is invalid, the quarantine changed, or persistence fails, the device remains quarantined. The existing offline maintenance resolver remains the break-glass path. See [`V2_ONLINE_RECOVERY.md`](V2_ONLINE_RECOVERY.md).
''',
)

append_once(
    "docs/v2/V2_P0_EXECUTION_SAFETY.md",
    "## Local-user-authorized online resolution",
    '''## Local-user-authorized online resolution

The accepted explicit-resolution invariant now also has an online transport that keeps the Hub running. It does not change who owns the safety state: `DesktopQuarantine` remains Hub-authoritative and durable, reconnect remains only a generation fence, and the old operation is never replayed.

The online path separates three identities: the historical operation owner, the currently authenticated Agent/device generation, and a separately provisioned local recovery key. The Agent device key cannot stand in for the recovery key. A Hub-signed challenge binds both historical and current generations plus the exact quarantine fingerprint; a local user signs one exact resolution decision; the Hub revalidates the current durable quarantine and uses the same persistence-gated `resolve_indeterminate` transition. See [`V2_ONLINE_RECOVERY.md`](V2_ONLINE_RECOVERY.md).
''',
)

append_once(
    "docs/DEPLOYMENT.md",
    "## Local-user online quarantine recovery",
    '''## Local-user online quarantine recovery

The optional online quarantine-recovery path is documented in [`v2/V2_ONLINE_RECOVERY.md`](v2/V2_ONLINE_RECOVERY.md). It does not expose recovery through northbound MCP and does not make the Agent device key a resolver credential.

For macOS, install `v2_recover` alongside `v2_agent`. Provision its Secure Enclave recovery key once from the logged-in Agent account, transfer only the exported P-256 public key through the authenticated administrative channel, and install it as `<HUB_STATE_DIR>/recovery-public-key.p256`. The Hub validates that file with the existing public trust-anchor symlink/permission rules and loads it only at startup. An absent verifier disables online recovery without changing fail-closed quarantine behavior.

The existing `v2_maint` offline resolver remains required as break-glass for an unreachable Agent, unavailable recovery key, failed local user-presence authorization, or damaged online recovery transport.
''',
)

append_once(
    "docs/TESTING.md",
    "## V2 online quarantine recovery",
    '''## V2 online quarantine recovery

Permanent automated coverage is defined by [`v2/acceptance/V2_ONLINE_RECOVERY_ACCEPTANCE.md`](v2/acceptance/V2_ONLINE_RECOVERY_ACCEPTANCE.md). It covers signed challenge/decision binding, stale generation and quarantine rejection, trust-anchor hardening, no-clobber local handoff, persistence-gated Hub resolution, idempotent identical delivery, conflicting-decision rejection, and restart/no-replay semantics.

Hosted macOS CI is sufficient for Security.framework compile/link and protocol/state-machine tests, but it is not evidence that a physical Secure Enclave key produced the intended deployment-user Touch ID/password/Apple Watch user-presence interaction. Run the trusted physical Mac acceptance before enabling the online path in a release.
''',
)

append_once(
    "packaging/README.md",
    "## macOS local-user online quarantine recovery",
    '''## macOS local-user online quarantine recovery

The macOS Agent remains a LaunchAgent in the interactive login session; online recovery does not add a daemon or a second service supervisor. Install the `v2_recover` binary alongside `v2_agent`. Its local challenge/authorization handoff uses the same `CUMG_V2_STATE_DIR` configured for the LaunchAgent.

Initialize the recovery key once as the Agent's logged-in user:

```bash
v2_recover init-key \\
  --public-key-out "$HOME/Library/Application Support/cumg-v2-agent/recovery-public-key.p256"
```

The private P-256 key remains in the Secure Enclave and requires user presence for signing. `init-key` is create-new and refuses an existing label. Move only the exported public key through the operator-authenticated provisioning channel and install it as `<HUB_STATE_DIR>/recovery-public-key.p256` with reviewed ownership/permissions. Restart the Hub so it explicitly loads the new recovery verifier.

When a Hub-signed challenge is present, use `v2_recover status` and then `v2_recover resolve` as documented in [`../docs/v2/V2_ONLINE_RECOVERY.md`](../docs/v2/V2_ONLINE_RECOVERY.md). Keep `v2_maint` available for offline break-glass recovery.
''',
)

print("final online recovery hardening/docs patch applied")
