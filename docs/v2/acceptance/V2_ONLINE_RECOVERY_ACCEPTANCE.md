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

Run this only on an operator-controlled Mac whose Agent already has the reviewed Cua/TCC permissions. Do not substitute the GitHub hosted macOS runner: hosted CI can compile/link Security.framework but cannot prove the intended physical user-presence interaction for the deployment user.

1. Provision a **new** Secure Enclave recovery key using `v2_recover init-key`; confirm a second init using the same label fails closed.
2. Install only its public key as `<HUB_STATE_DIR>/recovery-public-key.p256` with safe ownership/permissions, then restart the Hub to load it.
3. Start Hub and Agent and record the authenticated current generation.
4. Trigger a reviewed mutating Cua operation whose outcome can intentionally become ambiguous, producing durable `Indeterminate` plus `DesktopQuarantine`.
5. Confirm a different normal operation is blocked and that reconnect/generation advancement does not itself clear quarantine.
6. Keep the Hub running. Confirm the Agent receives a fresh Hub-signed recovery challenge bound to the old quarantine generation and the new current Agent generation.
7. Run `v2_recover status`; verify the displayed device, operation, generations and expiry match the quarantined operation and that audit assessment is `inconclusive`.
8. Let one challenge expire while quarantine remains and confirm an authenticated heartbeat causes a fresh nonce-bound challenge to arrive without restarting Hub or Agent; confirm the stale challenge is no longer usable.
9. Run `v2_recover resolve` once and **deny/cancel** the macOS user-presence prompt. Confirm no authorization is accepted and Hub quarantine remains durable.
10. Run it again, inspect the actual desktop, choose the correct exact decision, and complete the macOS user-presence prompt.
11. Confirm the Hub verifies the separately pinned recovery key, durably commits the resolution, then emits the Hub-signed `RecoveryResolved` acknowledgement.
12. Confirm the Agent removes the local challenge/authorization handoff only after that acknowledgement.
13. Confirm a new operation ID may now execute.
14. Confirm the old ambiguous operation was not resumed/replayed and cannot be re-admitted under the old operation ID.
15. Simulate loss of the success acknowledgement while keeping the live session; resend the **identical signed authorization** and confirm only an idempotent acknowledgement is returned, with one resolution audit record.
16. Attempt a changed authorization with the same request ID (for example a conflicting decision or changed evidence) and confirm rejection without changing the durable receipt/resolution.
17. Restart Hub and Agent; confirm quarantine remains resolved, the old operation remains terminal/non-replayable, and stale local handoff does not become authority.

Record the physical Mac model/macOS version, Cua version, commit SHA, exact acceptance command, and pass/fail evidence in the release closeout. Do not record screenshots, raw commands/results, credentials, private keys, or other desktop payloads.
