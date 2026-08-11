# computer-use-mcp-gateway

A lightweight Rust gateway that exposes a local computer-use MCP backend through a policy-controlled MCP Streamable HTTP endpoint.

> Status: **V1 hardened / pre-alpha; automated closeout complete**. The gateway connects to Cua over MCP stdio, applies a fail-closed capability boundary, and keeps the network listener on localhost by default. One operator-controlled acceptance check remains before V1 is formally closed; see [`docs/V1_ACCEPTANCE.md`](docs/V1_ACCEPTANCE.md).

## Start here

New to the project? Follow **[`docs/GETTING_STARTED.md`](docs/GETTING_STARTED.md)** from top to bottom. It covers:

1. installing Git/Rust and the CI-tested Cua Driver version on macOS, Windows, or Linux;
2. configuring platform permissions;
3. verifying Cua independently;
4. building and starting the gateway;
5. checking `/healthz`;
6. connecting a local MCP client;
7. adding remote access only after the local path works.

If setup fails, use **[`docs/TROUBLESHOOTING.md`](docs/TROUBLESHOOTING.md)** instead of opening the security boundary until it works.

### Short local path

Assuming Git, Rust 1.88+, and a working Cua Driver 0.19.3 are already installed:

```bash
git clone https://github.com/git-ksk/computer-use-mcp-gateway.git
cd computer-use-mcp-gateway
cargo build --locked
cargo run --locked -- --allow-tools list_apps,list_windows,get_accessibility_tree,get_screen_size
```

Then check:

```bash
curl --fail http://127.0.0.1:8100/healthz
```

A ready response has `status: ok` and `backend: ready` and may include a `backend_resources` snapshot for the gateway-owned backend child process:

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

The starter allowlist is non-mutating but **not non-sensitive**: application/window names and accessibility text can contain private information.

Local MCP endpoint:

```text
http://127.0.0.1:8100/mcp
```

For Codex CLI/IDE, ChatGPT desktop, ChatGPT web, and generic Streamable HTTP clients, see [`docs/CLIENTS.md`](docs/CLIENTS.md).

## Architecture

```text
MCP client
    |
    | local Streamable HTTP
    | or authenticated TLS proxy for remote use
    v
+-----------------------------+
| computer-use-mcp-gateway    |
|                             |
| Host / Origin guards        |
| deny-by-default tool policy |
| audit / timeout / reconnect |
| serialized backend access   |
+-------------|---------------+
              |
              | MCP stdio
              v
         cua-driver mcp
              |
      macOS / Windows / Linux
```

The gateway owns the MCP network/policy boundary. Cua owns desktop automation and OS-specific permissions.

## V1 capabilities

- MCP Streamable HTTP endpoint at `/mcp`
- compatibility smoke coverage for `2025-11-25` and stateless `2026-07-28` MCP lifecycles
- pinned official MCP conformance runner for V1-applicable server-boundary scenarios
- localhost-only binding by default (`127.0.0.1:8100`)
- Host validation and browser Origin validation on the MCP endpoint
- `cua-driver mcp` child process over MCP stdio
- dynamic backend tool rediscovery without a gateway restart
- **deny-by-default** exact-name tool policy; `*` is an explicit opt-in to every discovered tool
- conservative semantic classification: `observe`, `interact`, `system`, `dangerous`; unknown tools classify as `dangerous`
- denylist overrides allowlist
- backend connect/tool timeouts and bounded exponential reconnect
- upstream MCP cancellation propagated to the actual downstream request ID
- failed, timed-out, and cancelled tool calls are never replayed automatically because their side effects may be unknown
- serialized backend operations so independent clients cannot interleave actions on one physical desktop
- tool name, semantic class, policy decision, outcome, and duration audit fields without raw tool arguments/results
- `/healthz` readiness plus gateway-owned backend child PID/cumulative CPU/RSS telemetry where available
- graceful HTTP/backend shutdown
- optional Cua policy layer for argument-level defense in depth
- real-Cua CI smoke coverage on Linux, macOS, and Windows
- deterministic 100-call `tools/call` soak and hosted-Linux idle CPU/RSS regression gate
- manual trusted self-hosted macOS desktop E2E lane for screenshot → click → type → independent readback

The dual-protocol smoke and selected official conformance scenarios are **not** a full MCP conformance certification. The upstream complete requirement sets include capabilities and fixture-specific behavior that this tools-only gateway intentionally does not advertise. See [`docs/TESTING.md`](docs/TESTING.md).

V1 intentionally does **not** provide built-in public authentication/TLS, multi-machine routing, per-user desktop isolation, a custom computer-use engine, or a cloud control plane.

## Backend

