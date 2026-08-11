# Deployment

V1 is designed to run on the same machine as the computer-use backend and remain bound to loopback. Remote access is provided by a separate authenticated TLS reverse proxy or tunnel.

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

## Local startup

1. Install Rust 1.88+ and Cua Driver.
2. Configure the OS permissions required by Cua.
3. Copy the example environment and review the tool allowlist.

```bash
cp .env.example .env
set -a
source .env
set +a
cargo run --locked
```

Check readiness locally:

```bash
curl --fail http://127.0.0.1:8100/healthz
```

A healthy response means the gateway currently considers its backend connection ready. It is not an end-to-end proof that every desktop permission or tool action is usable.

## Tool policy

The gateway is deny-by-default:

- empty `CUMG_ALLOW_TOOLS` exposes no tools;
- a comma-separated allowlist exposes only those discovered tools;
- `CUMG_DENY_TOOLS` always overrides the allowlist;
- `CUMG_ALLOW_TOOLS=*` explicitly exposes every discovered backend tool and should be used only after reviewing the backend surface.

`examples/cua-policy.yaml` provides an optional second policy layer inside Cua for argument-aware restrictions.

## Host validation

The default accepted Host authorities are loopback-only. A reverse proxy commonly forwards the public hostname as `Host`, so a remote deployment usually needs an explicit value such as:

```text
CUMG_ALLOWED_HOSTS=computer.example.com
```

Include a port only when it is actually present in the forwarded authority.

Do not disable Host validation to make a deployment work. Either configure the exact public authority or deliberately rewrite/preserve the Host at the trusted proxy.

## Origin validation

Browser-originated requests with an `Origin` header are checked independently of Host. Add only exact origins that are expected:

```text
CUMG_ALLOWED_ORIGINS=https://client.example.com
```

Non-browser MCP clients may not send an Origin header. Do not add wildcard origins merely for convenience.

## Cloudflare Tunnel + Access

`examples/cloudflared.yml` shows the tunnel shape. Replace placeholders locally and keep real tunnel IDs and credential paths out of the repository.

A typical deployment keeps:

```text
CUMG_BIND=127.0.0.1:8100
CUMG_ALLOWED_HOSTS=computer.example.com
```

and protects `computer.example.com` with Cloudflare Access before routing the tunnel to `http://127.0.0.1:8100`.

If a browser-originated MCP client is part of the design, add its exact HTTPS origin to `CUMG_ALLOWED_ORIGINS` as well.

## Reverse-proxy requirements

Whichever proxy is used, it should provide:

- TLS for the remote connection;
- authentication before requests reach the gateway;
- an intentional Host forwarding/rewrite policy matching `CUMG_ALLOWED_HOSTS`;
- no direct route to the Cua stdio backend;
- no accidental public exposure of other local services.

The gateway should remain on loopback unless a different bind is a deliberate, reviewed deployment decision.

## Logging

Normal gateway logs intentionally omit raw tool arguments/results. Treat reverse-proxy logs separately: query strings, headers, authentication metadata, and request bodies can still leak information depending on proxy configuration.

Do not enable body logging for MCP traffic unless there is a narrowly scoped debugging need and the resulting data is handled as sensitive.

## Secrets

Do not commit:

- tunnel credential files or IDs tied to a real deployment;
- Access service tokens;
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
- cloud control plane.

All MCP clients connected to one V1 gateway ultimately share one serialized physical desktop/backend state.
