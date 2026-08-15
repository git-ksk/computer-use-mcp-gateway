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
- [`ROADMAP.md`](ROADMAP.md) — current maintenance priorities, future-minor admission, and the path to 1.0.

## Active V2 specification

Current V2 contracts and status live under [`v2/`](v2/). Start with [`v2/STATUS.md`](v2/STATUS.md).

The active specification covers the product boundary, execution safety, interaction context, Desktop and Browser semantic capabilities, backend parity, threat model, standardization seams, and optional usage accounting.

## Acceptance evidence

Closeout evidence that proves a bounded capability or environment lives under [`v2/acceptance/`](v2/acceptance/). Acceptance records are evidence, not the primary product specification.

## Historical and decision records

Superseded progress journals, early PoCs, and decisions that remain useful for provenance live under [`archive/`](archive/). Archived documents are retained intentionally; they must not be read as current runtime instructions unless an active document links to them for historical context.
