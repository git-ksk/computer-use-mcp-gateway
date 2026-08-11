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
- deny-by-default policy enforcement
- health and audit metadata

**Backend adapter**
- child-process connection lifecycle
- connection and operation timeouts
- bounded reconnect/backoff
- serialization of operations against one physical desktop
- no automatic replay of failed state-changing calls

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
  list_tools()
  call_tool()
  shutdown()
```

The first implementation is `CuaBackend`.

## State model

MCP `2026-07-28` removes protocol-level HTTP sessions. Public request handlers therefore cannot rely on client state stored in an HTTP MCP session.

Application-level state is independent of MCP transport sessions:

- `Gateway` owns the policy and a shared, policy-filtered tool snapshot.
- `CuaBackend` owns the current MCP client service and synchronization locks.
- `tools/list` refreshes backend discovery; if refresh fails it may serve the last policy-filtered cached snapshot.
- a policy-allowed `tools/call` missing from the current snapshot triggers one refresh and fails closed if discovery still cannot confirm the tool.
- backend operations are serialized because cursor/focus/UI snapshot state is shared mutable desktop state.

## Failure model

Read-only tool discovery can reconnect and retry after a transport failure. State-changing computer-use calls cannot be safely replayed because the desktop may already have partially applied the action. A failed call is therefore returned as an error after recovery is attempted for the next request.

## Security boundary

The gateway is not an internet authentication service. Remote deployments keep the process on loopback and place authenticated TLS termination in front of it. The `/mcp` boundary additionally validates Host authorities and browser Origin values.

Tool exposure is deny-by-default. Cua's own policy engine can provide a second, argument-aware capability ceiling.

## V2 boundary

V2 splits the local gateway into Hub + Agent:

```text
MCP Client
   |
   | MCP
   v
Hub
   |
   | typed RPC over WebSocket (initial candidate)
   v
Agent
   |
   +-- Cua MCP backend
   +-- future native backend
```

The Hub-to-Agent protocol must be transport-neutral so WebSocket can later be replaced by QUIC/gRPC without changing command semantics.
