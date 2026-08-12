# Security

Computer-use grants a client access to sensitive desktop capabilities. Treat this gateway as a security boundary, not merely a transport adapter.

## V1 defaults

- listen only on `127.0.0.1` unless a different bind is deliberately reviewed;
- require authenticated TLS termination before remote access;
- keep the backend on stdio instead of exposing it directly;
- validate inbound MCP Host authorities and browser Origin values;
- deny all tools when the gateway allowlist is empty;
- require explicit `CUMG_ALLOW_TOOLS=*` to expose every discovered backend tool;
- apply deny rules before forwarding a call;
- serialize operations against the one physical desktop;
- use bounded connection/tool timeouts and reconnect backoff;
- propagate upstream cancellation to the actual downstream MCP request ID;
- never automatically replay failed, timed-out, or cancelled tool calls;
- avoid logging raw tool arguments, results, screenshots, clipboard values, or credentials.

## Policy layers

Authorization remains exact-name based. `CUMG_DENY_TOOLS` overrides `CUMG_ALLOW_TOOLS`.

V1 also classifies tools as `observe`, `interact`, `system`, or `dangerous` for audit/review purposes. Unknown or newly discovered names are classified as `dangerous` until reviewed. Semantic classification does **not** grant access and does not widen the exact-name allowlist.

Cua's own policy engine is an optional second layer when argument-level constraints matter. Start from [`../examples/cua-policy.yaml`](../examples/cua-policy.yaml) and review it for the target machine.

Read-only operations can still expose private desktop data. Treat screenshots, accessibility information, window/app metadata, and similar observation capabilities as sensitive data access.

## Failure and cancellation semantics

Read-only discovery may reconnect and retry after a transport failure. Computer-use actions are different because the desktop may already have partially applied an action.

For an in-flight tool call, the gateway keeps the downstream MCP request ID. If the northbound request is cancelled, the gateway sends downstream `notifications/cancelled` for that same request ID and returns an error without replay. Tool timeout follows the same no-replay rule and attempts downstream cancellation before recovery for a later request.

The deterministic CI fixture verifies that the downstream cancellation ID matches the in-flight backend request ID.

## Host and Origin validation

The MCP boundary uses Host and Origin guards. Default accepted authorities/origins are loopback-oriented. For a remote deployment, configure the exact expected public authority/origin or deliberately rewrite Host at the trusted proxy. Do not disable these guards just to make a proxy configuration work.

See [`DEPLOYMENT.md`](DEPLOYMENT.md).

## Health metadata

`/healthz` reports readiness and may include operational metrics for the gateway-owned backend child process:

- PID;
- cumulative CPU seconds;
- RSS bytes.

This does not include raw desktop content, but remotely reachable health routes should still sit behind the same authenticated deployment boundary.

On macOS, Cua may use its supported application/daemon lifecycle, so these metrics describe the direct child owned by the gateway rather than aggregate Cua process usage.

## Cloudflare deployment

Recommended topology:

```text
remote MCP client
    |
authenticated TLS / Cloudflare Access
    |
Cloudflare Tunnel
    |
127.0.0.1:<gateway>
    |
Cua stdio
```

Keep the gateway on loopback. Do not commit real tunnel credentials, Access tokens, private hostnames, `.env` files, generated private keys, PKCS#12 bundles, or local `secrets/` directories. The repository ignore rules are defense in depth, not a substitute for a secret manager or repository secret scanning.

## Self-hosted desktop E2E

A desktop runner with macOS Accessibility and Screen Recording grants is a high-trust machine. `.github/workflows/desktop-e2e.yml` is therefore manual-only, `main`-only, and targets the dedicated `cua-desktop-e2e` runner label.

Use a dedicated test Mac rather than a daily-use workstation, and never execute untrusted pull-request code on that runner. See [`V1_ACCEPTANCE.md`](V1_ACCEPTANCE.md) for the final operator-controlled acceptance procedure.

## CI supply chain

Normal CI has read-only repository permissions and locked Rust dependency resolution. Before real-Cua smoke, CI verifies the pinned Cua installer, platform release payload, and installed executable identity so the installed binary must match the independently verified release payload.

The deterministic V1 quality fixture does not touch a desktop. It covers cancellation, 100-call soak behavior, short-window idle resource regression checks, backend process telemetry, and the selected applicable official MCP conformance scenarios.

## Logs and reporting

Gateway audit logs record coarse metadata such as tool name, semantic class, policy decision, outcome, and duration. Keep raw arguments/results and credentials out of normal logs.

For security-sensitive reports, do not include credentials or unrelated private desktop data in public issues. Prefer GitHub private vulnerability reporting when available.


## V2 trust model

V2 separates northbound authenticated client principals, Hub transport identity, grant-signing authority, and Agent device identity. Key rotation requires continuity proof; bounded admission and signed cancellation/reconnect semantics fail closed around ambiguous operations. The complete compromised-component analysis and non-claims are documented in [`V2_THREAT_MODEL.md`](V2_THREAT_MODEL.md).

V2-M1 passed its single-secure-Agent acceptance gate on 2026-08-12. The production candidate keeps TLS-protected gRPC separate from independently signed application identity, preserves principal -> stable device -> exact capability grants, and never forwards a northbound OAuth bearer token to the Agent. Ambiguous desktop cancellation remains `indeterminate` and quarantines the device rather than authorizing replay. Linux Hub application keys use systemd encrypted credentials in the packaged service; ACME owns ordinary server-certificate renewal; Hub/device/grant key rotation stays independent and continuity-proven. OpenTelemetry/OTLP default telemetry excludes sensitive operation payloads. See [`V2_M1_ACCEPTANCE.md`](V2_M1_ACCEPTANCE.md) and [`V2_THREAT_MODEL.md`](V2_THREAT_MODEL.md).

The post-M1 P0 hardening makes that ambiguity boundary explicit in an authoritative operation ledger. Authenticated issuer/subject ownership and Agent generation both fence settlement; dispatched uncertainty persists as an exact-operation desktop quarantine across reconnect/restart; queued pre-ambiguity work is cancelled instead of resumed; and reuse requires an explicit, auditable, persistence-gated resolution. The recovery evidence string is bounded metadata and must not contain raw desktop content, commands, results, or secrets. See [`V2_P0_EXECUTION_SAFETY.md`](V2_P0_EXECUTION_SAFETY.md).
