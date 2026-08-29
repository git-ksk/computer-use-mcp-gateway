# V2 Authorization and Capability Boundary Review

Status: reviewed design decision from #219 (2026-08-29).

This document records the CUMG authorization/capability review that used the current Cua Driver as an adjacent reference. It is not a Cua compatibility plan and does not change the product boundary in `V2_POSITIONING.md` or `V2_EXECUTION_ENVIRONMENT_BOUNDARY.md`.

## Decision summary

CUMG keeps its execution-safety control-plane boundary:

> A specific stateful interactive computer is delegated between authenticated Agent and Human authority without losing operation ownership, ambiguity state, recovery authority, or handoff continuity.

CUMG does not become an AI-PC, sandbox, fleet, remote-desktop, or generic policy-engine product.

The review produced two implementation follow-ups only:

- #221: typed backend-neutral semantic constraints at the existing authorization boundary;
- #222: a second real computer-use backend as GUI portability evidence.

Multi-principal caller identity remains #139. Windows/Linux local-user recovery remains #217. Hosted Hub work remains #215.

## Reference baseline

The review used Cua Driver documentation and source current on 2026-08-29, including `trycua/cua@63c700d78aec868e7151c8d982263a4f7f146ade`.

Relevant Cua properties at that baseline:

- every public SDK/MCP/daemon path reaches one native authorization coordinator before platform dispatch;
- configured managed and user policies are narrow-only and both must pass;
- configured invalid policy state fails closed before the action endpoint binds;
- policy snapshots are immutable for the runtime lifetime;
- policies can constrain selected tool arguments;
- caller identity/authentication is explicitly outside the policy engine;
- optional host authorization is a trusted embedding boundary, not an Agent tool;
- Computer History is opt-in, encrypted, local, metadata-bounded, and excludes raw screenshots, typed text, clipboard data, raw arguments/results, paths, window titles, and URLs.

These are reference properties, not CUMG API requirements.

## Current CUMG boundary

CUMG already separates authorities that must not be collapsed into one policy engine:

1. **Northbound caller identity** — `AuthenticatedClientPrincipal { issuer, subject }` from a verified authentication adapter.
2. **Exact semantic authorization** — `DeviceCapabilityAuthorizer` decides principal -> stable device -> exact `DeviceCapability`.
3. **Grant-signing ceiling** — the packaged external signer independently limits stable device, exact capability, TTL, and clock skew.
4. **Agent/device identity** — the Agent proves the enrolled device key and current authenticated generation.
5. **Hub transport identity** — independently pinned from the Agent device identity and grant key.
6. **Execution ownership/state** — CUMG owns operation identity, owner, dispatch fence, durable terminal/ambiguous state, quarantine, and no-auto-replay.
7. **Human Handoff authority** — `HandoffCoordinator` composes CUMG admission with the canonical `mcp-execution-handoff` authority FSM; CUMG does not duplicate that FSM.
8. **Local-user recovery authority** — a separate user-presence verifier may resolve the exact quarantine; Agent/device identity alone cannot do so.
9. **Execution provider/backend** — Cua or another provider is below the semantic adapter and does not own settlement authority.

The ordinary MCP execution path performs exact capability authorization before command dispatch. Tool discovery is only a filtered view and is never relied on as authorization. Handoff admission is an additional authority gate, not a replacement for exact capability authorization.

## Adopt / adapt / reject

