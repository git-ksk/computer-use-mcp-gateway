# Architecture

## V1

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

## V2 candidate boundary

V2 is **not** automatically "V1 plus multi-machine routing." The roadmap requires a competitor-gap PoC and an explicit GO/NO-GO decision before major V2 implementation.

The candidate is a **secure delegated device capability control plane**:

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
   +-- bounded filesystem capabilities
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

Transport is an implementation choice, not the product boundary. WebSocket may be a candidate transport, but the command/grant schema must remain transport-neutral so a later QUIC/gRPC or other transport change does not redefine semantics.

Cua remains an important GUI/computer-use backend, but Cua-specific tool names or wire behavior must not become the permanent Hub-to-Agent protocol. Direct process/shell execution is owned by the Agent itself and must not be implemented by automating a terminal window through Cua. Structured argv execution is the preferred default; free-form shell execution and filesystem mutation are separate, higher-risk capability surfaces with explicit policy.

The implementation order is intentionally shell-first: establish the secure Agent core, add direct process/shell execution, add only the bounded filesystem operations required by those workflows, retain Cua for GUI/computer-use during the transition, then add native GUI backends later. This keeps GUI backend replacement independent from the Agent's usefulness for development and operations tasks.

If the V2-M0 PoC cannot demonstrate a meaningful capability-control gap against existing computer-use/remote-device products, the roadmap says to stop rather than build another generic remote-device orchestrator.

See [`ROADMAP.md`](ROADMAP.md) for milestones and explicit non-goals, and [`V2_THREAT_MODEL.md`](V2_THREAT_MODEL.md) for V2 trust boundaries, compromise assumptions, key rotation, replay, cancellation, and residual risks.
