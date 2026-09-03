# V2 `0.4.0` release scope

> English is canonical. [日本語版 / Japanese translation](V2_040_RELEASE_SCOPE.ja.md)

Status: **active release candidate scope; no `v0.4.0` tag or GitHub Release has shipped yet**.

## Purpose

`0.4.0` consolidates the work that was previously split between Recovery & Reconciliation and Multi-principal Identity. The release boundary is now **Recovery, Identity & Semantic Authorization**.

This consolidation does not weaken acceptance. CUMG distinguishes:

1. **artifact inclusion** — code may be compiled and shipped in the candidate; and
2. **support claim** — an optional platform/provider capability is advertised as supported only after its explicit acceptance evidence exists.

A capability can therefore be present in the artifact while its support claim remains withheld.

## Included baseline

The `0.4.0` candidate includes the already-completed compatible work on `main`, including:

- durable Recovery & Reconciliation semantics, permanent no-replay and reviewed current-state/Human recovery paths (#103, #137, #136, #115, #255);
- recovery/operator hardening and readiness (#253, #254, #256);
- least-privilege filesystem observation-root separation (#104);
- reproducible informational performance benchmarking (#111);
- provider-neutral OIDC/JWT caller identity through the existing `AuthenticatedClientPrincipal` and exact authorizer (#139 / PR #269);
- the completed bounded Unix containment investigation (#96), with stronger Linux containment split to #267.

#221 typed backend-neutral semantic constraints are implementation-complete in the candidate change. Exact capability authorization, grant signing, Handoff, recovery authority, quarantine, and no-auto-replay remain separate authorities. The remaining base `0.4.0` work is normal release closeout.

## Support-claim matrix

| Surface | Candidate state | `0.4.0` support-claim rule |
| --- | --- | --- |
| Existing accepted macOS/single-Mac V2 profile | Accepted baseline | May remain supported subject to normal release closeout |
| Recovery/reconciliation core | Implemented/accepted | Included |
| Generic OIDC/JWT identity | Implementation + CI merged | Do not make the signed-token support claim until #139 physical/dogfood acceptance is recorded |
| Typed semantic authorization | #221 implementation-complete in candidate change | Included after merge/CI |
| Windows Hello recovery | Implementation + CI present | Do not claim Windows online-recovery support until #227 physical acceptance passes |
| Linux FIDO2 UV recovery | Implementation + CI present | Do not claim Linux online-recovery support until #228 physical acceptance passes |
| Cross-platform recovery parity | #217 open | Close only for the platform set actually claimed as supported |
| Cloud Run hosted Hub | Design only / NO-GO | Not a `0.4.0` support claim; #215 implementation/acceptance remains future work |
| Second real computer-use backend | #222 future evidence | Not required for `0.4.0`; current backend-neutral claims remain bounded to existing evidence |

## Release closeout gate

Before creating `v0.4.0`:

1. The #221 candidate change is merged and CI confirms its typed constraints, immutable final-command binding, durable bounded audit evidence, stale-decision fencing, full regression, and EN/JA normative docs.
2. The standing [`../PRODUCT_READINESS.md`](../PRODUCT_READINESS.md) gate is rerun for the exact candidate commit.
3. Durable/wire schema changes are documented and upgrade compatibility from the previous supported minor is proven; incompatible downgrade/rolling mixes continue to fail closed.
4. Source-free release-candidate artifacts are built from the exact candidate identity, verified after fresh extraction, and clean install/upgrade/paired rollback evidence remains green.
5. `v2_doctor` and `v2_status` show the reviewed reference deployment healthy, while unresolved quarantine or incompatible runtime/tool state still fails closed.
6. Recovery/no-auto-replay, dependency review, CodeQL, docs/link validation, conformance, and release packaging checks are green.
7. Release notes state the support-claim matrix above explicitly. Optional platform/provider implementations with pending acceptance must not be described as supported.
8. No new evidence demonstrates a released safety/reliability invariant failure that requires a blocker fix.

## What does not block the base artifact

The following may remain open if their associated support claim is withheld clearly:

- #217/#227/#228 physical Windows/Linux recovery acceptance;
- #139 signed-token dogfood acceptance, if generic signed-token identity is explicitly marked not-yet-supported in that candidate;
- #215 Cloud Run implementation/acceptance;
- #222 second-backend proof.

This exception is about support claims only. It does not permit an implementation known to violate a core CUMG safety invariant to ship.

## After `0.4.0`

The working next minors are:

- **`0.5.0` — Least-privilege Workspace:** #83, #105, #107.
- **`0.6.0` — Managed Developer Execution:** #106, #114, #267.

Cloud Run #215 and second-backend proof #222 remain evidence-driven future tracks until explicitly admitted to a numbered release.
