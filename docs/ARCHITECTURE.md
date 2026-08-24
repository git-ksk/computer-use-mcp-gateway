# Architecture

> English is the canonical documentation. [日本語版 / Japanese translation](ARCHITECTURE.ja.md)

V2 Hub + V2 Agent is the recommended runtime. V1 is retained as `v1_gateway` for regression/reference.

## V1 legacy/reference

The gateway is both an MCP server and an MCP client.

```text
Northbound                                  Southbound

MCP client
    |
    | MCP Streamable HTTP /mcp
    v
+-----------------------------+
| computer-use-mcp-gateway    |
|                             |
|  Host / Origin guards       |
|       |                     |
|  policy / audit             |
|       |                     |
|  dynamic tool snapshot      |
|       |                     |
|  backend abstraction        |
+-------|---------------------+
        |
        | MCP stdio
        v
   cua-driver mcp
```

### Responsibilities

**Gateway**
- MCP transport boundary
- Host/Origin transport guards
- backend lifecycle
- dynamic tool discovery and cached policy-filtered snapshot
- request forwarding
- deny-by-default exact-name policy enforcement
- semantic tool risk classification for audit/inspection
- upstream cancellation forwarding
- health and audit metadata

**Backend adapter**
- child-process connection lifecycle
- connection and operation timeouts
- bounded reconnect/backoff
- serialization of operations against one physical desktop
- downstream MCP cancellation using the actual in-flight request ID
- no automatic replay of failed or cancelled state-changing calls
- gateway-owned backend child PID/CPU/RSS telemetry where the platform supports it

**Cua backend**
- screenshots
- accessibility/UI trees
- click/type/scroll
- window/application control
- platform permissions

## Backend abstraction

V1 starts with Cua but does not hard-code Cua semantics into the public gateway surface.

```text
Backend
  connect()
  health()
  resource_metrics()
  list_tools()
  call_tool(..., cancellation)
  shutdown()
```

The first implementation is `CuaBackend`.

## State model

MCP `2026-07-28` removes protocol-level HTTP sessions. Public request handlers therefore cannot rely on client state stored in an HTTP MCP session.

Application-level state is independent of MCP transport sessions:

- `Gateway` owns the exact-name policy, semantic classifier, and a shared policy-filtered tool snapshot.
- `CuaBackend` owns the current MCP client service, its direct child PID, and synchronization locks.
- `tools/list` refreshes backend discovery; if refresh fails it may serve the last policy-filtered cached snapshot.
- a policy-allowed `tools/call` missing from the current snapshot triggers one refresh and fails closed if discovery still cannot confirm the tool.
- backend operations are serialized because cursor/focus/UI snapshot state is shared mutable desktop state.

## Tool classification

V1 keeps authorization and semantic classification separate.

Exact tool names remain the enforcement boundary through `CUMG_ALLOW_TOOLS` / `CUMG_DENY_TOOLS`. Independently, discovered/called tools are classified as:

- `observe`;
- `interact`;
- `system`;
- `dangerous`.

Known Cua-compatible names are mapped explicitly. Unknown/new backend tool names are classified as `dangerous` until reviewed. The classification is included in audit metadata and discovery counts; it does **not** silently broaden the exact-name allowlist.

## Failure and cancellation model

Read-only tool discovery can reconnect and retry after a transport failure. State-changing computer-use calls cannot be safely replayed because the desktop may already have partially applied the action. A failed call is therefore returned as an error after recovery is attempted for the next request.

When the northbound MCP request is cancelled, the gateway forwards that signal into `CuaBackend`. The backend creates the downstream call through rmcp's cancellable-request API, retains its actual downstream request ID, and sends `notifications/cancelled` with that ID. The cancelled call is returned as an error and is not replayed.

The per-call timeout follows the same no-replay safety rule: the backend sends a downstream cancellation notification for the in-flight request, then repairs the connection for a later request when possible.

## Health and resource telemetry

`/healthz` reports backend readiness plus an optional `backend_resources` snapshot for the backend child process directly owned by the gateway:

```json
{
  "status": "ok",
  "backend": "ready",
  "backend_resources": {
    "pid": 12345,
    "cpu_seconds": 0.12,
    "rss_bytes": 17817600
  }
}
```

