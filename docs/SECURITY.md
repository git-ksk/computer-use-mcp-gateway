# Security

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

Run `scripts/v2_desktop_acceptance.sh` only from a reviewed checkout on a trusted logged-in Mac, with both physical-action ACK variables explicitly set to `1`. Prefer a dedicated test Mac rather than a daily-use workstation. See [`V2_LOCAL_DESKTOP_ACCEPTANCE.md`](V2_LOCAL_DESKTOP_ACCEPTANCE.md) and [`V1_ACCEPTANCE.md`](V1_ACCEPTANCE.md).

P1 final physical acceptance ran on 2026-08-13 against trusted `main` commit `bb39390f3587902a7df918fe1ff4a8b28c328d50` as Desktop E2E run `31675515516`. The runner was registered ephemerally with the dedicated label, executed only the trusted `main` checkout, and automatically unregistered after the job. The V2 P1 step required exact quarantine to survive Hub/Agent restart and generation advance with no replay before explicit resolution and reuse.

## CI supply chain

Normal CI has read-only repository permissions and locked Rust dependency resolution. Before real-Cua smoke, CI verifies the pinned Cua installer, platform release payload, and installed executable identity so the installed binary must match the independently verified release payload.

The deterministic V1 quality fixture does not touch a desktop. It covers cancellation, 100-call soak behavior, short-window idle resource regression checks, backend process telemetry, and the selected applicable official MCP conformance scenarios.

## Logs and reporting

Gateway audit logs record coarse metadata such as tool name, semantic class, policy decision, outcome, and duration. Keep raw arguments/results and credentials out of normal logs.

For security-sensitive reports, do not include credentials or unrelated private desktop data in public issues. Prefer GitHub private vulnerability reporting when available.


## V2 trust model

V2 separates northbound authenticated client principals, Hub transport identity, grant-signing authority, and Agent device identity. Key rotation requires continuity proof; bounded admission and signed cancellation/reconnect semantics fail closed around ambiguous operations. The complete compromised-component analysis and non-claims are documented in [`V2_THREAT_MODEL.md`](V2_THREAT_MODEL.md).

V2-M1 passed its single-secure-Agent acceptance gate on 2026-08-12. The production candidate keeps TLS-protected gRPC separate from independently signed application identity, preserves principal -> stable device -> exact capability grants, and never forwards a northbound OAuth bearer token to the Agent. Ambiguous desktop cancellation remains `indeterminate` and quarantines the device rather than authorizing replay. Linux Hub application keys use systemd encrypted credentials in the packaged service; ACME owns ordinary server-certificate renewal; Hub/device/grant key rotation stays independent and continuity-proven. OpenTelemetry/OTLP default telemetry excludes sensitive operation payloads. See [`V2_M1_ACCEPTANCE.md`](V2_M1_ACCEPTANCE.md) and [`V2_THREAT_MODEL.md`](V2_THREAT_MODEL.md).

The post-M1 P0 hardening makes that ambiguity boundary explicit in an authoritative operation ledger. Authenticated issuer/subject ownership and Agent generation both fence settlement; dispatched uncertainty persists as an exact-operation desktop quarantine across reconnect/restart; queued pre-ambiguity work is cancelled instead of resumed; and reuse requires an explicit, auditable, persistence-gated resolution. The recovery evidence string is bounded metadata and must not contain raw desktop content, commands, results, or secrets. See [`V2_P0_EXECUTION_SAFETY.md`](V2_P0_EXECUTION_SAFETY.md).

### V2 payload-safe observability

V2 diagnostic output is a security boundary. Default tracing events and OTel metrics must not contain raw `DeviceCommand`/`DeviceResult` values, process stdout/stderr, shell text/argv/environment values, file paths or contents from operation payloads, OAuth bearer tokens or introspection secrets, exact grants, protocol signatures, or private key material. Error and Debug formatting used by the V2 Hub/Agent/backend/persistence boundary is reduced to stable error codes; unexpected signed protocol messages are represented by their message kind rather than by `Debug`-formatting the object. OAuth debug representations redact the introspection client secret and authenticated principal.

`operation_id`, stable `device_id` and Agent `generation` may appear in structured logs because they are needed to correlate safety state, but they are never metric labels. Principal issuer/subject is not logged by default. OTel metric attributes are restricted to closed domains such as capability, outcome, reason and persistence component. Request paths, tool/command names, principals and identifiers must not be added as metric attributes.

Higher verbosity through `RUST_LOG` does not relax the payload-free policy. Do not compensate for a diagnostic gap by logging command/result objects or underlying provider exceptions; add a bounded `error_code` or event field instead. External collectors, reverse proxies and service managers must likewise avoid body/header capture that would defeat the application boundary. See [`DEPLOYMENT.md`](DEPLOYMENT.md#overload-and-observability) for the event/metric taxonomy and incident correlation keys.

### Optional MCPUsage security boundary

Usage accounting does not become execution authority. The Hub first verifies OAuth and derives issuer+subject, then sends only that verified identity, CUMG `operation_id`, and bounded tool/accounting metadata to the loopback sidecar. Bearer tokens, tool arguments/results, screenshots, shell text, file contents, and introspection credentials do not cross the usage bridge. The sidecar rejects unexpected reserve fields.

A usage reserve/`markLiable()` failure fails closed before Agent dispatch. A settlement failure after dispatch never clears CUMG quarantine, converts `indeterminate`, retries the operation, or authorizes a competing principal. MemoryUsageStore restart can reset quota state but cannot reset durable CUMG safety state. The packaged sidecar is additionally constrained to localhost traffic. See [`V2_USAGE_ACCOUNTING.md`](V2_USAGE_ACCOUNTING.md).

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

P2 does not delegate the execution-safety authority to an external authorization system, policy engine, device fabric, or Computer Use runtime. The detailed review is in [`V2_P2_REPLACEMENT_SEAMS.md`](V2_P2_REPLACEMENT_SEAMS.md).

The two new seams are intentionally one-way and narrow:

- `DeviceCapabilityAuthorizer` may answer only whether one authenticated principal may use one exact `DeviceCapability` on one stable device ID. It cannot create/settle an operation, change ownership/generation, clear quarantine, or forward a northbound bearer token to the Agent.
- `ComputerUseBackendAdapter` may advertise typed capabilities and return the existing `BackendExecutionOutcome`. It cannot own the Hub ledger or resolution path. A cancellation, timeout, disconnect, or other post-side-effect uncertainty without sufficient backend evidence must remain `indeterminate` and flow into the unchanged Hub quarantine path.

A future SINT/Grantex/Open Agent Auth/OPA/Cedar adapter must fail closed when its authorization state is unavailable or ambiguous. A future Arm Device Connect or other fabric integration must treat discovery and liveness as routing inputs only: they are never proof of ownership, safe settlement, or safe reuse. A future OpenClaw or other Computer Use adapter must remain an executor under the CUMG operation ID and fences rather than introducing a second authoritative action lifecycle.

The existing compromised-backend boundary still applies. A malicious authenticated backend can lie about a claimed result or act outside CUMG; the adapter seam does not create remote attestation. P2 is designed to avoid making that trust boundary larger.
