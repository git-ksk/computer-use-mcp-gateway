from pathlib import Path


def replace_once(path: str, old: str, new: str) -> None:
    p = Path(path)
    text = p.read_text()
    count = text.count(old)
    if count != 1:
        raise SystemExit(f"{path}: expected one match, got {count}: {old[:160]!r}")
    p.write_text(text.replace(old, new, 1))


def append_once(path: str, marker: str, text: str) -> None:
    p = Path(path)
    current = p.read_text()
    if marker in current:
        return
    if not current.endswith("\n"):
        current += "\n"
    p.write_text(current + "\n" + text.rstrip() + "\n")

# Duplicate acknowledgement is valid only for the exact signed authorization
# already accepted in this live exchange, not merely a matching request id and
# decision subset.
replace_once(
    "src/v2_m1_hub.rs",
    '''#[derive(Default)]
struct RecoveryRuntimeState {
    pending: Option<RecoveryChallenge>,
    last_resolved: Option<RecoveryResolved>,
}
''',
    '''#[derive(Default)]
struct RecoveryRuntimeState {
    pending: Option<RecoveryChallenge>,
    last_resolved: Option<(RecoveryAuthorization, RecoveryResolved)>,
}
''',
)
replace_once(
    "src/v2_m1_hub.rs",
    '''            let duplicate = runtime
                .last_resolved
                .as_ref()
                .filter(|resolved| resolved.request_id == authorization.request_id)
                .cloned();
            (runtime.pending.clone(), duplicate)
        };
        if let Some(ack) = duplicate_ack {
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
    '''            let duplicate = runtime
                .last_resolved
                .as_ref()
                .filter(|(accepted, _)| accepted.request_id == authorization.request_id)
                .cloned();
            (runtime.pending.clone(), duplicate)
        };
        if let Some((accepted, ack)) = duplicate_ack {
            if accepted != authorization {
                return Err(HubServiceError::OnlineRecovery(
                    RecoveryError::ChallengeMismatch,
                ));
            }
            send_hub(outbound, HubToAgent::RecoveryResolved(ack)).await?;
            return Ok(());
        }
''',
)
replace_once(
    "src/v2_m1_hub.rs",
    '''            runtime.pending = None;
            runtime.last_resolved = Some(ack.clone());
''',
    '''            runtime.pending = None;
            runtime.last_resolved = Some((authorization.clone(), ack.clone()));
''',
)

# Heartbeats provide a natural bounded refresh point. If a quarantine remains
# and the 120s challenge expired, maybe_send_recovery_challenge issues a fresh
# nonce-bound challenge without requiring Hub or Agent restart.
replace_once(
    "src/v2_m1_hub.rs",
    '''                            send_hub(&outbound, HubToAgent::HeartbeatAck(ack)).await?;
                            heartbeat_deadline.as_mut().reset(tokio::time::Instant::now() + self.inner.config.heartbeat_timeout);
''',
    '''                            send_hub(&outbound, HubToAgent::HeartbeatAck(ack)).await?;
                            heartbeat_deadline.as_mut().reset(tokio::time::Instant::now() + self.inner.config.heartbeat_timeout);
                            self.maybe_send_recovery_challenge(
                                &outbound,
                                generation,
                                session_clock,
                            )
                            .await?;
''',
)

# Strengthen the idempotency regression: same request id but any changed signed
# payload (not just decision) is conflicting.
replace_once(
    "src/v2_m1_hub_online_recovery_tests.rs",
    '''    assert_eq!(handle.resolution_records().await.len(), 1);

    // The durable resolved operation remains a replay tombstone after restart.
''',
    '''    assert_eq!(handle.resolution_records().await.len(), 1);

    let mut changed_evidence = authorization.clone();
    changed_evidence.evidence = "different local evidence".into();
    assert!(matches!(
        hub.handle_recovery_authorization(
            changed_evidence,
            &outbound,
            current_generation,
            &clock,
        )
        .await,
        Err(HubServiceError::OnlineRecovery(RecoveryError::ChallengeMismatch))
    ));
    assert_eq!(handle.resolution_records().await.len(), 1);

    // The durable resolved operation remains a replay tombstone after restart.
''',
)

append_once(
    "docs/v2/V2_ONLINE_RECOVERY.md",
    "## Protocol compatibility and challenge renewal",
    '''## Protocol compatibility and challenge renewal

Online recovery adds Hub-Agent message variants and advances `HUB_AGENT_SCHEMA_VERSION` from 1 to 2. Schema validation remains fail-closed, so a deployment enabling this release must upgrade Hub and Agent as a coordinated pair rather than relying on mixed-version rolling compatibility. V1 gateway behavior is unchanged.

A recovery challenge expires after 120 seconds. While the desktop remains quarantined, normal authenticated Agent heartbeats cause the Hub to re-check the pending challenge and issue a fresh nonce-bound challenge after expiry. An operator therefore does not need to restart the Hub or Agent merely because a local approval window elapsed. Receiving a fresh challenge invalidates the prior local authorization handoff.
''',
)
append_once(
    "docs/v2/V2_ONLINE_RECOVERY.ja.md",
    "## Protocol互換性とchallenge更新",
    '''## Protocol互換性とchallenge更新

Online recoveryではHub-Agent message variantを追加し、`HUB_AGENT_SCHEMA_VERSION` を1から2へ進めます。schema validationはfail-closedのままなので、このreleaseを有効にする際はHubとAgentを協調して更新し、mixed-version rolling compatibilityには依存しません。V1 gatewayの動作は変わりません。

Recovery challengeは120秒で期限切れになります。desktopがquarantineのままなら、通常のauthenticated Agent heartbeatを契機にHubがpending challengeを再確認し、期限切れ後はfresh nonceを持つchallengeを再発行します。承認時間切れだけを理由にHub/Agentを再起動する必要はありません。fresh challenge受信時は以前のlocal authorization handoffを無効化します。
''',
)
append_once(
    "docs/DEPLOYMENT.md",
    "### Online recovery upgrade compatibility",
    '''### Online recovery upgrade compatibility

The online recovery transport advances `HUB_AGENT_SCHEMA_VERSION` from 1 to 2. Hub-Agent schema mismatch is rejected fail-closed, so deploy the corresponding v0.3 Hub and Agent as a coordinated upgrade; do not assume mixed-version rolling operation across this boundary. This requirement is limited to the V2 Hub-Agent application protocol and does not change V1 gateway compatibility.
''',
)

print("final online recovery review fixes applied")
