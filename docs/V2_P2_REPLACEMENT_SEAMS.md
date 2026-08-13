# V2 P2 replacement and integration seams

Status: **accepted on 2026-08-13. P2 adds narrow replacement seams but adopts no new external runtime, authorization protocol, device fabric, workload-identity system, or policy engine.**

Canonical product boundary: [`V2_POSITIONING.md`](V2_POSITIONING.md). Authoritative execution-safety model: [`V2_P0_EXECUTION_SAFETY.md`](V2_P0_EXECUTION_SAFETY.md). Primary tracker: Issue #24.

P2 asks a deliberately narrow question: which parts of CUMG are product-specific execution safety, and which parts should be replaceable when a maintained standard or OSS is clearly better? It does **not** authorize a rewrite or a new generic control plane.

## 1. Boundary inventory

The following remains CUMG-owned and authoritative:

- immutable operation identity from admission through settlement, quarantine, resolution, receipt, and replay tombstone;
- exclusive per-desktop ownership bound to the authenticated principal;
- device/session generation fencing and stale-result rejection;
- durable `indeterminate` state and exact-operation desktop quarantine;
- explicit, persistence-gated, auditable resolution;
- no automatic replay of an ambiguous operation after reconnect, restart, backend replacement, or device liveness recovery;
- evidence requirements for ordinary terminal settlement.

These are not adapter responsibilities. An integration may supply identity, an authorization decision, device connectivity, or execution, but it may not own, fork, mirror, or bypass the P0/P1 state machine.

The replaceable infrastructure boundary is narrower:

| Surface | Current implementation | P2 seam/decision |
| --- | --- | --- |
| northbound token validation | OAuth introspection behind `AccessTokenVerifier` | already replaceable; bearer token remains northbound and is stripped before Hub execution |
| principal -> device -> exact capability authorization | `ClientAuthorizationPolicy` | `DeviceCapabilityAuthorizer` seam added; replacement returns only an exact allow/deny decision |
| Computer Use execution | `CuaMcpAdapter` | `ComputerUseBackendAdapter` seam added; Cua remains the default implementation |
| Hub-Agent carrier | signed application protocol over gRPC/TLS | no replacement work; transport is not the current bottleneck |
| device composition | immutable `FixedMultiDeviceHub` over provisioned `SingleDeviceHub`s | no generic registry/fabric seam added; fixed composition remains the proven boundary |
| workload identity | separately provisioned Hub/device/grant/TLS material | no SPIFFE dependency while deployment remains small and explicitly provisioned |
| policy language/engine | exact in-process tuple policy | no OPA/Cedar dependency while rules remain intentionally small |
| persistence | CUMG checkpoints for authoritative execution state | not delegated; external stores may eventually host bytes but cannot redefine state transitions |

## 2. Adopted seams

### Exact authorization seam

`DeviceCapabilityAuthorizer` receives exactly:

```text
authenticated issuer + subject
stable device ID
exact DeviceCapability
```

and returns allow or deny. The default implementation delegates to the existing `ClientAuthorizationPolicy`. This preserves the existing MCP/OAuth boundary and allows a future Grantex, SINT, Open Agent Auth, OPA, Cedar, or other policy adapter without letting that system create operation IDs, transfer desktop ownership, resolve quarantine, or mint a terminal execution result.

Authorization infrastructure failure must fail closed at the northbound boundary. It must never be interpreted as permission to reuse or settle a quarantined desktop.

### Computer Use backend seam

`ComputerUseBackendAdapter` owns only backend lifecycle, capability advertisement, and execution of the typed Computer Use subset. It returns the existing `BackendExecutionOutcome` contract:

- `Completed(DeviceResult)` only when the backend contract provides the evidence required for an ordinary result;
- cancellation propagation with unknown side effect -> `CancellationPropagatedIndeterminate`;
- timeout with unknown side effect -> `TimedOutIndeterminate`.

The Agent still owns grant consumption, active-operation persistence, generation fencing, cancellation acknowledgement, and mapping unknown execution into Hub quarantine. Replacing Cua therefore does not create a second execution-safety state machine.

## 3. Candidate review

Public project status and interfaces were rechecked against primary project documentation on 2026-08-13.

