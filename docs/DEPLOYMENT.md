# Deployment

V2 Hub + V2 Agent is the recommended deployment. The later V1 sections remain for `v1_gateway` regression/reference only.

Get the V2 trust/key path working with [`GETTING_STARTED.md`](GETTING_STARTED.md) before exposing either listener.

## V2-M1 deployment boundary

V2-M1 has a separate production candidate from the V1 loopback gateway. The accepted single-device topology is:

```text
MCP client
    |
    | HTTPS + deployment authentication
    v
external IdP / reviewed TLS reverse proxy / authenticated tunnel
    |
    | verified identity (provider credential stops at northbound authentication)
    v
v2_hub northbound MCP (default deployment: 127.0.0.1:8081)
    |
    | AuthenticatedClientPrincipal -> stable device -> exact DeviceCapability grant
    |
    | outbound-Agent gRPC bidi over TLS
    v
v2_agent
    +-- direct process / shell / bounded filesystem
    +-- optional Cua MCP GUI adapter
```

The Agent-facing gRPC listener is a separate Hub service port (example 7443). Agents connect outbound and authenticate again at the application layer with the enrolled Ed25519 device identity. A public deployment must restrict this port at the host/cloud firewall and apply the deployment's normal TCP/TLS connection controls. The in-process session limits begin after transport acceptance and are defense in depth, not a raw handshake-flood defense.

Northbound authentication and CUMG authorization are deliberately separate. The current packaged Hub validates OAuth bearer tokens through RFC 7662 introspection, then constructs `AuthenticatedClientPrincipal { issuer, subject }`. RFC 7662 is one adapter, not a requirement on the CUMG core. The packaged runtime also supports a reviewed trusted-proxy/tunnel fixed-principal adapter for explicitly single-principal deployments; generic OIDC/JWT validation remains the preferred future direction for multi-principal signed-token deployments. After that point, only `DeviceCapabilityAuthorizer` decides `principal -> device -> exact capability`.

OIDC/JWT validation does not require a CUMG user database merely to identify the caller: signature plus configured issuer/audience/time/subject claims establish the principal. Authorization data is separate. The current default authorization store is the integrity-protected `CUMG_V2_NORTHBOUND_POLICY_FILE`, loaded into an in-process exact-tuple policy. A future database or external policy engine may sit behind `DeviceCapabilityAuthorizer` without changing the execution-safety state machine.

For a trusted authenticated proxy/tunnel, constrain the Hub listener to loopback. An explicitly single-principal deployment sets `CUMG_V2_TRUSTED_PROXY_ISSUER`, `CUMG_V2_TRUSTED_PROXY_SUBJECT`, and the required `CUMG_V2_TRUSTED_PROXY_SECRET_FILE`; these are mutually exclusive with the OAuth introspection settings. Generate a random header-safe secret of at least 32 bytes (for example 32 random bytes encoded as hex), store it as a private regular file, and provision the same value independently to the reviewed local proxy. The proxy **must overwrite**, not append or pass through, `X-CUMG-Trusted-Proxy-Token` on every request it forwards to the loopback Hub. The Hub validates and strips that token before global request admission, so an unrelated local process cannot consume the normal northbound concurrency/rate budget merely by reaching `127.0.0.1`.

After the local trust gate, trusted-proxy traffic has a separate peer ceiling: `CUMG_V2_TRUSTED_PROXY_MAX_PEER_CONCURRENCY` defaults to `4` and `CUMG_V2_TRUSTED_PROXY_MAX_PEER_REQUESTS_PER_MINUTE` defaults to `60`. Both must remain below the global defaults (`16` and `120`) so global headroom is preserved. Peer concurrency rejection is HTTP 503 and peer rate rejection is HTTP 429. The peer key is the verified loopback source IP; it is overload isolation, not user identity. The fixed CUMG principal still comes only from operator configuration, and caller `clientInfo` remains audit-only. The adapter strips common Authorization/Cloudflare identity headers before MCP dispatch. If the deployment needs per-user CUMG policy, use a signed-token/OIDC-style adapter that conveys a tamper-resistant authenticated identity. Never trust a caller-provided `X-User`/similar header merely because the listener is called a proxy mode.

