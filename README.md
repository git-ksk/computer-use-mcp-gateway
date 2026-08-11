# computer-use-mcp-gateway

A lightweight Rust gateway that exposes a local computer-use MCP backend through a policy-controlled remote MCP endpoint.

> Status: **V1 hardened / pre-alpha**. The gateway connects to Cua over MCP stdio, applies a fail-closed capability boundary, and exposes it through MCP Streamable HTTP.

## Architecture

```text
ChatGPT / Claude / Codex / any MCP client
                 |
          authenticated TLS proxy
                 |
          MCP Streamable HTTP
                 |
                 v
      computer-use-mcp-gateway
          (Rust, localhost)
       policy + transport guards
        audit + serialization
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
- Legacy `2025-11-25` and stateless `2026-07-28` MCP lifecycle compatibility
- Localhost-only binding by default (`127.0.0.1:8100`)
- Host validation and browser Origin validation on the MCP endpoint
- `cua-driver mcp` child process over MCP stdio
- Dynamic backend tool rediscovery without a gateway restart
- **Deny-by-default** tool policy; `*` is an explicit opt-in to every discovered tool
- Denylist overrides allowlist
- Backend connect/tool timeouts and bounded exponential reconnect
- Failed tool calls are never replayed automatically because their side effects may be unknown
- Serialized backend operations so independent clients cannot interleave actions on one physical desktop
- Tool name, policy decision, outcome, and duration audit fields without raw tool arguments/results
- `/healthz` backend readiness endpoint
- Graceful HTTP/backend shutdown
- Optional Cua policy layer for argument-level defense in depth
- Real-Cua CI smoke coverage on Linux, macOS, and Windows
- Manual, trusted self-hosted macOS desktop E2E lane for screenshot → click → type → independent readback

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

For an additional backend-side capability ceiling, start from [`examples/cua-policy.yaml`](examples/cua-policy.yaml) and set `CUA_DRIVER_POLICY_FILE` to the reviewed policy path.

## Run

Requirements:

- Rust 1.88+
- `cua-driver` available on `PATH`
- Cua permissions configured for the target OS

```bash
cp .env.example .env
set -a; source .env; set +a
cargo run --locked
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
| `CUMG_ALLOW_TOOLS` | empty | Comma-separated allowlist; empty denies all, `*` explicitly allows all discovered tools |
| `CUMG_DENY_TOOLS` | empty | Comma-separated hard denylist |
| `CUMG_ALLOWED_HOSTS` | loopback hosts | Accepted inbound `Host` authorities for `/mcp` |
| `CUMG_ALLOWED_ORIGINS` | loopback origins on bind port | Accepted browser origins for `/mcp` |
| `CUMG_CONNECT_TIMEOUT_SECS` | `15` | Backend connection timeout |
| `CUMG_TOOL_TIMEOUT_SECS` | `60` | Backend MCP operation timeout |
| `CUMG_RECONNECT_ATTEMPTS` | `3` | Connection attempts before failure |
| `CUMG_RECONNECT_BACKOFF_MS` | `250` | Initial exponential reconnect delay |
| `RUST_LOG` | `info` | Logging filter |

The binary itself fails closed when `CUMG_ALLOW_TOOLS` is empty. `.env.example` contains a small inspection-oriented starter allowlist. Use `CUMG_ALLOW_TOOLS=*` only when full backend exposure is intentional.

### Reverse proxy

Keep `CUMG_BIND` on loopback. For a public hostname behind Cloudflare Access/Tunnel or another authenticated TLS proxy, either preserve/rewrite the origin `Host` to an allowed loopback authority or explicitly add the public authority to `CUMG_ALLOWED_HOSTS`. If browser-originated MCP requests are expected, explicitly add their exact HTTPS origin to `CUMG_ALLOWED_ORIGINS`; do not disable the checks globally.

The gateway does **not** provide public authentication itself in V1. Authentication and TLS are a required upstream deployment boundary for remote use.

## Development

```bash
cargo fmt --check
cargo check --locked --all-targets
cargo test --locked
```

Normal CI also installs a checksum-verified pinned Cua Driver and runs the real gateway/Cua smoke on Linux, macOS, and Windows against both MCP protocol lifecycles. It additionally verifies that malicious Host and Origin values are rejected.

### Desktop E2E

`.github/workflows/desktop-e2e.yml` is deliberately `workflow_dispatch`-only, main-branch-only, and targets a dedicated runner labelled `cua-desktop-e2e`. The runner must be a logged-in macOS desktop with the CuaDriver application identity already granted Accessibility and Screen Recording permissions.

Do **not** attach a daily-use Mac as an unrestricted self-hosted runner to this public repository, and never enable this desktop workflow for pull-request events. The E2E fixture opens a fresh TextEdit instance through the gateway, obtains screenshot evidence, clicks the editor, types a unique marker, and independently verifies the resulting accessibility state.

## Security model

1. Bind to loopback by default.
2. Put TLS and remote authentication at a trusted reverse proxy before remote exposure.
3. Validate inbound Host and Origin values at the MCP boundary.
4. Fail closed on tool capability policy.
5. Never expose the Cua backend transport directly.
6. Do not automatically replay a failed state-changing computer-use call.
7. Serialize operations against the single physical desktop in V1.
8. Do not log MCP tool arguments, results, screenshots, or credentials by default.
9. Use Cua's own policy engine as a second, narrower enforcement layer where practical.

See [`docs/SECURITY.md`](docs/SECURITY.md) for the security notes.

## Roadmap

The canonical roadmap is maintained in the project design report. The repository's [`docs/ROADMAP.md`](docs/ROADMAP.md) is a supporting snapshot, not a replacement for that report.

## License

MIT. This is an independent project and is not affiliated with Cua AI or the Model Context Protocol project.
