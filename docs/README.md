# Documentation map

The documentation is grouped by purpose so current contracts are easy to distinguish from acceptance evidence and historical decision records.

## Operator and contributor guides

- [`GETTING_STARTED.md`](GETTING_STARTED.md) — install, configure, and reach the first working connection.
- [`ARCHITECTURE.md`](ARCHITECTURE.md) — runtime boundaries and authority split.
- [`SECURITY.md`](SECURITY.md) — trust boundaries, deployment requirements, and security invariants.
- [`TESTING.md`](TESTING.md) — automated, conformance, regression, and physical-desktop validation.
- [`DEPLOYMENT.md`](DEPLOYMENT.md) — service, network-edge, TLS, and production deployment guidance.
- [`CLIENTS.md`](CLIENTS.md) and [`TROUBLESHOOTING.md`](TROUBLESHOOTING.md) — client setup and diagnostics.
- [`PROJECT_GOVERNANCE.md`](PROJECT_GOVERNANCE.md) — maintainer model, change classes, merge rules, localization, and feature admission.
- [`VERSIONING.md`](VERSIONING.md) — SemVer, pre-1.0 compatibility, support/deprecation, and release procedure.
- [`v2/V2_RELEASE_ARTIFACTS.md`](v2/V2_RELEASE_ARTIFACTS.md) — release-artifact integrity plus the source-free single-Mac install/upgrade boundary.
- [`ROADMAP.md`](ROADMAP.md) — current maintenance priorities, future-minor admission, and the path to 1.0.

Repository-level community health files live at the repository root: [`../SECURITY.md`](../SECURITY.md) for private vulnerability reporting, [`../SUPPORT.md`](../SUPPORT.md) for support routing, [`../CODE_OF_CONDUCT.md`](../CODE_OF_CONDUCT.md) for participation expectations, and [`../GOVERNANCE.md`](../GOVERNANCE.md) as the standard governance entry point.

## Active V2 specification

Current V2 contracts and status live under [`v2/`](v2/). Start with [`v2/STATUS.md`](v2/STATUS.md).

The active specification covers the product boundary, execution safety, process/shell operation recovery, interaction context, Desktop and Browser semantic capabilities, backend parity, threat model, standardization seams, and optional usage accounting. See [`v2/V2_OPERATION_RECOVERY.md`](v2/V2_OPERATION_RECOVERY.md) for the durable no-replay recovery contract.

[`v2/V2_EXECUTION_ENVIRONMENT_BOUNDARY.md`](v2/V2_EXECUTION_ENVIRONMENT_BOUNDARY.md) clarifies the provider boundary: CUMG owns uncertainty-aware authority and recovery for a specific stateful desktop, while VM/sandbox/fleet provisioning remains replaceable infrastructure below or outside the core.

[`v2/V2_AUTHORIZATION_CAPABILITY_REVIEW.md`](v2/V2_AUTHORIZATION_CAPABILITY_REVIEW.md) records the reviewed authorization/capability boundary, narrow-only semantic-constraint direction, and second-real-backend portability requirement from #219.

## Acceptance evidence

Closeout evidence that proves a bounded capability or environment lives under [`v2/acceptance/`](v2/acceptance/). Acceptance records are evidence, not the primary product specification.

## Historical and decision records

Superseded progress journals, early PoCs, and decisions that remain useful for provenance live under [`archive/`](archive/). Archived documents are retained intentionally; they must not be read as current runtime instructions unless an active document links to them for historical context.
