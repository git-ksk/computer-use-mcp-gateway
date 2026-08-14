# V2 threat model

Status: **V2-M1 trust-model baseline**. M0 assumptions remain the foundation; M1 transport/process controls and residual deployment gaps are reflected below.

This document defines the security claims and non-claims for the delegated device capability control plane. It is intentionally stricter than a feature list: a control is not considered effective against a component that owns the key or execution surface needed to bypass that control.

## Security objectives

V2 should provide these properties when the Hub, Agent, and backend are operating as designed:

1. an Agent proves possession of the currently enrolled device key before a Hub accepts the connection;
2. an Agent authenticates a pinned Hub transport identity before accepting Hub commands;
3. an authenticated northbound client principal may receive a grant only for explicitly authorized device/capability pairs; M1 Agent-native operations use exact `DeviceCapability` scope rather than class-only authority;
4. a short-lived grant cannot be widened from one semantic capability class or exact M1 device capability to another;
5. stale device generations, stale capability revisions, consumed/revoked/expired grants, and unknown signing keys fail closed;
6. Hub and Agent key rotation require continuity proof rather than silent key replacement;
7. one device executes at most one operation at a time, while Hub admission and per-device queues remain bounded;
8. cancellation, disconnect, or ambiguous completion never causes automatic replay of a state-changing operation;
9. normal audit evidence does not contain raw screenshots, command arguments, backend results, clipboard values, or credentials;
10. backend-specific names and response formats stop at the Agent adapter boundary.

## Trust boundaries

```text
authenticated MCP client principal
        |
        | northbound authn result + Hub authorization policy
        v
+---------------- Hub ----------------+
| client policy                        |
| grant-signing authority              |
| transport identity                   |
| admission / lease / audit state      |
+--------------------------------------+
        |
        | signed, versioned Hub-Agent messages
        | + production confidentiality required
        v
+--------------- Agent ----------------+
| pinned Hub trust / device identity   |
| grant verifier set                   |
| single-operation execution gate      |
| backend adapter                      |
+--------------------------------------+
        |
        v
computer-use backend (Cua first)
        |
        v
operating system / desktop / user data
```

Northbound client authentication, Hub transport identity, grant-signing authority, and Agent device identity are separate credentials. Possession of one credential does not intentionally imply possession of the others.

## Assets

High-value assets include:

- the desktop session and any data visible or controllable through it;
- Agent device private keys;
- Hub transport private keys;
- Hub grant-signing private keys;
- northbound client authentication state and authorization mappings;
- operation/lease state used to prevent conflicting or replayed actions;
- capability advertisements and generation/revision state;
- audit evidence and revocation state.

Raw screenshots, raw backend responses, and command arguments are intentionally **not** normal control-plane audit assets because retaining them increases privacy impact.

## Threats by compromised component

### Malicious or compromised MCP client

A client may attempt to select another device, request a stronger capability, replay a prior grant, flood the Hub, or exploit backend-specific parameters.

Controls:

- the Hub consumes an already-authenticated principal identity rather than trusting a client-supplied device identity as authorization;
- M0 supports `principal -> device -> capability class`; M1 Agent-native operations additionally support exact `principal -> device -> DeviceCapability` authorization and reject class-only grants at the Agent boundary;
- the client does not receive Agent device keys or Hub transport keys;
- grants are signed, device-bound, one-shot, and Agent-enforced to a maximum five-minute lifetime; M1 Agent-native grants are also bound to the exact device capability;
- global admission and per-device queues are bounded;
- the Hub-Agent protocol exposes typed semantic commands rather than arbitrary Cua tool names/arguments.

The M1 northbound boundary now implements the MCP Authorization protected-resource side rather than a new OAuth server. `v2_hub` publishes RFC 9728 metadata, requires header bearer presentation, validates tokens through a configured RFC 7662 introspection endpoint, binds accepted tokens to the configured MCP resource audience, and constructs `AuthenticatedClientPrincipal` from the verified subject plus the configured authorization-server issuer. Required OAuth scopes gate entry to the MCP resource, while the separate local principal -> device -> exact `DeviceCapability` policy remains authoritative for delegated device access. The bearer header is removed before rmcp handler dispatch and no bearer field exists in the typed Hub-to-Agent command/grant path.

Residual risk: authorization-server/introspection availability and credential compromise remain deployment trust dependencies; public HTTPS termination and rate limiting still sit outside the current loopback northbound listener. A compromised Hub can still mint valid Agent grants using its own grant authority, so OAuth does not reduce the already-documented fully-compromised-Hub trust failure.

### Compromised Hub

