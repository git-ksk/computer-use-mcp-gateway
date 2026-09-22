# V2 v0.7.0 release scope and acceptance

> English is canonical. Japanese translation: [V2_070_RELEASE_SCOPE.ja.md](V2_070_RELEASE_SCOPE.ja.md).

Release: **v0.7.0 — Operator Ergonomics & Recovery Evidence**

## Admitted release scope

- #342: caller-facing operation-ID guidance requires a fresh CSPRNG-generated 128-bit ID before each effectful call when the caller supplies `operation_id`; patterned/counter/hand-authored values remain shape-valid but are explicitly unsupported generation practice.
- #295: reviewed V2 CLI binaries expose bounded package/source identity through `--version`; release-candidate smoke binds that human-readable identity back to the closed artifact manifest while keeping manifest/hash verification authoritative.
- #304: first-class read-only `cumg_status` exposes the existing unified operator-status model to MCP consumers without granting shell authority or creating a second health/authorization state machine.
- #289: execution-safety durable schema v14 preserves privacy-bounded application target identity for ambiguous launch/termination recovery and exposes it only through reviewed local recovery surfaces.
- #290: Agent M1 persistence schema v6 can retain reviewed operation-bound backend receipts and feed exact authoritative evidence into the existing signed self-reconciliation path; unsupported/weak evidence remains operator-required.
- #347: the macOS release candidate ships verified single-Mac disaster-recovery backup/restore with external manifest anchoring, exact runtime/Handoff identity checks, mutation-authority preservation, staged restore, explicit activation, quarantine/replay preservation, and fail-closed corruption/mismatch handling.

The v0.7.0 milestone also contains #359, #360, and #361 closed as **not planned**. They are not part of the admitted release implementation: #359 duplicates the backup/restore track completed by #347, #360 would extend Linux cgroup-v2 containment to managed jobs, and #361 would add SBOM/provenance-ready candidate evidence. Their closed milestone membership must not be read as a shipped support claim.

## Release identity and compatibility

The v0.7.0 release snapshot pins:

- crate/package: **0.7.0**;
- live control schema: **12**;
- capability-advertisement schema: **8**;
- persisted device-registry schema: **8**;
- Hub-Agent transport schema: **6**;
- execution-safety durable schema: **14**;
- Agent M1 persistence schema: **6**.

The live control/capability/registry/Hub-Agent pairing is unchanged from released v0.6.0 (12/8/8/6). This is not a rolling mixed-version guarantee: packaged Hub, Agent, maintenance tools, runtime manifest, and Handoff generation remain version-paired and exact-source bound.

v0.7.0 introduces two durable-state compatibility boundaries beyond v0.6.0:

1. execution-safety v13 state remains readable, but v14 state carrying private recovery-target metadata cannot be lossily downgraded to v13;
2. historical Agent M1 schema v5 remains readable when it has no receipt state, but receipt-bearing v6 state cannot be lossily represented as v5.

There is also one deliberate pre-1.0 northbound input migration: `terminate_application` now requires a bounded `application` identity alongside `process_id` so later local recovery can identify the caller-selected target. Existing v0.6 clients that send only `process_id` must refresh MCP tool discovery/schema before using that effectful operation. The Agent control command remains PID-only and the live control schema number does not change.

`cumg_status`, bounded `--version`, operation-ID generation guidance, and verified backup/restore are additive/operator-facing changes and do not grant new execution authority by themselves.

## Upgrade and rollback boundary

Upgrade from v0.6.0 to v0.7.0 remains a **paired runtime upgrade** through the reviewed artifact/maintenance path. Because the live 12/8/8/6 protocol identity is unchanged, no rolling-compatibility inference is allowed; exact package/source/runtime identity still governs activation.

Rollback is safe only through the version-paired rollback material captured before upgrade. Once v0.7 writes target-bearing execution-safety v14 state or receipt-bearing Agent M1 v6 state, a v0.6 runtime must not be asked to reinterpret that newer authoritative state. Restore the pre-upgrade v0.6 checkpoint plus paired v0.6 binaries/configuration instead. Never hand-edit checkpoints, strip recovery metadata/receipts, or move release tags to manufacture compatibility.

Verified backup/restore is a disaster-recovery path, not an upgrade rollback substitute. Release-paired rollback preserves the pre-upgrade runtime/state boundary; #347 backup/restore preserves one exact reviewed snapshot plus mutation/quarantine/replay truth and requires a separately retained external manifest digest. Neither path manufactures settlement or snapshot freshness.

## Acceptance evidence

