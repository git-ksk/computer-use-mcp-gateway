# V2 standard-first boundaries

Status: **accepted design direction, narrowed after final competitor review (2026-08-12)**.

This document records the long-term boundary between standards/OSS the project should adopt and custom semantics that remain part of the CUMG core.

Canonical product positioning is in [`V2_POSITIONING.md`](V2_POSITIONING.md).

## Decision

V2 follows a **standard-first, uncertainty-core-only** rule.

Do not build custom infrastructure merely because the current implementation can. Prefer maintained standards or OSS for authentication, delegated authorization, device fabric/registry, transport, certificate lifecycle, observability, service management, and generic policy/fleet concerns.

Keep project-specific semantics only where they are required to preserve safe ownership of state-changing operations on an interactive desktop whose real-world side effects may become uncertain.

The target layering is:

```text
MCP / OAuth / OIDC / IAM / delegated-auth / external policy
                         |
                  principal adapter
                         |
                  +-------------+
                  |  CUMG CORE  |
                  | operation ID|
                  | ownership   |
                  | fencing/gen |
                  | replay      |
                  | indeterminate
                  | quarantine  |
                  | resolution  |
                  +------+------+ 
                         |
                   backend adapter
                    /      |      \
                  Cua    native   other
```

## What standards and OSS should own

### Northbound authentication and delegated authorization

Use MCP Authorization/OAuth, OIDC, IAM-like systems, or maintained delegated-authorization/capability systems rather than inventing another general public authorization protocol.

The Hub reduces validated identity/authority to the local execution question:

```text
principal -> stable desktop -> exact DeviceCapability
```

Northbound credentials are **not** Agent credentials and must never be forwarded southbound as a substitute for a device-scoped execution grant.

Generic scope/expiry/revocation/delegation machinery is replaceable. What must survive replacement is the binding from authorized intent to the CUMG operation-ownership state machine.

### Device fabric, registry, and fleet concerns

Do not treat device discovery, registry, multi-tenant fleet state, generic reservation, or agent/device messaging as proprietary value.

Maintained systems such as Arm Device Connect or future equivalents may provide these layers if they preserve CUMG fencing, operation identity, quarantine, and replay invariants.

A fleet layer may route an operation to a desktop; it must not become authoritative for whether an ambiguous prior operation is safe to forget or reuse.

### Hub-Agent transport

Keep application semantics transport-neutral. gRPC bidirectional streaming over TLS remains the current production transport candidate, and the raw TLS + signed-JSON implementation remains useful as a regression/reference transport.

Use standard gRPC status where it represents transport/auth/resource failures. Do not map an uncertain physical side effect to an ordinary transport status and then lose the durable operation state.

### TLS and workload identity

Prefer ACME or other standard certificate automation for ordinary TLS lifecycle.

Keep workload/device identity replaceable behind interfaces. Provisioned Ed25519 remains acceptable where already proven, while SPIFFE/X.509, KMS/HSM-backed signing, or other reviewed identity implementations may replace credential plumbing later.

Credential replacement must not rewrite the operation-ownership state machine.

### Observability

Prefer OpenTelemetry/OTLP. Use `cumg.*` attributes only for genuinely project-specific execution-safety concepts such as operation state, ownership generation, quarantine, or resolution.

Never include raw commands, argv, screenshots, clipboard contents, file contents, credentials, or other sensitive operation payloads in default telemetry.

### Service management

Use operating-system service managers such as launchd and systemd. Do not build a custom process supervisor as product functionality.

### Generic policy engines

External policy engines may answer whether a principal is allowed to request a capability. They do not replace CUMG's physical operation ownership, ambiguous-outcome handling, or quarantine state.

## Custom semantics to keep

The project-owned core is now intentionally narrow:

- explicit operation IDs for state-changing desktop work;
- exclusive per-desktop operation ownership;
- lease/fencing/generation semantics required to preserve that ownership;
- stale-result rejection after generation or ownership changes;
- replay barriers for state-changing work;
- restart-safe ambiguous in-flight state;
- durable `indeterminate` operation state;
- device quarantine tied to the ambiguous operation;
- explicit, auditable resolution before reuse;
- no automatic replay of ambiguous state-changing work;
- cancellation semantics that distinguish a cancellation request from proven non-execution or proven termination;
- backend-neutral evidence sufficient to classify an operation as terminal or ambiguous;
- privacy-preserving policy/operation/outcome evidence.

