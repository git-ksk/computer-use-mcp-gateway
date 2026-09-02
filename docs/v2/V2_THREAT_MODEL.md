# V2 threat model

> English is the canonical documentation. [日本語版 / Japanese translation](V2_THREAT_MODEL.ja.md)

Status: **V2-M1 trust-model baseline**. M0 assumptions remain the foundation; accepted M1 transport/process controls, post-M1 execution-safety hardening, and residual deployment responsibilities are reflected below.

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
| grant signer client + public verifier|
| transport identity                   |
| admission / lease / audit state      |
+----------------+---------------------+
                 | typed exact-capability signing request
                 v
        +-------- external signer --------+
        | independent capability ceiling  |
        | grant-signing private key       |
        +----------------+----------------+
                         | signed GrantToken
                         v
+---------------- Hub ----------------+
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
- capability-grant signing private keys (held by the external signer in packaged production, not by `v2_hub`);
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

The M1 northbound boundary implements the MCP Authorization protected-resource side rather than a new OAuth server. `v2_hub` publishes RFC 9728 metadata, requires header bearer presentation, validates tokens through a configured RFC 7662 introspection endpoint, binds accepted tokens to the configured MCP resource audience, and constructs `AuthenticatedClientPrincipal` from the verified subject plus the configured authorization-server issuer. Required OAuth scopes gate entry to the MCP resource, while the separate local principal -> device -> exact `DeviceCapability` policy remains authoritative for delegated device access. The bearer header is removed before rmcp handler dispatch and no bearer field exists in the typed Hub-to-Agent command/grant path.

Residual risk: authorization-server/introspection availability and credential compromise remain deployment trust dependencies; public HTTPS termination and transport-edge rate limiting remain deployment responsibilities outside the loopback northbound listener. In packaged production a compromised `v2_hub` no longer possesses the grant-signing private key and cannot mint a grant while the external signer is absent or rejecting the request. A live signer may still authorize any exact capability intentionally present in its independent signer policy, so OAuth plus key isolation does not make a fully compromised Hub harmless.

### Compromised Hub

A fully compromised Hub is still a **high-severity trust failure** because it controls northbound authorization/admission and the active Hub transport identity. Packaged production now removes the grant-signing private key from that process: `v2_hub` can submit only bounded typed signing requests to a separate Unix-socket service, and that service applies its own stable-device/exact-capability, TTL, and issue-time-skew ceiling before constructing and signing the canonical grant. The Hub verifies every returned token against a pinned grant public key. If the signer is absent or rejects the request, the operation is cancelled before Agent dispatch and there is no in-process fallback.

Controls that still reduce blast radius or aid recovery:

- transport identity and grant-signing private key custody are split across separate processes/service identities in packaged production;
- the signer receives no arbitrary bytes to sign, generates the grant ID/canonical payload itself, and independently denies capabilities omitted from its policy;
- the signer bounds TTL and rejects issue times outside its own clock-skew window;
- Agent trust changes require signed key-rotation continuity;
- grants remain scoped and short-lived;
- Agent independently validates grant signatures, device generation, capability revision, and single-operation execution;
- a backend/Agent policy ceiling may be stricter than both Hub and signer grants;
- content-minimizing audit evidence can support investigation without storing raw desktop data.

Non-claim: external key custody is not per-operation user approval. A malicious Hub that still holds the Hub transport key and can reach a healthy signer can request any capability that the independently administered signer policy intentionally allows. Compromise of both Hub and signer restores the stronger trust failure. Deployments that require human/hardware approval for dangerous capabilities should add that authority at the signer boundary rather than assuming key isolation provides it. See [`V2_GRANT_SIGNING.md`](V2_GRANT_SIGNING.md).

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
- reconnect/liveness alone cannot clear quarantine; only an exact authenticated terminal-evidence report that matches the Hub's durable operation/device/original-generation/capability-revision/capability/dispatch-fence binding may self-reconcile a transient ambiguity, and that terminal transition is persistence-gated and auditable. Otherwise explicit operator resolution remains required.

Post-M1 P0 execution-safety details and residual recovery assumptions are recorded in [`V2_P0_EXECUTION_SAFETY.md`](V2_P0_EXECUTION_SAFETY.md).

Current M1 evidence includes TLS-protected gRPC bidirectional streaming with pinned certificate trust/domain validation plus independent Ed25519 application authentication. The earlier raw-TLS transport remains a regression/reference implementation and is TLS 1.3-only with a dedicated ALPN. The operator-facing `v2_hub` daemon and `v2_agent` are covered by end-to-end TLS/gRPC tests. Public-edge firewall/reverse-proxy controls, raw transport handshake shedding, external authorization-service availability, and credential/certificate custody remain deployment responsibilities; they are not permission to weaken the accepted application-level safety model.

