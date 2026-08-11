# Roadmap

This file is a repository snapshot of implementation status. The project design report remains the canonical long-term roadmap.

## V1 — Remote MCP Gateway

### M0: repository skeleton
- [x] independent Rust repository
- [x] architecture boundary documented
- [x] Cua backend selected
- [x] MCP 2026-07-28 / rmcp 3.x selected
- [x] localhost-first security posture

### M1: Cua backend adapter
- [x] start/connect to `cua-driver mcp`
- [x] discover and dynamically rediscover tools
- [x] forward `tools/call`
- [x] preserve backend MCP content/result envelopes when proxying
- [x] graceful child shutdown
- [x] readiness health probe
- [x] bounded reconnect with exponential backoff
- [x] connection and operation timeouts
- [x] serialize desktop operations across clients
- [x] never replay a failed state-changing tool call automatically

### M2: Streamable HTTP MCP server
- [x] expose `/mcp` through MCP Streamable HTTP
- [x] support the `2025-11-25` stateful lifecycle in compatibility smoke tests
- [x] support the `2026-07-28` stateless lifecycle in compatibility smoke tests
- [x] reject unsafe Origin values
- [x] reject unsafe Host authorities / DNS rebinding attempts
- [x] bind to localhost by default
- [ ] integrate the official MCP conformance requirement runner
- [ ] explicit downstream cancellation propagation test/guarantee

> Passing the repository's dual-protocol smoke tests is a compatibility claim, not an MCP conformance certification.

### M3: policy and observability
- [x] deny-by-default tool allowlist / denylist
- [x] explicit `*` opt-in for every discovered backend tool
- [x] optional Cua argument-level policy layer
- [x] per-call timeout
- [x] structured audit metadata
- [x] no sensitive argument/result logging by default
- [ ] semantic dangerous-tool classification (`observe` / `interact` / `system` / `dangerous`)
- [ ] backend CPU/RSS health metrics

### M4: CI and dogfood
- [x] Rust fmt/check/test CI with `Cargo.lock` and `--locked`
- [x] real-Cua smoke on Linux, macOS, and Windows
- [x] dual lifecycle smoke (`2025-11-25`, `2026-07-28`)
- [x] malicious Host/Origin rejection smoke
- [x] pinned Cua installer SHA-256 verification
- [x] pinned Cua release payload SHA-256 verification
- [x] installed `cua-driver` binary identity verification against the verified payload
- [x] manual trusted self-hosted macOS desktop E2E workflow
- [ ] execute desktop E2E on a dedicated TCC-granted test Mac
- [ ] Cloudflare Tunnel + Access deployment dogfood for this gateway
- [ ] ChatGPT remote MCP connection dogfood for this gateway
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
