# Troubleshooting

Work from the bottom of the stack upward:

```text
OS / desktop permissions
        ↓
Cua Driver
        ↓
computer-use-mcp-gateway
        ↓
reverse proxy / authentication
        ↓
MCP client
```

Do not debug a remote client until the local Cua and gateway checks pass.

## `cua-driver: command not found`

Open a new terminal after installation. On macOS/Linux, Cua normally exposes the CLI through `~/.local/bin`; ensure that directory is on `PATH`.

Verify:

```bash
cua-driver --version
cua-driver doctor
```

On Windows, open a new PowerShell window so the updated User `Path` is loaded.

## Cua works poorly or `doctor` reports a problem

Run:

```bash
cua-driver doctor
cua-driver call list_apps
```

Fix Cua before debugging the gateway. The gateway does not emulate missing platform capabilities.

## macOS: permissions show `unknown` or actions fail

Cua's macOS permissions should be associated with `CuaDriver.app`. Start the application-backed daemon first:

```bash
open -n -g -a CuaDriver --args serve
cua-driver permissions status
```

If grants are missing:

```bash
cua-driver permissions grant
```

Enable both **Accessibility** and **Screen & System Audio Recording** for CuaDriver in System Settings. If a grant changed, fully relaunch CuaDriver and check again.

If `CuaDriver.app` is missing specifically from **System Settings → Privacy & Security → Screen & System Audio Recording**, click `+`, select `/Applications/CuaDriver.app`, enable it, relaunch CuaDriver if necessary, and run:

```bash
cua-driver permissions status
```

Treat manual addition as a fallback only when macOS does not list CuaDriver automatically; it is not a routine installation step.

Avoid replacing the supported application/TCC lifecycle with an arbitrary unsigned helper process.

## Linux: `libXi.so.6` is missing

On Debian/Ubuntu-like systems:

```bash
sudo apt update
sudo apt install libxi6 at-spi2-core
```

Then rerun:

```bash
cua-driver doctor
```

## Linux: tool list exists but there is no usable GUI

A headless shell does not provide a desktop to click or inspect. Cua computer-use tools need a compatible live display session. Verify your X11/XWayland/AT-SPI environment with `cua-driver doctor`.

## Windows: GUI tools fail from a service or SSH session

Normal computer-use actions require an interactive Windows desktop session. Confirm that the Cua daemon is running in an interactive logon session:

```powershell
cua-driver doctor
cua-driver autostart kick
```

Then test a harmless read operation:

```powershell
cua-driver call list_apps
```

## Gateway exits during startup

First confirm the tested backend is available:

```bash
cua-driver --version
cua-driver call list_apps
```

Then run the gateway with logs visible:

```bash
cargo run --locked -- --allow-tools list_apps
```

Common causes are:

- `cua-driver` is not on `PATH`;
- Cua cannot initialize on the current desktop session;
- the backend connection exceeds `CUMG_CONNECT_TIMEOUT_SECS`;
- a custom `CUMG_BACKEND_COMMAND` or `CUMG_BACKEND_ARGS` is invalid;
- `CUMG_MAX_HTTP_CONCURRENCY` is set to `0`.

V1 splits `CUMG_BACKEND_ARGS` on ASCII whitespace; it does not implement shell-style quoting for embedded spaces.

## `/healthz` returns HTTP 503

The gateway reports 503 from `/healthz` when the backend is stopped or its MCP transport is unhealthy. The MCP concurrency guard is scoped to the MCP route, so an overloaded `/mcp` request does not turn `/healthz` into `gateway_overloaded`.

Check Cua directly, then inspect gateway logs:

```bash
cua-driver doctor
cua-driver call list_apps
```

A backend transport failure may be repaired for a later request, but the failed computer-use action is not replayed automatically.

By default `/healthz` intentionally returns only coarse readiness. If you explicitly need local process diagnostics, `CUMG_HEALTH_DETAILS=true` adds backend PID/CPU/RSS metadata. Leave that setting false for normal remote deployments and protect the entire public hostname, including `/healthz`, with the same reverse-proxy authentication boundary.

## MCP returns HTTP 503 with `gateway_overloaded`

The gateway has reached `CUMG_MAX_HTTP_CONCURRENCY` (default `16`). Excess MCP HTTP requests fail immediately rather than accumulating in an unbounded waiter queue.

This is separate from backend desktop serialization and separate from reverse-proxy rate limiting. If normal traffic reaches the ceiling, first check for a buggy client retry loop, abandoned concurrent requests, or an overly aggressive caller. Increase the limit only after understanding the workload; do not disable proxy-side rate limits or authentication as a workaround.