Exact capability grants remain part of the execution boundary, but generic delegated-authorization machinery around them is not assumed to be proprietary and may be replaced by maintained standards/OSS.

## Why broader control-plane claims are no longer sufficient

The final competitor review found substantial overlap beyond the original survey:

- **SINT Protocol** covers vendor-neutral physical-AI governance, capability tokens, action identity/claims, revocation, replay defense, edge enforcement, evidence, and fail-closed authority semantics.
- **Arm Device Connect** covers vendor-neutral physical-device discovery, registry, cryptographic identity, ACLs, distributed state, multi-tenant operation, and agent/device invocation.
- OpenClaw, OAHL, QuickDesk, Obot, Grantex/Open Agent Auth-class systems, and related projects cover additional Computer Use, reservation, governance, and delegated-authorization surfaces.

Therefore “vendor-neutral device control plane”, “scoped grants”, “device leases”, “physical-device governance”, “multi-machine”, or “MCP for remote devices” are not sufficient differentiation claims.

The durable custom boundary is the **interactive-desktop uncertainty state machine**: preserve who owns an operation and fail closed when its side effects cannot be proven.

## Real desktop cancellation

Transport-level cancellation can use standard MCP/gRPC mechanisms, but real desktop cancellation acceptance remains execution-backend specific.

A cancellation request is not proof that a click, drag, keystroke, process, or other state-changing desktop effect did not execute.

If the backend cannot prove non-execution or clean termination:

```text
operation -> indeterminate -> device quarantine -> explicit resolution
```

Do not convert that uncertainty into success, ordinary failure, or retryable transport error.

V2-M1 already demonstrated this with real Cua Driver cancellation and Hub-side quarantine. Future work should strengthen and generalize that invariant rather than dilute it behind broader fleet/control-plane features.

## Migration policy

Classify existing implementation into four buckets:

1. **Keep** — uncertainty-aware desktop operation ownership and recovery semantics.
2. **Adapt** — useful current implementations behind replaceable interfaces.
3. **Retire/replace** — custom infrastructure superseded by a maintained standard or OSS after equivalent safety is proven.
4. **Reuse externally** — consume adjacent OSS behind adapters instead of rebuilding its product surface.

Do not rewrite for architecture fashion. Any replacement must preserve or improve the existing security property and retain regression evidence.

## Core-first implementation order

After V2-M1, prioritize work in this order:

1. harden operation-state transitions and stale-result fencing;
2. make quarantine and explicit resolution first-class, durable, and crash-safe;
3. prove ownership across reconnect, Hub restart, Agent restart, and competing principals;
4. prove the same invariants across multiple independent desktops;
5. prove backend portability with a second execution backend or deterministic reference backend;
6. only then expand fleet UX, broad discovery, routing convenience, dashboards, or orchestration.

A machine registry is not an M2 success condition by itself.

## What must not be collapsed together

Do not collapse these boundaries merely to reduce code:

- northbound OAuth/delegated authorization and Agent execution credentials;
- TLS identity and operation ownership;
- gRPC/MCP cancellation and proof that a real desktop effect stopped;
- device liveness and permission to reuse a quarantined desktop;
- generic fleet reservation and ownership of an ambiguous desktop operation;
- observability identifiers and raw operation payload logging.

In particular, **never infer that a new connection, heartbeat, backend process, or device registry lease makes an old ambiguous desktop operation safe**.

## Review rule

When adding a new V2 subsystem, ask in this order:

1. Does it directly strengthen operation ownership, fencing, ambiguous-outcome handling, quarantine, explicit resolution, or no-replay safety?
2. If yes, treat it as core-priority work.
3. If no, is there a maintained standard/platform/OSS that already owns the concern?
4. If yes, integrate or replace rather than create a parallel custom implementation.
5. If no, document the exact execution-safety property that requires custom semantics and keep the custom surface narrow, backend-neutral, and transport-neutral.

If another maintained OSS later provides equivalent per-desktop operation ownership, durable `indeterminate` quarantine, explicit resolution, stale-result fencing, and no-auto-replay semantics, reevaluate integration or retirement instead of defending sunk cost.
