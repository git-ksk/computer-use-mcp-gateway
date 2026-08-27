# Roadmap

> English is the canonical documentation. [日本語版 / Japanese translation](ROADMAP.ja.md)

Status as of 2026-08-26: **V1 implementation is closed and retained only as a legacy/regression surface; V2 is the recommended runtime; the current released version is `v0.2.0`.**

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
- keep live control schema v9 and capability-advertisement schema v5 behavior explicit; capability schema v5 is the reviewed change that adds the signed payload-free reconciliation-report boundary, and mixed versions fail closed;
- keep Cua Driver upgrades as reviewed compatibility changes with pinned/repeatable evidence;
- maintain security, dependency, documentation, packaging, CI, conformance, soak, and resource-regression quality;
- keep the remaining V1-only compatibility observations (#14 and #15) explicitly blocked on their upstream Cua issues rather than treating them as active CUMG release blockers;
- fix compatible runtime/security/reliability defects as PATCH candidates;
- keep docs-only/editorial work version-neutral unless an immutable corrected release snapshot is operationally necessary.

A compatible fix merged after `v0.2.0` may contribute to a future `0.2.1`; the roadmap does not require a release merely because maintenance commits exist.

### Legacy V1 retirement candidate

`v1_gateway` remains in `main` for regression/reference and any still-existing legacy deployment. It is not a separately maintained `0.1.x` line and does not receive routine backports. V2 Hub + Agent is the recommended runtime.

Retiring V1 is now a valid future simplification candidate, but removal must be deliberate rather than incidental maintenance. Before removal:

- confirm no supported production deployment still depends on `v1_gateway`;
- decide which V1 regression/conformance fixtures remain valuable as backend-contract tests and migrate or archive them intentionally;
- resolve or close V1-only upstream-blocked issues such as #14/#15 as no longer applicable if the surface is retired;
- remove V1 configuration/deployment documentation and compatibility claims coherently;
- classify the removal as a pre-1.0 incompatible public-contract change and ship it only through an appropriate MINOR release with migration/release notes.

Until those conditions are met, keep V1 narrow and regression-only; do not expand it with new capabilities.

## Next minor: admission-driven, not number-driven

The next minor release is created only when accepted work changes or meaningfully expands the public contract, or when a deliberate pre-1.0 incompatibility is justified. Today that would normally be `0.3.0`, but the roadmap does not pre-commit a feature bundle to that number.

Candidate areas are evaluated independently.

### `0.3.0` closeout: V2 Production Hardening

The original `0.3.0` Production Hardening / Operational Readiness baseline is implemented. Issues `#64` through `#73` are all closed and established the production recovery, shutdown, persistence, audit, local-abuse, trust-lifecycle, and signing-authority foundations without weakening the authoritative operation/quarantine/no-auto-replay model.

The remaining explicit `0.3.0` release blocker is **#100 — local-user-authorized online quarantine recovery**. Its implementation exists in draft PR #101; release acceptance still requires the trusted physical-macOS Secure Enclave/user-presence flow and confirmation that a real ambiguous desktop operation is observed and resolved without replay.

`0.3.0` therefore does **not** absorb every issue discovered by later dogfood. New work blocks the release only when evidence shows that it violates an already-promised `0.3.0` safety/operability invariant or invalidates #100 acceptance. Otherwise it remains issue-driven follow-up hardening. This keeps release scope bounded while preserving fail-closed semantics.

The completed baseline was:

- recovery and restart safety: `#64`, `#65`, `#72`;
- persistence and incident closure: `#69`, `#73`;
- audit and local caller protection: `#70`, `#71`;
- trust lifecycle and signing authority: `#68`, `#67`, `#66`.

The existing release PR #99 predates substantial merged-main work. It must be refreshed or replaced from current `main` after #100 acceptance rather than merged as a stale release snapshot.

### First-class Human Handoff: integrated, now in dogfood hardening

Physical CUMG + `mcp-execution-handoff` acceptance has proven the bounded OS-window lifecycle through Agent -> Human -> verifying -> explicit Agent resume, including direct/TURN transport fallback, fresh exact-window verification, restart/context-expiry/generation-rollover recovery, no automatic replay, and zero residual quarantine. The acceptance-only Unix-socket bridge is therefore no longer the intended long-term runtime architecture.

Issue [#152](https://github.com/git-ksk/computer-use-mcp-gateway/issues/152) completed this integration and its merged-main physical OS-window acceptance. The topology is intentionally **first-class but optional**: ordinary CUMG capabilities do not require Handoff, but a deployment that enables Handoff must treat its authority decision as part of the execution boundary rather than as a best-effort sidecar. The controlled Agent owns the canonical Handoff FSM/checkpoint, WebRTC/TURN, capture, Human input, and local verification; the Hub retains CUMG authorization/ledger/quarantine and only a conservative pre-dispatch fence plus signed operator-control relay. Hub and Agent therefore do not run duplicate Handoff state machines. Live generation rollover uses an explicit same-surface `rebind_live`, and the final Agent gate re-validates the signed authority binding against the actual command immediately before Cua. Runtime/transport unavailability after Handoff is enabled must fail closed rather than silently bypass the coordinator. The legacy Unix bridge stays compatibility/regression-only.

The original dependency sequence has completed through component migration:

1. `#152` — first-class CUMG HandoffCoordinator and OS-window regression acceptance — **closed**;
2. `mcp-execution-handoff#48` — bounded PTY semantic dogfood — **closed**;
3. `mcp-execution-handoff#47` — reusable bounded OS/window primitives — **closed**;
4. CUMG `#176` / `#177` — migrate Window and Terminal runtime composition to upstream `WindowHandoffAdapter` / `TerminalHandoffAdapter` — **merged**;
5. `#157` — fail closed on legacy/current launchd coexistence — **closed**;
6. `#168` — dependency-complete, import-proven Handoff runtime packaging before production cutover — **closed**.

Remaining upstream closeout is intentionally outside CUMG authority: `mcp-execution-handoff#85` needs the first-class Window same-LAN direct physical rerun, `#91` tracks Terminal mobile connection/status presentation, and `#46`/`#45` own final Target Surface terminology/API convergence. CUMG must continue consuming the first-class components without pre-empting those upstream naming decisions. WebRTC video-quality work remains an independent Handoff concern.

### Operational dogfood follow-up after the production baseline

Sustained CUMG + Handoff dogfood after the original production-hardening baseline intentionally continued to exercise real failure/recovery paths. That work found additional issues without changing the core authority model. Track them as a stabilization queue rather than silently expanding the `0.3.0` release gate:

- execution/recovery semantics: `#179` partial input effects, `#180` quarantine-safe evidence lane, `#181` privacy-preserving evidence envelope, `#133` first-class reconciliation-readiness audit, plus `#115`/`#136`/`#137` recovery/retirement UX work;
- Handoff/operator lifecycle: `#184` in-band Handoff-begin self-interference and `#185` explicit one-shot single-Mac maintenance jobs;
- diagnostics and host reliability: `#141` privacy-safe structured execution errors, `#143` privacy-safe browser-staging startup stage/I/O diagnostics, `#112` disk/temp-exhaustion fail-closed diagnostics and recovery, and `#194` `v2_doctor` self-observation;
- each issue keeps its own severity, compatibility, tests, and acceptance boundary. A follow-up may be PATCH-compatible, admitted to a later minor, or deferred; none weakens quarantine/no-replay semantics to reduce backlog.

This queue records the practical result of continuing Handoff integration and physical dogfood while #100 remained the known `0.3.0` blocker.

### Current open issue inventory

As of 2026-08-26 the repository has **18 open issues**. Every open issue is intentionally listed here so an issue cannot silently fall out of roadmap visibility. This inventory is a tracking snapshot, not a promise that every item ships in the next release; issue closure/opening must update this section or a nearby roadmap section in the same documentation pass.

- **`0.3.0` release gate / closeout:** `#100` local-user-authorized online quarantine recovery is the only explicit runtime release blocker and still needs trusted physical-macOS acceptance; `#120` is release-document closeout and now retains only the final tag-time version/reference re-check.
- **Recovery and indeterminate-state UX:** `#103` extends durable operation recovery to effectful Desktop/Browser calls; `#109` confirms durable Hub completion from the online-recovery CLI; `#115` makes indeterminate operations actionable without unsafe replay; `#136` separates permanent replay tombstones from bounded retirement audit history; `#137` explores local-human acceptance of current state for low-impact indeterminate GUI operations. None may weaken quarantine, replay fencing, or persistence-gated settlement.
- **Runtime/process/filesystem hardening:** `#96` investigates deliberate Unix session-detachment escape from process-group supervision; `#104` separates filesystem observation roots from process working-directory roots. These are hardening follow-ups, not current `0.3.0` blockers.
- **Bounded workspace/developer capabilities:** `#83` adds retrievable references for truncated shell/process output; `#105` adds ranged file reads and deterministic directory continuation; `#106` adds explicitly managed long-running development jobs; `#107` adds bounded atomic workspace mutation without requiring shell; `#114` adds sandboxed Playwright/E2E execution. Each requires an explicit capability boundary rather than inheriting unrestricted shell authority.
- **Performance and repeatability:** `#111` adds a reproducible Gateway latency/concurrency benchmark. It is measurement infrastructure and does not redefine execution authority.
- **Post-`0.3.0` identity expansion:** `#139` adds generic OIDC/JWT northbound identity for multi-principal authorization and remains deliberately sequenced after the production-hardening closeout.
- **Upstream-blocked V1 compatibility:** `#14` (`get_screen_size` session/escalation) and `#15` (`list_apps` live-process discovery mismatch) remain blocked on upstream Cua. They are not active CUMG release blockers and may become no-longer-applicable if V1 is deliberately retired.

The classifications above are ordering/admission guidance only. Severity and acceptance requirements remain authoritative in each issue. If an open issue is not represented in this inventory or another explicit roadmap section, the roadmap is stale and should be corrected before declaring documentation closeout.

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
