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
- [x] trusted real-desktop E2E executed on a TCC-granted operator-controlled Mac (completed 2026-08-11)
- [x] Cloudflare Tunnel + Access deployment dogfood for this gateway (completed 2026-08-11)
- [x] ChatGPT remote MCP connection dogfood for this gateway (completed 2026-08-11)
- [x] idle gateway CPU/RAM regression benchmark in hosted Linux CI
- [x] deterministic 100-call `tools/call` soak test through the real gateway/backend-MCP path

### V1 finite completion gate

V1 acceptance is complete. V1 was closed on 2026-08-11 after automated/code-local checks plus operator-controlled real-desktop and remote-access acceptance:

- [x] pinned official MCP conformance runner integrated for V1-applicable server-boundary scenarios
- [x] downstream cancellation propagation explicitly tested/guaranteed
- [x] semantic dangerous-tool classification available
- [x] trusted real-desktop E2E executed on a TCC-granted operator-controlled Mac (2026-08-11)
- [x] representative Cloudflare Access/Tunnel + ChatGPT remote MCP dogfood completed (2026-08-11)
- [x] idle resource benchmark completed
- [x] 100-call soak test completed

See [`V1_ACCEPTANCE.md`](V1_ACCEPTANCE.md) for the recorded acceptance evidence. V1 is closed; do not add V1 features solely to duplicate capabilities that Cua or another backend already provides upstream. V2-M0 retains its independent GO/NO-GO gate.

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

> **V2-M0 implementation note (2026-08-11):** the control plane now includes outbound mutually authenticated Agent connectivity, signed connection-bound session/command/cancellation/result messages, signed-Hub-time grant clocking, northbound principal authorization, continuity-proven Hub/Agent/grant-key rotation, bounded Hub/Agent execution control, and a backend-neutral adapter contract. The explicit M0 decision is **GO to V2-M1**, not production-ready. Remote confidentiality and real deployment auth/persistence remain M1 requirements. See [`V2_M0_POC.md`](V2_M0_POC.md) and [`V2_THREAT_MODEL.md`](V2_THREAT_MODEL.md).

Before building a general Hub or multi-machine router:

- [x] document overlap/gaps against Cua upstream and representative remote-device / AI-remote-control MCP products
- [x] define one concrete delegated-capability scenario that is not already cleanly satisfied by those products
- [x] prove that scenario end-to-end with one device, including the outbound authenticated Agent hop
- [x] record an explicit GO/NO-GO decision based on the PoC rather than project momentum

Candidate PoC evidence:

1. [x] enroll one device with a cryptographic device identity;
2. [x] Agent establishes outbound-only authenticated connectivity where practical;
3. [x] issue a short-lived `observe` capability grant;
4. [x] reject an `interact` action until a separate grant/approval exists;
5. [x] hold one operation lease so conflicting control of the same desktop fails closed;
6. [x] reject replay of a consumed, revoked, or expired grant;
7. [x] emit audit evidence for device/grant/policy/outcome without storing raw screenshots, tool arguments, or results;
8. [x] prove reconnect cannot silently transfer an in-flight action lease.

Trust/protocol design required before GO:

- [x] device identity and enrollment model
- [x] separate MCP client→Hub authentication/authorization from Hub↔Agent authentication and key rotation, so the control plane can answer who may use which capability on which device
- [x] typed, versioned command/result schema independent of transport and backend
- [x] capability advertisement/negotiation model that includes `backend`, `backend_version`, `platform`, and `capability_schema_version`
- [x] capability revision/generation semantics so the Hub can detect stale discovery after Agent reconnects, backend upgrades, or policy-surface changes
- [x] fail-closed handling for capabilities not explicitly understood by the Hub/Agent contract
- [x] backend-adapter conformance tests that normalize backend/platform-specific tool behavior without leaking Cua-specific names or transport semantics into the Hub↔Agent protocol
- [x] short-lived grant format, expiry, revocation, and replay rules
- [x] operation IDs and lease ownership semantics
- [x] bounded backpressure across Hub global limits, per-device queue/lease ownership, and Agent single-operation execution
- [x] cancellation and reconnect semantics
- [x] audit identifiers and policy-decision evidence
- [x] threat model covering compromised Hub, Agent, backend, and MCP client

