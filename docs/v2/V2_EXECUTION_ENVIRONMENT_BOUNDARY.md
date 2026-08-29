# V2 execution-environment boundary

> English is the canonical documentation. [Japanese translation](V2_EXECUTION_ENVIRONMENT_BOUNDARY.ja.md)

Status: **active product-boundary clarification (2026-08-29)**.

This document clarifies how CUMG relates to managed agent computers, cloud sandboxes, and execution-environment providers. It does not replace [`V2_POSITIONING.md`](V2_POSITIONING.md); it makes one existing boundary explicit after reviewing the current Cua Driver + Cloud Fleets direction in #219.

## Decision

CUMG is **not an execution-environment provisioning product**.

CUMG should optimize for delegated control of a **specific, stateful interactive computer or desktop session** where execution authority, side effects, Human intervention, local credentials, and recovery state may need to survive transport loss, process restart, or a change of actor.

The execution environment may be:

- an existing physical macOS, Windows, or Linux computer;
- a remote workstation;
- a VM supplied by an operator;
- a managed cloud desktop supplied by Cua or another provider;
- a future native or hosted backend that can satisfy the CUMG evidence contract.

Whether a computer is physical or virtual, local or hosted, is not the product distinction. The distinction is that CUMG owns uncertainty-aware authority and recovery semantics for a **particular stateful desktop**, while provisioning and replacing compute remain external concerns.

## The CUMG-owned layer

```text
agent / external principal
          |
          v
        CUMG
  authorization + exact capability
  operation identity + ownership
  generation/capability fencing
  no-auto-replay
  Indeterminate + quarantine
  reconciliation / recovery
  Human authority transition
          |
          v
execution-provider / backend seam
     /          |           \
 physical    native       managed
 endpoint    backend      cloud desktop
```

CUMG should keep strengthening the following project-owned semantics:

1. **Operation identity and ownership.** A state-changing desktop operation has an explicit identity and authoritative owner.
2. **Ambiguous-side-effect handling.** Lost responses, cancellation, timeout, disconnect, or restart do not imply non-execution.
3. **Durable quarantine and no replay.** Unprovable effectful work remains `Indeterminate` and blocks unsafe reuse until an explicit reviewed recovery path resolves authority.
4. **Human Handoff and handback.** Human authority is a first-class transition that can fence Agent execution and return control only after the relevant verification boundary.
5. **Local-user recovery.** A real endpoint may require a separately rooted local-user authority to resolve an ambiguous state without turning Agent/device identity into recovery authority.
6. **Backend-neutral semantic capabilities.** Provider-specific identifiers and APIs terminate below the CUMG semantic surface.
7. **Privacy-bounded evidence.** Recovery and audit should retain enough evidence to protect execution safety without making raw desktop, command, credential, or typed-content retention the default.

These properties matter most when the computer/session itself has durable value: existing login state, local applications, device-bound credentials, user-presence mechanisms, OS permissions, or an interactive state that cannot safely be replaced.

## Adjacent layers to reuse

Managed agent-computer providers and sandbox systems may own capabilities such as:

- VM or sandbox provisioning;
- image distribution;
- warm pools and fleet scheduling;
- execution-environment replacement;
- provider-specific desktop drivers;
- infrastructure-level isolation and tenancy;
- generic policy languages or provider-specific audit products.

Those are valid downstream or surrounding layers. CUMG should integrate with maintained infrastructure where it can preserve the CUMG execution-safety contract rather than rebuilding those layers for parity.

A provider becoming more capable is therefore not, by itself, a reason to expand CUMG into that provider's product category.

## Explicit non-goals

Without a separate evidence-backed product-boundary review, CUMG does not build or claim differentiation through:

- VM provisioning;
- Kubernetes or KubeVirt orchestration;
- generic sandbox/fleet scheduling;
- generic device discovery/registry/fabric;
- a remote-desktop product;
- a hosted account/dashboard product;
- a provider-specific policy engine cloned for feature parity;
- disposable compute as a product surface.

Hosted deployment of the **Hub** is a separate concern. For example, #215 may establish a supported Cloud Run Hub profile without moving Agent/desktop execution into Cloud Run or turning CUMG into a Fleet product.

## Provider seam

Execution providers remain replaceable below the CUMG core.

A provider is compatible only if its adapter can preserve or conservatively map the evidence required by the CUMG operation state machine. In particular:

- provider reconnect must not silently replay an old effectful CUMG operation;
- a provider result from a stale generation must not finalize current authority;
- cancellation support must not be interpreted as proof of non-execution unless the provider can actually prove it;
- provider-specific session or object identifiers must not become stable northbound authority;
- provider failure may force a conservative `Indeterminate` outcome rather than weakening the CUMG contract.

This allows a physical endpoint, a native backend, or a managed cloud desktop to sit below the same CUMG execution-safety layer when the evidence semantics are sufficient.

## Competitive and design implication

CUMG should not compete on the claim that it has "a policy engine" or "an agent computer". Those categories have substantial adjacent implementations.

The project-specific value remains the integration of:

> **uncertainty-aware operation ownership + durable no-replay recovery + Human authority transitions for stateful interactive desktops**

A second materially different computer-use backend may be useful to prove that this boundary is real rather than accidentally Cua-specific. #219 owns the review that decides the minimum portability proof before such implementation is split into a follow-up issue.

## Roadmap rule

When evaluating a new subsystem, ask:

1. Does it strengthen operation ownership, ambiguity handling, quarantine, explicit recovery, Human authority transition, local-user recovery, or backend-neutral evidence for a specific stateful desktop?
2. If yes, it may belong in the CUMG core or a narrow project-owned adapter.
3. If no, is it execution-environment provisioning, fleet/device fabric, remote-desktop transport, generic identity/policy, or another maintained infrastructure concern?
4. If yes, keep it replaceable/external and integrate rather than cloning it.
5. Revise this boundary only through an explicit product review with implementation and acceptance evidence.

## Related

- [`V2_POSITIONING.md`](V2_POSITIONING.md) — canonical V2 product positioning and execution-safety boundary.
- [`../ARCHITECTURE.md`](../ARCHITECTURE.md) — Hub/Agent and backend/provider seams.
- [`V2_THREAT_MODEL.md`](V2_THREAT_MODEL.md) — security claims and compromise boundaries.
- [#213](https://github.com/git-ksk/computer-use-mcp-gateway/issues/213) — Product Readiness umbrella.
- [#215](https://github.com/git-ksk/computer-use-mcp-gateway/issues/215) — hosted Hub evaluation.
- [#217](https://github.com/git-ksk/computer-use-mcp-gateway/issues/217) — Windows/Linux local-user recovery parity.
- [#219](https://github.com/git-ksk/computer-use-mcp-gateway/issues/219) — Cua-informed authorization and product-boundary review.
