# Versioning and release policy

> English is the canonical documentation. [日本語版 / Japanese translation](VERSIONING.ja.md)

CUMG uses Semantic Versioning with an explicit pre-1.0 policy.

Current released line: **0.2.x**. `v0.2.0` represents the V2-complete milestone; later maintenance or documentation work does not retroactively change that tag.

## Version shape

Versions use `MAJOR.MINOR.PATCH`; Git tags use `vMAJOR.MINOR.PATCH`.

Examples: `0.2.1`, `0.3.0`, `0.10.0`, `1.0.0`.

There is no rule that `0.9.x` must be followed by `1.0.0`. `0.10.0`, `0.11.0`, and later pre-1.0 minors are valid until the project is ready to make the 1.x compatibility commitment.

## PATCH: `0.y.Z`

Use a patch release when the shipped public contract remains compatible within the current minor line.

Typical patch changes:

- bug fixes;
- compatible security hardening;
- reliability, observability, or resource fixes;
- compatible dependency/backend pin updates;
- packaged-default corrections that preserve the documented contract.

Docs-only changes normally do **not** require a crate version bump or tag. A docs-only patch release is reserved for cases where an immutable corrected release snapshot is operationally important, such as a serious published security/deployment instruction correction.

## MINOR: `0.Y.0`

Use a new minor release for a meaningful public-contract expansion or a deliberate incompatible pre-1.0 change.

Typical minor changes:

- new northbound capability or capability family;
- a new supported backend with an advertised compatibility contract;
- protocol/schema behavior that introduces a new compatibility boundary;
- deliberate incompatible command/configuration/behavior change;
- a substantial runtime feature that materially changes what operators can rely on.

Roadmap phase, PR count, elapsed time, or documentation milestone alone is not a reason to increment the minor version.

## Breaking changes before 1.0

A pre-1.0 breaking change ships in a minor release, never a patch release. It must be called out in `CHANGELOG.md`, include migration guidance when users must change configuration/integration behavior, and update relevant compatibility/schema/security docs.

Security emergency changes may break compatibility when preserving compatibility would preserve a vulnerability. Release notes must state the break and security reason without prematurely publishing exploit-sensitive detail.

## Schema versions are independent

Project/crate versions and protocol schema versions serve different purposes.

- `CONTROL_SCHEMA_VERSION` changes when the control-schema compatibility boundary changes.
- capability-advertisement schema version changes when that compatibility boundary changes.
- a crate release does not automatically increment either schema.
- a schema change does not determine the crate version arithmetically; use PATCH/MINOR based on public compatibility impact.

## `1.0.0` criteria

`1.0.0` means **CUMG is willing to maintain a stable public compatibility contract**, not that every conceivable feature is implemented.

Before `1.0.0`, all of the following should be true:

1. the supported northbound semantic surface and core execution-safety invariants are explicitly designated stable;
2. control/capability schema compatibility and failure behavior are documented for supported upgrades;
3. the supported backend/deployment compatibility matrix is documented and backed by repeatable acceptance evidence;
4. versioning, release, security, support, and deprecation rules are documented and actually followed;
5. maintainers are prepared to keep backward-compatible changes within 1.x and reserve intentional public-contract breaks for a future major release, except emergency security cases.

Feature count is not a 1.0 gate. Fleet management, remote desktop, generic device fabric, broad orchestration, or every backend capability are not required for 1.0 unless the product boundary changes first.

## Deprecation and support

Before 1.0, deprecation is preferred when practical, but an incompatible change may ship in the next minor release with migration notes.

Starting with 1.0:

- compatible additions/deprecations use minor releases;
- fixes use patch releases;
- intentional public-contract removals/breaks require a new major release;
- normally a deprecated public surface remains available for at least one subsequent minor release before removal, unless security requires earlier removal.

Before 1.0, only the **latest released minor line** is actively supported. Older 0.x lines are best-effort and do not receive routine backports. Severe security backports are discretionary exceptions, not an LTS promise.

## Release procedure

A normal release is prepared from `main` through a dedicated release PR.

1. Ensure intended implementation/docs changes are already merged to `main`.
2. Create `release/vMAJOR.MINOR.PATCH` from current `main`.
3. Update `Cargo.toml` and the corresponding package version in `Cargo.lock`.
4. Add the release section to `CHANGELOG.md`, including compatibility/breaking notes and meaningful acceptance evidence.
5. Update only status/version references intended to describe the newly released state.
6. Run required CI and any release-specific acceptance required by the change class.
7. Merge the release PR through the protected-`main` process.
8. Create an annotated `vMAJOR.MINOR.PATCH` tag on the resulting `main` commit.
9. Create the matching GitHub Release. Mark 0.x releases as pre-release; `1.0.0` and later are stable unless explicitly published as alpha/beta/RC.

Published tags are immutable. Do not move or reuse a release tag. Fix forward with a new patch/minor release.

## Pre-release identifiers

Use SemVer pre-release identifiers only when a candidate build needs validation before final release, for example `0.3.0-rc.1`, `1.0.0-beta.1`, or `1.0.0-rc.1`. Do not create them merely to inflate milestone count.

## Changelog rule

`CHANGELOG.md` is release-oriented, not a duplicate commit log. Include user/operator-relevant capability, compatibility, migration, security, supported-backend, reliability/operations, and acceptance changes. Routine internal refactors and purely editorial changes do not need individual bullets unless they materially affect a release claim.
