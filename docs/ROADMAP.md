# Roadmap

> English is the canonical documentation. [日本語版 / Japanese translation](ROADMAP.ja.md)

Status as of 2026-08-31: **V1 implementation is closed and retained only as a legacy/regression surface; V2 is the recommended runtime; the current released version is `v0.3.0`.**

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

## Released baseline: `0.3.x`; active candidate: `0.4.0`

`v0.3.0` remains the released V2 Production Hardening / Operational Readiness baseline. Compatible fixes may still qualify for a patch release, but the active feature candidate is now **`v0.4.0`**.

The `0.4.0` candidate intentionally consolidates the work that had previously been split across the old `0.4.0 Recovery & Reconciliation` and `0.5.0 Multi-principal Identity` plans. No `v0.4.0` release/tag has shipped yet, so keeping those already-integrated changes behind an artificial minor-version boundary would add release overhead without creating a meaningful compatibility boundary.

### `0.4.0` integrated release plan

`0.4.0` is the working **Recovery, Identity & Semantic Authorization** release. The canonical candidate scope and support-claim matrix are maintained in [`v2/V2_040_RELEASE_SCOPE.md`](v2/V2_040_RELEASE_SCOPE.md). It combines the completed recovery/reconciliation foundation with provider-neutral multi-principal identity and the typed semantic authorization boundary, while keeping optional platform/hosted support claims narrower than the artifact itself.

| `0.4.0` track | Issues / PR | Status | Release role |
| --- | --- | --- | --- |
| Core recovery semantics | #103, #137, #136 | Complete | Required baseline |
| Shared WebAuthn/CTAP verifier | #256 / PR #258 | Complete | Required shared recovery dependency; no platform support claim by itself |
| Recovery dogfood hardening | #253, #254, #115, #255 | Complete | Included compatibility/hardening evidence |
| Multi-principal OIDC/JWT identity | #139 / PR #269 | Implementation + CI complete; physical signed-token dogfood pending | Included implementation; provider-specific support claim waits for acceptance |
| Typed semantic authorization | #221 | Implementation complete in candidate change | Included after merge/CI |
| Windows Hello recovery | #227 / PR #252 | Implementation + CI green; physical acceptance pending | Optional platform support gate; base `0.4.0` release need not claim Windows recovery support |
| Linux FIDO2 UV recovery | #228 / PR #259 | Implementation + CI complete; physical acceptance pending | Optional platform support gate; base `0.4.0` release need not claim Linux recovery support |
| Cross-platform recovery parity umbrella | #217 | Physical acceptance dependent | May remain open until each platform actually advertised as supported has evidence |
| Hosted Cloud Run Hub | #215 | Design complete; implementation/acceptance pending | **Not a `0.4.0` support claim**; future hosted-deployment track |

Use this order for the release candidate:

1. **Merge/verify #221**: implementation is complete in the candidate change; require full regression/CI and EN/JA normative documentation before treating the gate as closed.
2. **Run the `0.4.0` release closeout** against the standing [`PRODUCT_READINESS.md`](PRODUCT_READINESS.md) gate: version/durable-schema compatibility, source-free candidate artifacts, clean install/upgrade/rollback, doctor/status, recovery/no-replay, dependency/CodeQL, docs, and release notes.
3. **Complete #139 signed-token dogfood before advertising generic signed-token identity as release-supported.** The implementation may be present in the artifact while that explicit support claim remains acceptance-gated.
4. **Keep #227/#228 physical gates honest.** Their code can ship in the candidate, but Windows Hello or Linux FIDO2 online recovery is not advertised as supported until its own physical acceptance passes. #217 closes only after the claimed platform set is actually proven.
5. **Do not pull #215 implementation into the release gate.** The Cloud Run design is useful evidence already on `main`, but hosted Hub support remains NO-GO until its separate durable-state/fencing/ingress/acceptance contract is implemented.

This is a release-scope consolidation, not a weakening of acceptance. The artifact may contain implementation whose **support claim is narrower than its compiled surface**; release notes and status docs must state those boundaries explicitly.

### Legacy V1 retirement candidate

`v1_gateway` remains in `main` for regression/reference and any still-existing legacy deployment. It is not a separately maintained `0.1.x` line and does not receive routine backports. V2 Hub + Agent is the recommended runtime.

Retiring V1 is now a valid future simplification candidate, but removal must be deliberate rather than incidental maintenance. Before removal:

