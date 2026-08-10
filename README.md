# computer-use-mcp-gateway

A lightweight Rust gateway that exposes local computer-use MCP backends through a remote MCP endpoint.

> Status: **V1 scaffold / pre-alpha**. The architecture and backend boundary are committed; transparent MCP tool forwarding is the next implementation milestone.

## Why

Desktop computer-use servers are usually local `stdio` MCP processes. Remote clients such as ChatGPT need an HTTP MCP endpoint, but exposing the desktop driver directly couples internet-facing concerns to OS automation.

This project separates them:

```text
ChatGPT / Claude / Codex / any MCP client
                 |
          MCP Streamable HTTP
                 |
                 v
      computer-use-mcp-gateway
          (Rust, localhost)
                 |
             MCP stdio
                 |
                 v
             Cua Driver
                 |
       macOS / Windows / Linux
```

The gateway owns the network boundary. The computer-use backend owns desktop automation.

## V1 scope

- MCP Streamable HTTP endpoint (`/mcp`)
- Localhost-only binding by default
- Spawn/connect to `cua-driver mcp` over stdio
- Discover backend tools dynamically
- Forward tool calls and results without reimplementing computer-use
- Tool allow/deny policy hook
- Structured audit/health logging
- Designed to sit behind Cloudflare Tunnel / Access or another reverse proxy

### Explicit non-goals for V1

- Multi-machine routing
- A custom desktop automation engine
- A cloud control plane
- Custom Hub-to-Agent transport

## V2 direction

```text
LLM --MCP--> Hub --typed RPC/WebSocket--> Agent --MCP/native--> CUA/backend
                                      |--> Agent (Windows)
                                      |--> Agent (Linux)
```

V2 adds outbound agents and multi-machine routing. The public northbound API remains MCP; the southbound transport can evolve independently.

## Backend

The first backend is [Cua Driver](https://github.com/trycua/cua), which provides an MCP server over stdio via:

```bash
cua-driver mcp
```

On macOS, Cua's TCC/process-identity requirements must be respected. The gateway must not casually replace Cua's supported app/embedded launch lifecycle.

## MCP version

The gateway targets MCP `2026-07-28` through the official Rust SDK (`rmcp` 3.x). The public HTTP endpoint is intended to be stateless at the protocol layer.

## Development

Rust is intentionally not installed automatically by this repository.

Once Rust 1.88+ is available:

```bash
cargo check
cargo test
cargo run -- --help
```

Default development configuration:

```bash
cp .env.example .env
```

## Security model

V1 follows a strict boundary:

1. Bind the gateway to `127.0.0.1` by default.
2. Put TLS/authentication at a trusted reverse proxy such as Cloudflare Access.
3. Never expose the Cua stdio process directly.
4. Apply tool policy before forwarding calls.
5. Record tool name, duration, outcome, and policy decision without logging sensitive arguments by default.

See [`docs/SECURITY.md`](docs/SECURITY.md).

## Roadmap

See [`docs/ROADMAP.md`](docs/ROADMAP.md).

## License

MIT. This is an independent project and is not affiliated with Cua AI or the Model Context Protocol project.