| Candidate | Layer | Replacement benefit | Coupling / maintenance | Security and invariant fit | P2 decision |
| --- | --- | --- | --- | --- | --- |
| [SINT Protocol](https://github.com/sint-ai/sint-protocol) | delegated capability/policy/evidence | rich capability tokens, delegation, revocation, approval and evidence primitives | high: broad physical-AI governance/control-plane surface and TypeScript reference stack overlap with concerns CUMG does not want to own | can sit before the exact authorizer seam, but its own action/evidence lifecycle must not become the CUMG settlement authority | **defer integration**; keep as an external authorizer candidate |
| [Grantex](https://grantex.dev/) | delegated authorization | closest fit for scoped, time-limited, revocable agent delegation while complementing OAuth/MCP | medium: protocol v1.0 is published, but the project explicitly says the current MCP auth adapter is not ready for general production deployment | clean fit if a verified grant is reduced to the exact CUMG principal/device/capability decision; CUMG still owns operation identity and execution settlement | **preferred future delegated-auth experiment, not a production dependency now** |
| [Open Agent Auth](https://github.com/alibaba/open-agent-auth) | delegated authorization/workload identity/policy/audit | broad identity binding, fine-grained authorization and MCP integration | high: public-beta framework with OAuth/OIDC/WIMSE/VC/policy infrastructure and a larger Java/Spring deployment model | could feed the exact authorizer seam, but request/workload/audit lifecycle must not replace CUMG ownership/quarantine semantics | **defer** |
| [Arm Device Connect](https://github.com/arm/device-connect) | device discovery/registry/fabric | maintained generic discovery, registry, agent tooling and multi-network device connectivity | high for current scope: introduces a generic device mesh/registry and Python/NATS/Zenoh infrastructure; public agent tools are currently beta | useful only below/around a future provisioning layer. Device discovery or reconnect may never acquire, replace, or clear an unresolved CUMG desktop | **do not integrate in P2**; fixed-set composition remains safer and sufficient |
| [OpenClaw](https://docs.openclaw.ai/nodes/computer-use) | Computer Use runtime/node control | uniform Computer Use command surface and paired-node runtime | high: adds another gateway, pairing/node identity, capability policy and runtime. Current Computer Use authorization is durable enablement/pairing rather than CUMG's per-operation uncertainty model | possible future `ComputerUseBackendAdapter`, but only if OpenClaw is treated as an executor. Its node/pairing state may not settle or replay CUMG operations | **keep direct Cua default; no OpenClaw dependency now** |
| [ROSClaw](https://github.com/ros-claw/rosclaw) | physical execution reference/runtime | strong conceptual match for exact action identity, exclusive resource ownership, durable interrupted-action recovery and no-resume semantics | very high if adopted as a runtime; broader robotics/body/runtime scope is outside CUMG | strongest compatibility reference for invariants, but importing/forking it would duplicate the authoritative execution lifecycle | **compatibility/reference only; no fork** |
| [SPIFFE](https://spiffe.io/docs/latest/spiffe-specs/spiffe_workload_api/) | workload identity | stable, interoperable workload identity and short-lived SVID delivery | medium operational cost; useful once dynamic workloads/trust domains justify it | can replace workload credential plumbing without owning desktop operations; reconnect/identity rotation remains only a fence/liveness event | **defer until deployment scale warrants it** |
| [OPA](https://www.openpolicyagent.org/docs) / [Cedar](https://docs.cedarpolicy.com/) | generic policy engine | mature separation of authorization decision logic from application code | unnecessary complexity while CUMG policy is an exact small tuple; introduces policy distribution/schema/operational concerns | both can fit behind `DeviceCapabilityAuthorizer`; unavailable/undefined policy must fail closed | **do not adopt now; reevaluate only when policy complexity materially grows** |

## 4. Why no device-fabric seam was added

A generic registry abstraction would create an attractive but currently unsafe extension point: mutable discovery, replacement routing, failover selection, and fleet state could start influencing which physical desktop receives an operation. P1 is intentionally stronger because device selection is an immutable fixed map and each `SingleDeviceHub` owns independent authoritative state.

Before any future fabric integration, regression evidence must prove:

1. an external registry cannot cause an unresolved Device A operation to route to Device B;
2. a newly discovered/reconnected endpoint cannot inherit Device A ownership or generation;
3. quarantine survives fabric restart/re-registration;
4. stale route/device/capability generations cannot settle work;
5. failover never replays an ambiguous operation;
6. the CUMG checkpoint remains the only authority for whether the desktop is reusable.

Until a concrete integration needs this boundary, adding a registry interface would increase abstraction without increasing safety.

## 5. Why no external authorization dependency was added

The existing MCP OAuth resource-server boundary already reduces a bearer token to an authenticated principal and strips the bearer before Hub/Agent execution. P2 only needed to remove the concrete in-process policy coupling after that point.

The new authorizer seam is deliberately smaller than the candidate protocols. A future adapter can verify whatever external grant or policy system is selected, then answer the exact CUMG authorization question. It cannot pass an external token southbound as ambient authority or substitute an external action ID for the CUMG operation ID.

## 6. Regression contract for any future integration

An integration is acceptable only if all of the following remain true under deterministic tests and, for Computer Use backends, the trusted physical regression:

- one CUMG operation ID per logical desktop operation;
- exclusive ownership remains per stable desktop;
- owner/device/session/generation mismatches fail closed;
- late or duplicate results cannot replace an existing terminal receipt or clear quarantine;
- backend cancellation/timeout/disconnect without sufficient evidence becomes durable `indeterminate`;
- Hub/Agent/backend/fabric restart cannot automatically replay an ambiguous operation;
- explicit resolution targets the exact ambiguous operation and remains auditable/persistence-gated;
- an external authorization or policy outage denies new work rather than bypassing checks;
- no bearer/delegation/workload credential becomes a substitute for the southbound exact one-shot grant;
- external discovery/liveness does not imply ownership or safe reuse.

## 7. P2 acceptance boundary

P2 is complete when the replacement decision is documented, the two narrow seams compile and pass invariant regressions, the existing multi-device/backend portability tests remain green, and the final main code passes the trusted real-Cua regression.

P2 does **not** require adopting an external dependency. In this review, not adopting one is the safer result: none of the evaluated systems provides a clear net improvement to the current fixed-set deployment without importing a broader control plane or weakening the separation between generic infrastructure and the CUMG execution-safety core.
