# Roadmap

This file is a repository snapshot of implementation status. The project design report remains the canonical long-term roadmap.

## Positioning rule

V1 is a hardened MCP-to-computer-use gateway. V2 must **not** proceed merely because multi-machine routing is technically possible.

The ecosystem already contains substantial overlap in computer-use engines, daemon/session policy, AI-native remote desktop, remote-device MCP access, and multi-device orchestration. The V2 candidate is therefore a **secure delegated device capability control plane**. Its differentiation must come from identity, grants, leases, replay/cancellation safety, policy evidence, and backend-neutral control semantics rather than screen transport or computer-use execution itself.

Before major V2 implementation, V2-M0 must demonstrate one concrete security/control workflow that the compared existing products do not already satisfy cleanly. If that gap cannot be demonstrated, pause V2 rather than building another remote-device orchestrator.

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
- [x] integrate the pinned official MCP conformance runner for V1-applicable server-boundary scenarios
- [x] explicitly propagate upstream cancellation to the downstream MCP request ID and test it

> Passing the repository's dual-protocol smoke tests plus the selected official conformance scenarios is **not** a full MCP conformance certification. The upstream frozen requirement sets include capabilities and fixture-specific scenarios that this tools-only gateway intentionally does not advertise.

### M3: policy and observability
- [x] deny-by-default tool allowlist / denylist
- [x] explicit `*` opt-in for every discovered backend tool
- [x] optional Cua argument-level policy layer
- [x] per-call timeout
- [x] structured audit metadata
- [x] no sensitive argument/result logging by default
- [x] semantic tool classification (`observe` / `interact` / `system` / `dangerous`), with unknown tools classified conservatively as `dangerous`
- [x] gateway-owned backend child PID / cumulative CPU seconds / RSS health metrics

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
- [x] idle gateway CPU/RAM regression benchmark in hosted Linux CI
- [x] deterministic 100-call `tools/call` soak test through the real gateway/backend-MCP path

### V1 finite completion gate

Automated/code-local V1 closeout is complete. Only operator-controlled acceptance remains before calling V1 fully closed:

- [x] pinned official MCP conformance runner integrated for V1-applicable server-boundary scenarios
- [x] downstream cancellation propagation explicitly tested/guaranteed
- [x] semantic dangerous-tool classification available
- [ ] trusted desktop E2E executed on the dedicated test Mac
- [ ] representative Cloudflare Access/Tunnel + ChatGPT remote MCP dogfood completed
- [x] idle resource benchmark completed
- [x] 100-call soak test completed

See [`V1_ACCEPTANCE.md`](V1_ACCEPTANCE.md) for the two remaining operator-controlled checks. After they pass (or are explicitly waived), close V1 and do not add features solely to duplicate capabilities that Cua or another backend already provides upstream.

## V2 — Secure delegated device capability control plane candidate

Conceptual boundary:

```text
MCP client
   |
   v
Hub
   |
   | authenticated, backend-neutral command/grant protocol
   v
outbound Agent
   |
   v
pluggable computer-use backend (Cua first)
```

Cua remains an important backend. Cua-specific tool names or transport behavior must not become the permanent Hub↔Agent protocol.

### V2-M0: competitor-gap PoC + trust model — GO/NO-GO gate

Before building a general Hub or multi-machine router:

- [ ] document overlap/gaps against Cua upstream and representative remote-device / AI-remote-control MCP products
- [ ] define one concrete delegated-capability scenario that is not already cleanly satisfied by those products
- [ ] prove that scenario end-to-end with one device
- [ ] record an explicit GO/NO-GO decision based on the PoC rather than project momentum

Candidate PoC evidence:

1. enroll one device with a cryptographic device identity;
2. Agent establishes outbound-only authenticated connectivity where practical;
3. issue a short-lived `observe` capability grant;
4. reject an `interact` action until a separate grant/approval exists;
5. hold one operation lease so conflicting control of the same desktop fails closed;
6. reject replay of a consumed, revoked, or expired grant;
7. emit audit evidence for device/grant/policy/outcome without storing raw screenshots, tool arguments, or results;
8. prove reconnect cannot silently transfer an in-flight action lease.

Trust/protocol design required before GO:

- [ ] device identity and enrollment model
- [ ] Hub↔Agent authentication and key-rotation model
- [ ] typed, versioned command/result schema independent of transport and backend
- [ ] capability advertisement/negotiation model
- [ ] short-lived grant format, expiry, revocation, and replay rules
- [ ] operation IDs and lease ownership semantics
- [ ] cancellation and reconnect semantics
- [ ] audit identifiers and policy-decision evidence
- [ ] threat model covering compromised Hub, Agent, backend, and MCP client

If the PoC does not establish a meaningful gap, stop here.

### V2-M1: single secure remote Agent

Only after V2-M0 GO:

- [ ] outbound Agent connection
- [ ] heartbeat/reconnect with bounded backoff
- [ ] one-device routing
- [ ] per-device operation lease / serialization ownership
- [ ] short-lived capability-grant validation
- [ ] fail-closed stale/offline-agent behavior
- [ ] clean cancellation/disconnect semantics
- [ ] Cua adapter behind the backend capability contract

### V2-M2: multi-machine Hub

- [ ] machine registry
- [ ] explicit machine selection/routing
- [ ] per-machine policy and capability ceiling
- [ ] concurrent execution across independent machines while retaining per-machine serialization
- [ ] stale/offline device handling that fails closed
- [ ] backend-neutral capability discovery
- [ ] audit trail without raw screenshots/arguments/results by default

### V2-M3: delegated approvals

- [ ] semantic capability classes (`observe`, `interact`, `system`, `dangerous`)
- [ ] optional approval boundary for dangerous capabilities
- [ ] short-lived delegated grants instead of blanket backend exposure
- [ ] explicit grant expiry/revocation/replay semantics
- [ ] policy-decision evidence in audit metadata

## V3 — Pluggable native backends

- [ ] backend capability contract
- [ ] macOS native adapter
- [ ] Windows native adapter
- [ ] Linux native adapter
- [ ] Cua remains supported as a backend

V3 adapters must remain behind the same backend-neutral capability contract established by V2. Do not couple the Hub↔Agent command model to Cua-specific wire details.

## Explicit non-goals

- building a new screenshot/input computer-use engine
- building screen streaming or a general remote desktop UI
- claiming differentiation merely because multiple machines can be routed
- exposing a Cua-specific protocol as the permanent Hub↔Agent API
- blanket long-lived device-control credentials
- automatic replay of ambiguous state-changing computer-use calls