A fully compromised Hub is a **high-severity trust failure**. The Hub normally controls authorization, admission, transport signing, and grant issuance. If an attacker obtains both the active Hub transport key and grant-signing authority, the Agent cannot distinguish those commands from authorized Hub commands.

Controls that still reduce blast radius or aid recovery:

- transport identity and grant-signing keys are separate and independently rotatable;
- Agent trust changes require signed key-rotation continuity;
- grants remain scoped and short-lived;
- Agent independently validates grant signatures, device generation, capability revision, and single-operation execution;
- a backend/Agent policy ceiling may be stricter than the Hub grant;
- content-minimizing audit evidence can support investigation without storing raw desktop data.

Non-claim: cryptography cannot make a fully compromised authorized Hub harmless. M1/M3 deployments should consider isolating the grant-signing key from the Hub process or requiring a separate approval authority for dangerous capabilities.

### Compromised Agent

A compromised Agent can misuse the desktop capabilities available to its OS account, lie about backend results, or expose local data outside the protocol.

Controls:

- the Hub can revoke the enrolled device identity for future sessions;
- device-key rotation requires old-key and new-key continuity proof;
- Hub audit can record that a result was signed by the enrolled Agent identity;
- stale/reconnected generations fail closed at the Hub.

Non-claim: a Hub cannot cryptographically prove that a compromised Agent reported truthful desktop state or prevent local actions performed outside the protocol. The Agent host remains a high-trust machine and should use least-privilege OS/backend policy.

### Compromised backend

A malicious computer-use backend runs below the Agent semantic adapter and may ignore requested semantics, perform extra local actions, or fabricate results.

Controls:

- backend-specific behavior is isolated behind an adapter contract;
- adapter conformance validates normalized capability advertisements and result types;
- capability advertisement is explicit and versioned;
- the Agent should apply the narrowest backend policy/OS permissions practical.

Non-claim: an adapter cannot sandbox a malicious backend by itself. Backend provenance, version pinning, and OS isolation remain required deployment controls.

### Network attacker

A network attacker may impersonate a peer, modify messages, replay a prior handshake/command, observe sensitive metadata, or terminate a connection.

Controls already proven in M0:

- fresh Agent and Hub nonces bind each authentication transcript;
- Agent verifies a pinned Hub identity;
- Hub verifies the enrolled device identity;
- session acceptance, commands, cancellation, results, and cancellation acknowledgements are signed and connection-bound;
- oversized frames are rejected before declared payload allocation;
- a signed Hub time anchors grant-expiry evaluation to monotonic elapsed time on the Agent;
- connection loss after dispatch becomes durable `Indeterminate` state plus exact-operation desktop quarantine and is never automatically replayed;
- the authoritative Hub operation record additionally binds issuer/subject ownership and device generation, so a competing principal or stale Agent generation cannot settle the operation;
- reconnect/liveness cannot clear quarantine; explicit resolution is persistence-gated and auditable.

Post-M1 P0 execution-safety details and residual recovery assumptions are recorded in [`V2_P0_EXECUTION_SAFETY.md`](V2_P0_EXECUTION_SAFETY.md).

Current M1 evidence includes TLS-protected gRPC bidirectional streaming with pinned certificate trust/domain validation plus independent Ed25519 application authentication. The earlier raw-TLS transport remains a regression/reference implementation and is TLS 1.3-only with a dedicated ALPN. An operator-facing single-device `v2_hub` daemon now exists and is covered together with `v2_agent` by an end-to-end TLS/gRPC test. Residual deployment risk remains: public endpoint hardening, connection/TLS-handshake rate limits, real northbound authentication, and production certificate lifecycle are not yet complete, so this is not yet a production exposure claim.

### Replay and stale-state attacker

Controls:

- grant IDs are consumed once and may be revoked;
- unknown/retired grant-signing keys fail closed;
- device generation changes on reconnect and credential rotation;
- capability revisions are checked on every command/result;
- operation IDs are retained as terminal/indeterminate state and cannot be silently reused; M1 still needs bounded pruning/rollover semantics for terminal Agent tombstones during extremely long-lived generations;
- handshake proof replay fails against fresh nonces.

Completed/cancelled replay tombstones are bounded by authenticated device generation. A fresh generation makes older signed commands stale before they reach the execution gate, so old terminal IDs may be pruned. `Indeterminate` Hub operations are intentionally exempt from this pruning and continue to quarantine the device until explicit operator resolution.

### Denial of service

Controls:

- bounded wire frames;
- bounded Hub global active operations;
- bounded per-device queues;
- single active Agent operation;
- existing V1 northbound concurrency controls remain conceptually separate from the V2 Hub admission layer.

Residual risk: resource exhaustion outside those bounds (connection count, TLS handshakes, upstream identity provider, host CPU/memory) requires deployment-level rate limiting and observability.