## V2 returns `device_indeterminate`

`device_indeterminate` means an **earlier state-changing operation has an unproven outcome and is already quarantining the device**. The returned `blocking_operation_id` names that earlier ambiguous operation; it is not a new retry ID for the request that was just refused. Do not replay the old operation or clear state merely because the Agent reconnects.

First inspect the latest durable Hub state while the Hub is still serving:

```bash
v2_maint inspect-quarantine \
  --state-dir /var/lib/cumg-v2/hub
```

Then correlate the exact blocking operation with the Agent checkpoint before doing manual checkpoint/log archaeology:

```bash
v2_maint audit-reconciliation \
  --state-dir /var/lib/cumg-v2/hub \
  --agent-state-dir /var/lib/cumg-v2/agent \
  --operation-id op_...
```

For the normal operator view, generate the incident brief instead of manually combining those outputs:

```bash
v2_maint incident-brief \
  --state-dir /var/lib/cumg-v2/hub \
  --agent-state-dir /var/lib/cumg-v2/agent \
  --operation-id op_... \
  --mutation-authority-dir /var/lib/cumg-v2/mutation-authority \
  --format text
```

These inspection commands are read-only and may run while the services are live. They do not clear quarantine, sign recovery authority, contact the backend, or replay work. `incident-brief` embeds the exact reconciliation audit and can optionally add only allowlisted observational findings from an owner-private diagnostics JSON file; those observations cannot widen `supported_decisions`. Treat legacy terminal markers, request/fingerprint matches, reconnect, elapsed time, and heuristic UI/application state as non-authoritative unless the audit explicitly reports an accepted authoritative proof. If evidence remains insufficient, keep quarantine intact.

Choose a manual decision only from independent evidence for the exact operation: `confirmed_completed` requires proof that the intended effect completed; `confirmed_not_executed` requires proof that no effect occurred; `confirmed_effect_applied_uncommitted` is reserved for the bounded text-input case where input delivery occurred but a distinct submit/commit did not. None makes the old operation retry-safe.

Only the authority-bearing offline mutation requires stopping the Hub. Install `v2_hub` and `v2_maint` from the same reviewed build/release, stop `v2_hub` completely, and then run the exact resolution. For example:

```bash
v2_maint resolve \
  --state-dir /var/lib/cumg-v2/hub \
  --operation-id op_... \
  --decision confirmed_not_executed \
  --evidence "ticket-1234: operator verified no side effect"
```

Do not run a newer arbitrary `v2_maint` checkout against an older deployed Hub's state. The maintenance path preserves the checkpoint's existing durable writer contract and fails before publication if the candidate cannot be represented. If pairing is uncertain, deploy/use the matching Hub + maintenance artifacts first rather than editing checkpoint JSON. Restart the Hub only after maintenance exits successfully.

See [`DEPLOYMENT.md`](DEPLOYMENT.md) for the full inspection, reconciliation, retirement, and offline-recovery contracts.

## Single-Mac upgrade caller disconnected or timed out

Do not infer success or start a second upgrade from the client timeout. The reviewed upgrade persists its last durable maintenance phase independently of the invoking MCP/client. Inspect it with:

```bash
v2_maint upgrade-status
```

The record is read-only operational evidence and never authorizes replay, quarantine resolution, rollback, or mutation-authority transfer. `in_progress` means the last phase was durably recorded but no terminal result was established. Check `python3 scripts/v2_launchd_maintenance_job.py inspect`: if the same one-shot job is still active, do not launch another job; if no job is active and the transaction remains `in_progress`, treat it as incomplete and inspect the recorded phase before any recovery. `failed_before_install` means the install boundary was not crossed. `failed_closed_after_stop` means services may intentionally remain stopped; inspect the recorded rollback asset before explicit recovery. `operator_action_required` means restore/cleanup/status handling did not prove a clean terminal state.

For build failures, the source path defaults to `CUMG_V2_CARGO_BUILD_JOBS=2` and refuses preflight below `CUMG_V2_MIN_BUILD_FREE_MIB=6144`. A Cargo `ENOSPC`/`No space left on device` after preflight is recorded as `build_storage_exhausted` and exits before service drain, mutation-authority migration, or install. Restore capacity and make a fresh explicit upgrade attempt only after `upgrade-status` shows `failed_before_install`; do not delete Git WIP/untracked files as automatic cleanup.

## MCP connects but shows no tools

The gateway is **deny-by-default**. An empty allowlist is intentionally a valid zero-tool configuration.

Start with an explicit tool:

```bash
cargo run --locked -- --allow-tools list_apps
```