`cpu_seconds` is cumulative process CPU time; `rss_bytes` is resident memory. If a platform/process lookup cannot provide a snapshot, `backend_resources` may be `null` without changing the readiness decision.

On macOS, Cua may proxy through its supported application/daemon lifecycle, so these metrics describe the direct child the gateway owns, not aggregate resource use across every Cua process.

## Security boundary

The gateway is not an internet authentication service. Remote deployments keep the process on loopback and place authenticated TLS termination in front of it. The `/mcp` boundary additionally validates Host authorities and browser Origin values.

Tool exposure is deny-by-default. Cua's own policy engine can provide a second, argument-aware capability ceiling.

## V2 runtime

The actual northbound runtime is now the V2 Hub. The default binary and the explicit `v2_hub` binary share that entrypoint; `v2_agent` remains a separate outbound desktop process. The old single-process V1 entrypoint is preserved as `v1_gateway`.

Quota, billing, and usage accounting are deployment-layer concerns outside the CUMG core. A reverse proxy, MCP edge, or other operator-controlled component may enforce them before requests reach the Hub, but that component cannot alter CUMG operation identity, authorization, generation fencing, durable execution state, quarantine, replay admission, or recovery.

The authoritative operation record now uses execution-safety schema v5. Schema v5 preserves all schema-v4 exact dispatch/reconciliation semantics and adds a distinct durable retirement ledger for policy-eligible, permanently unknowable `Indeterminate` operations. Schema v3 introduced bounded audit correlation labels and optional keyed shell/process request fingerprints; those remain non-authoritative and cannot change owner/device/generation/capability fences, terminal state, retry semantics, or replay admission. Schema v4 additionally persists an effectful operation's exact pre-send dispatch binding (capability revision plus one-shot grant ID, alongside the already-authoritative operation/device/original-generation/capability fields) and an explicit reconciliation status. The Agent keeps a separate bounded payload-free terminal-evidence journal and reports it only after a fresh authenticated session; the Hub can self-reconcile an `Indeterminate` record only on exact binding/evidence equality. The candidate terminal checkpoint is committed before the live controller is swapped, so persistence failure leaves quarantine intact. Read-only maintenance exposes labels, fingerprint presence/comparison, reconciliation status, and bounded auto-resolution history while withholding raw request/result data, fingerprint/key values, owner principal, and dispatch-fence value. Schema-v1/v2/v3/v4 checkpoints remain readable within their representational limits; downgrade that would lose newer state is rejected. Retirement never changes the historical execution outcome: the operation stays `Indeterminate`, its exact ID stays permanently non-replayable, the device quarantine alone is released, and only `Scroll`/`MovePointer` are currently eligible after a strictly newer durable device generation fences the original session. The transition exists only in offline local maintenance, not northbound MCP; maintenance must explicitly pin the versioned retirement policy, and durable retirement records are capped at 64 so this permanent-tombstone mechanism remains storage-bounded.

## V2 accepted boundary

V2 is **not** "V1 plus multi-machine routing." The completed competitor-gap PoC and GO/NO-GO review narrowed the accepted boundary to **uncertainty-aware execution safety for delegated control of stateful interactive desktops**. The Hub/Agent topology remains an implementation vehicle for that safety boundary, not a generic fleet/device-fabric product:

```text
MCP Client
   |
   | MCP
   v
Hub
   |
   | authenticated, typed, backend-neutral command/grant protocol
   v
outbound Agent
   |
   +-- direct process/shell executor
   +-- bounded filesystem read/list capabilities
   +-- GUI/computer-use adapter
        +-- Cua MCP backend
        +-- future native GUI backend
```

The differentiated control semantics are intended to center on:

- cryptographic device identity and enrollment;
- short-lived capability grants with expiry/revocation/replay rules;
- explicit operation IDs and per-device lease ownership;
- fail-closed cancellation/reconnect behavior;
- policy-decision evidence without raw desktop-content logging;
- a backend-neutral capability contract.

