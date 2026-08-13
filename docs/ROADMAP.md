# Roadmap

This file is the implementation roadmap snapshot. Historical acceptance detail lives in the milestone acceptance/progress documents.

Canonical V2 product boundary: [`V2_POSITIONING.md`](V2_POSITIONING.md).  
Standard/OSS boundary: [`V2_STANDARDIZATION.md`](V2_STANDARDIZATION.md).
P2 replacement-seam review: [`V2_P2_REPLACEMENT_SEAMS.md`](V2_P2_REPLACEMENT_SEAMS.md).

## Positioning rule

V1 is a hardened MCP-to-computer-use gateway.

V2 is **not** a generic multi-machine MCP bridge, remote-desktop product, delegated-authorization protocol, or broad vendor-neutral physical-device control plane.

The final competitor review on 2026-08-12 found material overlap in the broader category, including SINT Protocol, Arm Device Connect, OpenClaw, OAHL, QuickDesk, Obot, and delegated-authorization systems.

The project-specific V2 boundary is therefore narrower:

> **uncertainty-aware execution safety for delegated control of stateful interactive desktops**

The core invariant is:

```text
external principal
      |
specific desktop + exact capability
      |
operation ID + exclusive ownership + fencing
      |
state-changing desktop action
      |
cancel / timeout / disconnect / lost response
      |
can non-execution or termination be proven?
      |
  yes -> terminal
  no  -> indeterminate -> quarantine -> explicit resolution
```

An ambiguous state-changing operation is never automatically replayed because a client, Hub, Agent, transport, backend, or device reconnects.

## V1 — Remote MCP Gateway

**Status: closed 2026-08-11.**

V1 established the hardened local/remote MCP boundary around Cua:

- localhost-first Streamable HTTP MCP;
- Host/Origin protection;
- deny-by-default exact tool policy;
- conservative semantic classification;
- backend timeouts/reconnect without automatic replay of failed state-changing calls;
- serialized physical desktop operations;
- downstream cancellation propagation;
- privacy-preserving audit metadata;
- real-Cua Linux/macOS/Windows smoke coverage;
- trusted macOS desktop E2E;
- Cloudflare Access/Tunnel + ChatGPT remote MCP dogfood;
- conformance, soak, and resource regression gates.

See [`V1_ACCEPTANCE.md`](V1_ACCEPTANCE.md) for acceptance evidence.

Do not add V1 features solely to duplicate maintained backend functionality.

## V2-M0 — competitor-gap PoC + trust model

**Status: GO, 2026-08-11.**

M0 proved the initial single-device delegated-capability control semantics:

- cryptographic device identity;
- outbound authenticated Agent connectivity;
- principal -> device -> exact capability authorization;
- short-lived grants and replay rejection;
- explicit operation IDs;
- per-device lease/serialization ownership;
- generation/capability revision checks;
- bounded admission and backpressure;
- signed cancellation/result semantics;
- reconnect rules that cannot silently transfer in-flight ownership;
- backend-neutral adapter contract;
- threat model for compromised Hub, Agent, backend, and client.

See [`V2_M0_POC.md`](V2_M0_POC.md) and [`V2_THREAT_MODEL.md`](V2_THREAT_MODEL.md).

## V2-M1 — single secure remote Agent

**Status: PASS, 2026-08-12.**

M1 established the production-candidate single-device foundation:

- gRPC bidirectional streaming over TLS while preserving transport-neutral application semantics;
- standard MCP Authorization/OAuth northbound boundary;
- OAuth bearer token non-forwarding;
- separate Hub/device/grant/TLS identity lifecycles;
- restart-safe replay/trust checkpoints;
- bounded queueing, rate/connection shedding, and per-device ownership;
- OpenTelemetry/OTLP observability;
- launchd/systemd packaging;
- Agent-native process/shell/read-only-filesystem capabilities;
- Cua behind a backend adapter contract;
- real Cua Driver 0.19.3 cancellation E2E where cancellation propagation does **not** imply non-execution;
- `indeterminate` outcome + device quarantine when the backend cannot prove a safe terminal result.

See [`V2_M1_ACCEPTANCE.md`](V2_M1_ACCEPTANCE.md).

### Post-M1 correction

M1 proved useful semantics, but the final competitor review showed that broader claims such as these are not sufficient differentiation by themselves:

- vendor-neutral physical-device control plane;
- scoped/expiring capability grants;
- action IDs and replay defense;
- device identity/registry/fleet state;
- device reservation/lease;
- generic physical-AI governance;
- multi-machine routing.

Accordingly, subsequent work must prioritize the narrower uncertainty-aware desktop execution core rather than broadening the control-plane surface.

## V2-M2 — uncertainty-aware core hardening

**Primary tracking issue: #24.**

M2 is no longer “build a multi-machine Hub” as a feature milestone. The first objective is to make the M1 operation-safety semantics explicit, durable, and difficult to violate.

### P0: authoritative operation state machine

- [x] define one reviewed operation-state model for dispatch, running, cancellation, terminal outcomes, and `indeterminate`;
- [x] define evidence required to prove non-execution or clean termination;
- [x] ensure timeout/cancel/disconnect/lost-result paths cannot collapse uncertainty into ordinary failure;
- [x] reject late/stale results after session/device/ownership generation changes;
- [x] prevent duplicate terminal finalization;
- [x] add invariant/property coverage for illegal transitions;
- [x] keep replay/tombstone state bounded without forgetting unresolved ambiguity.

### P0: first-class quarantine and explicit resolution

- [x] persist quarantine independently of connection/session lifetime;
- [x] bind quarantine to the exact ambiguous operation and device generation;
- [x] expose an explicit, auditable resolution path;
- [x] record resolver principal, operation ID, decision, and relevant evidence metadata;
- [x] ensure resolution can never replay the old operation;
- [x] crash/restart test while quarantined;
- [x] crash/restart test during resolution;
- [x] deny normal work until the exact ambiguous state is resolved.

