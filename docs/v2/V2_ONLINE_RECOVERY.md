# V2 local-user-authorized online recovery

> English is the canonical documentation. [日本語版 / Japanese translation](V2_ONLINE_RECOVERY.ja.md)

Status: **implemented behind explicit recovery-key provisioning; physical Secure Enclave user-presence acceptance remains a release gate.**

This document defines the online recovery path for a desktop that entered durable `Indeterminate` quarantine. It removes the normal operational need to stop the Hub and run offline maintenance, without weakening the existing no-auto-replay or persistence-gated recovery boundary.

## Security boundary

The Agent device identity is **not** recovery authority. A compromised Agent may lie about local desktop/backend state, so possession of the Agent device key must never be enough to clear quarantine.

Online recovery therefore uses a separately provisioned endpoint recovery key. On macOS a small stable-signed CryptoKit helper creates a Secure Enclave P-256 key protected by `userPresence + privateKeyUsage`. Only the Secure Enclave sealed `dataRepresentation` is persisted, in an owner-private file managed by CUMG; the private key itself never leaves the Secure Enclave. The Hub stores only the P-256 public verifier. Initial provisioning uses create-new file semantics, and both the sealed-key file and Hub public-key file are validated with strict path/symlink/permission rules.

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

The challenge lifetime is 300 seconds (5 minutes). Reconnect/generation change invalidates the local handoff and requires a fresh Hub challenge. Historical operation generation and current authenticated generation are deliberately distinct: generation is a stale-session fence, not recovery ownership.

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

Build/install `v2_recover` and the stable-signed `v2_recovery_enclave_helper` alongside `v2_agent`. From the logged-in Agent user account, create an owner-private recovery directory and initialize a new Secure Enclave recovery key once:

```bash
install -d -m 700 "$HOME/Library/Application Support/cumg-v2-agent/recovery"
v2_recover init-key \
  --key-file "$HOME/Library/Application Support/cumg-v2-agent/recovery/recovery-key.sealed" \
  --secure-enclave-helper "$HOME/Library/Application Support/computer-use-mcp-gateway/bin/v2_recovery_enclave_helper" \
  --public-key-out "$HOME/Library/Application Support/cumg-v2-agent/recovery/recovery-public-key.p256"
```

`init-key` uses create-new semantics and refuses an existing sealed-key path. The sealed file is not a software private key: it is the bounded Secure Enclave representation required to re-open the non-exportable key on this Mac. Keep it owner-private and local. Transfer only the exported public key through the operator-authenticated provisioning channel to the Hub and install it as:

```text
<HUB_STATE_DIR>/recovery-public-key.p256
```

The Hub state directory and public-key file must satisfy the existing trust-anchor parent/symlink/permission checks. Restarting the Hub after first provisioning is required because the verifier is loaded at Hub startup. Removing the verifier disables online recovery; it does not weaken quarantine semantics.

When quarantine occurs, the connected Agent receives a fresh challenge. The canonical operator workflow is `v2_recover guide`:

```bash
v2_recover guide \
  --hub-state-dir "<HUB_STATE_DIR>" \
  --agent-state-dir "$HOME/Library/Application Support/cumg-v2-agent/state" \
  --hub-public-key-file "$HOME/Library/Application Support/cumg-v2-agent/trust/hub.pub" \
  --key-file "$HOME/Library/Application Support/cumg-v2-agent/recovery/recovery-key.sealed" \
  --secure-enclave-helper "$HOME/Library/Application Support/computer-use-mcp-gateway/bin/v2_recovery_enclave_helper" \
  --mutation-authority-dir "<MUTATION_AUTHORITY_DIR>" \
  --wait-secs 60
```

The guided path is orchestration only. It first verifies the Hub-signed challenge, builds the existing #233 incident brief, and copies `IncidentBrief.cumg.supported_decisions` unchanged into a pure recovery plan. Observational diagnostics can be displayed as explanation but cannot create or widen a supported decision. An empty authoritative decision set terminates as `keep_quarantine`; no user-presence signing or authorization publication occurs.

When an authoritative decision is available, `guide` requires an interactive Human terminal and accepts only an explicit selection from that closed set (or cancellation). Agent/LLM input cannot be piped into the authority-bearing path. After the Human has reviewed the incident, the CLI immediately re-reads the exact signed challenge and incident brief. Any operation/device/original-generation/current-generation/fingerprint/nonce binding change, incident-state change, or supported-decision change fails closed and requires a new review before signing.