Production hardening additionally bounds every authenticated Agent session. The Hub pauses new admission during a bounded pre-expiry drain and normally closes the stream only after already-admitted work settles, forcing a fresh nonce-bound handshake and generation. If the hard deadline arrives first, transport closure remains fail-closed and unresolved dispatched work may become `Indeterminate` plus quarantine rather than becoming replayable.

### Replay and stale-state attacker

Controls:

- grant IDs are consumed once and may be revoked;
- unknown/retired grant-signing keys fail closed;
- device generation changes on reconnect and credential rotation;
- capability revisions are checked on every command/result;
- operation IDs cannot be silently reused; completed/cancelled Agent replay tombstones are bounded by authenticated device generation, while Hub `Indeterminate` operations remain durable until explicit resolution;
- handshake proof replay fails against fresh nonces.

Completed/cancelled replay tombstones are bounded by authenticated device generation. A fresh generation makes older signed commands stale before they reach the execution gate, so old terminal IDs may be pruned. `Indeterminate` Hub operations are intentionally exempt from this pruning and continue to quarantine the device until either exact authoritative terminal evidence self-reconciles the same already-dispatched operation or an explicit operator resolution is durably committed. Neither path replays the operation.

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
- Agent terminal replay tombstones are generation-bounded and may be pruned only after a fresh authenticated generation makes old signed commands stale; Hub `Indeterminate` operations remain durable and quarantine the device until explicit resolution;
- a Hub connection loss after dispatch marks the operation `indeterminate`;
- `indeterminate`, `completed`, and `cancelled` operation IDs cannot be re-admitted;
- an `indeterminate` operation quarantines its device at Hub admission, so a different operation is also rejected until explicit resolution;
- reconnect does not transfer an existing generation-bound operation lease.

Agent-native process cancellation is stronger than GUI-backend cancellation: Unix process groups and Windows Job Objects terminate ordinary descendants that remain in the supervised process-control domain, and those descendants are also cleaned up when the top-level process completes. On Unix this is not an OS-wide sandbox: a deliberately detached descendant that creates another session/process group (for example with `setsid()`) is outside the current process-group cleanup guarantee and must not be relied on as a supported persistence mechanism. The Cua MCP adapter instead propagates cancellation to the exact in-flight downstream request ID, but propagation is not treated as proof that a desktop side effect stopped. A propagated cancellation or timeout therefore maps to an `indeterminate` disposition and device quarantine. The same rule applies when a mutating request was dispatched and the backend later returns a generic tool error, a malformed/unprovable completion, or loses the response channel: those failure shapes do not prove non-execution. At the adapter/Agent boundary they are classified as `BackendOutcomeIndeterminate`; the Hub persists durable `Indeterminate` with reason `BackendOutcomeUnproven`, cancels queued work for that desktop, and requires explicit persistence-gated resolution before reuse. Read-only commands may still return a definite backend error. Lack of backend-level proof of non-execution must never be interpreted as successful cancellation, safe retry, or permission to replay.

Definite Agent-native process/shell validation or spawn failures do not need raw host detail to be actionable. The signed Agent->Hub result carries only a closed `DeviceErrorCode`; northbound maps reviewed categories such as `working_directory_denied`, `working_directory_invalid`, `invalid_timeout`, `invalid_program`, `program_denied`, `too_many_arguments`, `environment_key_denied`, `invalid_environment`, `too_many_environment_entries`, and `process_spawn_failed` to fixed messages. The requested or allowed path, program/argv, environment key/value, and raw OS error are never returned. Shell preserves the safe category of its inner process error rather than collapsing it to a generic wrapper code. Executor/configuration failures that are not deliberately classified remain `internal_failure`/generic fail-closed errors. Native runtime timeout and cancellation remain proven process-termination outcomes (`timed_out` / `cancelled`), while an unproven post-dispatch outcome remains `Indeterminate` and quarantined; this error taxonomy does not weaken no-replay or quarantine semantics.

## Browser transfer data boundary

Browser transfer is intentionally narrower than filesystem access. Upload northbound traffic carries bounded bytes and a path-safe logical name; it mints a context/generation/revision-bound one-shot ref whose backend value is an Agent-private staging handle. The Agent creates the actual file beneath its hardened state directory, rejects symlink/directory/replacement/size violations, and re-proves a canonical regular file immediately before the southbound Cua call. Raw host paths are never accepted from or returned to the northbound caller.

