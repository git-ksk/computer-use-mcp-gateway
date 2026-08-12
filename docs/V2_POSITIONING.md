# V2 product positioning

Status: **accepted direction, narrowed after final competitor review (2026-08-12)**.

This document defines the product boundary for V2 after V2-M1 acceptance and the final competitor review. The boundary is intentionally narrower than “secure remote computer use”, “vendor-neutral device control plane”, or “multi-machine MCP”.

## Positioning

`computer-use-mcp-gateway` V2 is an **uncertainty-aware execution-safety layer for delegated control of stateful interactive desktops**.

The short product statement is:

> Authorization decides whether an agent may act. CUMG additionally decides who owns the desktop operation and refuses to guess when its side effects become uncertain.

CUMG does **not** claim differentiation merely because it is:

- vendor-neutral;
- a delegated-authorization or capability-token system;
- a physical-device or fleet control plane;
- a computer-use engine or screenshot/input implementation;
- an AI-native remote desktop product;
- a generic MCP gateway;
- a generic capability broker;
- a device registry, reservation service, or multi-machine router.

Those areas already have substantial OSS and standards coverage. Projects such as SINT Protocol and Arm Device Connect materially overlap the broader “vendor-neutral physical-device execution/governance” category, while OpenClaw, OAHL, QuickDesk, Obot, and delegated-authorization projects cover additional adjacent layers.

The defensible CUMG boundary is narrower: **exclusive ownership and fail-closed recovery for state-changing operations on an interactive desktop when execution outcome becomes ambiguous**.

## Core scenario

The core problem is this sequence:

```text
external principal
      |
      v
specific interactive desktop
      |
      v
exact capability
      |
      v
exclusive operation ownership + fencing
      |
      v
state-changing action
(click / type / drag / process / other effect)
      |
      v
cancel / timeout / disconnect / lost response
      |
      v
can non-execution or termination be proven?
      |
  +---+---+
  |       |
 yes      no
  |       |
terminal  indeterminate
          |
          v
      device quarantine
          |
          v
    explicit resolution
```

An ambiguous operation is never automatically replayed merely because the client, Hub, Agent, transport, or backend reconnects.

## Thin-waist architecture

```text
IdP / MCP OAuth / OIDC / IAM / delegated-auth protocol
                    |
            authorization adapter
                    |
          +-------------------+
          |     CUMG CORE     |
          | operation ID      |
          | ownership / lease |
          | fencing / gen     |
          | replay barrier    |
          | indeterminate     |
          | quarantine        |
          | explicit resolve  |
          | no auto-replay    |
          +---------+---------+
                    |
              backend adapter
             /       |        \
           Cua     native     other
```

The layers above and below the core are replaceable. Generic authorization, device fabric, transport, fleet registry, and execution backend are not the product-specific value.

## What CUMG owns

### 1. Physical operation identity and ownership

Every state-changing desktop action must have an explicit operation identity and an authoritative owner.

CUMG owns:

- explicit operation IDs;
- per-device operation admission and exclusive ownership;
- generation/fencing checks;
- serialization of conflicting desktop actions;
- reconnect/restart rules that cannot silently transfer ownership;
- rejection of stale Agent/session results that no longer own the operation generation.

### 2. Ambiguous side-effect state

A cancellation request, disconnect, timeout, or lost response does not prove that a click, drag, keystroke, process, or other state-changing effect did not execute.

When non-execution or termination cannot be proven, CUMG must persist an `indeterminate` outcome rather than guessing success or failure.

`indeterminate` is not an ordinary transport error. It is durable execution state.

### 3. Fail-closed quarantine and explicit recovery

For ambiguous state-changing work, CUMG owns:

- replay rejection;
- restart-safe ambiguous in-flight state;
- device quarantine;
- explicit resolution before the affected desktop can be reused;
- preservation of the ambiguous operation identifier across restart/reconnect;
- no automatic replay after reconnect, failover, or client retry.

The resolution path must make the safety decision explicit rather than infer safety from liveness or a new connection.

### 4. Exact execution boundary

External authorization is reduced to the local execution question:

```text
principal -> stable desktop -> exact DeviceCapability
```

CUMG may consume MCP Authorization/OAuth, OIDC, IAM-like systems, SINT-style capability systems, Grantex/Open Agent Auth-class protocols, or other maintained authorization sources.

Their credentials are not Agent credentials and must not be forwarded southbound as a substitute for a device-scoped execution grant.

The custom value is not inventing another generic authorization protocol; it is binding an authorized intent to the operation-ownership state machine above.

### 5. Backend-neutral execution evidence

Cua is the initial GUI/computer-use backend, not the product boundary.

Native platform adapters, OpenClaw-backed execution, or other implementations may be integrated if they can provide enough evidence to map an operation into one of the supported terminal/ambiguous outcomes without weakening the core state machine.

A backend may prove clean termination and avoid quarantine. If it cannot, CUMG stays conservative.

## Keep / adapt / retire / reuse

### Keep: project-owned core

Keep custom semantics only where they directly encode uncertainty-aware desktop execution safety:

- explicit operation identity;
- exclusive per-desktop operation ownership;
- lease/fencing/generation semantics needed to preserve ownership;
- stale-result rejection;
- replay barriers for state-changing operations;
- durable `indeterminate` state;
- quarantine and explicit resolution;
- no automatic replay of ambiguous state-changing work;
- cancellation semantics that distinguish requested cancellation from proven non-execution or proven termination;
- privacy-preserving operation/policy/outcome evidence.

### Adapt: keep replaceable

Preserve interfaces around existing implementations that may later be replaced:

- principal/authentication adapters;
- grant issuers and verifiers;
- Agent/workload identity providers and verifiers;
- Hub-Agent transport bindings;
- policy-engine integrations;
- persistence/checkpoint stores;
- device registry/fleet providers;
- backend adapters.