The initial backend is [Cua Driver](https://github.com/trycua/cua):

```bash
cua-driver mcp
```

The repository CI currently pins Cua Driver **0.19.3** as its reviewed compatibility input. Newer Cua releases may work, but should not be treated as tested until the CI pin is deliberately updated.

On macOS, keep Cua's supported application/TCC lifecycle intact. The gateway does not replace Cua's OS automation implementation. The resource fields in `/healthz` describe the direct backend child owned by the gateway, not necessarily aggregate resource use across every Cua process.

For an additional backend-side capability ceiling, review [`examples/cua-policy.yaml`](examples/cua-policy.yaml) and configure `CUA_DRIVER_POLICY_FILE`.

## Configuration

All settings are available through CLI/environment configuration. Run `cargo run --locked -- --help` for CLI flags. `.env.example` provides a persistent environment template.

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

The binary itself fails closed when `CUMG_ALLOW_TOOLS` is empty. Use `CUMG_ALLOW_TOOLS=*` only when full backend exposure is intentional and reviewed.

V1 splits `CUMG_BACKEND_ARGS` on ASCII whitespace and does not implement shell-style quoting for embedded spaces. See [`docs/DEPLOYMENT.md`](docs/DEPLOYMENT.md).

## Remote access

Do **not** bind directly to `0.0.0.0` just to make the gateway remote.

Keep the gateway on loopback and place an authenticated TLS reverse proxy/tunnel in front of it. [`docs/DEPLOYMENT.md`](docs/DEPLOYMENT.md) includes a Cloudflare Access/Tunnel path and explains the Host/Origin guard configuration.

The gateway does not provide public authentication itself in V1.

## Development

```bash
cargo fmt --check
cargo check --locked --all-targets
cargo test --locked
python3 -m py_compile \
  scripts/cua_gateway_smoke.py \
  scripts/cua_desktop_e2e.py \
  scripts/mock_mcp_backend.py \
  scripts/v1_quality_gate.py \
  scripts/v1_conformance.py
cargo build --locked
python3 scripts/v1_quality_gate.py
python3 scripts/v1_conformance.py
python3 scripts/check_docs.py
```

Normal CI independently verifies the pinned Cua installer SHA-256, platform release payload SHA-256, and installed `cua-driver` identity before running real gateway/Cua smoke tests on Linux, macOS, and Windows against both exercised MCP lifecycles. It also runs cancellation, 100-call soak, resource, and selected official conformance gates. The separate read-only Docs workflow checks repository-local Markdown links.

See [`docs/TESTING.md`](docs/TESTING.md) for the exact guarantees and limits.

## Documentation

- **[`docs/GETTING_STARTED.md`](docs/GETTING_STARTED.md)** — clean-machine install-to-first-working-local-connection guide
- **[`docs/CLIENTS.md`](docs/CLIENTS.md)** — MCP client configuration, including local and authenticated remote examples
- **[`docs/TROUBLESHOOTING.md`](docs/TROUBLESHOOTING.md)** — symptom-based setup/debugging guide
- [`docs/DEPLOYMENT.md`](docs/DEPLOYMENT.md) — localhost-first remote deployment and reverse-proxy requirements
- [`docs/V1_ACCEPTANCE.md`](docs/V1_ACCEPTANCE.md) — the two operator-controlled checks remaining before formal V1 closure
- [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md) — V1 boundaries, state, cancellation, metrics, and gated V2 boundary
- [`docs/SECURITY.md`](docs/SECURITY.md) — trust boundaries, policy, CI supply chain, and desktop-runner safety
- [`docs/TESTING.md`](docs/TESTING.md) — CI matrix, closeout quality gates, conformance scope, and desktop E2E
- [`docs/ROADMAP.md`](docs/ROADMAP.md) — implementation snapshot and V2 GO/NO-GO gate; the project design report remains canonical

## Security model

1. Bind to loopback by default.
2. Put TLS and remote authentication at a trusted reverse proxy before remote exposure.
3. Validate inbound Host and Origin values at the MCP boundary.
4. Fail closed on exact-name tool capability policy.
5. Treat semantic classification as audit/review metadata, not authorization.
6. Never expose the Cua backend transport directly.
7. Propagate cancellation downstream and never automatically replay ambiguous calls.
8. Serialize operations against the single physical desktop in V1.
9. Do not log MCP tool arguments, results, screenshots, or credentials by default.
10. Use Cua's own policy engine as a second, narrower enforcement layer where practical.

See [`docs/SECURITY.md`](docs/SECURITY.md) before using the gateway on a sensitive or remotely reachable desktop.

## V1 closure

All automated/code-local closeout work is intended to be enforced by CI. Formal V1 closure requires the two operator-controlled checks in [`docs/V1_ACCEPTANCE.md`](docs/V1_ACCEPTANCE.md):

1. execute the trusted macOS GUI E2E on the dedicated TCC-granted test Mac;
2. complete representative Cloudflare Access/Tunnel + ChatGPT remote MCP dogfood.

Do not start V2 simply because implementation work is complete.

## V2 candidate

V2 is **not** automatically a generic Hub + Agent / multi-machine expansion. The roadmap gates major V2 work on a competitor-gap PoC and an explicit GO/NO-GO decision.

The candidate direction is a **secure delegated device capability control plane**:

```text
MCP client --MCP--> Hub --authenticated typed command/grant protocol--> outbound Agent --> backend
```

The intended differentiation is capability-control semantics—device identity, short-lived grants, operation leases, replay/cancellation safety, and policy evidence—not another screenshot/input engine or remote-desktop transport.

Cua remains the first backend, but the Hub-to-Agent contract must remain backend- and transport-neutral. See [`docs/ROADMAP.md`](docs/ROADMAP.md) and [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md).

## License

MIT. This is an independent project and is not affiliated with Cua AI or the Model Context Protocol project.