Download accepts no destination path or root capability from the caller. The Agent creates a private per-operation canonical root, maps exact `BrowserDownload` authorization plus a fresh click-capable page ref to Cua's reviewed MCP-host approval mechanism, and rejects any Cua completion whose opaque id is not a single component or whose object is not a direct regular file in that exact root. Reported length, actual length, caller maximum, and the global 16 MiB ceiling must all agree before a bounded read. Logical-name collisions require explicit overwrite and replacement occurs only after the new object is safely finalized.

Transfer refs and staging die with context/generation/revision lifecycle. Definite pre-dispatch validation/refusal failures are cleaned immediately; cancellation, timeout, generic backend error, response loss, or unprovable completion after provider dispatch leaves only Agent-private staging until teardown and enters the ordinary indeterminate quarantine. This avoids racing an in-flight backend read/write and preserves no-auto-replay. The threat model therefore does not claim to sandbox a compromised Agent/Cua process; it claims that an uncompromised V2 boundary does not expose generic host filesystem authority northbound.

## Key rotation

### Enrollment and TLS trust-anchor lifecycle

Fresh fixed-device enrollment is deliberately offline. `v2_keyctl prepare-agent-enrollment` generates a create-new device secret plus the exact Hub/grant/TLS trust inputs under a private staging directory and emits only a non-secret manifest/device ID. The Agent portion must cross an operator-authenticated provisioning channel; the Hub receives only the device public key. This does not create mutable runtime discovery or a network enrollment oracle.

TLS trust is also independent of the Hub Ed25519 application identity. Possession of a compromised TLS server key does not by itself satisfy the signed Hub handshake, but it invalidates the confidentiality assumption. In the private pinned-root model CUMG intentionally does not implement CRL/OCSP. Therefore an old compromised leaf is removed from the Agent trust boundary by an explicit root/server-identity maintenance cutover and Agent root reprovisioning. The old root rejects the replacement chain and the replacement root accepts it in the dedicated TLS regression. Expiry checks emit a stable operational alert and never modify trust automatically.

### Agent credential rotation

The logical device ID remains stable. Replacement requires a rotation statement signed by both the currently enrolled device key and the proposed new device key. Rotation invalidates the current capability session and advances generation state before reconnect.

The packaged rotation runbook treats that continuity document as one-shot: stop the Agent, generate the dual-signed replacement offline, let the Hub verify and persist the new verifier, then start the Agent with the new secret. After a fresh authenticated generation succeeds, remove the rotation-file setting; a subsequent Hub restart must succeed from persisted trust without reusing the document. See [`../../packaging/README.md`](../../packaging/README.md).

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

Schema-v3 reconciliation metadata deliberately separates four evidence classes. **Correlation evidence** is bounded workflow/client labeling plus, for explicitly canonicalized shell/process contracts, a keyed request fingerprint; it can establish only which higher-level step/candidate request is being discussed. **Observational evidence** is an independent read-only postcondition or state check and is advisory unless a capability contract explicitly promotes it. **Authoritative terminal evidence** is the existing signed/verified execution evidence that can settle the operation state machine. **Operator resolution** is the explicit persistence-gated decision applied only after the operator has sufficient evidence for the exact quarantined operation. Correlation/fingerprint equality alone is never terminal evidence, replay permission, or proof that a side effect occurred. Fingerprint keys and values are not normal audit assets, and key rotation must degrade comparison to unavailable rather than reinterpret a request as different.

Schema v4 operationalizes only the **authoritative terminal evidence** class for self-reconciliation. The Hub persists the original dispatch binding before send: stable device ID, original device generation, exact `DeviceCapability`, capability revision, and one-shot grant ID. The Agent's durable journal contains no request/result payload and is capped at 64 entries; an entry may be created only after the normal execution path has a definite terminal result. A fresh authenticated session signs the journal against new session nonces. The Hub verifies the current device signature and reporting generation, then requires exact equality with its older quarantined record before settlement. A proof from the current/stale wrong generation, wrong device, wrong operation, wrong capability/revision/fence, an unsupported evidence/state pair, or a missing journal entry cannot clear quarantine. `auto_reconciling`, `auto_resolved`, `operator_required`, and `unrecoverable_evidence_gap` are explicit durable/audit states. The Hub writes the candidate terminal checkpoint before making the live quarantine removal visible. A compromised Agent therefore gains no generic “clear quarantine” primitive beyond the exact-result authority it already possessed for that dispatched operation while online; it cannot name a different operation/fence or use a correlation fingerprint to manufacture execution evidence.