### Retire or replace

Do not preserve custom infrastructure merely because it already exists. Prefer maintained standards or OSS when equivalent behavior can be proven without weakening the core invariants.

Candidates include:

- MCP Authorization / OAuth / OIDC;
- generic delegated-authorization protocols or capability-token systems;
- generic physical-device discovery/registry/fabric layers;
- TLS and certificate lifecycle;
- workload identity such as SPIFFE when scale justifies it;
- OpenTelemetry/OTLP;
- OS service supervision;
- generic policy engines;
- generic fleet-routing components.

A replacement is acceptable only after regression evidence shows that the CUMG execution-safety invariant is preserved or improved.

### Reuse externally

CUMG should be willing to consume or integrate with maintained OSS rather than reimplement overlapping surfaces.

Important projects/categories to monitor include:

- **SINT Protocol** — capability tokens, physical-AI governance, action identity/replay/evidence and edge authority;
- **Arm Device Connect** — vendor-neutral physical-device discovery, identity, registry, ACL, multi-tenant state and agent/device connectivity;
- **OpenClaw** — agent runtime, paired nodes and Computer Use execution;
- **OAHL** — hardware capability abstraction and device reservation;
- **QuickDesk** — remote Computer Use and multi-device/fleet UX;
- **Obot** — identity, MCP governance, workstation enrollment and audit;
- **Grantex / Open Agent Auth-class systems** — delegated authorization.

Integration is preferred whenever the external component can remain outside the CUMG uncertainty-aware execution core.

## Competitive boundary as of 2026-08-12

| Project/category | Strong overlap | Boundary CUMG should retain |
| --- | --- | --- |
| SINT Protocol | capability tokens, physical execution governance, action claims, replay defense, revocation, edge enforcement, terminal evidence | interactive-desktop ambiguous side-effect state, persistent quarantine, and explicit safe reuse resolution |
| Arm Device Connect | vendor-neutral device fabric, identity, registry, ACL, distributed state, multi-tenant agent/device invocation | desktop operation ownership and uncertainty state machine rather than generic fleet/device connectivity |
| OpenClaw | paired nodes, multi-node control, Computer Use, command/capability policy, cancellation | external-principal binding plus conservative ambiguous desktop-operation recovery |
| OAHL | hardware capabilities, device policy, exclusive reservation | restart/reconnect-safe ownership, stale-result fencing, and ambiguous-execution quarantine semantics |
| QuickDesk | remote Computer Use, MCP, multi-device/fleet | execution safety rather than remote-desktop transport/UX |
| Obot | identity, MCP governance, device enrollment and audit | physical desktop operation ownership and side-effect ambiguity handling |
| delegated-auth protocols | scopes, expiry, revocation, agent identity | binding authorized intent into the desktop operation state machine |

The project must assume these neighbors will improve. Differentiation should therefore be defended by executable invariants and tests, not category wording.

## Core-first implementation priority

Future implementation order is deliberately **core-first**.

### Priority 1 — harden the operation state machine

- make operation ownership transitions explicit and exhaustively tested;
- define which evidence is sufficient for terminal `completed`, `failed`, or proven-cancelled outcomes;
- make every uncertain transition converge to durable `indeterminate` rather than a transport-shaped error;
- reject late/stale results after ownership generation changes.

### Priority 2 — make quarantine and resolution first-class

- persist quarantine independently of connection/session lifetime;
- expose an explicit, auditable resolution path;
- require the resolver to identify the ambiguous operation being resolved;
- ensure resolution cannot accidentally authorize replay of the old operation;
- test restart/crash during both quarantine and resolution.

### Priority 3 — prove ownership under reconnect/restart/concurrency

- competing principals cannot steal or inherit an in-flight/ambiguous operation;
- reconnect does not create a new owner for old work;
- Hub/Agent restart preserves the necessary fencing and ambiguity state;
- stale Agent generations cannot finalize old operations.

### Priority 4 — multi-device proof, not fleet product work

Only after the state machine above is strong, prove that:

1. Device A can remain quarantined after an ambiguous action.
2. Device B remains independently usable by another authorized principal.
3. A second principal cannot acquire Device A until explicit resolution.
4. Hub restart preserves both devices' independent states.
5. reconnect/failover never replays Device A's ambiguous action.

Do not prioritize fleet UX, broad discovery, dashboards, or orchestration before these invariants pass.

### Priority 5 — backend portability proof

Prove the core is not accidentally Cua-specific by integrating at least one second execution backend or a deterministic reference backend with materially different cancellation/result behavior.

The backend must adapt to the CUMG state machine; the CUMG state machine must not be weakened to fit the backend.

## Decision rule for future subsystems

Before implementing a new subsystem, ask in this order:

1. Does this directly strengthen desktop operation ownership, ambiguity handling, quarantine, explicit resolution, or no-replay safety?
2. If yes, it is core-priority work.
3. If no, is the concern already owned by a maintained standard, platform, or OSS?
4. If yes, integrate or replace rather than building a parallel implementation.
5. If no external solution fits, document the exact execution-safety property that requires custom semantics and keep that custom surface narrow, backend-neutral, and transport-neutral.

## GO / NO-GO rule

**GO:** improve and prove uncertainty-aware execution safety for delegated control of interactive desktops.

**NO-GO by default:** build a general agent authorization protocol, general physical-device fabric, generic fleet manager, remote-desktop product, or multi-machine router merely because those features are technically possible.

If another maintained OSS later provides equivalent per-desktop operation ownership, fencing, durable `indeterminate` quarantine, explicit resolution, and no-auto-replay semantics, reevaluate integration or retirement instead of defending sunk cost.