Transport is an implementation choice, not the product boundary. The M1 production candidate is now **gRPC bidirectional streaming over TLS**, while the earlier raw TLS transport remains as a regression/reference implementation. The application command/grant schema remains transport-neutral so a later WebSocket, QUIC, or other transport adapter does not redefine semantics. During the first gRPC migration slice, Protobuf owns the RPC/carrier framing while the existing independently signed application messages remain unchanged inside the bounded carrier; this deliberately avoids coupling transport migration to a simultaneous security-protocol rewrite.

Cua remains an important GUI/computer-use backend, but Cua-specific tool names or wire behavior must not become the permanent Hub-to-Agent protocol. The reviewed parity target and stateful interaction boundary are documented in [`V2_CUA_PARITY_MATRIX.md`](v2/V2_CUA_PARITY_MATRIX.md) and [`V2_INTERACTION_CONTEXT.md`](v2/V2_INTERACTION_CONTEXT.md). V2 therefore defines a backend-neutral semantic GUI vocabulary: `ListWindows`, `LaunchApplication`, `InspectWindow`, and `VerifyUiState` are typed CUMG capabilities, while Cua `list_windows`, `launch_app`, `get_window_state`, `verify_state`, AX roles, and session helpers terminate at the adapter. This is not a minimum-common-denominator restriction: each backend advertises the semantic subset it actually implements. See [`V2_GUI_SEMANTIC_CAPABILITIES.md`](v2/V2_GUI_SEMANTIC_CAPABILITIES.md). Direct process/shell execution is owned by the Agent itself and must not be implemented by automating a terminal window through Cua. Structured argv execution avoids implicit shell parsing but is still `Dangerous`: scripts/interpreters can execute arbitrary code and cwd policy does not confine argv filesystem access. M1 therefore scopes Agent-native process grants to the exact device capability. Separate `ReadFile`/`ListDirectory` observation capabilities canonicalize paths under approved roots, bound returned content, reject symlink escape, and return coarse errors. These read-only operations are a narrower capability surface; they do not constrain `ExecuteProcess`. Free-form shell execution is a separate implemented `Dangerous` capability with an exact grant and distinct command/result type; it intentionally invokes a fixed OS shell and therefore accepts shell parsing risk without widening `ExecuteProcess`. Explicit filesystem mutation remains a separate future higher-risk surface.

Handoff follows an **optional integration, first-class authority** rule. Ordinary CUMG process/shell execution and GUI operation do not require a Handoff runtime, and deployments may omit the Handoff coordinator entirely. When Handoff is configured, however, it is not treated as a best-effort sidecar: CUMG consults the coordinator at the admission boundary and the Agent re-validates the signed authority/surface binding immediately before backend execution, so active Human authority can fence Agent dispatch before Cua or another target-surface backend acts. Disabling or omitting Handoff must remove that optional capability rather than create a weaker fallback path. The canonical Handoff FSM, checkpoint/recovery semantics, Human transport, capture, and input remain owned by the Handoff runtime on the controlled Agent; CUMG must not duplicate that state machine or absorb WebRTC/TURN mechanics into its core.

The implementation order is intentionally shell-first: establish the secure Agent core, add direct process/shell execution, add only the bounded filesystem operations required by those workflows, retain Cua for GUI/computer-use during the transition, then add native GUI backends later. This keeps GUI backend replacement independent from the Agent's usefulness for development and operations tasks.

The M1 operator-facing `v2_agent` process now uses outbound gRPC bidirectional streaming over TLS as the production-candidate carrier. It keeps receiving heartbeats/cancellation while Agent-native work runs off the async receive loop, and it performs bounded reconnect after transport loss. The companion `v2_hub` process is the single-device always-on-VM runtime: it authenticates the enrolled Agent, persists generation/admission state before risky transitions, maintains heartbeat/offline state, issues exact-capability grants, and conservatively marks ambiguous disconnect outcomes `indeterminate`. The raw TLS carrier remains a regression/reference implementation rather than the deployment default.

The V2-M0 PoC and later competitor review applied that stop rule: CUMG does not broaden into another generic remote-device orchestrator, fleet platform, device fabric, remote desktop, or delegated-authorization protocol.

See [`ROADMAP.md`](ROADMAP.md) for milestones and explicit non-goals, and [`V2_THREAT_MODEL.md`](v2/V2_THREAT_MODEL.md) for V2 trust boundaries, compromise assumptions, key rotation, replay, cancellation, and residual risks.
