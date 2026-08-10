# Roadmap

## V1 — Remote MCP Gateway

### M0: repository skeleton
- [x] independent Rust repository
- [x] architecture boundary documented
- [x] Cua backend selected
- [x] MCP 2026-07-28 / rmcp 3.x selected
- [x] localhost-first security posture

### M1: Cua backend adapter
- [ ] start/connect to `cua-driver mcp`
- [ ] discover tools
- [ ] forward `tools/call`
- [ ] preserve text/image/structured MCP content
- [ ] graceful child shutdown
- [ ] health probe / restart policy

### M2: Streamable HTTP MCP server
- [ ] expose `POST /mcp`
- [ ] MCP 2026-07-28 conformance
- [ ] reject unsafe Origin values
- [ ] bind to localhost by default
- [ ] cancellation propagation

### M3: policy and observability
- [ ] tool allowlist / denylist
- [ ] dangerous-tool classification
- [ ] per-call timeout
- [ ] structured audit metadata
- [ ] no sensitive argument logging by default
- [ ] backend CPU/RSS health metrics

### M4: dogfood
- [ ] Cloudflare Tunnel + Access
- [ ] ChatGPT remote MCP connection
- [ ] macOS smoke test
- [ ] idle CPU/RAM benchmark
- [ ] 100-call soak test

## V2 — Hub + Agent

- [ ] machine registry
- [ ] outbound Agent connection
- [ ] typed RPC schema
- [ ] WebSocket transport
- [ ] heartbeat / reconnect / cancellation
- [ ] per-machine policy
- [ ] multi-machine MCP routing

## V3 — Pluggable native backends

- [ ] backend capability contract
- [ ] macOS native adapter
- [ ] Windows native adapter
- [ ] Linux native adapter
- [ ] Cua remains supported as a backend