## Cancellation and ambiguous outcomes

State-changing computer use is not safely retryable merely because a transport response was lost.

Rules:

- cancellation before dispatch prevents Agent execution;
- cancellation after dispatch is a signed Hub->Agent request and signed Agent acknowledgement;
- an Agent persists terminal operation IDs across restart to reject local replay; the retained set is not yet bounded inside one very long-lived generation and is an M1 acceptance item;
- a Hub connection loss after dispatch marks the operation `indeterminate`;
- `indeterminate`, `completed`, and `cancelled` operation IDs cannot be re-admitted;
- an `indeterminate` operation quarantines its device at Hub admission, so a different operation is also rejected until explicit resolution;
- reconnect does not transfer an existing generation-bound operation lease.

Agent-native process cancellation is stronger than GUI-backend cancellation: Unix process groups and Windows Job Objects terminate the supervised process tree, and background descendants are also cleaned up when the top-level process completes. The Cua MCP adapter instead propagates cancellation to the exact in-flight downstream request ID, but propagation is not treated as proof that a desktop side effect stopped. A propagated cancellation or timeout therefore maps to an `indeterminate` disposition and device quarantine. Lack of a backend-level proof of non-execution must never be interpreted as successful cancellation.

## Key rotation

### Agent credential rotation

The logical device ID remains stable. Replacement requires a rotation statement signed by both the currently enrolled device key and the proposed new device key. Rotation invalidates the current capability session and advances generation state before reconnect.

If the current Agent key is lost rather than available for continuity proof, recovery must be an explicit administrative re-enrollment flow; it must not be represented as ordinary in-band rotation.

### Hub transport identity rotation

An Agent begins with a pinned Hub verifying key. Each in-band replacement requires monotonically increasing rotation epoch plus signatures from both old and new Hub keys. A key that does not chain from the currently trusted key is rejected.

### Grant-signing key rotation

Grant tokens identify the signing key. Agents may temporarily trust old and new grant verifiers during a bounded overlap. Retiring the old verifier causes newly presented grants from that key to fail closed even if their nominal TTL has not elapsed.

## Clock model

Grant issue/expiry times originate at the Hub. The signed session acceptance carries Hub wall-clock time. After verification, the Agent advances that anchor with a local monotonic clock and uses the derived time for grant validation during the connection.

This avoids routine wall-clock skew or backward wall-clock adjustment silently extending a grant. A fully compromised Agent can still falsify its own execution environment; that is covered by the compromised-Agent non-claim above.

## Privacy and audit

Normal audit events may contain stable identifiers such as device ID, generation, grant ID, operation ID, semantic capability class, policy outcome, reason, and timing metadata.

They should not contain raw screenshots, raw backend output, raw command arguments, clipboard values, typed text, credentials, or full accessibility trees. Debug capture that intentionally includes such content is a separate high-sensitivity mode and must not be enabled implicitly.

## V2-M1 acceptance and residual deployment responsibilities

The V2-M1 implementation gate passed on 2026-08-12. The M1 code now includes verified northbound principal construction, production key/certificate lifecycle procedures, bounded service connection/rate shedding, bounded replay pruning, real-Cua cancellation quarantine, OpenTelemetry/OTLP integration, and OS service packaging. See [`V2_M1_ACCEPTANCE.md`](acceptance/V2_M1_ACCEPTANCE.md).

The threat model still requires the deployment to preserve these external responsibilities:

- the public authorization server/introspection endpoint and the Hub's configured issuer/resource/audience must be reviewed as one trust boundary;
- raw TCP/TLS handshake floods must be constrained by the host firewall/security group and, where used, a reviewed reverse proxy/load balancer. Application-layer limits intentionally begin after the transport is accepted;
- application-key recovery material and systemd/macOS credential files must be protected outside the repository. Loss of an old Hub/device key cannot be silently converted into an ordinary continuity rotation;
- `ExecuteProcess` and `Shell` remain exact `Dangerous` capabilities, not filesystem sandboxes; cwd/root checks do not constrain arbitrary process argv or shell syntax;
- macOS GUI automation still depends on the operator-controlled Cua/TCC trust boundary; a compromised Agent or desktop backend remains outside the non-compromise guarantees stated above;
- default telemetry must remain payload-free. Enabling collector/proxy body logging or high-sensitivity debug capture creates a separate sensitive-data boundary;
- an `indeterminate` operation must remain quarantined until explicit resolution. Network recovery, backend reconnect, or service restart is not permission to replay it.

These are deployment assumptions/residual risks, not missing V2-M1 protocol features. Multi-machine identity, fleet attestation, and native GUI adapters are intentionally deferred to later milestones.