### Authenticated Agent session lifetime

`v2_hub` bounds every authenticated Agent transport with `CUMG_V2_MAX_AGENT_SESSION_LIFETIME_SECS` (default `3600`). `CUMG_V2_AGENT_SESSION_REAUTH_DRAIN_SECS` (default `30`) reserves the final part of that lifetime for a controlled reauthentication drain. The drain value must be non-zero and strictly smaller than the hard lifetime.

When the reauthentication window begins, the Hub emits `v2_agent_session_reauth_requested` / `cumg.v2.agent_session_reauth_requested`, pauses **new** operation admission for that session, and lets already-admitted work settle. If the pending set drains before the hard deadline, the Hub closes the stream normally; the Agent's existing reconnect lifecycle performs a fresh hello/challenge/proof/accepted handshake and advances to a new generation. This normal path does not create a quarantine.

The hard lifetime is not advisory. If already-dispatched work is still unsettled when the deadline arrives, the Hub emits the high-visibility `v2_agent_session_lifetime_exceeded` event plus `cumg.v2.agent_session_lifetime_exceeded` and closes the transport. Existing execution-safety cleanup then fails closed: work whose side effect cannot be proven terminal may become `Indeterminate` and quarantine exactly as with any other connection loss. Increasing the lifetime or drain window must never be used to auto-replay or clear such ambiguity.

### Planned Hub shutdown and restart

`v2_hub` treats `SIGINT`, `SIGTERM`, and `SIGHUP` as planned shutdown signals. On the first signal it closes the operation-admission gate, keeps the Agent transport alive, and waits up to `CUMG_V2_DRAIN_TIMEOUT_SECS` (default `30`) for work that was already admitted to reach a durable terminal or indeterminate state. Requests that have not crossed the dispatch boundary are rejected/cancelled rather than starting new side effects during the drain.

A successful drain then shuts down the gRPC and northbound HTTP servers. If the bounded drain timeout expires, shutdown continues with the existing fail-closed restart behavior: any work that had crossed the dispatch boundary without terminal proof remains eligible for `Indeterminate` + quarantine on restart. The timeout never authorizes replay or clears ambiguity.

Configure the service manager's stop/kill timeout to be **longer** than `CUMG_V2_DRAIN_TIMEOUT_SECS`; otherwise the supervisor can kill the Hub before its own bounded drain completes. The packaged systemd unit uses `TimeoutStopSec=45s` for the default 30-second drain. Apply the same ordering to operator-maintained launchd or other service-manager definitions.

### Offline quarantine resolution

A durable `Indeterminate` quarantine is never cleared by reconnect or restart. After an operator establishes out-of-band evidence for the exact ambiguous operation, stop `v2_hub` completely and use the offline maintenance CLI instead of editing checkpoint JSON:

```bash
cargo run --locked --bin v2_maint -- resolve \
  --state-dir /var/lib/cumg-v2/hub \
  --operation-id op_... \
  --decision confirmed_not_executed \
  --evidence "ticket-1234: operator verified no side effect"
```

`confirmed_completed` is also available when the side effect is positively confirmed. Evidence is required and remains subject to `MAX_RESOLUTION_EVIDENCE_BYTES`; keep it to bounded audit metadata and never place commands, results, desktop content, credentials, tokens, or secrets in it.

The Hub and maintenance CLI take the same exclusive state-directory lock. `v2_maint` therefore fails closed while any `SingleDeviceHub` instance still owns that state directory. A successful resolution restores the checkpoint through the normal schema-validation path, invokes the existing authoritative `resolve_indeterminate` transition, and appends a new checkpoint through the same create-new/fsync persistence path. The resulting `ResolutionRecord` remains durable even after terminal operation tombstones are pruned on later generations. Restart the Hub only after the CLI exits successfully.

### Linux Hub

