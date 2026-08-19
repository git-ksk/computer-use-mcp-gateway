# V2 local-user-authorized online recovery

> English is the canonical documentation. [日本語版 / Japanese translation](V2_ONLINE_RECOVERY.ja.md)

Status: **implemented behind explicit recovery-key provisioning; physical Secure Enclave user-presence acceptance remains a release gate.**

This document defines the online recovery path for a desktop that entered durable `Indeterminate` quarantine. It removes the normal operational need to stop the Hub and run offline maintenance, without weakening the existing no-auto-replay or persistence-gated recovery boundary.

## Security boundary

The Agent device identity is **not** recovery authority. A compromised Agent may lie about local desktop/backend state, so possession of the Agent device key must never be enough to clear quarantine.

Online recovery therefore uses a separately provisioned endpoint recovery key. On macOS the private key is generated in the Secure Enclave and protected by Keychain access control requiring user presence plus private-key use. The Hub stores only the P-256 public verifier. Initial provisioning refuses to reuse an existing key label, and the Hub validates the public-key file with the same no-symlink / safe-permissions checks used for other public trust anchors.

The initial implementation is intentionally macOS-only for local approval. Windows and Linux do not gain a weaker software-key substitute; they continue to use the existing explicit offline maintenance path until an equivalent reviewed user-presence provider exists.

## Protocol

```text
Hub durable quarantine
        |
        | Hub-signed fresh challenge
        v
Agent verifies Hub identity/current generation
        |
        | private local handoff
        v
cumg-v2-recover status / local inspection
        |
        | exact user decision
        | Secure Enclave user-presence signature
        v
Agent relays signed RecoveryAuthorization
        |
        v
Hub verifies recovery key + challenge + quarantine CAS
        |
        | resolve_indeterminate + durable checkpoint
        v
Hub-signed RecoveryResolved acknowledgement
        |
        v
Agent removes local handoff; only new operation IDs may run
```

A challenge is bound to:

- stable device ID;
- exact ambiguous operation ID;
- the historical generation in which that operation became quarantined;
- the current authenticated Agent generation allowed to relay the authorization;
- a SHA-256 fingerprint of the current quarantine record;
- a fresh 32-byte nonce;
- issue and expiry timestamps.

The challenge lifetime is 120 seconds. Reconnect/generation change invalidates the local handoff and requires a fresh Hub challenge. Historical operation generation and current authenticated generation are deliberately distinct: generation is a stale-session fence, not recovery ownership.

The local authorization binds the exact challenge plus:

- a random recovery request ID;
- audit assessment;
- one explicit decision: `confirmed_completed` or `confirmed_not_executed`;
- bounded evidence metadata (maximum 1 KiB);
- a P-256/SHA-256 signature from the separately provisioned recovery key.

Changing the decision, operation, device, generation, fingerprint, nonce, expiry, assessment, or evidence after signing invalidates the signature or challenge match.

## Audit assessment and privacy

The normal V2 checkpoint deliberately excludes raw GUI commands, screenshots, backend responses, clipboard data, credentials, and other high-sensitivity payloads. A generic post-hoc Agent audit therefore cannot safely prove whether an arbitrary GUI side effect completed.

The initial local recovery CLI reports `audit_assessment=inconclusive`. The local user inspects the current desktop and explicitly chooses the resolution. This is preferable to manufacturing a false automatic proof. Future capability-specific audit providers may return `completed` or `not_executed` only when they can establish that state without widening the persisted privacy boundary.

Evidence is metadata only. It must not contain screenshots, raw command/result payloads, credentials, typed secrets, or unrelated desktop content.

## Hub resolution rules

The Hub accepts an online authorization only when all of the following remain true:

1. a recovery verifier was explicitly provisioned;
2. the current live Agent generation equals the generation in the challenge/authorization;
3. the challenge is unexpired;
4. the P-256 signature verifies;
5. stable device ID and exact operation ID match;
6. historical quarantine generation matches;
7. the current durable quarantine fingerprint is unchanged;
8. evidence is bounded and the request shape is canonical.

Resolution reuses the existing `resolve_indeterminate` state transition. The Hub snapshots the fail-closed execution state, applies the resolution, and persists the new checkpoint. If persistence fails, the in-memory execution controller is restored to the quarantined snapshot and no success acknowledgement is sent.

A successful resolution never resumes or replays the old operation. Even `confirmed_not_executed` only reopens the desktop for a **new** operation ID.

A duplicate delivery of the same accepted request ID in the same live recovery exchange is acknowledged idempotently. A conflicting decision is not allowed to replace a prior resolution.

## Local handoff