Schema v5 addresses a different threat: permanent denial of service caused by legacy ambiguity whose terminal truth is no longer recoverable. The retirement primitive does **not** grant terminal-result authority. It is restricted to offline local maintenance, an explicitly named versioned reviewed policy whose initial allowlist is `Scroll`/`MovePointer`, an exact quarantined operation, and a durable registry generation strictly newer than the original dispatch. Schema v5-v10 couple permanent replay tombstones to a 64-record detailed history cap. Schema v11 separates those concerns: detailed history and full retired operation detail stay bounded at 64 while a lossless exact-ID replay-deny index is independently capped at 4096; its exhaustion still leaves quarantine intact. The old operation remains `Indeterminate` and permanently replay-tombstoned; only the admission quarantine is released. A forged/stale generation, an auto-reconciling operation, a missing dispatch marker, duplicate/conflicting transition, or any non-allowlisted/high-impact capability fails closed. This prevents retirement from becoming a generic "assume success", "assume cancellation", or replay bypass. The allowlist is deliberately a reviewed operational-risk policy rather than a semantic proof that all applications treat scroll/pointer movement as side-effect-free: application-defined state changes remain part of the accepted unknown outcome. A newer generation invalidates stale interaction contexts/refs, and pre-ambiguity queued work is already cancelled, so retirement cannot resume a dependent old action chain.

## V2-M1 acceptance and residual deployment responsibilities

The V2-M1 implementation gate passed on 2026-08-12. The M1 code now includes verified northbound principal construction, production key/certificate lifecycle procedures, bounded service connection/rate shedding, bounded replay pruning, real-Cua cancellation quarantine, OpenTelemetry/OTLP integration, and OS service packaging. See [`V2_M1_ACCEPTANCE.md`](acceptance/V2_M1_ACCEPTANCE.md).

The threat model still requires the deployment to preserve these external responsibilities:

- the public authorization server/introspection endpoint and the Hub's configured issuer/resource/audience must be reviewed as one trust boundary;
- raw TCP/TLS handshake floods must be constrained by the host firewall/security group and, where used, a reviewed reverse proxy/load balancer. Application-layer limits intentionally begin after the transport is accepted;
- application-key recovery material and systemd/macOS credential files must be protected outside the repository. Loss of an old Hub/device key cannot be silently converted into an ordinary continuity rotation;
- `ExecuteProcess` and `Shell` remain exact `Dangerous` capabilities, not filesystem sandboxes; cwd/root checks do not constrain arbitrary process argv or shell syntax;
- `ReadFile` / `ListDirectory` use a separate explicit file-root policy. File roots may be narrower than process cwd roots, are never inferred from them, retain canonicalization/symlink-escape denial, and missing configuration fails Agent startup closed;
- macOS GUI automation still depends on the operator-controlled Cua/TCC trust boundary; a compromised Agent or desktop backend remains outside the non-compromise guarantees stated above;
- default telemetry must remain payload-free. Enabling collector/proxy body logging or high-sensitivity debug capture creates a separate sensitive-data boundary;
- an `indeterminate` operation must remain quarantined until a persistence-gated settlement exists. Network recovery, backend reconnect, or service restart alone is not settlement and is never permission to replay it. The only automatic exception is exact signed terminal evidence for the same prior dispatch binding; missing/mismatched evidence remains operator-required/fail-closed.

These are deployment assumptions/residual risks, not missing V2-M1 protocol features. Multi-machine identity, fleet attestation, and additional native GUI backends remain intentionally deferred to later milestones.

## Local-user online recovery authority

Online quarantine recovery does not grant the Agent device identity administrative recovery authority. A compromised Agent remains able to lie about local desktop state, so an Agent/device signature alone cannot clear `DesktopQuarantine`.

A deployment may explicitly provision a separate endpoint recovery verifier. The initial macOS provider keeps the corresponding P-256 private key in the Secure Enclave with user-presence/private-key-use access control and persists only its owner-private sealed Secure Enclave representation. The Hub signs a fresh short-lived challenge bound to the exact durable quarantine, historical operation generation, current authenticated Agent generation, and nonce. The local user's signed decision is accepted only while that challenge and the current quarantine fingerprint still match. The Agent transports the authorization but cannot construct a valid recovery signature itself.

This is user-presence authorization, not cryptographic proof that an arbitrary GUI side effect did or did not occur. Because normal checkpoints intentionally exclude raw GUI payloads and screenshots, the generic initial audit assessment is `inconclusive`; the local user inspects the current desktop and chooses the exact resolution. A future automatic assessment must be capability-specific and must not widen the ordinary audit/privacy boundary.

Resolution remains persistence-gated and never resumes the old operation. If the verifier is absent, the challenge is stale, the signature is invalid, the quarantine changed, or persistence fails, the device remains quarantined. The existing offline maintenance resolver remains the break-glass path. See [`V2_ONLINE_RECOVERY.md`](V2_ONLINE_RECOVERY.md).
