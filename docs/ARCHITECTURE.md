# Architecture

## V1

The gateway is both an MCP server and an MCP client.

```text
Northbound                                  Southbound

MCP client
    |
    | Streamable HTTP / POST /mcp
    v
+-----------------------------+
| computer-use-mcp-gateway    |
|                             |
|  HTTP transport             |
|       |                     |
|  policy / audit             |
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
- backend lifecycle
- dynamic tool discovery
- request forwarding
- policy enforcement
- health and audit metadata

**Backend**
- screenshots
- accessibility/UI trees
- click/type/scroll
- window/application control
- platform permissions

## Backend abstraction

V1 starts with Cua but must not hard-code Cua semantics into the public gateway surface.

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

MCP `2026-07-28` removes protocol-level HTTP sessions. Public request handlers therefore cannot store client state inside an HTTP MCP session. Shared backend state is owned by an application-level `GatewayState` and is independent of MCP transport sessions.

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