Use `packaging/systemd/cumg-v2-hub.service` plus `packaging/systemd/hub.env.example` as templates. The unit uses systemd encrypted credentials for the Hub and grant-signing application keys and a systemd credential path for the ACME-managed TLS private key. Provision long-lived application keys into the encrypted credential store outside the repository, for example:

```bash
sudo systemd-creds encrypt --name=hub-secret /secure/admin/hub.key   /etc/credstore.encrypted/hub-secret
sudo systemd-creds encrypt --name=grant-secret /secure/admin/grant.key   /etc/credstore.encrypted/grant-secret
```

Keep the recovery/rotation copy in the operator's normal secret manager. Do not retain plaintext administrative copies in the checkout. The service receives `%d/hub-secret`, `%d/grant-secret`, and `%d/tls-key` paths; private bytes are not environment-variable values.

For northbound OAuth introspection, use the optional encrypted-credential drop-in in `packaging/systemd/cumg-v2-hub-oauth-credential.conf.example` rather than putting the client secret in `hub.env`. For trusted-proxy mode, use `packaging/systemd/cumg-v2-hub-trusted-proxy-credential.conf.example`; provision the same random secret separately to the proxy/tunnel and never place the value itself in `hub.env`.

#### Optional MemoryUsageStore sidecar

Usage accounting is disabled by default. To enable it, install `packaging/systemd/cumg-v2-usage-sidecar.service`, copy `usage.env.example` outside the repository, build/install `mcp-usage-control` core from source locally, and install the optional `cumg-v2-hub-usage.conf.example` drop-in. The Hub then uses `CUMG_V2_USAGE_ENDPOINT=http://127.0.0.1:8787/`.

The drop-in couples the sidecar lifecycle to Hub restart; therefore an explicit packaged Hub restart recreates the non-durable MemoryUsageStore. If you manually supervise Hub and sidecar separately, a Hub-only restart does not reset a still-running sidecar. In either case, usage reset never clears CUMG operation/quarantine checkpoints. Do not use this Memory store as a financial ledger. See [`V2_USAGE_ACCOUNTING.md`](v2/V2_USAGE_ACCOUNTING.md).

### TLS renewal

Keep certificate issuance/renewal with the deployment's ACME client. Do not point `v2_hub` directly at a symlinked ACME `live/` private key because the Hub secret loader intentionally rejects symlinks. Configure the ACME deploy hook to run:

```bash
scripts/v2-install-renewed-tls.sh   ACME_CERT_PEM ACME_KEY_PEM   /etc/cumg-v2/tls/server.pem /etc/cumg-v2/tls/server.key
sudo systemctl try-restart cumg-v2-hub.service
```

The hook validates that the certificate and private key parse and match before same-directory atomic replacement. The deployed key is mode 0600. Application Hub/device/grant identity rotation is independent; follow `packaging/README.md` and use `v2_keyctl` for continuity documents.

### Linux Agent

Install `packaging/systemd/cumg-v2-agent.service` as a **user service** and customize `packaging/systemd/agent.env.example` outside the repository. The template intentionally avoids a filesystem namespace that would silently change the explicitly configured process/filesystem capability semantics. Store the device secret as a regular 0600 file and keep Hub/grant/TLS trust anchors non-group/other-writable.

### macOS Agent

Customize `packaging/launchd/com.github.git-ksk.cumg-v2-agent.plist`, replacing `@BINARY@` and `@HOME@`, then install it as a user LaunchAgent. Cua-backed GUI automation must run in the logged-in user session so Accessibility/Screen Recording TCC attribution remains explicit. Secret/trust files live outside the repository under the user's Application Support tree with restrictive permissions. Keep `CUMG_V2_CUA_BACKEND_VERSION` pinned to the exact reviewed Cua version; a concrete value is checked against the MCP handshake on initial connection and reconnect.

For an existing V1 production endpoint moving to V2, follow the guarded [`V2 production cutover runbook`](v2/V2_PRODUCTION_CUTOVER.md). Do not treat a successful local V2 start as permission to stop V1.

### Overload and observability