- #342, #295, #304, #289, #290, and #347 are completed before this release gate; the v0.7.0 milestone has no open admitted-scope issue.
- #289/#290 regression proves durable target/receipt compatibility, exact binding, restart persistence, fail-closed mismatch/downgrade behavior, and no replay when authoritative evidence is absent.
- #295 release-candidate regression executes packaged `--version` after fresh extraction and rejects package/source identity that differs from the artifact manifest.
- #304 reuses the unified status composition and has deterministic tests proving the MCP-facing surface is read-only, privacy-bounded, quarantine-aware, and non-authoritative.
- #347 deterministic tests cover writer/authority-lock refusal, exact quarantine/replay/mutation-authority round trip, external manifest digest enforcement, corrupt/missing/extra/symlink/permission/newer-schema rejection, Handoff identity mismatch, clean-profile enforcement, staged-copy tamper detection, bounded remediation, signer -> Hub -> Agent order, and authoritative reconciliation after restart without backup/restore settlement authority.
- A non-destructive trusted-macOS smoke against the running production profile proved backup refuses while effectful services are loaded with bounded `effectful_service_loaded / stop_reviewed_services` remediation and creates no output.
- Local release gates require Python harness regression, Rust all-target format/check/clippy/tests, docs/link validation, and `git diff --check`.
- Protected CI requires CodeQL, dependency review, docs links, backend passthrough, native Linux/macOS/Windows Cua smoke, Rust CI, and Release Candidate Artifacts build/verify/smoke.

## Product Readiness checklist result

The standing [Product Readiness checklist](../PRODUCT_READINESS.md) is applied to this exact release scope:

- [x] Distribution scope remains explicit: source-only GitHub pre-release; CI release-candidate archives are evidence, not official binary Release assets.
- [x] Package/source/schema identity is exact and bounded through release manifest, installed runtime manifest, `--version` cross-checks, checksums, and closed file allowlists.
- [x] Clean install/upgrade/rollback remain version-paired and fail closed; v0.7 durable state that cannot be represented by v0.6 is never downgraded in place.
- [x] Mixed/incompatible/future state fails closed; historical v0.6 live schema identity remains explicitly documented rather than inferred from equal schema numbers.
- [x] Unified `v2_status`/`v2_doctor`/`cumg_status` remain diagnostic/read-only composition surfaces and never become authorization or recovery authority.
- [x] Operation-ID guidance is CSPRNG-specific without pretending server-side entropy heuristics can prove caller randomness.
- [x] Recovery target metadata and backend receipts remain privacy-bounded and authority-classified; observational evidence cannot clear quarantine.
- [x] Verified backup/restore preserves exact paired runtime identity, durable state, mutation owner/epoch, unresolved quarantine, replay barriers, and reviewed in-root trust/secrets while excluding ephemeral/lock/log/socket/cache/stale-key material.
- [x] Restore is staged before activation, requires the external manifest digest and clean exact profile, creates a fresh coordination lock/generation, and never hand-edits checkpoints or manufactures settlement.
- [x] Ambiguous effectful work remains `Indeterminate`/quarantined with no automatic retry/replay; quarantine may shrink only through the existing authoritative reconciliation contract.
- [x] EN/JA operator-critical documentation, release notes, support-claim boundaries, and migration/rollback guidance are synchronized.
- [x] Deterministic regression plus protected exact-release-commit CI/RC artifacts are required green before tagging.

## Support-claim boundary

v0.7.0 does **not** absorb unfinished support claims merely because their implementation exists elsewhere in the tree.

- #139 provider-neutral OIDC/JWT plumbing remains compiled, but provider-specific physical signed-token support remains withheld pending its acceptance evidence.
- #217 cross-platform recovery parity remains open; no blanket Windows/Linux recovery parity claim is created here.
- #228 Linux FIDO2 UV recovery remains withheld pending trusted physical Linux + real UV-capable FIDO2 acceptance.
- The open `v0.6.1 — Support Claim Expansion` milestone is a separate evidence track and is **not a prerequisite** for v0.7.0; none of its unfinished claims are imported into this release.
- #222 second-real-backend semantic-neutrality proof remains future portability evidence and is not a v0.7 support claim.
- Hosted Cloud Run Hub/Handoff (#215/#275-#284) remains unsupported.
- #360 managed-job cgroup-v2 extension and #361 SBOM/provenance-ready evidence are not shipped by this release.

## Publication boundary

`v0.7.0` is a **source-only GitHub pre-release**. CI release-candidate archives remain release evidence only and are not attached as official binary assets. #361 is not implemented, so this release does not claim the SBOM/license/provenance evidence required for official binary publication; CI candidates are also not claimed as Apple-notarized public installers.

The immutable annotated `v0.7.0` tag and matching GitHub pre-release are created only after the dedicated release PR merges and the exact resulting `main` commit has required protected checks plus Release Candidate Artifacts green. Published tags are never moved or reused.
