# Security

> English is the canonical documentation. [日本語版 / Japanese translation](SECURITY.ja.md)

Computer-use grants a client access to sensitive desktop capabilities. Treat this gateway as a security boundary, not merely a transport adapter.

## V1 defaults

- listen only on `127.0.0.1` unless a different bind is deliberately reviewed;
- require authenticated TLS termination before remote access;
- keep the backend on stdio instead of exposing it directly;
- validate inbound MCP Host authorities and browser Origin values;
- deny all tools when the gateway allowlist is empty;
- require explicit `CUMG_ALLOW_TOOLS=*` to expose every discovered backend tool;
- apply deny rules before forwarding a call;
- serialize operations against the one physical desktop;
- use bounded connection/tool timeouts and reconnect backoff;
- propagate upstream cancellation to the actual downstream MCP request ID;
- never automatically replay failed, timed-out, or cancelled tool calls;
- avoid logging raw tool arguments, results, screenshots, clipboard values, or credentials.

## Policy layers

Authorization remains exact-name based. `CUMG_DENY_TOOLS` overrides `CUMG_ALLOW_TOOLS`.

V1 also classifies tools as `observe`, `interact`, `system`, or `dangerous` for audit/review purposes. Unknown or newly discovered names are classified as `dangerous` until reviewed. Semantic classification does **not** grant access and does not widen the exact-name allowlist.

Cua's own policy engine is an optional second layer when argument-level constraints matter. Start from [`../examples/cua-policy.yaml`](../examples/cua-policy.yaml) and review it for the target machine.

Read-only operations can still expose private desktop data. Treat screenshots, accessibility information, window/app metadata, and similar observation capabilities as sensitive data access.

## Failure and cancellation semantics

Read-only discovery may reconnect and retry after a transport failure. Computer-use actions are different because the desktop may already have partially applied an action.

For an in-flight tool call, the gateway keeps the downstream MCP request ID. If the northbound request is cancelled, the gateway sends downstream `notifications/cancelled` for that same request ID and returns an error without replay. Tool timeout follows the same no-replay rule and attempts downstream cancellation before recovery for a later request.

The deterministic CI fixture verifies that the downstream cancellation ID matches the in-flight backend request ID.

## Host and Origin validation

The MCP boundary uses Host and Origin guards. Default accepted authorities/origins are loopback-oriented. For a remote deployment, configure the exact expected public authority/origin or deliberately rewrite Host at the trusted proxy. Do not disable these guards just to make a proxy configuration work.

See [`DEPLOYMENT.md`](DEPLOYMENT.md).

## Health metadata

`/healthz` reports readiness and may include operational metrics for the gateway-owned backend child process:

- PID;
- cumulative CPU seconds;
- RSS bytes.

This does not include raw desktop content, but remotely reachable health routes should still sit behind the same authenticated deployment boundary.

On macOS, Cua may use its supported application/daemon lifecycle, so these metrics describe the direct child owned by the gateway rather than aggregate Cua process usage.

## Cloudflare deployment

Recommended topology:

```text
remote MCP client
    |
authenticated TLS / Cloudflare Access
    |
Cloudflare Tunnel
    |
127.0.0.1:<gateway>
    |
Cua stdio
```

Keep the gateway on loopback. Do not commit real tunnel credentials, Access tokens, private hostnames, `.env` files, generated private keys, PKCS#12 bundles, or local `secrets/` directories. The repository ignore rules are defense in depth, not a substitute for a secret manager or repository secret scanning.

## Local physical desktop acceptance

A Mac with Accessibility and Screen Recording grants is a high-trust machine. Physical desktop acceptance is therefore operator-controlled and local-only; normal GitHub Actions use GitHub-hosted runners and do not receive those desktop grants.

Run `scripts/v2_desktop_acceptance.sh` only from a reviewed checkout on a trusted logged-in Mac, with all required physical-action ACK variables explicitly set to `1`. Prefer a dedicated test Mac rather than a daily-use workstation. See [`V2_LOCAL_DESKTOP_ACCEPTANCE.md`](v2/acceptance/V2_LOCAL_DESKTOP_ACCEPTANCE.md) and [`V1_ACCEPTANCE.md`](V1_ACCEPTANCE.md).

