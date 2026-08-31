# Versioning and release policy

> English is the canonical documentation. [日本語版 / Japanese translation](VERSIONING.ja.md)

CUMG uses Semantic Versioning with an explicit pre-1.0 policy.

Current released line: **0.3.x**. `v0.3.0` represents the V2 Production Hardening / Operational Readiness milestone; `v0.2.0` remains the immutable V2-complete historical tag.

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

Project/crate versions, wire protocol schemas, capability-advertisement schemas, and durable-state schemas serve different purposes.

- `CONTROL_SCHEMA_VERSION` changes when the live control-schema compatibility boundary changes.
- capability-advertisement schema version changes when that live advertisement boundary changes.
- `DEVICE_REGISTRY_SNAPSHOT_SCHEMA_VERSION` and `GRANT_LEDGER_SNAPSHOT_SCHEMA_VERSION` version their persisted structures independently; a future `CONTROL_SCHEMA_VERSION` bump must not change them unless the persisted structure itself changes.
- historical v0.2.x checkpoints used the then-current control schema number as the persisted registry/grant-ledger tag. Runtime restore supports only the explicitly reviewed v0.2.0-and-later lineage: registry `2/capability 2`, `3/capability 3`, `4..=7/capability 4`, and grant-ledger tags `2..=7`. Prototype tag `1`, unknown/future tags, and impossible control/capability pairings fail closed.
- historical capability advertisements are not promoted into live authority during migration. The Hub validates the historical pairing, restores device identity/generation, marks the device offline, and requires a fresh Agent advertisement using the current live schema before dispatch.
- a crate release does not automatically increment any schema.
- a schema change does not determine the crate version arithmetically; use PATCH/MINOR based on public compatibility impact.

A backward-compatible persisted-state migration that restores an already-supported release line without changing wire/configuration behavior is PATCH-eligible. Introducing a new incompatible persisted-state shape, removing support for a previously documented persisted-state version, or requiring operator state transformation is a new compatibility boundary and normally requires a MINOR release before 1.0. This backward-compatible checkpoint migration therefore does **not** by itself require a MINOR version bump.

## Durable-state writer compatibility and maintenance pairing

The execution-safety durable-state schema is an operational compatibility boundary independent of the crate version. Offline recovery must not silently rewrite an older authoritative checkpoint into a newer representation merely because the operator happened to run a newer maintenance binary.

`v2_maint resolve` therefore preserves the input checkpoint's supported writer contract, checks that the proposed post-resolution state is representable, and fails **before publication** when it is not. Packaged deployments must install `v2_hub` and `v2_maint` as a version-paired set from the same reviewed build/release artifact and upgrade the pair together. Keep the corresponding paired binaries with any rollback checkpoint that may be restored. A random newer source checkout is not a supported substitute for the maintenance binary paired with an older deployed Hub.

Read-only inspection commands such as `inspect-quarantine` do not become recovery authority and may read a supported checkpoint without mutating it. For authority-bearing maintenance, if the intended Hub cannot consume the required durable representation, upgrade through the documented compatible Hub + maintenance path first; do not hand-edit state, force a schema downgrade, or move a release tag.

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

## Release-candidate artifacts

The currently published `v0.3.0` release remains **source-only** unless a later GitHub Release explicitly contains reviewed binary assets. CI artifacts are never silently promoted into a supported distribution.

The `Release Candidate Artifacts` workflow still builds bounded native candidates on Linux, macOS, and Windows. Manifest schema v2 records the package version, exact CUMG source commit, Hub/Agent application-schema version, platform/architecture, exact allowlisted files, sizes, and SHA-256 identities. Linux and Windows candidates remain distribution evidence only.

The macOS single-Mac candidate additionally implements the #237 install-capable profile. It binds an exact reviewed `mcp-execution-handoff` commit, includes `v2_recover` plus the Secure Enclave helper, ships bounded install/upgrade support files and LaunchAgent templates, and embeds a separately manifested self-contained Handoff runtime payload. Fresh verification fails closed on checksum drift, unexpected/missing files, unsafe paths, symlinks, inner/outer commit mismatch, or incomplete production dependencies. The artifact manifest remains distribution evidence; the installed schema-3 `runtime-manifest.json` remains the runtime identity checked by `v2_doctor`.

A supported clean single-Mac installation means no CUMG/Handoff source checkout is required. Deployment trust/secrets, stable device/resource/proxy identity, Cua, Node.js, and the operator-selected Apple code-signing identity remain separately provisioned inputs. Before activation the installer copies verified artifact bytes into private staging and applies the existing stable Team-ID code-signing boundary to TCC-sensitive local executables/helpers; CI candidates are not claimed to be Apple-notarized public installers.

The same verified macOS artifact is the normal upgrade input. The bundled one-shot maintenance wrapper preserves the existing no-auto-retry durable upgrade transaction, fail-closed quarantine/Handoff/mutation-authority checks, paired rollback asset, restart ordering, and post-upgrade `v2_doctor` verification. The historical source-build path remains maintainer-only.

See [`v2/V2_RELEASE_ARTIFACTS.md`](v2/V2_RELEASE_ARTIFACTS.md) for the normative install/upgrade/distribution contract. An **official** binary GitHub Release must additionally attach reviewed SBOM/license inventory plus provenance/attestation binding the published archive checksum to the protected source/workflow identity. Publication metadata, signatures, SBOMs, and attestations never become execution/recovery authority.

## Release procedure

A normal release is prepared from `main` through a dedicated release PR.

1. Ensure intended implementation/docs changes are already merged to `main`.
2. Create `release/vMAJOR.MINOR.PATCH` from current `main`.
3. Update `Cargo.toml` and the corresponding package version in `Cargo.lock`.
4. Add the release section to `CHANGELOG.md`, including compatibility/breaking notes and meaningful acceptance evidence.
5. Update only status/version references intended to describe the newly released state.
6. Complete the standing [`PRODUCT_READINESS.md`](PRODUCT_READINESS.md) checklist for the admitted release scope, recording evidence or an explicit scope-based N/A rationale for each item.
7. Run required CI and any release-specific acceptance required by the change class.
8. Merge the release PR through the protected-`main` process.
9. Create an annotated `vMAJOR.MINOR.PATCH` tag on the resulting `main` commit.
10. Create the matching GitHub Release. Mark 0.x releases as pre-release; `1.0.0` and later are stable unless explicitly published as alpha/beta/RC.

Published tags are immutable. Do not move or reuse a release tag. Fix forward with a new patch/minor release.

## Pre-release identifiers

Use SemVer pre-release identifiers only when a candidate build needs validation before final release, for example `0.3.0-rc.1`, `1.0.0-beta.1`, or `1.0.0-rc.1`. Do not create them merely to inflate milestone count.

## Changelog rule

`CHANGELOG.md` is release-oriented, not a duplicate commit log. Include user/operator-relevant capability, compatibility, migration, security, supported-backend, reliability/operations, and acceptance changes. Routine internal refactors and purely editorial changes do not need individual bullets unless they materially affect a release claim.
