# Roadmap

> English is the canonical documentation. [日本語版 / Japanese translation](ROADMAP.ja.md)

Status as of 2026-08-21: **V1 closed, V2 execution-safety baseline complete, current released version `v0.2.0`.**

This roadmap describes current maintenance priorities, admission rules for future public-contract work, and the path toward a stable 1.x contract. It is not a promise that every candidate feature will ship, and release numbers are not assigned merely because a roadmap section exists.

Version selection follows [`VERSIONING.md`](VERSIONING.md). Project/change governance follows [`PROJECT_GOVERNANCE.md`](PROJECT_GOVERNANCE.md). The canonical V2 product boundary remains [`v2/V2_POSITIONING.md`](v2/V2_POSITIONING.md).

## Product boundary

CUMG's project-specific core is:

> **uncertainty-aware execution safety for delegated control of stateful interactive desktops**

The invariant that future work must preserve is:

```text
specific authenticated principal
        |
specific desktop + exact capability
        |
operation ID + exclusive ownership + generation/capability fencing
        |
state-changing action dispatched
        |
completion provable?
   yes -> terminal
   no  -> indeterminate -> durable quarantine -> explicit resolution
```

An ambiguous state-changing operation is never automatically retried or replayed because a client, Hub, Agent, transport, backend, or device reconnects.

The completed V1/V2 implementation history and acceptance evidence remain available through [`V1_ACCEPTANCE.md`](V1_ACCEPTANCE.md), [`v2/STATUS.md`](v2/STATUS.md), [`v2/acceptance/`](v2/acceptance/), and [`archive/`](archive/). This file intentionally focuses on work that is still relevant after the V2 closeout.

## Current maintenance line: `0.2.x`

`0.2.x` preserves the released V2 public direction. Work that remains compatible with that contract should stay on the patch line rather than inventing a new milestone number.

Current priorities:

- preserve the authoritative operation/quarantine/resolution/no-auto-replay state machine;
- keep control schema v8 and capability-advertisement schema v5 behavior explicit; v5 is the reviewed change that adds the signed payload-free reconciliation-report boundary and mixed versions fail closed;
- keep Cua Driver upgrades as reviewed compatibility changes with pinned/repeatable evidence;
- maintain security, dependency, documentation, packaging, CI, conformance, soak, and resource-regression quality;
- investigate and close or explicitly document the remaining V1 compatibility/quality issues:
  - issue #14 — read-only `get_screen_size` session/escalation semantics;
  - issue #15 — inconsistent Cua Driver application/process discovery identity;
  - issue #20 — portable behavior for the V1 idle resource quality gate without weakening Linux enforcement;
- fix compatible runtime/security/reliability defects as PATCH candidates;
- keep docs-only/editorial work version-neutral unless an immutable corrected release snapshot is operationally necessary.

A compatible fix merged after `v0.2.0` may contribute to a future `0.2.1`; the roadmap does not require a release merely because maintenance commits exist.

## Next minor: admission-driven, not number-driven

The next minor release is created only when accepted work changes or meaningfully expands the public contract, or when a deliberate pre-1.0 incompatibility is justified. Today that would normally be `0.3.0`, but the roadmap does not pre-commit a feature bundle to that number.

Candidate areas are evaluated independently.

### `0.3.0` candidate: V2 Production Hardening

The current candidate theme for `0.3.0` is **V2 Production Hardening / Operational Readiness**: close the reliability, recoverability, observability, local-abuse, and trust-lifecycle gaps found during sustained V2 operation without weakening the authoritative operation/quarantine/no-auto-replay safety model.

The target issue set is `#64` through `#73`:

- recovery and restart safety: `#64` production quarantine resolution, `#65` SIGTERM plus bounded operation drain, `#72` operator-visible quarantine alerting;
- persistence and incident closure: `#69` bounded in-generation checkpoint growth, `#73` persistence crash-loop root-cause confirmation or evidence-backed exclusion;
- audit and local caller protection: `#70` northbound client correlation, `#71` loopback caller rate limiting/trust gate;
- trust lifecycle: `#68` bounded Agent session lifetime and device-key rotation procedure, `#67` repeatable enrollment/trust-anchor lifecycle, `#66` grant-signing isolation/external signer boundary.

Implementation remains **issue-driven and PR-isolated**, in this preferred dependency order:

1. `#65` planned-shutdown safety;
2. `#69` and `#73` persistence boundedness and incident root cause;
3. `#64` audited production quarantine resolution;
4. `#72`, `#70`, and `#71` operator visibility, audit correlation, and local abuse resistance;
5. `#68` and `#67` session/device/trust-anchor lifecycle;
6. `#66` signing-authority isolation, after the lifecycle boundary is explicit.

A change that is PATCH-compatible in isolation (for example `#65` or `#69`) may still ship first in `0.3.0` when no intervening `0.2.x` release is operationally necessary; SemVer classification is based on the release as a whole, not on forcing every included fix to require a minor bump. The milestone must not turn into one aggregate implementation PR: each issue retains its own acceptance evidence and review boundary.

