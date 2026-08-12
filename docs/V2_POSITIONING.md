# V2 product positioning

Status: **accepted direction (2026-08-12)**.

This document defines the product boundary for V2 after V2-M1 acceptance. It is intentionally narrower than “secure remote computer use” or “multi-machine MCP”.

## Positioning

`computer-use-mcp-gateway` V2 is a **vendor-neutral delegated device execution control plane** for stateful physical computers.

It does not aim to be:

- a computer-use engine or screenshot/input implementation;
- an AI-native remote desktop product;
- a generic MCP gateway;
- a generic agent authorization protocol;
- a generic capability broker;
- a fleet product differentiated only by reaching multiple machines.

Those areas already have substantial standards and OSS coverage. CUMG should integrate with them when doing so preserves the project’s execution-safety guarantees.

The durable product boundary is the point where an externally authorized principal is allowed to perform a state-changing operation on a specific physical device whose side effects can become ambiguous.

## Thin-waist architecture

```text
IdP / MCP OAuth / OIDC / IAM / delegated-auth protocol
                    |
            authorization adapter
                    |
          +-------------------+
          |     CUMG CORE     |
          | exact capability  |
          | operation ID      |
          | lease / fencing   |
          | generation        |
          | replay barrier    |
          | indeterminate     |
          | quarantine        |
          | no auto-replay    |
          +---------+---------+
                    |
              backend adapter
             /       |        \
           Cua     native     other
```

The layers above and below the core are replaceable. The middle execution-safety state machine is the product-specific value.

## What CUMG owns

### 1. Authorization translation at the device boundary

The northbound identity system is not the device credential.

A verified principal is reduced to the local authorization question:

```text
principal -> stable device -> exact DeviceCapability
```

The Hub may consume MCP Authorization/OAuth, OIDC, IAM-like systems, or delegated-authorization protocols. Their bearer credentials must not be forwarded to the Agent as a substitute for a device-scoped grant.

CUMG owns the translation from externally authorized intent into an exact device-execution authority.

### 2. Physical operation ownership

A stateful physical desktop is an exclusive resource while an operation is being executed.

CUMG owns:

- explicit operation IDs;
- per-device lease/admission ownership;
- generation/fencing checks;
- serialization of conflicting physical actions;
- restart/reconnect rules that cannot silently transfer ownership.

Multi-machine support is useful only if these semantics remain independent per device.

### 3. Ambiguous side-effect safety

A cancellation request, disconnect, timeout, or lost response does not prove that a click, drag, keystroke, shell command, or other state-changing effect did not execute.

When non-execution or termination cannot be proven, CUMG must retain an `indeterminate` outcome rather than guessing success or failure.

### 4. Fail-closed recovery

For ambiguous state-changing work, CUMG owns:

- replay rejection;
- bounded replay tombstones;
- restart-safe ambiguous in-flight state;
- device quarantine;
- explicit resolution before reuse;
- no automatic replay merely because a client, Hub, Agent, or backend reconnects.

### 5. Backend-neutral enforcement

Cua is an initial GUI/computer-use backend, not the product boundary.

The execution-safety contract must survive a backend change. Native platform adapters, OpenClaw-style execution backends, or other implementations may be integrated later if they conform to the same capability, operation, cancellation, and ambiguity semantics.

## Keep / adapt / retire / reuse

### Keep

Keep custom semantics only where they encode the stateful-device safety properties above:

- exact `DeviceCapability` enforcement;
- operation identity;
- lease/fencing ownership;
- device/capability generation checks;
- replay barriers;
- indeterminate state;
- quarantine and explicit resolution;
- no automatic replay of ambiguous state-changing work;
- privacy-preserving policy/outcome evidence.

### Adapt

Preserve current implementations behind replaceable interfaces where the semantics are useful but the credential, transport, or storage mechanism may change:

- Agent/workload identity providers and verifiers;
- grant issuers and verifiers;
- Hub-Agent transport bindings;
- policy-engine integration;
- persistence/checkpoint stores;
- backend adapters.

### Retire or replace

Do not preserve custom infrastructure merely because it already exists. Prefer maintained standards or OSS when equivalent behavior can be proven without weakening the core invariants.

Candidates include:

- MCP Authorization / OAuth / OIDC;
- TLS and certificate lifecycle;
- workload identity such as SPIFFE when scale justifies it;
- OpenTelemetry/OTLP;
- OS service supervision;
- generic policy engines;
- generic delegated-authorization protocols.

A replacement is acceptable only after regression evidence shows that the existing CUMG safety property is preserved or improved.

### Reuse externally

CUMG may consume or integrate with maintained OSS rather than reimplementing overlapping product surfaces. Examples worth monitoring include OpenClaw, OAHL, QuickDesk, Obot, and delegated-authorization projects such as Grantex/Open Agent Auth-class systems.

Integration is preferred when the external component can remain outside the CUMG execution-safety core.

## Competitive boundary as of 2026-08-12

| Project/category | Strong overlap | Boundary CUMG should retain |
| --- | --- | --- |
| OpenClaw | paired nodes, multi-node control, Computer Use, command/capability policy, cancellation | external-principal exact delegation plus physical operation ownership and ambiguity handling |
| OAHL | hardware capabilities, device policy, exclusive reservation | stronger cryptographic/replay/generation and ambiguous-execution state semantics |
| QuickDesk | remote Computer Use, MCP, multi-device/fleet | authorization translation and execution safety rather than screen transport |
| Obot | identity, MCP governance, device enrollment and audit | stateful physical operation ownership and side-effect ambiguity handling |
| delegated-authorization protocols | scopes, expiry, revocation, agent identity | safe translation from authorized intent into stateful device execution |

These projects may evolve. Their existence is a reason to keep the CUMG-owned surface narrow, not a reason to freeze the current implementation.

## V2-M2 objective

V2-M2 must prove that the M1 execution-safety core survives multiple devices and principals. A registry or router alone is not sufficient.

Minimum acceptance scenarios:

1. Device A becomes `indeterminate` after an ambiguous state-changing operation and is quarantined.
2. Device B remains independently usable by another authorized principal.
3. A second principal cannot steal, inherit, or silently replace Device A’s in-flight or ambiguous lease.
4. Hub restart preserves the ambiguous ownership/quarantine decision.
5. Reconnect or failover never automatically replays an ambiguous operation.
6. Device generation and capability revision prevent stale routing after reconnect, backend change, or policy-surface change.
7. Authorization and backend adapters can be changed without changing the core operation-state machine.

The multi-machine implementation should be designed around these invariants rather than adding fleet features first and retrofitting safety later.

## Decision rule for future subsystems

Before implementing a new subsystem, ask in this order:

1. Is this concern already owned by a maintained protocol, platform standard, or OSS?
2. Can it be integrated without weakening the current execution-safety invariant?
3. If yes, integrate or replace rather than building a parallel custom implementation.
4. If no, identify the exact stateful-device safety property that requires custom semantics.
5. Keep that custom surface narrow, backend-neutral, and transport-neutral.

## Short product statement

> Authorization decides whether an agent may act. CUMG additionally decides who owns the physical operation and refuses to guess when its side effects become uncertain.

A more infrastructure-oriented description is:

> CUMG extends vendor-neutral agent authorization into safe execution ownership for stateful physical computers.
