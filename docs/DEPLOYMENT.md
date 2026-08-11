# Deployment

V1 is designed to run on the same machine as the computer-use backend and remain bound to loopback. Remote access comes from a separate authenticated TLS reverse proxy or tunnel.

Get the local path working with [`GETTING_STARTED.md`](GETTING_STARTED.md) before following this guide.

## Required topology

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

Create a policy that matches the intended users or machine credentials. Do not publish the tunnel hostname first and plan to add Access later.

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

First verify that unauthenticated access is rejected by Access. Then connect using the intended identity/OAuth/service-token mechanism.

For a Service Token, Cloudflare's standard request headers are:

```text
CF-Access-Client-Id
CF-Access-Client-Secret
```

Never put the secret in the repository. [`CLIENTS.md`](CLIENTS.md) shows an environment-backed Codex configuration.

## Reverse-proxy requirements

If you use something other than Cloudflare, preserve the same properties:

- HTTPS/TLS for the remote connection;
- authentication before requests reach the gateway;
- an intentional Host forwarding/rewrite policy matching `CUMG_ALLOWED_HOSTS`;
- exact Origin allowlisting when browser Origins are expected;
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

`.gitignore` excludes `.env` and `.env.*` except `.env.example`.

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