The Agent state directory contains only short-lived recovery handoff files:

- `recovery-challenge.json` — Hub-signed challenge;
- `recovery-authorization.json` — local-user-signed authorization awaiting relay.

The files are bounded and private. Authorization publication is create/no-clobber so a second local decision cannot silently overwrite a pending one. The Agent verifies challenge consistency before relay and removes the handoff only after a valid Hub-signed resolution acknowledgement. A fresh authenticated generation clears stale handoff state.

These files are not execution authority: the Hub's durable quarantine/checkpoint remains authoritative.

## macOS provisioning and use

Build/install the `v2_recover` binary alongside `v2_agent`. From the logged-in Agent user account, initialize a new Secure Enclave recovery key once:

```bash
v2_recover init-key \
  --public-key-out "$HOME/Library/Application Support/cumg-v2-agent/recovery-public-key.p256"
```

`init-key` uses create-new semantics and refuses an existing recovery-key label. Transfer only the exported public key through the operator-authenticated provisioning channel to the Hub and install it as:

```text
<HUB_STATE_DIR>/recovery-public-key.p256
```

The Hub state directory and public-key file must satisfy the existing trust-anchor parent/symlink/permission checks. Restarting the Hub after first provisioning is required because the verifier is loaded at Hub startup. Removing the verifier disables online recovery; it does not weaken quarantine semantics.

When quarantine occurs, the connected Agent receives a fresh challenge. On the Mac:

```bash
v2_recover status \
  --state-dir "$HOME/Library/Application Support/cumg-v2-agent/state" \
  --hub-public-key-file "$HOME/Library/Application Support/cumg-v2-agent/trust/hub.pub"
```

Inspect the actual desktop, then approve exactly one decision:

```bash
v2_recover resolve \
  --state-dir "$HOME/Library/Application Support/cumg-v2-agent/state" \
  --hub-public-key-file "$HOME/Library/Application Support/cumg-v2-agent/trust/hub.pub" \
  --decision confirmed-completed \
  --evidence "local user inspected the current desktop"
```

The signing operation requires macOS user presence. Cancellation/denial leaves quarantine in place.

## Key lifecycle and failure cases

Recovery-key rotation is a separately reviewed administrative trust change. Do not silently replace `<HUB_STATE_DIR>/recovery-public-key.p256` while relying on a running Hub; stage and review the replacement, then restart the Hub so the new verifier becomes explicit process state.

Loss of the recovery private key does not make the desktop reusable. Reprovision a new recovery key through the authenticated administrative channel or use the existing offline maintenance path. Suspicion that the Agent/device key is compromised does not authorize recovery because the recovery key is independent.

If the Agent cannot connect, the Secure Enclave key is unavailable, user presence cannot be completed, or the online protocol itself is damaged, retain `v2_maint` as the break-glass path. The offline resolver remains an explicit, persistence-gated administrative action.

## Acceptance requirements

Automated coverage must include:

- Hub challenge signature and expiry;
- stable device/current-generation/historical-generation binding;
- exact decision/signature binding;
- changed quarantine fingerprint rejection;
- recovery public-key trust-anchor permission/symlink rejection;
- authorization create/no-clobber behavior;
- bounded/non-empty evidence;
- durable Hub resolution and no old-operation replay;
- duplicate accepted-request acknowledgement and conflicting/stale authorization rejection.

Before enabling the macOS online path in a release, a trusted physical Mac must additionally prove:

1. an ambiguous desktop operation enters durable quarantine;
2. Agent reconnect does not clear it;
3. the Hub remains running;
4. the challenge is visible only to the local user state directory;
5. `v2_recover resolve` produces an actual macOS user-presence prompt backed by the Secure Enclave key;
6. denying user presence leaves quarantine unchanged;
7. approving the exact decision causes a durable Hub resolution and signed acknowledgement;
8. a new operation succeeds afterward;
9. the old ambiguous operation is never replayed.

## Protocol compatibility and challenge renewal

Online recovery adds Hub-Agent message variants and advances `HUB_AGENT_SCHEMA_VERSION` from 1 to 2. Schema validation remains fail-closed, so a deployment enabling this release must upgrade Hub and Agent as a coordinated pair rather than relying on mixed-version rolling compatibility. V1 gateway behavior is unchanged.

A recovery challenge expires after 120 seconds. While the desktop remains quarantined, normal authenticated Agent heartbeats cause the Hub to re-check the pending challenge and issue a fresh nonce-bound challenge after expiry. An operator therefore does not need to restart the Hub or Agent merely because a local approval window elapsed. Receiving a fresh challenge invalidates the prior local authorization handoff.