`0.3.0` is not accepted merely because every issue is closed. Before release, the resulting public/operator contract must satisfy the minor-release acceptance gate below, and any scope that cannot preserve the documented V2 safety invariants must be deferred rather than weakened to meet the milestone.

### Post-acceptance candidate: first-class Human Handoff coordination

Physical CUMG + `mcp-execution-handoff` acceptance has proven the bounded OS-window lifecycle through Agent -> Human -> verifying -> explicit Agent resume, including direct/TURN transport fallback, fresh exact-window verification, restart/context-expiry/generation-rollover recovery, no automatic replay, and zero residual quarantine. The acceptance-only Unix-socket bridge is therefore no longer the intended long-term runtime architecture.

Issue [#152](https://github.com/git-ksk/computer-use-mcp-gateway/issues/152) tracks the next integration step: make Handoff a first-class internal CUMG coordination capability while retaining one canonical Handoff semantic source of truth. Its OS-window regression gate must pass before using the same coordinator for `mcp-execution-handoff#48` Terminal/PTY dogfood.

Preferred dependency order:

1. `#152` — first-class CUMG HandoffCoordinator and OS-window regression acceptance;
2. `mcp-execution-handoff#48` — one bounded PTY session, Agent/Human-exclusive input, verifying, explicit resume;
3. `mcp-execution-handoff#47` — finish remaining OS/window primitive extraction from Window + Terminal evidence;
4. `mcp-execution-handoff#45` — converge terminology/public API only after the reusable boundary is demonstrated.

This work is issue-driven and is not assigned to the `0.3.0` Production Hardening milestone merely because the Window acceptance landed during that development line. WebRTC video-quality work remains an independent Handoff concern and is not a prerequisite for the coordinator/Terminal sequence.

### Post-`0.3.0` candidate: multi-principal northbound identity

Issue [#139](https://github.com/git-ksk/computer-use-mcp-gateway/issues/139) is deliberately **not** part of the `0.3.0` Production Hardening closeout. It is the next admitted northbound-authentication expansion candidate after that operational-readiness work is closed. The tracking milestone is `Post-v0.3 — Multi-principal Northbound Identity`; the milestone intentionally does not pre-assign a release number.

The target architecture is:

```text
external OAuth/OIDC identity provider
        |
verified signed token (provider boundary)
        |
generic OIDC/JWT adapter
        |
AuthenticatedClientPrincipal { issuer, subject }
        |
DeviceCapabilityAuthorizer
        |
principal -> stable device -> exact DeviceCapability
```

The adapter must remain provider-neutral and fail closed. It verifies signature, issuer, audience, time claims, subject, algorithm policy, and bounded JWKS/metadata rotation before producing the existing CUMG principal. Caller-supplied identity headers and MCP `clientInfo` remain audit metadata only and never become authorization authority.

This work does **not** make CUMG an identity provider, account database, session manager, or token issuer. Existing RFC 7662 introspection and the explicitly single-principal trusted-proxy adapter remain supported deployment choices. A signed-token deployment may remove a fixed-principal local proxy bridge when that bridge exists only to establish identity; a reverse proxy or tunnel may still remain for transport, origin hardening, rate limiting, or defense in depth.

Acceptance requires at least two verified subjects receiving different exact device/capability decisions through the existing `DeviceCapabilityAuthorizer`, with bad signature/issuer/audience/time/key/subject/algorithm cases failing closed and no regression to the existing authentication adapters.

### Remaining semantic parity decisions

The current Cua parity matrix deliberately leaves these legitimate gaps explicit:

- `ClipboardWrite` supports plain text only; image/file clipboard write parity is not implemented;
- `LaunchApplication` does not expose Cua `additional_arguments` or `webkit_inspector_port`.

A gap should be implemented only when there is a concrete workflow need and a bounded backend-neutral contract can be defined without exposing a generic backend passthrough. It is also valid to keep a gap explicitly unsupported.

See [`v2/V2_CUA_PARITY_MATRIX.md`](v2/V2_CUA_PARITY_MATRIX.md).

### Additional backend or native GUI adapter

A second real Computer Use backend or a native GUI adapter is a candidate only if it provides a concrete operational, portability, support, or security benefit.

Any adapter must remain below the same CUMG authority boundary:

- no second operation lifecycle or settlement authority;
- no backend-specific IDs as permanent northbound capability identifiers;
- unsupported or unprovable post-dispatch outcomes remain indeterminate;
- no weakening of principal/device/capability/generation fencing;
- no automatic replay.

Compile-time interface compatibility alone is not acceptance evidence for a backend that can cause real desktop side effects.

### Pluggable external capability providers

CUMG may support optional external capability providers when they add a useful execution surface without turning CUMG itself into an agent or duplicating an upstream implementation. Developer-workspace providers are a representative candidate class. A DevSpace-class provider can supply Codex-like workspace primitives such as project/worktree context, repository instructions, file editing, patching, shell execution, and Git-aware state, while a Serena-class provider can supply semantic code navigation and symbol-aware workspace intelligence. Either may sit behind a reviewed CUMG adapter without becoming part of the CUMG core.

The integration boundary must remain capability-oriented rather than becoming a generic MCP proxy:

- the upstream chat/agent harness remains responsible for planning, tool selection, project reasoning, and multi-step agent loops;
- CUMG must not acquire a second autonomous coding/operations agent loop merely because a provider offers higher-level tools;
- provider tools are mapped to explicit CUMG semantic capabilities with bounded inputs/results and read-only versus state-changing classification;
- provider-specific tool names, opaque authority, and arbitrary passthrough do not become the permanent northbound contract;
- state-changing provider work remains under the same authenticated principal, exact-capability grant, operation ownership, fencing, ambiguity, quarantine, cancellation, and no-auto-replay rules as native capabilities;
- an external provider may be replaced or omitted without redefining the CUMG core product boundary.

This is an extensibility direction, not a commitment to bundle a particular provider. Admission requires a concrete workflow benefit and evidence that the adapter preserves CUMG's execution-safety invariant.

### Higher-risk capability surfaces

Explicit filesystem mutation, richer clipboard data, application launch arguments, or other consequential surfaces may be considered as separate exact capabilities. They are not implicitly inherited from an existing shell, GUI, browser, or backend integration.

Admission requires a reviewed threat boundary, bounded inputs/results, fail-closed authorization, ambiguity handling, tests, and physical acceptance when behavior depends on a real desktop/provider.

### Replaceable infrastructure

Transport, identity, policy, device-fabric, and backend implementations may be replaced or integrated with maintained standards/OSS when doing so provides a concrete benefit and preserves the CUMG safety invariant.

Do not adopt infrastructure merely for architectural fashion. Existing reviewed implementations remain valid until replacement evidence is stronger than the migration cost and risk. See [`v2/V2_STANDARDIZATION.md`](v2/V2_STANDARDIZATION.md).

## Minor-release acceptance gate

Before a future minor release is cut, its public-contract scope must be explicit and all applicable gates must pass:

1. the feature/compatibility boundary is documented before or with implementation;
2. change class from [`PROJECT_GOVERNANCE.md`](PROJECT_GOVERNANCE.md) is identified;
3. security/execution-safety changes include threat-model and targeted regression updates;
4. control/capability schema changes are explicit, fail closed, and include upgrade/mismatch behavior;
5. backend parity/status documentation states implemented, unsupported, and intentionally excluded behavior precisely;
6. English canonical and paired Japanese normative docs are synchronized where semantics change;
7. deterministic CI passes, plus trusted physical acceptance for Class D changes;
8. migration/deprecation notes are present for incompatible pre-1.0 changes;
9. the final release is prepared from merged `main` through the release process in [`VERSIONING.md`](VERSIONING.md).

A successful prototype is not sufficient release evidence when it bypasses these boundaries.

## Path to `1.0.0`

There is no target date or required feature count for `1.0.0`.

The 1.0 decision is a compatibility commitment. Readiness is reached when the criteria in [`VERSIONING.md`](VERSIONING.md) are true in practice, especially:

- the supported northbound semantic surface and execution-safety invariants are explicitly stable;
- control/capability schema upgrade and mismatch behavior is documented;
- supported backend/deployment compatibility is documented and repeatably accepted;
- governance, release, security, support, and deprecation rules have been exercised rather than only written down;
- maintainers are prepared to preserve backward compatibility within 1.x.

Remaining parity gaps do **not** automatically block 1.0. Each gap must instead be classified as supported, intentionally unsupported, deferred, or deprecated so users know the stable boundary.

`0.9.x` is not a countdown. The project may use `0.10.0`, `0.11.0`, and later pre-1.0 minors until the compatibility commitment is justified.

## Explicit non-goals

The following remain NO-GO by default unless the product boundary is deliberately reconsidered with evidence:

- building a new screenshot/input computer-use engine;
- screen streaming or a general remote-desktop product;
- another generic delegated-authorization protocol;
- another generic physical-device fabric/registry when maintained infrastructure already serves the need;
- fleet dashboards, broad discovery, failover, or orchestration merely because multiple machines are technically possible;
- arbitrary backend-tool passthrough or raw backend identifiers as a public API;
- arbitrary browser JavaScript execution as a parity shortcut;
- blanket long-lived device-control credentials;
- automatic replay of ambiguous state-changing work;
- treating reconnect, heartbeat, backend restart, or device liveness as proof that an unresolved operation is safe to forget.

## Re-evaluation rule

Roadmap candidates are reviewed against current standards, maintained OSS, backend capabilities, user workflows, and the accepted CUMG invariants before implementation.

If maintained OSS later provides equivalent or stronger per-desktop operation ownership, fencing, durable indeterminate quarantine, explicit resolution, and no-auto-replay semantics, reevaluate integration or retirement instead of defending sunk cost.