The Hub defaults to bounded Agent sessions/session starts and bounded northbound MCP request concurrency/rate. Excess Agent sessions use gRPC `RESOURCE_EXHAUSTED`; excess northbound requests use HTTP 429 or 503. Keep external firewall/reverse-proxy limits as the outer control.

OTLP is opt-in through standard OpenTelemetry variables. `OTEL_EXPORTER_OTLP_ENDPOINT` enables traces and metrics; `OTEL_EXPORTER_OTLP_TRACES_ENDPOINT` and `OTEL_EXPORTER_OTLP_METRICS_ENDPOINT` enable individual signals; `OTEL_SDK_DISABLED=true` disables export. The packaged build uses the standard OTLP `grpc` transport. Default telemetry intentionally excludes command/result bodies, argv, stdout/stderr, file contents, screenshots, clipboard data, bearer tokens, OAuth introspection secrets, grants, signatures and private key material. Protocol failures log only a message kind plus safe error metadata, never the full signed protocol object.

V2 structured events use control-plane correlation fields only when available. The principal incident keys are `operation_id`, `device_id` and `generation`; `capability`, `outcome`, `error_code`, `indeterminate_reason`, `reconnect_attempt` and `backend` add bounded diagnostic context. Authenticated principal issuer/subject is not emitted by default. Northbound audit events additionally record bounded MCP `clientInfo` name/version/description as `client_name`, `client_version`, and `client_description`. These values are caller-supplied **audit metadata only** (`identity_source=mcp_client_info_untrusted`): they never select the authenticated principal, authorize a capability, change operation ownership, or cross the Hub/Agent trust boundary. `v2_northbound_operation_requested` carries the same `operation_id` used by downstream Hub execution events so tooling/human callers can be correlated without treating their claimed client name as identity. The main event families cover Agent session start/accept/supersede/end/reconnect/exhaustion, northbound client initialization/request correlation, operation admission/dispatch/terminal failure or completion, cancellation request/acknowledgement, indeterminate/quarantine/resolution, persistence failure, authorization failure, overload rejection, backend ambiguity/timeout and stale result/session rejection.

OTel counters intentionally expose only closed, low-cardinality attribute domains:

- `cumg.v2.agent_session_started`;
- `cumg.v2.agent_session_rejected{reason}`;
- `cumg.v2.reconnect_attempt` and `cumg.v2.reconnect_exhausted`;
- `cumg.v2.operation_completed{capability,outcome}`;
- `cumg.v2.operation_indeterminate{reason}`;
- `cumg.v2.quarantine_created` and `cumg.v2.quarantine_resolved`;
- `cumg.v2.persistence_failure{component}`;
- `cumg.v2.auth_failure{reason}`;
- `cumg.v2.backend_failure{reason}`;
- `cumg.v2.stale_result_rejected`;
- `cumg.v2.northbound_request_rejected{reason}`.

Never add `operation_id`, `device_id`, principal/subject, request path, command/tool name or other unbounded values as metric attributes. Those belong in structured logs/traces only when required for incident correlation. Collector, proxy and service-manager logging must preserve the same payload-free boundary; do not enable HTTP/gRPC body capture or Authorization-header logging around the process.

#### Quarantine alerting recipe

Every newly-created quarantine emits a dedicated `ERROR`-level `v2_quarantine_created` event with bounded correlation fields (`operation_id`, `device_id`, `generation`, capability, and indeterminate reason) in addition to incrementing `cumg.v2.quarantine_created`. Treat either signal as operator-action-required; neither signal changes or resolves the quarantine. On systemd, a minimal local watch is:

```bash
journalctl -u cumg-v2-hub.service --priority=err --grep=v2_quarantine_created --follow
```

