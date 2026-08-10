# Security

Computer-use is equivalent to granting remote keyboard, mouse, screen, and potentially shell access. The gateway is therefore a security boundary, not merely a transport adapter.

## V1 defaults

- Listen only on `127.0.0.1` unless explicitly overridden.
- Do not implement anonymous public internet exposure.
- Use an authenticating reverse proxy for remote access.
- Keep Cua on stdio; do not expose the backend port/process directly.
- Apply policy before forwarding a tool call.
- Avoid logging raw tool arguments or screenshot content.

## Policy levels

Planned categories:

- `observe`: screenshot, accessibility snapshot, window/app listing
- `interact`: click, type, scroll, drag, keyboard
- `system`: clipboard, app launch, file interactions
- `dangerous`: shell, destructive filesystem actions, credential-sensitive actions

The backend's own permission system remains authoritative. Gateway policy only narrows capability; it must never silently widen backend permissions.

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

Do not bind directly to `0.0.0.0` merely because Cloudflare Tunnel is present.
