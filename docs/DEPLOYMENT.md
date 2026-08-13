# Deployment

V2 Hub + V2 Agent is the recommended deployment. The later V1 sections remain for `v1_gateway` regression/reference only.

Get the V2 trust/key path working with [`GETTING_STARTED.md`](GETTING_STARTED.md) before exposing either listener.

## V2-M1 deployment boundary

V2-M1 has a separate production candidate from the V1 loopback gateway. The accepted single-device topology is:

```text
MCP client
    |
    | HTTPS + MCP Authorization / OAuth
    v
reviewed TLS reverse proxy / load balancer
    |
    | loopback HTTP
    v
v2_hub northbound MCP (default deployment: 127.0.0.1:8081)
    |
    | AuthenticatedClientPrincipal -> stable device -> exact DeviceCapability grant
    | OAuth token stops here
    |
    | outbound-Agent gRPC bidi over TLS
    v
v2_agent
    +-- direct process / shell / bounded filesystem
    +-- optional Cua MCP GUI adapter
```

The Agent-facing gRPC listener is a separate Hub service port (example 7443). Agents connect outbound and authenticate again at the application layer with the enrolled Ed25519 device identity. A public deployment must restrict this port at the host/cloud firewall and apply the deployment's normal TCP/TLS connection controls. The in-process session limits begin after transport acceptance and are defense in depth, not a raw handshake-flood defense.

### Linux Hub

Use `packaging/systemd/cumg-v2-hub.service` plus `packaging/systemd/hub.env.example` as templates. The unit uses systemd encrypted credentials for the Hub and grant-signing application keys and a systemd credential path for the ACME-managed TLS private key. Provision long-lived application keys into the encrypted credential store outside the repository, for example:

```bash
sudo systemd-creds encrypt --name=hub-secret /secure/admin/hub.key   /etc/credstore.encrypted/hub-secret
sudo systemd-creds encrypt --name=grant-secret /secure/admin/grant.key   /etc/credstore.encrypted/grant-secret
```

Keep the recovery/rotation copy in the operator's normal secret manager. Do not retain plaintext administrative copies in the checkout. The service receives `%d/hub-secret`, `%d/grant-secret`, and `%d/tls-key` paths; private bytes are not environment-variable values.

For northbound OAuth introspection, use the optional encrypted-credential drop-in in `packaging/systemd/cumg-v2-hub-oauth-credential.conf.example` rather than putting the client secret in `hub.env`.

#### Optional MemoryUsageStore sidecar

Usage accounting is disabled by default. To enable it, install `packaging/systemd/cumg-v2-usage-sidecar.service`, copy `usage.env.example` outside the repository, build/install `mcp-usage-control` core from source locally, and install the optional `cumg-v2-hub-usage.conf.example` drop-in. The Hub then uses `CUMG_V2_USAGE_ENDPOINT=http://127.0.0.1:8787/`.

The drop-in couples the sidecar lifecycle to Hub restart; therefore an explicit packaged Hub restart recreates the non-durable MemoryUsageStore. If you manually supervise Hub and sidecar separately, a Hub-only restart does not reset a still-running sidecar. In either case, usage reset never clears CUMG operation/quarantine checkpoints. Do not use this Memory store as a financial ledger. See [`V2_USAGE_ACCOUNTING.md`](V2_USAGE_ACCOUNTING.md).

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

Customize `packaging/launchd/com.github.git-ksk.cumg-v2-agent.plist`, replacing `@BINARY@` and `@HOME@`, then install it as a user LaunchAgent. Cua-backed GUI automation must run in the logged-in user session so Accessibility/Screen Recording TCC attribution remains explicit. Secret/trust files live outside the repository under the user's Application Support tree with restrictive permissions.

### Overload and observability

The Hub defaults to bounded Agent sessions/session starts and bounded northbound MCP request concurrency/rate. Excess Agent sessions use gRPC `RESOURCE_EXHAUSTED`; excess northbound requests use HTTP 429 or 503. Keep external firewall/reverse-proxy limits as the outer control.

OTLP is opt-in through standard OpenTelemetry variables. `OTEL_EXPORTER_OTLP_ENDPOINT` enables traces and metrics; `OTEL_EXPORTER_OTLP_TRACES_ENDPOINT` and `OTEL_EXPORTER_OTLP_METRICS_ENDPOINT` enable individual signals; `OTEL_SDK_DISABLED=true` disables export. The packaged build uses the standard OTLP `grpc` transport. Default telemetry intentionally excludes command/result bodies, argv, stdout/stderr, file contents, screenshots, clipboard data, bearer tokens, OAuth introspection secrets, grants, signatures and private key material. Protocol failures log only a message kind plus safe error metadata, never the full signed protocol object.

V2 structured events use control-plane correlation fields only when available. The principal incident keys are `operation_id`, `device_id` and `generation`; `capability`, `outcome`, `error_code`, `indeterminate_reason`, `reconnect_attempt` and `backend` add bounded diagnostic context. Authenticated principal issuer/subject is not emitted by default. The main event families cover Agent session start/accept/supersede/end/reconnect/exhaustion, operation admission/dispatch/terminal failure or completion, cancellation request/acknowledgement, indeterminate/quarantine/resolution, persistence failure, authorization failure, overload rejection, backend ambiguity/timeout and stale result/session rejection.

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

For an incident, correlate Hub and Agent by `device_id` + `generation`, then follow `operation_id`. A `v2_operation_indeterminate` event must be followed by durable quarantine until a `v2_quarantine_resolved` event exists for that operation. Persistence failures expose a safe `error_code` such as `persistence_checkpoint_too_large` without a path or serialized checkpoint. Reconnect exhaustion and heartbeat timeouts are visible independently from TLS/transport connection failures. OAuth introspection unavailability is distinct from authorization denial, and a quarantine admission rejection remains `device_indeterminate` rather than being retried or auto-replayed.

See [`V2_M1_ACCEPTANCE.md`](V2_M1_ACCEPTANCE.md) for the final security gate and [`../packaging/README.md`](../packaging/README.md) for lifecycle details.

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