For OTLP-backed monitoring, alert whenever the **increase/delta of `cumg.v2.quarantine_created` is greater than zero** over the collector's shortest reliable alert window (for example one to five minutes), and page or otherwise notify the operator responsible for the device. Metric exporters may translate the meter name to backend-specific syntax; alert on the exported counter corresponding to this exact OpenTelemetry meter rather than adding `operation_id` or `device_id` labels. Use the paired `v2_quarantine_created` error event to recover those incident identifiers, then follow the offline resolution procedure above. Clear the operational alert only after explicit resolution is evidenced by `v2_quarantine_resolved` / `cumg.v2.quarantine_resolved`; reconnect or process restart is not resolution.

The Hub also bounds same-generation checkpoint growth. After a successful checkpoint reaches `CUMG_V2_CHECKPOINT_GENERATION_ROLLOVER_BYTES` (default `524288`, at most half of the 1 MiB checkpoint ceiling), the Hub pauses new operation admission, lets already-admitted work settle, then closes the authenticated Agent session cleanly. The Agent reconnects with a fresh generation, and the existing generation fence makes prior signed commands stale before the Hub prunes old terminal replay/receipt records. `Indeterminate` operations and quarantine are never pruned by this rollover; a quarantined device therefore remains quarantined across every generation. This is a reliability compaction boundary, not permission to replay or forget ambiguity.

For an incident, correlate Hub and Agent by `device_id` + `generation`, then follow `operation_id`. A `v2_operation_indeterminate` event must be followed by durable quarantine until a `v2_quarantine_resolved` event exists for that operation. Persistence failures expose a safe `error_code` such as `persistence_checkpoint_too_large` without a path or serialized checkpoint. Reconnect exhaustion and heartbeat timeouts are visible independently from TLS/transport connection failures. OAuth introspection unavailability is distinct from authorization denial, and a quarantine admission rejection remains `device_indeterminate` rather than being retried or auto-replayed.

See [`V2_M1_ACCEPTANCE.md`](v2/acceptance/V2_M1_ACCEPTANCE.md) for the final security gate and [`../packaging/README.md`](../packaging/README.md) for lifecycle details.

## V1 legacy/reference topology

```text
Remote MCP client
    |
    | HTTPS + authentication
    v
trusted reverse proxy / tunnel
    |
    | loopback HTTP
    v
127.0.0.1:8100/mcp
    |
    | MCP stdio
    v
cua-driver mcp
```

The gateway does not implement public authentication or TLS termination in V1. Do not expose it directly to the public internet.

## Preflight

Before adding a tunnel, all of these should work on the computer being controlled:

```bash
cua-driver --version
cua-driver doctor
cua-driver call list_apps
curl --fail http://127.0.0.1:8100/healthz
```

A local MCP client should also be able to connect to:

```text
http://127.0.0.1:8100/mcp
```

If not, stop here and use [`TROUBLESHOOTING.md`](TROUBLESHOOTING.md).

## Tool policy before remote exposure

The gateway is deny-by-default:

- empty `CUMG_ALLOW_TOOLS` exposes no tools;
- a comma-separated allowlist exposes only matching discovered tools;
- `CUMG_DENY_TOOLS` always overrides the allowlist;
- `CUMG_ALLOW_TOOLS=*` explicitly exposes every discovered backend tool.

Do not use `*` as a remote-deployment shortcut. Review exact computer-use capabilities first. Read-only tools can still disclose sensitive desktop data.

`examples/cua-policy.yaml` provides an optional second policy layer inside Cua for argument-aware restrictions.

## Backend command configuration

`CUMG_BACKEND_COMMAND` is the executable and `CUMG_BACKEND_ARGS` is the argument string. V1 splits `CUMG_BACKEND_ARGS` on ASCII whitespace; it does not implement shell-style quoting or escaping. The default `mcp` value is safe, but arguments containing embedded spaces cannot currently be represented reliably through this setting.

Do not put secrets in backend command arguments. They may be visible to local process inspection even though the gateway avoids logging argument values.

## Host and Origin rules

### Host

The default accepted Host authorities are loopback-only. A reverse proxy must therefore either:

1. intentionally rewrite/preserve the origin `Host` as a loopback authority such as `127.0.0.1:8100`; or
2. forward the public hostname and configure the gateway with the exact authority:

```text
CUMG_ALLOWED_HOSTS=computer.example.com
```

Include a port only when it is actually present in the forwarded authority.

Do not disable Host validation to make a deployment work.

### Origin

Browser-originated requests with an `Origin` header are checked independently of Host. Add only exact expected origins:

```text
CUMG_ALLOWED_ORIGINS=https://client.example.com
```

Non-browser MCP clients may omit `Origin`. Do not use wildcard origins for convenience.

## HTTP overload and health-route hardening

The gateway keeps a local defense-in-depth ceiling in front of the MCP HTTP route. `CUMG_MAX_HTTP_CONCURRENCY` defaults to `16`. Once all slots are in use, another MCP HTTP request fails immediately with HTTP `503` and `error: gateway_overloaded` instead of joining an unbounded waiter queue.

This does **not** replace the existing backend operation serialization: one physical desktop is still protected by the backend `operation_lock`. The HTTP ceiling protects the northbound process boundary from request accumulation before those serialized operations execute.

Keep reverse-proxy or Cloudflare rate limiting as an independent outer control. The local concurrency ceiling is not authentication and should not be described as a complete denial-of-service defense.

`/healthz` returns only coarse readiness by default:

```json
{"status":"ok","backend":"ready"}
```

Set `CUMG_HEALTH_DETAILS=true` only when detailed local diagnostics are intentionally required. That opt-in adds backend process metadata such as PID, cumulative CPU seconds, and RSS. Remote deployments should normally leave it disabled.

Authentication at the reverse proxy must cover the **entire public hostname**, including `/mcp`, `/healthz`, and any future auxiliary route. Do not create an unauthenticated path exception merely to make a remote health check convenient. A path-specific proxy policy that protects `/mcp` but exposes `/healthz` is not the documented deployment model.

## Cloudflare Access + Tunnel

Cloudflare is one example deployment, not a required dependency. Current Cloudflare guidance recommends creating the **Access application before publishing the tunnel route**; otherwise the hostname can be publicly reachable without the intended Access policy.

Cloudflare also recommends remotely managed tunnels for most deployments. This repository keeps a locally managed YAML example because its origin settings can be reviewed alongside the gateway. Either management model is acceptable if the resulting security properties are equivalent.

Official Cloudflare references:

```text
https://developers.cloudflare.com/cloudflare-one/access-controls/applications/http-apps/self-hosted-public-app/
https://developers.cloudflare.com/tunnel/advanced/local-management/create-local-tunnel/
https://developers.cloudflare.com/tunnel/advanced/origin-parameters/
```

### 1. Create the Access application first

In Cloudflare Zero Trust, create a self-hosted Access application for your intended hostname, for example:

```text
computer.example.com
```

Create a policy that matches the intended users or machine credentials. The application/policy must protect the hostname as a whole, including `/healthz`; do not scope authentication only to `/mcp`. Do not publish the tunnel hostname first and plan to add Access later.

For automated MCP clients that cannot complete an interactive identity-provider login, a Cloudflare Access **Service Token** with a **Service Auth** policy is one option. See [`CLIENTS.md`](CLIENTS.md) for a Codex header example.

### 2. Create a tunnel

For the locally managed example:

```bash
cloudflared tunnel login
cloudflared tunnel create computer-use-mcp-gateway
```

Record the tunnel UUID and generated credentials-file path. These are deployment secrets/identifiers and must not be committed to this repository.

### 3. Prepare the tunnel configuration

Copy the repository example outside version control or into an ignored local path:

```bash
cp examples/cloudflared.yml ~/.cloudflared/computer-use-mcp-gateway.yml
```

Replace:

- `YOUR_TUNNEL_ID`;
- the credentials-file path;
- `computer.example.com`;
- optional Access `teamName` and `audTag` values when enabling local JWT validation.

The example contains:

```yaml
originRequest:
  httpHostHeader: 127.0.0.1:8100
```

Cloudflare documents `httpHostHeader` as the `Host` header sent to the local service. Keeping this rewrite means the gateway's default loopback Host allowlist can remain unchanged.

