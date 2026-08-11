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

A read-only tool is not necessarily low-sensitivity. Screenshots, accessibility trees, window titles, app lists, and clipboard reads can disclose private information even when they do not mutate the desktop. Treat `observe` capabilities as data-access permissions, not merely harmless diagnostics.

Future semantic categories may provide a friendlier configuration surface:

- `observe`: screenshot, accessibility snapshot, window/app listing
- `interact`: click, type, scroll, drag, keyboard
- `system`: clipboard, app launch, file interactions
- `dangerous`: shell, destructive filesystem actions, credential-sensitive actions

Until those categories exist, configure exact tool names and review backend upgrades that add new capabilities. Dynamic rediscovery still applies the gateway policy before a newly discovered tool can become visible.

## Host and Origin validation

`/mcp` uses rmcp's Streamable HTTP Host and Origin guards. The default accepted Host authorities are loopback-only. The default browser origins are derived from the configured loopback bind port.

For a reverse proxy, prefer preserving or rewriting the origin Host to an allowed loopback authority where practical. If the proxy forwards a public Host, add that exact authority to `CUMG_ALLOWED_HOSTS`. Add browser origins to `CUMG_ALLOWED_ORIGINS` only when they are actually needed. Do not solve a deployment mismatch by disabling Host or Origin validation globally.

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

If Cloudflare forwards the public hostname in `Host`, set `CUMG_ALLOWED_HOSTS` to that exact hostname (and port if applicable). See [`DEPLOYMENT.md`](DEPLOYMENT.md) and [`../examples/cloudflared.yml`](../examples/cloudflared.yml).

## Self-hosted desktop E2E

A TCC-granted desktop runner is a high-trust machine. `.github/workflows/desktop-e2e.yml` is therefore deliberately manual-only, limited to `main`, and requires a dedicated `cua-desktop-e2e` runner label. It must never run for public pull requests or arbitrary fork code.

Use a dedicated test Mac rather than a daily-use workstation. The runner must already be logged into a GUI session and have CuaDriver's Accessibility and Screen Recording permissions configured before the workflow is enabled.

## CI supply chain

Normal CI uses read-only repository permissions and locked Rust dependency resolution.

For the Cua compatibility target, CI verifies the chain before any real-Cua smoke test:

1. Pin a specific Cua Driver version.
2. Download the versioned installer and verify its SHA-256.
3. Download the platform-specific release payload and verify its pinned SHA-256 independently of the installer.
4. Extract the expected `cua-driver` executable from that verified payload and record its SHA-256.
5. Run the verified installer.
6. Resolve the installed `cua-driver` from `PATH` and require its executable SHA-256 to match the executable extracted from the verified release payload.
7. Only then build the gateway with `cargo build --locked` and run real-Cua protocol smoke tests.

This protects CI from a mutable convenience installer and also detects an installer that would install bytes different from the independently verified release payload. It does not make a broader claim that the upstream release itself is trustworthy; the pinned hashes are the repository's reviewed compatibility inputs.

## Secrets and logs

Do not commit tunnel credentials, Access tokens, private hostnames, personal filesystem paths, screenshots, or desktop artifacts. `.env` and `.env.*` are ignored except for `.env.example`.

Gateway audit logs intentionally record coarse metadata such as tool name, policy decision, outcome, and duration. Do not add raw arguments/results to normal logs. Failure logs from the backend should likewise avoid user content where possible.

## Reporting vulnerabilities

For security-sensitive findings, avoid posting credentials, screenshots, or exploit material containing private data in a public issue. Use GitHub's private vulnerability reporting feature when it is enabled for the repository; otherwise contact the maintainer through a private channel before publishing sensitive details.