When first-class Handoff coordination is enabled, `CUMG_V2_HANDOFF_CONTROL_SOCKET` is a separate local operator plane, not a northbound MCP tool. Keep its parent directory private and non-symlinked; the Hub creates the socket as `0600` but only relays signed/session-fenced control to the Agent-owned canonical Handoff runtime. The local caller cannot submit principal/device/Window authority: CUMG supplies the exact target from a still-valid interaction context and current Agent session fence, and the Agent re-validates device/generation/revision plus the exact command surface immediately before Cua. The takeover locator returned by explicit operator control is capability material and must not be copied into generic logs or public reports. Agent-local Handoff runtime failure remains fail-closed and is never auto-restarted to restore authority.

P1 final physical acceptance ran on 2026-08-13 against trusted `main` commit `bb39390f3587902a7df918fe1ff4a8b28c328d50` as Desktop E2E run `31675515516`. The runner was registered ephemerally with the dedicated label, executed only the trusted `main` checkout, and automatically unregistered after the job. The V2 P1 step required exact quarantine to survive Hub/Agent restart and generation advance with no replay before explicit resolution and reuse.

## CI supply chain

Normal CI has read-only repository permissions and locked Rust dependency resolution. Before real-Cua smoke, CI verifies the pinned Cua installer, platform release payload, and installed executable identity so the installed binary must match the independently verified release payload.

The deterministic V1 quality fixture does not touch a desktop. It covers cancellation, 100-call soak behavior, short-window idle resource regression checks, backend process telemetry, and the selected applicable official MCP conformance scenarios.

## Logs and reporting

Gateway audit logs record coarse metadata such as tool name, semantic class, policy decision, outcome, and duration. Keep raw arguments/results and credentials out of normal logs.

For security-sensitive reports, do not include credentials or unrelated private desktop data in public issues. Prefer GitHub private vulnerability reporting when available.

## V2 trust model

V2 separates northbound authenticated client principals, Hub transport identity, grant-signing authority, and Agent device identity. Key rotation requires continuity proof; bounded admission and signed cancellation/reconnect semantics fail closed around ambiguous operations. The complete compromised-component analysis and non-claims are documented in [`V2_THREAT_MODEL.md`](v2/V2_THREAT_MODEL.md).

V2-M1 passed its single-secure-Agent acceptance gate on 2026-08-12. The production candidate keeps TLS-protected gRPC separate from independently signed application identity, preserves principal -> stable device -> exact capability grants, and never forwards a northbound OAuth bearer token to the Agent. Authenticated Agent sessions have a bounded maximum lifetime with a pre-expiry reauthentication drain; the hard deadline never converts unresolved dispatched work into replayable state. Ambiguous desktop cancellation remains `indeterminate` and quarantines the device rather than authorizing replay. Fresh fixed-device enrollment is an offline create-new bundle transferred over an operator-authenticated channel; no mutable public enrollment endpoint is introduced. Packaged Linux production isolates the grant-signing private key in a separate `v2_grant_signer` service; `v2_hub` retains only the signer public verifier/socket and has no signing fallback when that service is unavailable. The signer applies an independent exact-capability/TTL/clock-skew ceiling before canonical signing. ACME owns ordinary server-certificate renewal; Hub/device/grant key rotation stays independent and continuity-proven. TLS server/trust-root expiry is checked independently, and private-root compromise requires explicit Agent trust-root reprovisioning rather than CRL/OCSP or automatic trust widening. OpenTelemetry/OTLP default telemetry excludes sensitive operation payloads. See [`V2_M1_ACCEPTANCE.md`](v2/acceptance/V2_M1_ACCEPTANCE.md), [`V2_THREAT_MODEL.md`](v2/V2_THREAT_MODEL.md), and [`V2_GRANT_SIGNING.md`](v2/V2_GRANT_SIGNING.md).

The post-M1 P0 hardening makes that ambiguity boundary explicit in an authoritative operation ledger. Authenticated issuer/subject ownership and Agent generation both fence settlement; dispatched uncertainty persists as an exact-operation desktop quarantine across reconnect/restart; queued pre-ambiguity work is cancelled instead of resumed; and reuse requires an explicit, auditable, persistence-gated resolution. The recovery evidence string is bounded metadata and must not contain raw desktop content, commands, results, or secrets. See [`V2_P0_EXECUTION_SAFETY.md`](v2/V2_P0_EXECUTION_SAFETY.md).