- confirm no supported production deployment still depends on `v1_gateway`;
- decide which V1 regression/conformance fixtures remain valuable as backend-contract tests and migrate or archive them intentionally;
- resolve or close V1-only upstream-blocked issues such as #14/#15 as no longer applicable if the surface is retired;
- remove V1 configuration/deployment documentation and compatibility claims coherently;
- classify the removal as a pre-1.0 incompatible public-contract change and ship it only through an appropriate MINOR release with migration/release notes.

Until those conditions are met, keep V1 narrow and regression-only; do not expand it with new capabilities.

## Post-v0.3 productization sequence

CUMG is now past the initial V2 production-hardening release. Post-v0.3 work should move the project from a security-focused source release toward an installable, operable product without weakening the execution-safety boundary. Minor numbers below are the current working sequence, not calendar promises: a minor is cut only when its admitted public-contract scope and evidence are complete.

The working sequence is:

- **`0.3.x` — released baseline / compatible maintenance:** #213 Product Readiness closeout, #237 artifact-backed install/upgrade, #104 filesystem-root separation, #111 reproducible benchmark, and bounded #96 investigation are complete. #215 Cloud Run design is complete but hosted support remains a future NO-GO implementation track rather than an open-ended `0.3.x` release blocker.
- **`0.4.0` — Recovery, Identity & Semantic Authorization:** consolidate the completed recovery/reconciliation work (#103/#137/#136/#253/#254/#115/#255/#256), provider-neutral OIDC/JWT identity (#139), and typed backend-neutral semantic authorization (#221). #221 implementation is complete in the candidate change; merge/CI plus normal release closeout remain. #139/#227/#228 retain explicit physical/support-claim acceptance where applicable; unaccepted platform providers are not advertised as supported merely because their code is present.
- **`0.5.0` — Least-privilege Workspace:** reduce reliance on Dangerous shell authority through bounded retrievable output (#83), ranged/deterministic filesystem observation (#105), and atomic workspace mutation under explicitly separate writable roots (#107).
- **`0.6.0` — Managed Developer Execution:** add explicitly managed long-running jobs (#106), separately sandboxed Playwright/E2E execution (#114), and optional Linux cgroup-v2 execution containment (#267), informed by completed #96 rather than by background-shell escape compatibility.

The minor numbers are working release boundaries, not calendar promises. A feature can be compiled into an artifact while its optional platform/provider support claim remains withheld pending explicit acceptance; the release notes must make that distinction visible. If implementation evidence later requires another split or defer, preserve the safety boundary rather than the numbering.

### Cross-cutting Product Readiness track

The initial umbrella [#213](https://github.com/git-ksk/computer-use-mcp-gateway/issues/213) is complete. Future release preparation uses the standing [`PRODUCT_READINESS.md`](PRODUCT_READINESS.md) checklist; split concrete implementation work into narrower issues when a gate exposes an actionable gap.

Productization is not complete merely because a capability works in source-tree dogfood. Every post-v0.3 milestone should improve or preserve the following product-level foundations:

1. **Distribution and release integrity.** Source releases remain valid, but an installable product path should eventually provide reviewed per-platform artifacts, deterministic checksums, provenance/attestation, an SBOM plus third-party license/notice inventory, and platform signing/notarization where applicable. Release artifacts must contain only intended files and no credentials/private endpoints. Clean-machine artifact-install smoke should validate what users actually receive rather than only the source checkout.
2. **Install, upgrade, and rollback.** Maintain an explicit supported path for first install, coordinated Hub/Agent/maintenance/helper upgrades, durable-state migration, and rollback. Version-paired components and checkpoint compatibility must remain explicit; incompatible mixed versions fail closed rather than attempting silent rolling compatibility. A release that changes durable or wire state should prove upgrade from the previous supported minor and document the safe rollback boundary.
3. **First-run and configuration UX.** Keep one clear reference deployment per supported platform, validate configuration before effectful service start where practical, make missing/unsafe secrets and trust anchors actionable, and use `v2_doctor`/preflight-style checks so a new operator can distinguish configuration, permission, capacity, trust, and backend failures without reading internal state files. Safe defaults remain least-privilege and fail closed.
4. **Operational readiness.** Treat service lifecycle, health/readiness, quarantine, recovery, TLS/key expiry, storage pressure, restart/drain, backup/restore, and incident runbooks as product behavior. Operator signals must be bounded and privacy-safe. A deployment should have a documented way to detect "needs operator action" without exposing raw desktop, command, credential, or identity content.
5. **Reliability, performance, and resource budgets.** Maintain deterministic regression, soak, concurrency, restart/reconnect, fault-injection, and capacity evidence. #111 establishes a reproducible informational latency/throughput distribution harness; future releases should also keep explicit CPU/RSS/disk/output/concurrency ceilings where those limits form part of safe operation. Workstation measurements are regression evidence, not production capacity marketing claims.
6. **Security, privacy, and supply chain.** Preserve the threat model, exact capability authorization, no-auto-replay, secret isolation, key/certificate rotation, private vulnerability reporting, dependency review, CodeQL, and content-minimizing telemetry. New distribution automation must not weaken source/dependency provenance or turn signing infrastructure into runtime authority.
7. **Compatibility, support, and deprecation.** Publish the supported CUMG minor line, tested OS/Cua/backend/deployment matrix, schema mismatch behavior, and migration/deprecation notes. Before 1.0 only the latest released minor is actively supported; compatibility claims must remain evidence-backed rather than inferred from nearby versions.
8. **Onboarding and documentation.** A supported reference path should take an operator from installation to healthy diagnostics, a first read-only call, a deliberately authorized effectful call, and the documented recovery path without requiring repository archaeology. EN/JA normative documentation must remain aligned for security, deployment, versioning, and operator-critical behavior.

These are cross-cutting gates rather than a promise to build a hosted dashboard, account system, auto-updater, generic device fleet, or remote-desktop product. A product feature is admitted only when it fits the existing CUMG boundary or that boundary is deliberately revised with evidence.

### `0.3.0` closeout: V2 Production Hardening

The original `0.3.0` Production Hardening / Operational Readiness baseline is implemented. Issues `#64` through `#73` are all closed and established the production recovery, shutdown, persistence, audit, local-abuse, trust-lifecycle, and signing-authority foundations without weakening the authoritative operation/quarantine/no-auto-replay model.

The final explicit `0.3.0` runtime release blocker, **#100 — local-user-authorized online quarantine recovery**, is complete. Trusted physical-macOS Secure Enclave/user-presence acceptance resolved a real ambiguous desktop operation without replay; authorization publication was user-presence-gated, the quarantine cleared only after the verified resolution, and Hub restart preserved the terminal resolution without reviving the old operation.

`0.3.0` therefore does **not** absorb every issue discovered by later dogfood. New work blocks the release only when evidence shows that it violates an already-promised `0.3.0` safety/operability invariant or invalidates #100 acceptance. Otherwise it remains issue-driven follow-up hardening. This keeps release scope bounded while preserving fail-closed semantics.

The completed baseline was:

- recovery and restart safety: `#64`, `#65`, `#72`;
- persistence and incident closure: `#69`, `#73`;
- audit and local caller protection: `#70`, `#71`;
- trust lifecycle and signing authority: `#68`, `#67`, `#66`.

The stale release PR #99 predates substantial merged-main work and was superseded by a fresh release snapshot from current `main` after #100 acceptance.

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

The previously referenced upstream Handoff closeout (#45, #46, #85, and #91) is complete, as are the later LocalAuthentication lifecycle fixes #147/#149. CUMG should keep consuming upstream first-class components rather than forking their semantics. Remaining upstream UX polish such as `mcp-execution-handoff#150` stays an independent Handoff concern and is not a CUMG release blocker unless evidence shows it violates a CUMG safety/operability invariant.

### Operational dogfood follow-up after the production baseline

Sustained CUMG + Handoff dogfood after the original production-hardening baseline intentionally continued to exercise real failure/recovery paths. That work found additional issues without changing the core authority model. Track them as a stabilization queue rather than silently expanding the `0.3.0` release gate:

- execution/recovery semantics: `#179` partial input effects, `#180` quarantine-safe evidence lane, `#181` privacy-preserving evidence envelope, `#133` first-class reconciliation-readiness audit, plus `#115`/`#136`/`#137` recovery/retirement UX work;
- Handoff/operator lifecycle: `#184` in-band Handoff-begin self-interference and `#185` explicit one-shot single-Mac maintenance jobs;
- diagnostics and host reliability: `#141` privacy-safe structured execution errors, `#143` privacy-safe browser-staging startup stage/I/O diagnostics, `#112` disk/temp-exhaustion fail-closed diagnostics and recovery, and `#194` `v2_doctor` self-observation;
- each issue keeps its own severity, compatibility, tests, and acceptance boundary. A follow-up may be PATCH-compatible, admitted to a later minor, or deferred; none weakens quarantine/no-replay semantics to reduce backlog.

This queue records the practical result of continuing Handoff integration and physical dogfood while #100 was the known `0.3.0` blocker. Those follow-ups remain outside the completed v0.3.0 runtime gate unless their own evidence invalidates a released invariant.

### Current open issue inventory

The repository's open issues are classified by the revised release sequence so work cannot silently fall out of roadmap visibility. Milestones are ordering/admission guidance; an optional support-claim acceptance issue may remain open after the base artifact is released if that support claim is explicitly withheld.

- **`0.3.x — released baseline / maintenance`:** no blocking implementation gate remains. #215 is removed from the patch-line milestone: its design is complete, Cloud Run remains unsupported, and future implementation is tracked as a separate hosted-deployment concern rather than keeping `0.3.x` artificially open.
- **`0.4.0 — Recovery, Identity & Semantic Authorization`:** #221 implementation is complete in the candidate change; merge/CI is the remaining feature gate. #139 implementation is merged and physical signed-token dogfood remains; #227/#228 implementation is present with platform-specific physical acceptance pending under #217. Those acceptance items gate their respective support claims, not unrelated `0.4.0` capabilities.
- **`0.5.0 — Least-privilege Workspace`:** #83 adds bounded retrievable process/shell output, #105 adds ranged/deterministic filesystem observation, and #107 adds bounded atomic workspace mutation without inheriting unrestricted shell authority.
- **`0.6.0 — Managed Developer Execution`:** #106 adds explicit managed-job lifecycle, #114 adds separately sandboxed Playwright/E2E execution, and #267 owns optional Linux cgroup-v2 containment.
- **Future / evidence-driven:** #215 hosted Cloud Run Hub implementation and #222 second-real-backend semantic neutrality remain intentionally outside a numbered release gate until their prerequisites/evidence justify admission.
- **Upstream-blocked V1 compatibility:** #14 and #15 remain blocked on upstream Cua and are not active CUMG release blockers.

If an open issue is not represented here or in another explicit roadmap section, treat the roadmap as stale and correct it before declaring release closeout.

The Cua authorization/product-boundary research in #219 is completed by [`v2/V2_AUTHORIZATION_CAPABILITY_REVIEW.md`](v2/V2_AUTHORIZATION_CAPABILITY_REVIEW.md); its admitted follow-ups are #221 and #222.

### `0.4.0` identity and semantic-authorization component

Issue [#139](https://github.com/git-ksk/computer-use-mcp-gateway/issues/139) is now part of the integrated `0.4.0` candidate rather than a separate `0.5.0` release. Its implementation is merged: provider-neutral signed-token verification reduces a verified external identity to the existing `AuthenticatedClientPrincipal`, while exact principal/device/capability authorization remains unchanged. Physical signed-token dogfood remains the final #139 support-claim acceptance item.

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

The adapter remains provider-neutral and fail closed. It verifies signature, issuer, audience, time claims, subject, asymmetric algorithm policy, and bounded JWKS caching/rotation before producing the existing CUMG principal. Caller-supplied identity headers and MCP `clientInfo` remain audit metadata only and never become authorization authority.

Issue [#221](https://github.com/git-ksk/computer-use-mcp-gateway/issues/221) implements typed, backend-neutral, narrow-only semantic constraints at the finalized command boundary without turning CUMG into a generic policy engine. Exact capability authorization, grant signing, Handoff, recovery authority, quarantine, and no-auto-replay remain separate authorities.

This integrated release does **not** make CUMG an identity provider, account database, session manager, generic policy engine, or token issuer. Existing RFC 7662 introspection and explicitly single-principal trusted-proxy deployments remain supported choices, and optional signed-token/platform support claims remain acceptance-gated.

### Remaining semantic parity decisions

The current Cua parity matrix deliberately leaves these legitimate gaps explicit:

- `ClipboardWrite` supports plain text only; image/file clipboard write parity is not implemented;
- `LaunchApplication` does not expose Cua `additional_arguments` or `webkit_inspector_port`.

A gap should be implemented only when there is a concrete workflow need and a bounded backend-neutral contract can be defined without exposing a generic backend passthrough. It is also valid to keep a gap explicitly unsupported.

See [`v2/V2_CUA_PARITY_MATRIX.md`](v2/V2_CUA_PARITY_MATRIX.md).

### Additional backend or native GUI adapter

Issue #222 owns the current P1 portability-evidence candidate: prove a small overlapping semantic slice on a second materially different real computer-use/native-GUI backend. This evidence must exercise real side effects and ambiguity semantics; compile-time compatibility or the deterministic reference executor alone is not sufficient for a cross-GUI-backend claim.

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
- at least one documented install/upgrade/rollback path uses reviewed release artifacts rather than requiring ad-hoc source-tree assembly, and release integrity/provenance is reproducible;
- first-run diagnostics, supported compatibility matrices, operational recovery, and resource/health signals are sufficiently clear that normal deployment does not depend on maintainer-only repository knowledge;
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
