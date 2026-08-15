# Project governance

> English is the canonical documentation. [日本語版 / Japanese translation](PROJECT_GOVERNANCE.ja.md)

## Scope

This document defines how `computer-use-mcp-gateway` (CUMG) is maintained, reviewed, merged, and released. It does not replace the runtime security model in [`SECURITY.md`](SECURITY.md), [`v2/V2_THREAT_MODEL.md`](v2/V2_THREAT_MODEL.md), or the product boundary in [`v2/V2_POSITIONING.md`](v2/V2_POSITIONING.md).

## Maintainer model

CUMG currently uses a maintainer-led model.

- The maintainer is the final decision maker for scope, security boundary, compatibility, release timing, and merge decisions.
- External review and design discussion are welcome, but lack of consensus does not require indefinite delay.
- A decision must not silently weaken documented execution-safety invariants, fabricate acceptance evidence, or describe an unimplemented capability as implemented.
- Security-significant disagreements should be resolved by evidence: code, tests, acceptance results, safe protocol traces, or authoritative upstream documentation.

If the maintainer set expands, update this document before adding mandatory multi-maintainer approval rules.

## Project invariants

Unless an explicitly reviewed replacement proves an equal or stronger safety property, changes preserve these rules:

1. ambiguous state-changing work is not automatically retried or replayed;
2. post-dispatch uncertainty converges on the authoritative CUMG `Indeterminate` / quarantine / explicit-resolution path;
3. exact principal, device, capability, operation identity, revision, and generation fences remain authoritative where applicable;
4. raw backend/provider authority is not exposed northbound as a generic escape hatch;
5. secrets, credentials, private endpoints, raw desktop payloads, and sensitive provider errors are excluded from normal logs, telemetry, examples, and acceptance artifacts;
6. compatibility claims are backed by reproducible validation rather than assumed from version proximity.

## Change classes and gates

### A. Editorial / documentation-only

Required: documentation validation, `git diff --check`, and confirmation that no source/config/workflow change slipped into the diff. A documentation change that alters capability status, schema meaning, security semantics, compatibility, or release policy is normative and must use the stronger gate below.

### B. Normal implementation / maintenance

Required: normal CI, relevant deterministic tests when behavior changes, intentional `Cargo.lock` updates, and documentation for user-visible behavior or compatibility changes.

### C. Public-contract / security-boundary / execution-safety

In addition to B, require explicit failure/downgrade review, updates to the relevant threat-model/security/status/parity/architecture docs, targeted regression coverage, and any acceptance evidence needed by the claim. Ambiguity must fail closed rather than being inferred as safe completion, safe cancellation, safe retry, or replay permission.

### D. Privileged / physical-desktop acceptance

Claims that depend on real desktop, TCC, GUI, provider, or privileged-host behavior require the reviewed trusted-host acceptance path. Untrusted pull-request code must not run on a privileged self-hosted desktop runner.

## Pull requests and `main`

- `main` is the release source of truth.
- Normal changes reach `main` through a pull request; direct pushes are not part of the normal workflow.
- One PR should represent one coherent change or tightly coupled change set.
- Required CI checks must be green and review threads resolved before merge.
- While CUMG has one maintainer, external approval is not mandatory; this avoids a governance rule that the maintainer cannot independently satisfy.
- Squash merge is the normal merge method so `main` keeps one logical commit per PR.
- Branches are deleted after merge unless intentionally long-lived.

An emergency repository-repair action may bypass the normal flow only when the repository cannot otherwise be restored safely. The follow-up state must be documented and returned to the normal PR path immediately.

## Documentation and localization

- English documentation is canonical.
- When normative meaning changes in a paired English/Japanese document, update both in the same PR. This includes security semantics, schema versions, capability status, compatibility, and release policy.
- Editorial follow-up may be synchronized separately, but reciprocal links and heading structure should remain aligned.
- Historical/archive documents may preserve historical wording when their historical status is explicit.

## Dependencies and upstream compatibility

- Pin behavior-sensitive upstream components when reproducibility matters.
- Major dependency updates are reviewed changes, not blind refreshes.
- A Cua Driver compatibility-target change requires the validation described in [`TESTING.md`](TESTING.md).
- For external commands, APIs, and release-specific behavior, authoritative upstream documentation wins over stale repository instructions.

## Feature admission

A feature is not accepted merely because it is technically possible. Before adding custom generic infrastructure, review whether maintained standards or OSS can provide it without weakening CUMG's invariants.

A proposed feature should answer: what problem it solves, whether it changes a public/security contract, what new failure states appear, how they are represented, what evidence closes the change, and why the functionality belongs in CUMG rather than an external maintained component.

The current GO / NO-GO boundary remains in [`ROADMAP.md`](ROADMAP.md) and [`v2/V2_POSITIONING.md`](v2/V2_POSITIONING.md).

## Releases

Version selection, support, deprecation, and release mechanics are defined in [`VERSIONING.md`](VERSIONING.md). Release claims remain evidence-backed; a larger version number is not evidence by itself.