### P0: ownership and fencing under failure

- [x] competing principals cannot steal or inherit in-flight ownership;
- [x] reconnect cannot silently transfer old ownership;
- [x] Hub restart preserves quarantine/ownership decisions;
- [x] Agent restart preserves enough state to reject replay/stale finalization;
- [x] stale Agent generations cannot finalize old operations;
- [x] duplicate/late cancellation acknowledgements cannot clear quarantine incorrectly;
- [x] network partition/reconnect races are covered around dispatch and result delivery.

P0 execution-safety hardening is accepted on 2026-08-12. The detailed gap analysis, invariants, security review, and residual work are recorded in [`V2_P0_EXECUTION_SAFETY.md`](V2_P0_EXECUTION_SAFETY.md).

## V2-M2 acceptance — multi-device invariant proof

Only after the P0 core is strong should multi-device work advance beyond the minimum routing needed to prove invariants.

M2 acceptance requires:

1. [x] Device A enters `indeterminate` after an ambiguous state-changing action and remains quarantined.
2. [x] Device B remains independently usable by another authorized principal.
3. [x] Another principal cannot acquire, inherit, or replace Device A's unresolved ownership.
4. [x] Hub restart preserves independent A/B state.
5. [x] reconnect/failover does not replay Device A's ambiguous operation.
6. [x] stale device/Agent/capability generations cannot route or finalize old work.
7. [x] queues/load shedding do not bypass per-device ownership invariants.

A machine registry, device list, or successful routing to two machines is **not** sufficient M2 acceptance evidence.

P1 fixed-set proof accepted boundary: the implementation composes existing per-device P0 Hubs with independent checkpoints and no shared fleet scheduler. Final release-closeout physical acceptance passed on 2026-08-13 against `main` commit `bb39390f3587902a7df918fe1ff4a8b28c328d50` in Desktop E2E run `31675515516`, including durable quarantine across Hub/Agent restart, newer-generation reconnect without replay, explicit resolution, and reuse.

## V2-M3 — backend portability and OSS integration

After the uncertainty-aware core and multi-device invariant proof pass:

### Backend portability

- [x] integrate a second backend or deterministic reference executor with materially different cancellation/result behavior;
- [x] require adapters to provide evidence for terminal versus ambiguous classification;
- [x] prove the operation-state machine remains unchanged across backends;
- [x] map unsupported evidence conservatively to `indeterminate` rather than backend-specific guesses.

### Replace/reuse generic infrastructure

Review maintained standards/OSS before expanding custom implementations:

- [x] delegated authorization candidates reviewed; add the exact `DeviceCapabilityAuthorizer` seam, keep existing OAuth/introspection and in-process policy as defaults, and defer SINT/Grantex/Open Agent Auth dependencies;
- [x] device registry/fabric candidates reviewed; defer Arm Device Connect and keep immutable fixed-set composition because a generic discovery/failover plane is not yet safer or necessary;
- [x] Computer Use runtime candidates reviewed; add `ComputerUseBackendAdapter`, keep direct Cua as the default, and defer OpenClaw integration until it provides a concrete operational advantage;
- [x] ROSClaw reviewed for compatibility/invariant reuse only; do not fork or import its broader robotics runtime/state machine;
- [x] SPIFFE reviewed and deferred until dynamic workload/trust-domain scale justifies its operational cost;
- [x] OPA/Cedar-class generic policy engines reviewed and deferred while authorization remains an exact principal/device/capability tuple.

The P2 review is accepted when these decisions and narrow seams pass regression. A checked review item does **not** mean the external system was adopted. Replacement remains conditional on evidence that the uncertainty-aware execution invariant is preserved or improved. See [`V2_P2_REPLACEMENT_SEAMS.md`](V2_P2_REPLACEMENT_SEAMS.md).

Final P2 integration acceptance is **accepted / closed on 2026-08-13**. Trusted merged `main` commit `bb39390f3587902a7df918fe1ff4a8b28c328d50` passed Desktop E2E run `31675515516`, supplementing the deterministic replacement-seam, multi-device, backend-portability, observability, and resource-regression gates.

## Later product/fleet work

Only after M2/M3 core acceptance should the project prioritize convenience/product surface such as:

- fleet dashboard/UX;
- broad device discovery;
- routing convenience;
- orchestration/workflows;
- richer approval UX;
- additional native GUI adapters.

These features are optional consumers of the core, not the core itself.

## Explicit non-goals

- building a new screenshot/input computer-use engine;
- building screen streaming or a general remote desktop UI;
- building another generic agent authorization protocol;
- building another generic physical-device fabric/registry when maintained OSS can serve the role;
- claiming differentiation merely because multiple machines can be routed;
- claiming differentiation merely because grants are scoped/short-lived or devices have leases;
- exposing a Cua-specific protocol as the permanent Hub-Agent contract;
- blanket long-lived device-control credentials;
- automatic replay of ambiguous state-changing work;
- treating reconnect, heartbeat, backend restart, or device liveness as proof that an old ambiguous operation is safe to forget.

## GO / NO-GO rule

**GO:** strengthen and prove operation ownership, stale-result fencing, ambiguity handling, quarantine, explicit resolution, and no-auto-replay for delegated interactive-desktop control.

**NO-GO by default:** broaden into generic auth, physical-device fabric, fleet management, remote desktop, or orchestration merely because those features are technically possible.

If maintained OSS later provides equivalent per-desktop ownership, durable `indeterminate` quarantine, explicit resolution, stale-result fencing, and no-auto-replay semantics, reevaluate integration or retirement rather than defending sunk cost.
