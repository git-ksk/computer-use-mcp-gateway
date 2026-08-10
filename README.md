# computer-use-mcp-gateway

A lightweight Rust gateway that exposes a local computer-use MCP backend through a policy-controlled remote MCP endpoint.

> Status: **V1 implemented / pre-alpha**. The gateway connects to Cua over MCP stdio, discovers its tools dynamically, and forwards them through a Streamable HTTP MCP endpoint.

## Architecture

```text
ChatGPT / Claude / Codex / any MCP client
                 |
          MCP Streamable HTTP
                 |
                 v
      computer-use-mcp-gateway
          (Rust, localhost)
          policy + audit
                 |
             MCP stdio
                 |
                 v
          cua-driver mcp
                 |
       macOS / Windows / Linux
```

The gateway owns the network and policy boundary. Cua owns desktop automation and OS-specific permissions.

## V1

Implemented V1 capabilities:

- MCP Streamable HTTP endpoint at `/mcp`
- Sessionless public MCP transport
- Localhost-only binding by default (`127.0.0.1:8100`)
- `cua-driver mcp` child process over MCP stdio
- Dynamic backend tool discovery at startup
- Transparent tool/result forwarding
- Optional comma-separated allowlist and denylist; deny wins
- Tool name, policy decision, outcome, and duration audit fields without tool arguments/results
- `/healthz` backend readiness endpoint
- Graceful HTTP/backend shutdown
- Designed for an authenticated reverse proxy such as Cloudflare Tunnel / Access

V1 intentionally does **not** implement multi-machine routing, a custom computer-use engine, a cloud control plane, or a custom Hub-to-Agent protocol.

## V2 direction

```text
LLM --MCP--> Hub --typed RPC/WebSocket--> Agent --MCP/native--> CUA/backend
                                      |--> Agent (Windows)
                                      |--> Agent (Linux)
```

V2 can move the device-side process behind an outbound Agent while keeping MCP as the northbound client API. The Hub-to-Agent command model should remain transport-neutral so its transport can evolve independently.

## Backend

The initial backend is [Cua Driver](https://github.com/trycua/cua), connected via:

```bash
cua-driver mcp
```

On macOS, keep Cua's supported application/TCC process lifecycle intact. The gateway does not replace Cua's OS automation implementation.

## Run

Requirements:

- Rust 1.88+
- `cua-driver` available on `PATH`
- Cua permissions configured for the target OS

```bash
cp .env.example .env
set -a; source .env; set +a
cargo run
```

Default endpoints:

```text
MCP     http://127.0.0.1:8100/mcp
Health  http://127.0.0.1:8100/healthz
```

### Configuration

| Variable | Default | Purpose |
| --- | --- | --- |
| `CUMG_BIND` | `127.0.0.1:8100` | HTTP bind address |
| `CUMG_MCP_PATH` | `/mcp` | MCP endpoint path |
| `CUMG_BACKEND_COMMAND` | `cua-driver` | Backend executable |
| `CUMG_BACKEND_ARGS` | `mcp` | Backend command arguments |
| `CUMG_ALLOW_TOOLS` | empty | Optional comma-separated allowlist |
| `CUMG_DENY_TOOLS` | empty | Optional comma-separated denylist |
| `RUST_LOG` | `info` | Logging filter |

If `CUMG_ALLOW_TOOLS` is empty, all discovered backend tools are eligible unless denied. If an allowlist is set, tools outside it are blocked. `CUMG_DENY_TOOLS` always wins.

## Development

```bash
cargo fmt --check
cargo check --all-targets
cargo test
```

CI runs the same checks on pushes and pull requests.

## Security model

1. Bind to loopback by default.
2. Put TLS and remote authentication at a trusted reverse proxy before remote exposure.
3. Never expose the Cua stdio process directly.
4. Apply gateway policy before forwarding a tool call.
5. Do not log MCP tool arguments or results by default.
6. Treat backend tool descriptions and annotations as untrusted metadata; enforcement comes from gateway policy, not annotations.

See [`docs/SECURITY.md`](docs/SECURITY.md) for the security notes.

## Roadmap

The canonical roadmap is maintained in the project design report. The repository's [`docs/ROADMAP.md`](docs/ROADMAP.md) is a supporting snapshot, not a replacement for that report.

## License

MIT. This is an independent project and is not affiliated with Cua AI or the Model Context Protocol project.