If you intentionally remove that rewrite and forward the public hostname instead, set:

```text
CUMG_ALLOWED_HOSTS=computer.example.com
```

### 4. Enable Access-token validation at the tunnel where practical

Cloudflare's **Protect with Access** origin setting makes `cloudflared` validate the Access JWT before proxying traffic to the gateway. For a locally managed config, the shape is:

```yaml
originRequest:
  httpHostHeader: 127.0.0.1:8100
  access:
    required: true
    teamName: YOUR_TEAM_NAME
    audTag:
      - YOUR_ACCESS_APPLICATION_AUD_TAG
```

This is defense in depth on top of Cloudflare Access policy evaluation. Replace placeholders with the values from your Access application.

### 5. Create the DNS route

After the Access application exists:

```bash
cloudflared tunnel route dns computer-use-mcp-gateway computer.example.com
```

### 6. Run the gateway on loopback

For example:

```bash
cargo run --locked -- --allow-tools list_apps,list_windows,get_accessibility_tree,get_screen_size
```

Do not change `CUMG_BIND` to `0.0.0.0` merely because a tunnel is being used.

### 7. Run the tunnel

```bash
cloudflared tunnel \
  --config ~/.cloudflared/computer-use-mcp-gateway.yml \
  run computer-use-mcp-gateway
```

The remote MCP URL becomes:

```text
https://computer.example.com/mcp
```

### 8. Verify authentication, then MCP

First verify that unauthenticated access is rejected by Access for both `/mcp` and `/healthz`. Then connect using the intended identity/OAuth/service-token mechanism.

For a Service Token, Cloudflare's standard request headers are:

```text
CF-Access-Client-Id
CF-Access-Client-Secret
```

Never put the secret in the repository. [`CLIENTS.md`](CLIENTS.md) shows an environment-backed Codex configuration.

## Reverse-proxy requirements

If you use something other than Cloudflare, preserve the same properties:

- HTTPS/TLS for the remote connection;
- authentication for the whole public hostname, including health/auxiliary routes, before requests reach the gateway;
- an intentional Host forwarding/rewrite policy matching `CUMG_ALLOWED_HOSTS`;
- exact Origin allowlisting when browser Origins are expected;
- an explicit request-rate/concurrency policy appropriate to the deployment in addition to the gateway's local concurrency ceiling;
- no direct route to the Cua stdio backend;
- no accidental public exposure of other local services;
- preferably an additional origin-side verification mechanism so a proxy-policy bypass does not silently become anonymous access.

The gateway should remain on loopback unless a different bind is a deliberate, reviewed network design.

## MCP clients

Local and remote client examples are in [`CLIENTS.md`](CLIENTS.md).

A remote client must be able to satisfy the authentication mechanism chosen at the reverse proxy. Browser SSO, OAuth, static bearer credentials, and service-token headers are not interchangeable; choose an auth flow the client actually supports.

## Logging

Normal gateway logs intentionally omit raw tool arguments/results. Treat reverse-proxy logs separately: headers, authentication metadata, and request bodies can leak sensitive information depending on proxy configuration.

Do not enable body logging for MCP traffic unless there is a narrowly scoped debugging need and the resulting data is handled as sensitive.

## Secrets

Do not commit:

- tunnel credential files or real tunnel IDs;
- Access service-token Client Secrets;
- production hostnames when they are intended to remain private;
- screenshots or desktop E2E artifacts containing user data;
- personal filesystem paths;
- `.env` files.

`.gitignore` excludes `.env` variants plus generated `*.key`, PKCS#12, `*.secret`, and `secrets/` material. Ignore rules are only defense in depth; production credentials belong in the selected secret store.

## Current deployment limitations

V1 has no built-in:

- public authentication;
- TLS termination;
- multi-machine routing;
- per-user desktop isolation;
- distributed locking;
- shell-style quoting for `CUMG_BACKEND_ARGS`;
- cloud control plane.

All MCP clients connected to one V1 gateway ultimately share one serialized physical desktop/backend state.
