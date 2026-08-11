# MCP client setup

The gateway exposes an MCP **Streamable HTTP** server. Use the gateway endpoint rather than connecting clients directly to `cua-driver mcp`; connecting directly to Cua bypasses the gateway's Host/Origin checks, tool policy, audit metadata, serialization, timeout/reconnect behavior, and no-replay boundary.

## Endpoint

Local:

```text
http://127.0.0.1:8100/mcp
```

Remote, after following [`DEPLOYMENT.md`](DEPLOYMENT.md):

```text
https://computer.example.com/mcp
```

The gateway itself does not implement public authentication in V1. A remote endpoint must be protected by an authenticated TLS reverse proxy/tunnel.

## Codex CLI / Codex IDE / ChatGPT desktop

OpenAI currently documents Streamable HTTP MCP support in Codex and the ChatGPT desktop MCP-server settings. These clients can share the same Codex MCP configuration on a host.

The authoritative OpenAI documentation is:

```text
https://developers.openai.com/codex/mcp
```

### Local configuration

Add this to `~/.codex/config.toml`:

```toml
[mcp_servers.computer_use_gateway]
url = "http://127.0.0.1:8100/mcp"
default_tools_approval_mode = "prompt"
```

`prompt` is intentional. Client-side approvals are an additional guardrail; they do not replace the gateway allowlist or Cua's native policy layer.

You can inspect configured servers from Codex CLI:

```bash
codex mcp list
```

In the Codex TUI, `/mcp` shows active MCP servers.

### ChatGPT desktop UI

Current OpenAI documentation describes this path:

1. Open **Settings**.
2. Select **MCP servers**.
3. Select **Add server**.
4. Choose **Streamable HTTP**.
5. Enter `http://127.0.0.1:8100/mcp`.
6. Save and restart the client when prompted.

UI labels can change as the product evolves; use the current OpenAI MCP documentation above if the controls differ.

### Codex IDE extension

Current OpenAI documentation describes an equivalent **MCP servers → Add server → Streamable HTTP** flow in the IDE extension. The `config.toml` example above is also a useful explicit configuration path.

## Codex through Cloudflare Access

For a non-interactive client, a Cloudflare Access **Service Token** is one practical authentication option. Cloudflare service tokens use a Client ID and Client Secret request header pair.

Do not store the secret directly in `config.toml`. Put the credentials in the process environment instead:

### macOS / Linux

```bash
export CF_ACCESS_CLIENT_ID='your-client-id'
export CF_ACCESS_CLIENT_SECRET='your-client-secret'
```

### Windows PowerShell

```powershell
$env:CF_ACCESS_CLIENT_ID = "your-client-id"
$env:CF_ACCESS_CLIENT_SECRET = "your-client-secret"
```

Then configure Codex to obtain the HTTP header values from those environment variables. Keep the TOML inline table on one line so the snippet is valid when copied directly:

```toml
[mcp_servers.computer_use_gateway]
url = "https://computer.example.com/mcp"
default_tools_approval_mode = "prompt"
env_http_headers = { "CF-Access-Client-Id" = "CF_ACCESS_CLIENT_ID", "CF-Access-Client-Secret" = "CF_ACCESS_CLIENT_SECRET" }
```

The Cloudflare Access application must have a policy that accepts the service token. Treat the Client Secret as a credential: do not commit it, paste it into issues, or put it into normal gateway logs.

Cloudflare's current Service Token documentation is:

```text
https://developers.cloudflare.com/cloudflare-one/access-controls/service-credentials/service-tokens/
```

## ChatGPT web

ChatGPT web is a different integration path from the local Codex configuration above. OpenAI currently documents that ChatGPT web does **not** connect directly to a local MCP server. Use a remote MCP endpoint, or a supported OpenAI secure-tunnel mechanism, and follow the current ChatGPT developer-mode/MCP-app instructions.

Authoritative OpenAI guidance:

```text
https://help.openai.com/en/articles/12584461-developer-mode-and-full-mcp-connectors-in-chatgpt
```

The current setup flow uses the app/MCP configuration UI to provide the remote endpoint and authentication, scan tools, and create/test the app. Availability of developer mode, write actions, and administrative controls depends on the current ChatGPT plan/workspace and can change, so this repository deliberately does not duplicate a plan matrix.

For this gateway, the endpoint is:

```text
https://computer.example.com/mcp
```

Before connecting ChatGPT web, verify that:

- the endpoint is HTTPS;
- authentication is enforced before traffic reaches the gateway;
- `CUMG_ALLOWED_HOSTS` or the proxy Host rewrite matches the actual request path;
- any browser `Origin` that must be accepted is explicitly configured;
- the gateway allowlist exposes only reviewed computer-use tools.

## Other MCP clients

For another client, choose **Streamable HTTP** and configure the local or remote URL above. If the client supports custom headers, OAuth, or bearer tokens, configure them to match the authentication layer in front of the remote gateway.

A client that sends an unexpected `Host` or browser `Origin` may receive HTTP 403 by design. Do not work around that by globally disabling the transport guards; fix the proxy/client configuration or explicitly allow the exact expected value.

## Verify what the client can see

After connection, compare the visible tool list with the gateway allowlist. A newly discovered backend tool is not exposed unless the gateway policy permits its exact name (or `*` was deliberately configured).

If the client shows no tools or cannot connect, see [`TROUBLESHOOTING.md`](TROUBLESHOOTING.md).