Execution-safety schema v3 adds privacy-preserving reconciliation correlation without changing that authority boundary. Effectful northbound calls may attach bounded `workflow_id`, `workflow_step_id`, and `client_correlation_id` labels; they are untrusted audit labels only and cannot change principal ownership, capability authorization, generation fences, retry safety, or quarantine resolution. Where canonicalization is explicitly defined, shell/process requests may also carry a keyed HMAC-SHA256 fingerprint created before dispatch from a deployment-private key. Fingerprint equality means only “same canonical request under the same key generation”; it never proves execution/completion, authorizes replay, or clears quarantine. Normal inspection exposes only fingerprint presence or a local same/different/unavailable comparison, never the fingerprint/key itself. Schema-v1/v2 state remains readable when it does not claim v3-only fields, while unsafe downgrade/forged combinations fail closed. Raw command/argv/cwd/env/result/credential content remains excluded from durable audit and inspection output.

Execution-safety schema v4 adds one deliberately narrow automatic settlement path for transient response-loss/restart ambiguity. Before an effectful command can be sent, the Hub durably binds the operation to the exact stable device, original Agent generation, exact capability, capability revision, and the one-shot grant identifier used as the dispatch fence. The Agent may durably retain at most 64 payload-free terminal proofs produced only after its ordinary execution path has already reached a terminal result that the Hub would accept. On a later authenticated generation the Agent re-signs that bounded journal against the fresh session nonces. The Hub may move an existing `Indeterminate` operation to `auto_resolved` only when the signed proof exactly matches every stored binding and the evidence class is already authoritative (`VerifiedAgentResult`, `VerifiedRemoteError`, or `ProvenProcessTermination`). The candidate terminal checkpoint is committed before live quarantine is removed. A missing proof becomes `unrecoverable_evidence_gap`; a stale, forged, mismatched, or otherwise unprovable claim becomes/remains `operator_required` or fails closed. This path never re-sends a command, retries an operation, interprets a request fingerprint as execution evidence, or exposes/stores raw command, environment, result, credential, grant token, or fingerprint value. The persisted grant ID is a one-shot opaque correlation fence, not a bearer grant/token and is not exposed by normal maintenance output. Schema v1/v2/v3 checkpoints remain readable when they do not claim v4-only state; downgrade that would discard v4 reconciliation state is rejected. Capability schema v5 makes the reconciliation-report frame a coordinated Hub/Agent protocol boundary so mixed old/new peers fail the handshake closed instead of partially interpreting the stream.

Execution-safety schema v5 adds a separate **unknown-outcome retirement** path for an older `Indeterminate` operation whose terminal outcome can no longer be truthfully reconstructed. Retirement is not settlement: the operation remains historically `Indeterminate`, receives no terminal receipt, and is never rewritten as completed/cancelled/not-executed. The reviewed v1 retirement policy is intentionally narrow (`Scroll` and `MovePointer` only), must be explicitly named by offline maintenance, requires the durable device registry to show a strictly newer generation than the original dispatch, requires `operator_required` or `unrecoverable_evidence_gap`, and is available only through the exclusive offline local-maintenance authority. Durable retirement history is capped at 64 entries; exhaustion fails closed instead of allowing permanent tombstones to grow checkpoint state without bound. The exact old operation ID remains a permanent replay tombstone; clearing quarantine cannot dispatch, resume, re-sign, or reconstruct it. Every retirement persists `outcome=unknown`, capability/policy, original and authorizing generations, prior reconciliation state, bounded reason metadata, local maintenance authority, timestamp, and `replayed=false`. High-impact capabilities fail closed. Schema-v1/v2/v3/v4 checkpoints remain readable within their representational limits, but a checkpoint containing v5 retirement state cannot be downgraded to a schema that would erase that distinction.

### V2 payload-safe observability

V2 diagnostic output is a security boundary. Default tracing events and OTel metrics must not contain raw `DeviceCommand`/`DeviceResult` values, process stdout/stderr, shell text/argv/environment values, file paths or contents from operation payloads, OAuth bearer tokens or introspection secrets, exact grants, protocol signatures, or private key material. Error and Debug formatting used by the V2 Hub/Agent/backend/persistence boundary is reduced to stable error codes; unexpected signed protocol messages are represented by their message kind rather than by `Debug`-formatting the object. OAuth debug representations redact the introspection client secret and authenticated principal.

`operation_id`, stable `device_id` and Agent `generation` may appear in structured logs because they are needed to correlate safety state, but they are never metric labels. Principal issuer/subject is not logged by default. OTel metric attributes are restricted to closed domains such as capability, outcome, reason and persistence component. Request paths, tool/command names, principals and identifiers must not be added as metric attributes.