If a configured tool still does not appear, confirm the exact backend tool name. The gateway only exposes tools that are both discovered from Cua and allowed by policy.

Do not use `CUMG_ALLOW_TOOLS=*` as a routine troubleshooting shortcut on a remote or sensitive desktop.

## An allowed tool is reported unavailable

The backend tool surface can vary by Cua version/platform/mode. The gateway refreshes discovery and still fails closed if the policy-allowed name is not present.

Check:

```bash
cua-driver --version
```

This repository's CI compatibility target is Cua Driver 0.19.3. If you are on another version, compare its tool surface before changing the gateway policy.

## HTTP 403: Host rejected

The MCP endpoint validates the inbound `Host` authority to reduce DNS-rebinding risk.

Local requests should use the normal loopback authority:

```text
127.0.0.1:8100
localhost:8100
```

For a reverse proxy, either:

- deliberately rewrite the origin `Host` to an allowed loopback authority; or
- add the exact public authority to `CUMG_ALLOWED_HOSTS`.

Example:

```text
CUMG_ALLOWED_HOSTS=computer.example.com
```

Include a port only when the forwarded Host actually contains it. Do not disable Host validation globally.

## HTTP 403: Origin rejected

Browser-originated MCP requests may contain `Origin`. The match is exact, including scheme and port.

Example:

```text
CUMG_ALLOWED_ORIGINS=https://client.example.com
```

`https://client.example.com` and `http://client.example.com` are different origins. So are non-default ports.

Non-browser MCP clients often do not send `Origin`; do not add wildcard origins simply to make an unrelated proxy error disappear.

## Cloudflare returns a login page, 401, or 403 to an automated MCP client

An interactive identity-provider login is not suitable for every headless/automated MCP client.

For machine access, configure an authentication mechanism the client can actually present on every required request. One option is a Cloudflare Access Service Token with a matching **Service Auth** policy.

Cloudflare's standard headers are:

```text
CF-Access-Client-Id
CF-Access-Client-Secret
```

For Codex, [`CLIENTS.md`](CLIENTS.md) shows how to source those headers from environment variables instead of committing the secret to `config.toml`.

Do not remove Access authentication to make an MCP client connect.

## Cloudflare reaches the tunnel but the gateway rejects Host

Prefer an intentional origin Host rewrite in the tunnel configuration:

```yaml
originRequest:
  httpHostHeader: 127.0.0.1:8100
```

Cloudflare documents `httpHostHeader` as the Host header sent to the local service. Alternatively, keep the public Host and explicitly configure `CUMG_ALLOWED_HOSTS` to match it.

See [`../examples/cloudflared.yml`](../examples/cloudflared.yml) and [`DEPLOYMENT.md`](DEPLOYMENT.md).

## Tool call times out

The default backend operation timeout is 60 seconds. You can raise it for a known slow operation:

```text
CUMG_TOOL_TIMEOUT_SECS=120
```

A timeout is ambiguous for a computer-use action: the click/type/app action may have partially happened before the response was lost. The gateway therefore does **not** automatically retry that call.

Inspect the desktop state before manually retrying a state-changing operation.

## Two clients interfere with each other

All backend operations are serialized in V1 because the clients ultimately share one physical cursor/focus/application state. Serialization prevents operation interleaving but does not provide per-user desktop isolation.

If two users require independent desktops, run independent machine/session environments or wait for a future multi-machine/session-isolation design; do not assume V1 provides tenant isolation.

## Client still shows an old tool list

The gateway dynamically refreshes backend discovery when listing tools, but some clients cache or snapshot the server tool surface.

First reconnect/restart or refresh the MCP server in the client. For hosted ChatGPT apps, use the current product's tool refresh/rescan flow where available.

Do not broaden the gateway allowlist just because a client UI is stale.

## Need more gateway logs

You can increase Rust tracing verbosity:

### macOS / Linux

```bash
RUST_LOG=debug cargo run --locked -- --allow-tools list_apps
```

### Windows PowerShell

```powershell
$env:RUST_LOG = "debug"
cargo run --locked -- --allow-tools "list_apps"
```

The gateway intentionally avoids logging raw MCP tool arguments/results, screenshots, clipboard values, or credentials. Still review logs before posting them publicly because hostnames, tool names, timing, and local environment details may be sensitive.

## Still stuck

Collect the minimum non-sensitive diagnostics:

```text
OS/version
rustc --version
cua-driver --version
cua-driver doctor   (redact sensitive paths/identifiers if needed)
healthz status
exact gateway error category
MCP client name/version
local vs remote connection
```

Never attach real screenshots, credentials, Access tokens, private hostnames, or raw desktop contents to a public issue.
