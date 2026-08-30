# V2 online recovery acceptance

Status: **automated gate complete on the refreshed PR head; trusted physical macOS Secure Enclave acceptance remains required before release.**

Canonical contract: [`../V2_ONLINE_RECOVERY.md`](../V2_ONLINE_RECOVERY.md).

## Automated acceptance

The permanent automated suite must prove:

- Hub-signed challenge verification and expiry;
- an expired challenge is renewed on authenticated Agent heartbeat without Hub/Agent restart;
- exact stable-device and current-generation binding;
- separate historical quarantine generation binding;
- current quarantine fingerprint/CAS binding;
- exact local-user decision and evidence included in the P-256 signature;
- stale/tampered authorization rejection;
- no-clobber local authorization publication;
- public recovery verifier symlink/unsafe-permission rejection;
- persistence-gated `resolve_indeterminate` transition;
- idempotent delivery only for the exact same signed authorization already accepted in the live exchange;
- rejection of any altered authorization that reuses an accepted request ID, including a conflicting decision or changed evidence;
- restart preserves the resolved tombstone and does not make the old operation replayable;
- existing offline `v2_maint` recovery remains available as break-glass.

The ordinary project gates (`fmt`, `check --locked --all-targets`, tests, clippy `-D warnings`, documentation/link checks, passthrough contract) remain required because online recovery changes the Hub-Agent application schema.

## Trusted physical Mac acceptance

Run this only on an operator-controlled Mac whose Agent already has the reviewed Cua/TCC permissions. Do not substitute the GitHub hosted macOS runner: hosted CI can compile the CryptoKit helper but cannot prove the intended physical user-presence interaction for the deployment user.

The permanent isolated harness is `tests/v2_online_recovery_physical.rs`. It does not install the PR build into the live single-Mac profile and it never creates a Secure Enclave key by itself. Provision the reviewed recovery key first, pass only its exported public key to the harness, then run the ignored test with an explicit new absolute acceptance root and `CUMG_V2_ONLINE_RECOVERY_E2E_ACK=1`. When the harness prints `ONLINE_RECOVERY_PHYSICAL_READY`, run the PR-head `v2_recover status` / `resolve` against the printed temporary Agent state and Hub public-key file.

Example isolated invocation (use a fresh owner-private directory and the reviewed PR-head binaries):

```bash
ACCEPT_ROOT="$(mktemp -d /tmp/cumg-online-recovery-acceptance.XXXXXX)"
chmod 700 "$ACCEPT_ROOT"
RECOVERY_ROOT="$ACCEPT_ROOT/recovery"
mkdir -m 700 "$RECOVERY_ROOT"

./target/release/v2_recover init-key \
  --key-file "$RECOVERY_ROOT/recovery-key.sealed" \
  --secure-enclave-helper "$PWD/target/release/v2_recovery_enclave_helper" \
  --public-key-out "$RECOVERY_ROOT/recovery-public-key.p256"

CUMG_V2_ONLINE_RECOVERY_E2E_ACK=1 \
CUMG_V2_ONLINE_RECOVERY_ACCEPTANCE_ROOT="$ACCEPT_ROOT/runtime" \
CUMG_V2_ONLINE_RECOVERY_PUBLIC_KEY_FILE="$RECOVERY_ROOT/recovery-public-key.p256" \
cargo test --locked --test v2_online_recovery_physical \
  physical_secure_enclave_online_recovery_never_replays_ambiguous_cua_operation \
  -- --ignored --nocapture
```

Leave that test running after `ONLINE_RECOVERY_PHYSICAL_READY`. In another terminal, use the printed `state_dir` and `hub_public_key_file` with `v2_recover status`, then call `resolve` with the same `--key-file` and `--secure-enclave-helper`. First deny/cancel the user-presence prompt and confirm the harness remains quarantined; then repeat `resolve` and approve the exact reviewed decision. The acceptance root contains no reusable production authority and may be removed only after the test has completed and evidence has been recorded.

The Secure Enclave helper is a bounded subprocess. `v2_recover` allows at most 60 seconds for helper completion; a helper that stops responding is killed and reaped and the CLI returns `recovery_helper_timeout`. A timeout, user denial/cancellation (`recovery_user_presence_denied`), unavailable LocalAuthentication (`recovery_helper_auth_unavailable`), malformed helper response, or abnormal helper exit never publishes an authorization and never changes Hub quarantine. After any such failure, obtain/verify the current challenge again with `v2_recover status` before making a fresh explicit recovery attempt. Do not infer either recovery decision from the timeout itself.

1. Provision a **new** Secure Enclave recovery key using `v2_recover init-key` with an owner-private absolute `--key-file` and the reviewed stable-signed `--secure-enclave-helper`; confirm a second init using the same key-file path fails closed.
2. Install only its public key as `<HUB_STATE_DIR>/recovery-public-key.p256` with safe ownership/permissions, then restart the Hub to load it.
3. Start Hub and Agent and record the authenticated current generation.
4. Trigger a reviewed mutating Cua operation whose outcome can intentionally become ambiguous, producing durable `Indeterminate` plus `DesktopQuarantine`.
5. Confirm a different normal operation is blocked and that reconnect/generation advancement does not itself clear quarantine.
6. Keep the Hub running. Confirm the Agent receives a fresh Hub-signed recovery challenge bound to the old quarantine generation and the new current Agent generation.
7. Run `v2_recover status`; verify the displayed device, operation, generations and expiry match the quarantined operation and that audit assessment is `inconclusive`.
8. Let one challenge expire while quarantine remains and confirm an authenticated heartbeat causes a fresh nonce-bound challenge to arrive without restarting Hub or Agent; confirm the stale challenge is no longer usable.
9. Run `v2_recover resolve` once and **deny/cancel** the macOS user-presence prompt. Confirm `recovery_user_presence_denied`, no authorization is accepted, no helper remains, and Hub quarantine remains durable.
10. Run a deliberately abandoned/no-response helper acceptance attempt and leave the authorization prompt unanswered. Confirm the CLI returns `recovery_helper_timeout` within the 60-second bound, the helper is terminated/reaped, no authorization is published, and the same quarantine remains visible to `v2_doctor` / `v2_recover status`.
11. Run it again with a fresh valid challenge, inspect the actual desktop, choose the correct exact decision, and complete the macOS user-presence prompt.
12. Confirm the Hub verifies the separately pinned recovery key, durably commits the resolution, then emits the Hub-signed `RecoveryResolved` acknowledgement.
13. Confirm the Agent removes the local challenge/authorization handoff only after that acknowledgement.
14. Confirm a new operation ID may now execute.
15. Confirm the old ambiguous operation was not resumed/replayed and cannot be re-admitted under the old operation ID.
16. Simulate loss of the success acknowledgement while keeping the live session; resend the **identical signed authorization** and confirm only an idempotent acknowledgement is returned, with one resolution audit record.
17. Attempt a changed authorization with the same request ID (for example a conflicting decision or changed evidence) and confirm rejection without changing the durable receipt/resolution.
18. Restart Hub and Agent; confirm quarantine remains resolved, the old operation remains terminal/non-replayable, and stale local handoff does not become authority.

Record the physical Mac model/macOS version, Cua version, commit SHA, exact acceptance command, and pass/fail evidence in the release closeout. Do not record screenshots, raw commands/results, credentials, private keys, or other desktop payloads.
