# computer-use-mcp-gateway

A lightweight Rust gateway that exposes a local computer-use MCP backend through a policy-controlled MCP Streamable HTTP endpoint.

> Status: **V2 recommended runtime / V1 legacy-reference (2026-08-13)**. The default `computer-use-mcp-gateway` binary now runs the V2 Hub; desktops run the separate `v2_agent`. V1 remains available as `v1_gateway` for regression/reference. See [`docs/V2_USAGE_ACCOUNTING.md`](docs/V2_USAGE_ACCOUNTING.md) for the optional MCPUsage integration.

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

### Recommended runtime

Build all binaries first:

```bash
git clone https://github.com/git-ksk/computer-use-mcp-gateway.git
cd computer-use-mcp-gateway
cargo build --locked
```

The recommended deployment is **V2 Hub + V2 Agent**. Key/trust/TLS provisioning is intentionally explicit, so use [`docs/GETTING_STARTED.md`](docs/GETTING_STARTED.md) and [`docs/DEPLOYMENT.md`](docs/DEPLOYMENT.md) rather than replacing those boundaries with an insecure one-line example. `cargo run --locked -- --help` now shows the V2 Hub options; `cargo run --locked --bin v2_agent -- --help` shows the desktop Agent options.

The V2 northbound MCP exposes the typed capabilities already present in the V2 contract, including the existing Cua-backed `list_apps`, `get_screen_size`, `click`, and `drag` operations. It does **not** expose a generic/raw Cua passthrough.

For V1-only regression or legacy operation:

```bash
cargo run --locked --bin v1_gateway -- --allow-tools list_apps,list_windows,get_accessibility_tree,get_screen_size
```

V1 remains source-compatible for regression but is no longer the recommended runtime.

## Architecture

```text
MCP client
    |
    | OAuth-protected MCP
    v
V2 Hub northbound
    |  optional MCPUsage reserve
    |  CUMG authorization / ownership / generation / quarantine
    |  optional MCPUsage markLiable
    |
    | gRPC bidirectional stream over TLS
    v
V2 Agent
    |
    | MCP stdio
    v
Cua Driver
```

CUMG is the execution/replay/quarantine authority. Optional MCPUsage is accounting authority only and cannot clear `indeterminate`, authorize replay, or replace explicit resolution. With usage disabled, V2 uses `NoopUsageController` and requires no Node sidecar.

The exact authority split, 0/1-unit settlement rules, failure semantics, and non-durable `MemoryUsageStore` boundary are documented in [`docs/V2_USAGE_ACCOUNTING.md`](docs/V2_USAGE_ACCOUNTING.md).

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

The default binary is the V2 Hub. Run `cargo run --locked -- --help` for its Hub/northbound options and `cargo run --locked --bin v2_agent -- --help` for Agent options. Packaged examples live under `packaging/`.

Optional usage accounting is enabled only when `CUMG_V2_USAGE_ENDPOINT` is set to a literal loopback endpoint such as `http://127.0.0.1:8787/`; otherwise the Noop controller preserves normal V2 behavior. The sidecar uses a required positive `CUMG_USAGE_LIMIT_PER_PRINCIPAL` and a non-durable `MemoryUsageStore`.

### Legacy V1 settings

The following variables belong to `v1_gateway` and remain for regression/reference:

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

For V2, keep the northbound MCP listener loopback-only and terminate the public HTTPS resource at a reviewed proxy/load balancer. V2 validates OAuth/introspection before constructing the principal that reaches CUMG or MCPUsage. The Agent connects outbound to the Hub over the existing gRPC/TLS carrier.

The older Cloudflare/V1 deployment guidance remains documented for `v1_gateway`; do not confuse that legacy path with the recommended V2 OAuth boundary. See [`docs/DEPLOYMENT.md`](docs/DEPLOYMENT.md).

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
- [`docs/V1_ACCEPTANCE.md`](docs/V1_ACCEPTANCE.md) — V1 closure evidence
- [`docs/V2_POSITIONING.md`](docs/V2_POSITIONING.md) — canonical V2 product boundary and core-first priority
- [`docs/V2_STANDARDIZATION.md`](docs/V2_STANDARDIZATION.md) — standard/OSS replacement boundary versus custom uncertainty-aware execution semantics
- [`docs/V2_M1_ACCEPTANCE.md`](docs/V2_M1_ACCEPTANCE.md) — accepted single secure remote Agent evidence
- [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md) — V1 boundaries, state, cancellation, metrics, and V2 boundary
- [`docs/SECURITY.md`](docs/SECURITY.md) — trust boundaries, policy, CI supply chain, and desktop-runner safety
- [`docs/TESTING.md`](docs/TESTING.md) — CI matrix, closeout quality gates, conformance scope, and desktop E2E
- [`docs/ROADMAP.md`](docs/ROADMAP.md) — implementation snapshot; V2 positioning is further narrowed by `V2_POSITIONING.md`

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

V1 was closed on 2026-08-11 after automated/code-local checks plus trusted real-desktop and Cloudflare Access/Tunnel + ChatGPT remote MCP dogfood. See [`docs/V1_ACCEPTANCE.md`](docs/V1_ACCEPTANCE.md).

Do not expand V1 indefinitely merely because adjacent backend features are technically possible.

## V2 direction

V2-M0 through the final V2 execution-safety, multi-device invariant, backend-portability, replacement-seam, observability, resource-regression, and trusted real-desktop acceptance work are complete as of 2026-08-13.

CUMG is **not** trying to win the broad category of vendor-neutral physical-device control planes. That space already overlaps materially with projects such as SINT Protocol and Arm Device Connect, in addition to OpenClaw, OAHL, QuickDesk, Obot, and delegated-authorization systems.

The V2 core is:

```text
external authorization
        |
        v
specific desktop + exact capability
        |
        v
operation ID + exclusive ownership + fencing
        |
        v
state-changing action
        |
        v
ambiguous outcome?
        |
        +--> no  -> terminal
        |
        +--> yes -> indeterminate -> quarantine -> explicit resolution
```

An ambiguous state-changing operation is never automatically replayed because a client, Hub, Agent, transport, backend, or device reconnects.

### Core-first boundary

V2 closeout established the authoritative operation state machine, durable `indeterminate` quarantine, explicit audited resolution, ownership/generation fencing, restart/reconnect no-auto-replay behavior, fixed-set multi-device invariant proof, backend portability, payload-safe observability, and trusted real-Cua desktop acceptance.

Future work must preserve those invariants. Generic authentication, delegated authorization, device fabric/registry, fleet routing, remote desktop, dashboards, orchestration, telemetry infrastructure, TLS lifecycle, and service supervision remain outside the V2 core and should use standards or maintained OSS when appropriate rather than growing a second generic control plane.

See [`docs/V2_POSITIONING.md`](docs/V2_POSITIONING.md) and [`docs/V2_STANDARDIZATION.md`](docs/V2_STANDARDIZATION.md).

## License

MIT. This is an independent project and is not affiliated with Cua AI or the Model Context Protocol project.