**V2-M0 decision (2026-08-11): GO to V2-M1.** The PoC establishes a narrower capability-control gap worth pursuing: explicit client→device→capability authorization, independently verifiable short-lived grants, continuity-proven Hub/Agent/grant-key rotation, generation-bound leases, bounded admission, signed cancellation/result semantics, replay-safe ambiguous outcomes, and a backend-neutral adapter boundary. This is a GO to build the **single secure remote Agent** slice only; it is not a production-readiness claim and does not authorize skipping M1 encrypted transport, real northbound auth integration, persistence, or live backend cancellation acceptance. See [`V2_M0_POC.md`](V2_M0_POC.md) and [`V2_THREAT_MODEL.md`](V2_THREAT_MODEL.md).

If later M1 evidence shows these controls do not provide a meaningful operational advantage over maintained alternatives, revisit the decision rather than continuing by momentum.

### V2-M1: single secure remote Agent

Only after V2-M0 GO:

> **M1 implementation note (2026-08-11):** the foundation includes TLS 1.3 with pinned trust + dedicated ALPN, signed heartbeat messages, bounded reconnect policy, one-device routing, restart-safe public trust/replay checkpoints, and an end-to-end outbound TLS integration covering Ed25519 authentication → heartbeat → routing/admission/lease → short-lived grant → signed typed result. M1 acceptance is still open: long-lived reconnect lifecycle, live backend cancellation, real northbound auth integration, and private-key provisioning remain. See [`V2_M1_PROGRESS.md`](V2_M1_PROGRESS.md).

> **Shell-first product direction (2026-08-11):** the self-owned Agent is not intended to remain only a secure wrapper around Cua. Direct process/shell execution is a first-class Agent capability and is the next implementation priority. Shell/process operations must execute locally in the Agent without driving a terminal GUI or routing through Cua. GUI/computer-use remains available through the Cua adapter during the transition and can later gain native platform adapters. The preferred delivery order is **Agent core → direct process/shell → bounded filesystem capabilities → GUI via Cua → native GUI backends**.

- [x] outbound Agent connection over the accepted encrypted M1 channel
- [x] reusable outbound lifecycle and encrypted multi-session reconnect acceptance
- [ ] operator-facing long-lived Agent process/service lifecycle
- [x] separate file-based key/trust-anchor provisioning boundary with fail-closed filesystem checks
- [ ] production secret-store/certificate rotation integration for the deployed service
- [x] heartbeat/reconnect semantics with bounded backoff
- [x] one-device routing
- [x] versioned capability advertisement with revision/generation tracking
- [x] per-device operation lease / serialization ownership
- [x] bounded per-device queueing/load shedding before work reaches the Agent
- [x] short-lived capability-grant validation
- [x] fail-closed stale/offline-agent and stale-capability behavior
- [ ] first-class direct process executor in the Agent (`program` + `argv` + explicit `cwd`, bounded output, timeout/cancellation, no terminal GUI)
- [ ] explicit higher-risk shell-command capability for shell syntax/pipelines; keep it distinct from structured argv execution
- [ ] bounded filesystem capability surface required by shell workflows, with path/policy controls rather than unrestricted implicit filesystem authority
- [ ] clean live cancellation/disconnect semantics across the backend boundary
  - [x] exact downstream cancellation propagation + indeterminate device quarantine in deterministic MCP acceptance
  - [ ] real-Cua desktop cancellation acceptance
- [x] Cua adapter behind the backend capability contract with adapter conformance coverage
- [ ] keep GUI/computer-use behind a pluggable adapter boundary: Cua remains the initial GUI backend; native GUI backends are a later step and must not block shell-first M1 utility

### V2-M2: multi-machine Hub

- [ ] machine registry
- [ ] explicit machine selection/routing
- [ ] per-machine policy and capability ceiling
- [ ] client identity → device → capability authorization at the Hub boundary
- [ ] concurrent execution across independent machines while retaining per-machine serialization
- [ ] global Hub concurrency/rate limits plus bounded per-device queues
- [ ] stale/offline device and stale-capability-generation handling that fails closed
- [ ] backend-neutral capability discovery including backend/platform/version metadata
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
