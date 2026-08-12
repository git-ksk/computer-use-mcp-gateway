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

> **M1 implementation note (2026-08-12):** the raw-TLS regression transport is TLS 1.3-only with pinned trust + dedicated ALPN. The production-candidate gRPC transport uses TLS + HTTP/2 with pinned certificate trust/domain validation while the independent Ed25519 application identity remains mandatory; a minimum TLS-version policy for the deployed gRPC endpoint must be documented rather than inferred from the raw-TLS PoC. The foundation also includes signed heartbeat messages, bounded reconnect policy, one-device routing, restart-safe public trust/replay checkpoints, and end-to-end process execution/cancellation evidence. See [`V2_M1_PROGRESS.md`](V2_M1_PROGRESS.md).

> **Shell-first product direction (2026-08-11):** the self-owned Agent is not intended to remain only a secure wrapper around Cua. Direct process/shell execution is a first-class Agent capability and is the next implementation priority. Shell/process operations must execute locally in the Agent without driving a terminal GUI or routing through Cua. GUI/computer-use remains available through the Cua adapter during the transition and can later gain native platform adapters. The preferred delivery order is **Agent core → direct process/shell → bounded filesystem capabilities → GUI via Cua → native GUI backends**.

> **Transport direction (2026-08-12):** keep Hub↔Agent application semantics transport-neutral, retain the existing raw TLS + signed-JSON implementation as a regression/reference transport, and use **gRPC bidirectional streaming over TLS with Protobuf code generation as the M1 production candidate**. The first migration slice intentionally carries the existing independently signed V2 messages inside bounded Protobuf frames so transport migration does not rewrite grants, leases, replay protection, cancellation, or Agent execution at the same time. A native-Protobuf application schema may follow incrementally. The initial Hub deployment target is a small always-on VM rather than a request-lifetime serverless runtime.

- [x] outbound Agent connection over the accepted encrypted M1 channel
- [x] gRPC bidirectional streaming transport candidate over TLS, preserving the existing signed application protocol during migration
- [x] reusable outbound lifecycle and encrypted multi-session reconnect acceptance
- [x] operator-facing long-lived Agent process lifecycle (`v2_agent`) with outbound gRPC/TLS, bounded reconnect, heartbeat liveness, Ctrl-C shutdown, and non-blocking process cancellation; OS-specific service packaging remains deployment work
- [x] separate file-based key/trust-anchor provisioning boundary with fail-closed filesystem checks
- [x] Agent replay/trust checkpoint wired into the long-lived service so consumed grants and terminal/in-flight operation IDs survive process restart before execution can replay
- [ ] production secret-store/certificate rotation integration for the deployed service
- [x] operator-facing single-device Hub gRPC service (`v2_hub`) for the always-on VM target, with persisted generation/admission state, heartbeat timeout, exact-capability grant issuance, bounded queueing, cancellation, reconnect cleanup, and TLS key/certificate loading
- [x] standard northbound MCP Authorization protected-resource boundary on `v2_hub` using RFC 9728 discovery + OAuth bearer validation through RFC 7662 introspection, reducing verified identity to `AuthenticatedClientPrincipal` before the existing principal -> device -> exact-capability policy; bearer tokens never cross the Hub-Agent boundary
- [x] bound replay state across reconnects: Agent and Hub terminal tombstones are pruned at authenticated generation rollover, while indeterminate Hub operations are retained until explicit resolution; grant-consumption tombstones are expiry-pruned and checkpoint files use bounded retention
- [x] heartbeat/reconnect semantics with bounded backoff
- [x] one-device routing
- [x] versioned capability advertisement with revision/generation tracking
- [x] per-device operation lease / serialization ownership
- [x] bounded per-device queueing/load shedding before work reaches the Agent
- [x] short-lived capability-grant validation with an Agent-enforced 5-minute maximum lifetime and exact `DeviceCapability` scoping for M1 Agent-native operations
- [x] fail-closed stale/offline-agent and stale-capability behavior
- [x] first-class direct process executor in the Agent (`program` + `argv` + explicit `cwd`, bounded output, timeout/cancellation, no terminal GUI)
- [x] explicit higher-risk `Shell` capability for shell syntax/pipelines, distinct from structured argv execution and requiring an exact `DeviceCapability::Shell` grant. The Agent invokes a fixed OS shell (`/bin/sh -c` on Unix, `cmd.exe /D /S /C` on Windows), bounds command size/output/time, applies the same cwd/environment policy as `ExecuteProcess`, and supervises/cancels the full process tree; this is not a filesystem sandbox
- [x] bounded read-only filesystem observation surface (`ReadFile` / `ListDirectory`) with exact capability grants, canonical path/root checks, symlink-escape rejection, bounded file bytes/directory entries, and command-local coarse errors; `ExecuteProcess` remains `Dangerous` and its argv is explicitly **not** filesystem-sandboxed
- [ ] clean live cancellation/disconnect semantics across all execution backends
  - [x] Agent-native process cancellation while the gRPC stream remains responsive; child is killed/waited and the operation ID becomes terminal before reconnect
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