Only after that fresh validation does the existing Secure Enclave helper request macOS user presence. Denial, cancellation, timeout, or an unavailable authentication facility does not publish authorization and leaves quarantine intact. The guided authorization remains the same reviewed online-recovery protocol; no new recovery authority or replay path is introduced.

`authorization=published` is not completion. Guided recovery always waits for the exact Hub-signed `RecoveryResolved` acknowledgement using #109 semantics. The request/device/current-generation/operation/decision binding must verify, after which the workflow reports `durable_completion=verified` and `old_operation_replayed=false`. It then performs a read-only exact quarantine check and invokes the existing `v2_status` JSON surface. A healthy status yields `recovery_outcome=verified_healthy`; if recovery completed durably but an unrelated Handoff/runtime/mutation-authority/backend/recovery-mode problem remains, the workflow reports `recovery_outcome=verified_with_unrelated_status_problem` and exposes the bounded `v2_status` reason rather than erasing the durable recovery result.

For Agent-assisted explanation or UI composition, the same command has a strictly read-only JSON planning mode:

```bash
v2_recover guide \
  --hub-state-dir "<HUB_STATE_DIR>" \
  --agent-state-dir "$HOME/Library/Application Support/cumg-v2-agent/state" \
  --hub-public-key-file "$HOME/Library/Application Support/cumg-v2-agent/trust/hub.pub" \
  --json
```

JSON mode never prompts, signs, publishes, clears quarantine, or replays work. It excludes the private challenge fingerprint/nonce and credential/key/raw-command/argv/clipboard/screenshot/principal material. It is therefore suitable for an Agent/UI to explain what CUMG currently supports without allowing that Agent/UI to make the recovery decision.

The lower-level commands remain available for advanced diagnostics and break-glass operation:

```bash
v2_recover status \
  --state-dir "$HOME/Library/Application Support/cumg-v2-agent/state" \
  --hub-public-key-file "$HOME/Library/Application Support/cumg-v2-agent/trust/hub.pub"

v2_recover resolve \
  --state-dir "$HOME/Library/Application Support/cumg-v2-agent/state" \
  --hub-public-key-file "$HOME/Library/Application Support/cumg-v2-agent/trust/hub.pub" \
  --key-file "$HOME/Library/Application Support/cumg-v2-agent/recovery/recovery-key.sealed" \
  --secure-enclave-helper "$HOME/Library/Application Support/computer-use-mcp-gateway/bin/v2_recovery_enclave_helper" \
  --decision confirmed-completed \
  --evidence "local user inspected the current desktop" \
  --wait-secs 30
```

`resolve` still requires macOS user presence, and `confirm` can re-check an already-published request using its exact safe metadata. A missing acknowledgement, timeout, stale receipt, or mismatch is not success and does not authorize retry/replay. These low-level commands do not supersede the normal `guide` flow.

## Key lifecycle and failure cases

Recovery-key rotation is a separately reviewed administrative trust change. Do not silently replace `<HUB_STATE_DIR>/recovery-public-key.p256` while relying on a running Hub; stage and review the replacement, then restart the Hub so the new verifier becomes explicit process state.

Loss of the sealed Secure Enclave recovery-key representation does not make the desktop reusable. Reprovision a new recovery key through the authenticated administrative channel or use the existing offline maintenance path. Suspicion that the Agent/device key is compromised does not authorize recovery because the recovery key is independent.

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
- duplicate accepted-request acknowledgement and conflicting/stale authorization rejection;
- owner-private durable acknowledgement receipt, exact CLI confirmation binding, and publication-vs-completion distinction.

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

Online recovery is part of the current `HUB_AGENT_SCHEMA_VERSION = 4` application protocol. Schema validation remains fail-closed, so a deployment enabling this release must upgrade Hub and Agent as a coordinated pair rather than relying on mixed-version rolling compatibility. V1 gateway behavior is unchanged.

A recovery challenge expires after 300 seconds (5 minutes). While the desktop remains quarantined, normal authenticated Agent heartbeats cause the Hub to re-check the pending challenge and issue a fresh nonce-bound challenge after expiry. An operator therefore does not need to restart the Hub or Agent merely because a local approval window elapsed. Receiving a fresh challenge invalidates the prior local authorization handoff.