| Reference concept | Decision | CUMG disposition |
| --- | --- | --- |
| One logical authorization point before provider dispatch | **Adopt** | Keep exact CUMG authorization mandatory for ordinary northbound execution. A later typed semantic constraint decision may narrow it but cannot bypass it. |
| Fail closed on configured missing/invalid authorization state | **Adopt** | A deployment that declares a constraint/authority source must not silently continue when it is invalid or unavailable. |
| Immutable authorization snapshot for one runtime authority generation | **Adopt** | Authority widening must require a reviewed restart/revision/generation transition; no Agent-facing hot-widen path. |
| Agent cannot widen administrator authority | **Adopt** | Caller, Agent, provider, or narrower session policy may only intersect with an operator ceiling. |
| Per-capability argument constraints | **Adapt** | Add only typed backend-neutral semantic ceilings with real threat/operational value (#221). Do not accept arbitrary provider/tool argument policy as the permanent contract. |
| Managed/admin plus user/session policy layering | **Adapt** | Model only narrow-only composition. CUMG does not need a generic policy-language stack to obtain this property. |
| Policy decision audit | **Adapt** | Emit stable, privacy-bounded decision/reason/snapshot metadata. Never make raw arguments, text, URLs, screenshots, credentials, or policy contents normal audit. |
| Trusted host residual authorization callback | **Reject as a core CUMG FSM** | CUMG already has distinct Handoff and local-user recovery authorities. A future embedding hook may be admitted only for a concrete boundary and must not create a second Human handoff/consent state machine. |
| `standard` / `bounded` / `unrestricted` product modes | **Reject** | CUMG keeps explicit exact capability authorization and fail-closed deployment configuration. There is no northbound unrestricted mode. |
| YAML/Rego/OPA policy feature parity | **Reject** | Generic policy language is replaceable infrastructure, not CUMG differentiation. External engines may implement the authorizer seam if they preserve CUMG semantics. |
| Computer History as a general activity product | **Reject** | CUMG retains only evidence needed for execution safety, recovery, and bounded operations. The privacy-minimizing metadata posture is worth preserving. |
| Fleet/sandbox/VM provisioning | **Reject** | Execution environments remain downstream infrastructure. CUMG does not schedule or provision generic disposable compute. |
| Separate provider/backend provenance | **Adapt** | Keep provider identity distinct from caller, Human, and device authority. #222 may record bounded provenance for portability evidence without exposing provider IDs northbound. |
| A second real computer-use backend | **Adapt / required evidence** | The deterministic reference executor proves core state-machine replaceability, but not real GUI semantic neutrality. #222 owns a small real-backend portability proof. |

## Typed semantic constraints: smallest useful model

Issue #221 owns implementation. This review deliberately does not prescribe a policy DSL.

The authorization sequence should remain conceptually:

```text
verified caller principal
        |
exact principal/device/DeviceCapability authorization
        |
optional typed semantic constraint intersection
        |
Handoff/session authority admission where applicable
        |
durable CUMG operation admission + dispatch fence
        |
Agent revalidation / independent grant ceiling
        |
backend adapter / execution provider
```

A semantic constraint is valid only when its meaning survives backend replacement. Initial candidates include a smaller text-input byte ceiling, a reviewed normalized application identity set, and a reviewed browser origin/scheme set. Existing process/filesystem/root limits remain dedicated security controls; an argument matcher must not be represented as an OS sandbox.

Constraint denial before provider dispatch is a definite refusal, not `Indeterminate`. After provider dispatch, the existing ambiguity model is unchanged: timeout, reconnect, cancellation, malformed completion, or a generic provider error is never proof of non-execution unless the backend supplies evidence CUMG explicitly recognizes as authoritative.

## Handoff and recovery composition

Authorization answers whether an Agent principal may request a semantic capability. It does not answer who currently owns the interactive surface.

When Human Handoff is active:

- exact principal/device/capability authorization remains necessary;
- `HandoffCoordinator` may suspend Agent authority despite an otherwise allowed capability;
- the canonical Handoff runtime owns Agent/Human authority epochs, `Done -> verifying`, and resume policy;
- CUMG owns the semantic postcondition/evidence requirement needed for safe handback;
- a transport reconnect cannot manufacture Agent authority or settle an ambiguous operation.

Local-user quarantine recovery remains a separate authority. A policy allow decision cannot clear quarantine, rewrite historical truth, or authorize replay.

## Identity separation

Do not conflate these identities:

- northbound authenticated Agent/service principal;
- Human Handoff authority/epoch;
- stable device identity and current Agent generation;
- Hub transport identity;
- grant-signing authority;
- local-user recovery verifier;
- downstream adapter/provider provenance.

Issue #139 expands only the first item. Issue #222 may add bounded provider provenance. Neither changes the authority of the others.

## Second-backend portability evidence

Issue #222 is required before claiming that real GUI/computer-use semantics are demonstrated across heterogeneous backends. A compile-time interface or deterministic fake is not enough for that claim.

Valid evidence requires a materially different real backend with a small overlapping semantic slice, including observation, effectful input, stale-generation/revision rejection, and a deliberately ambiguous post-dispatch failure. The latter must still become durable `Indeterminate` plus quarantine, survive reconnect, and never auto-replay.

The provider may run on a physical endpoint, an operator-managed VM, or a managed cloud desktop. Provider provisioning/fleet lifecycle remains outside the CUMG core.

## Audit boundary

Normal authorization/execution evidence may contain fixed categories and bounded identifiers needed for safety, such as capability, decision/reason category, operation identity, generation/revision, and a policy/constraint snapshot version or digest.

Normal evidence must not add raw:

- screenshots/video/audio;
- typed text/keystrokes or clipboard content;
- URLs or arbitrary command arguments/results;
- filesystem paths solely for policy diagnostics;
- credentials/tokens;
- provider-private response payloads or opaque IDs;
- policy source contents.

This is consistent with the existing V2 threat model: observability may help operators understand a denial or recovery state, but it must not become a second content-retention product.

## Product boundary reaffirmed

This review does not admit:

- VM provisioning or KubeVirt/Kubernetes orchestration;
- generic sandbox/fleet scheduling;
- a generic device registry/fabric;
- a remote-desktop product;
- a hosted account/dashboard SaaS;
- Cua API/tool/identifier compatibility as a northbound goal;
- a second Handoff FSM/WebRTC/TURN implementation;
- policy-engine feature count as product differentiation;
- any weakening of `Indeterminate`, quarantine, reconciliation, or no-auto-replay.

Cua Cloud Fleets, E2B, Daytona, or another provider may be used as downstream execution infrastructure only when a compatible provider/adapter boundary can preserve these semantics.

## References

- Cua permission policies: <https://cua.ai/docs/reference/cua-driver/permission-policies>
- Cua policy enforcement/trust model: <https://cua.ai/docs/concepts/how-permission-policies-work>
- Cua Computer History: <https://cua.ai/docs/how-to-guides/driver/use-computer-history>
- CUMG positioning: [`V2_POSITIONING.md`](V2_POSITIONING.md)
- Execution-environment boundary: [`V2_EXECUTION_ENVIRONMENT_BOUNDARY.md`](V2_EXECUTION_ENVIRONMENT_BOUNDARY.md)
- Threat model: [`V2_THREAT_MODEL.md`](V2_THREAT_MODEL.md)