Higher verbosity through `RUST_LOG` does not relax the payload-free policy. Do not compensate for a diagnostic gap by logging command/result objects or underlying provider exceptions; add a bounded `error_code` or event field instead. External collectors, reverse proxies and service managers must likewise avoid body/header capture that would defeat the application boundary. See [`DEPLOYMENT.md`](DEPLOYMENT.md#overload-and-observability) for the event/metric taxonomy and incident correlation keys.


## V2 P1 fixed-set multi-device security review

P1 adds only fixed composition around the P0 core. The security review covers the requested cross-device failure classes:

- **cross-device ownership bleed:** each device owns a separate `SingleDeviceHub`, authoritative controller, checkpoint directory, queue, live session and generation. No API transfers an unresolved operation or quarantine between entries;
- **device ID / generation confusion:** routing requires an exact pre-provisioned stable device ID, while the selected P0 service still verifies its provisioned device identity, signed session material, operation identity, capability revision and generation. A reconnect advances only that device's generation;
- **stale routing:** the fixed map is immutable after construction. There is no discovery, reassignment or failover-to-another-device operation that could route an old A result into B;
- **shared/global queue bypass:** P1 introduces no shared queue. Admission/load shedding remains inside each existing per-device Hub, so A's quarantine cannot be bypassed through B's capacity or queue;
- **checkpoint restore consistency:** construction rejects duplicate state directories. Hub restart reconstructs each P0 checkpoint independently; failure to restore one device is not interpreted as permission to inherit another device's state;
- **duplicate/late result or cancellation acknowledgement:** the unchanged P0 operation/owner/generation fences reject stale settlement and duplicate finalization; separate service instances additionally prevent a signed A stream from becoming B's execution stream;
- **resolution target confusion:** recovery is invoked through the exact device's `HubHandle` and the exact ambiguous operation ID. There is no fleet-wide lookup that can resolve a same-looking operation on another device;
- **compromised backend evidence:** unchanged trust boundary. A malicious authenticated Agent/backend may falsely claim terminal evidence or perform side effects outside CUMG. The reference executor proves adapter classification rules for conforming backends; it is not remote attestation or Byzantine proof.

The proof intentionally does not add generic authorization, mutable device enrollment/discovery, a fleet scheduler, new policy language, native GUI backends, or a ROSClaw fork.

## V2 P2 replacement-seam security boundary

P2 does not delegate the execution-safety authority to an external authorization system, policy engine, device fabric, or Computer Use runtime. The detailed review is in [`V2_P2_REPLACEMENT_SEAMS.md`](v2/V2_P2_REPLACEMENT_SEAMS.md).

The two new seams are intentionally one-way and narrow:

- `DeviceCapabilityAuthorizer` may answer only whether one authenticated principal may use one exact `DeviceCapability` on one stable device ID. It cannot create/settle an operation, change ownership/generation, clear quarantine, or forward a northbound bearer token to the Agent.
- `ComputerUseBackendAdapter` may advertise typed capabilities, normalize backend-specific GUI observations into the bounded CUMG model, and return the existing `BackendExecutionOutcome`. It cannot own the Hub ledger or resolution path. Backend tool/session names are not themselves authorization capabilities. For a mutating command after provider dispatch, cancellation, timeout, disconnect, generic backend error, response loss, or malformed/unprovable completion without sufficient evidence of non-execution is classified at the adapter/Agent boundary as `BackendOutcomeIndeterminate`; the Hub persists durable `Indeterminate` with reason `BackendOutcomeUnproven` and follows the unchanged quarantine/explicit-resolution/no-auto-replay path. Read-only backend failures may remain definite. GUI snapshots may contain sensitive window titles, labels, values, and screenshots; they remain request results and must not be copied into default telemetry.

A future SINT/Grantex/Open Agent Auth/OPA/Cedar adapter must fail closed when its authorization state is unavailable or ambiguous. A future Arm Device Connect or other fabric integration must treat discovery and liveness as routing inputs only: they are never proof of ownership, safe settlement, or safe reuse. A future OpenClaw or other Computer Use adapter must remain an executor under the CUMG operation ID and fences rather than introducing a second authoritative action lifecycle.

The existing compromised-backend boundary still applies. A malicious authenticated backend can lie about a claimed result or act outside CUMG; the adapter seam does not create remote attestation. P2 is designed to avoid making that trust boundary larger.
