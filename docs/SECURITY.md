# Security

Computer-use is equivalent to granting remote keyboard, mouse, screen, clipboard, application, and potentially shell/filesystem capabilities. The gateway is therefore a security boundary, not merely a transport adapter.

## V1 defaults

- Listen only on `127.0.0.1` unless explicitly overridden.
- Do not implement anonymous public internet exposure.
- Require an authenticating TLS reverse proxy for remote access.
- Keep Cua on stdio; do not expose its backend transport directly.
- Validate inbound MCP `Host` authorities.
- Validate browser `Origin` values when they are present.
- Fail closed on gateway tool policy: an empty allowlist exposes no tools.
- Require explicit `CUMG_ALLOW_TOOLS=*` to expose every discovered backend tool.
- Apply deny rules before forwarding a call.
- Serialize backend operations against the one physical desktop.
- Bound backend connection and operation duration with timeouts.
- Reconnect after transport failure with bounded exponential backoff.
- Never replay a failed tool call automatically; its side effects may already have occurred.
- Avoid logging raw tool arguments, results, screenshots, clipboard values, or credentials.

## Two policy layers

The gateway performs a coarse capability check by MCP tool name. `CUMG_DENY_TOOLS` always overrides `CUMG_ALLOW_TOOLS`.

Cua's own policy engine should be used as an optional second layer when argument-level constraints matter. [`../examples/cua-policy.yaml`](../examples/cua-policy.yaml) demonstrates a deny-by-default backend policy with a bounded `type_text` rule. Gateway policy may narrow Cua; it must never be used as a reason to widen Cua's OS permissions or backend policy.

Future semantic categories may provide a friendlier configuration surface:

- `observe`: screenshot, accessibility snapshot, window/app listing
- `interact`: click, type, scroll, drag, keyboard
- `system`: clipboard, app launch, file interactions
- `dangerous`: shell, destructive filesystem actions, credential-sensitive actions

Until those categories exist, configure exact tool names and review backend upgrades that add new capabilities. Dynamic rediscovery still applies the gateway policy before a newly discovered tool can become visible.

## Host and Origin validation

`/mcp` uses rmcp's Streamable HTTP Host and Origin guards. The default accepted Host authorities are loopback-only. The default browser origins are derived from the configured loopback bind port.

For a reverse proxy, prefer preserving a loopback origin Host where possible. If the proxy forwards a public Host, add that exact authority to `CUMG_ALLOWED_HOSTS`. Add browser origins to `CUMG_ALLOWED_ORIGINS` only when they are actually needed. Do not solve a deployment mismatch by disabling Host or Origin validation globally.

`/healthz` is intentionally non-sensitive readiness metadata. Authentication still belongs at the reverse proxy for all remotely reachable routes.

## Backend failure semantics

Backend discovery may be retried after a connection failure because it is read-only. A failed computer-use action is different: timeout or transport failure can leave the desktop in an unknown state. The gateway therefore repairs the backend connection for a subsequent request but returns an error for the current request and does not replay it.

All backend calls are serialized in V1. This favors correctness over throughput because independent clients otherwise share the same cursor, focus, windows, accessibility snapshot indices, and application state.

## Cloudflare deployment

Recommended V1 topology:

```text
Internet
  |
Cloudflare Access
  |
Cloudflare Tunnel
  |
127.0.0.1:<gateway>
  |
Cua stdio
```

Do not bind directly to `0.0.0.0` merely because Cloudflare Tunnel is present. Access authentication and TLS are upstream requirements; the V1 gateway does not implement public authentication itself.

## Self-hosted desktop E2E

A TCC-granted desktop runner is a high-trust machine. `.github/workflows/desktop-e2e.yml` is therefore deliberately manual-only, limited to `main`, and requires a dedicated `cua-desktop-e2e` runner label. It must never run for public pull requests or arbitrary fork code.

Use a dedicated test Mac rather than a daily-use workstation. The runner must already be logged into a GUI session and have CuaDriver's Accessibility and Screen Recording permissions configured before the workflow is enabled.

## CI supply chain

Normal CI pins the Cua Driver compatibility target and downloads the versioned release installer rather than a mutable convenience URL. The installer file's SHA-256 is verified before execution. Rust dependency resolution is locked for normal builds after `Cargo.lock` is committed, and CI uses `--locked` so dependency changes require an explicit repository update.
