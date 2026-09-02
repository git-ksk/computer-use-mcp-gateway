# Product Readiness gate

> English is canonical. [日本語版 / Japanese translation](PRODUCT_READINESS.ja.md)

This is the standing release-readiness checklist established by issue #213. It is a **release gate**, not a feature backlog: a release may mark an item not applicable only when its supported distribution/deployment scope makes that explicit. Productization must never weaken CUMG's execution-safety, quarantine, no-auto-replay, authorization, or privacy boundaries.

For each release PR, copy the checklist below into the PR or linked acceptance record and attach concrete evidence. Do not infer readiness from a nearby version or from source-tree dogfood alone.

## Release checklist

### Distribution and release integrity

- [ ] Supported distribution scope is explicit per platform/profile: official installable artifact, reviewed candidate evidence, or source-supported only.
- [ ] Every published archive has a deterministic checksum and a closed manifest; unexpected/missing files, unsafe paths, symlinks, or digest drift fail closed.
- [ ] Release artifacts contain no credentials, private endpoints, temporary acceptance data, repository trees, or unrelated build products.
- [ ] Signing/notarization claims match reality. A CI candidate is not described as notarized or official unless that boundary has been implemented and accepted.
- [ ] Any official binary release includes the reviewed SBOM/license inventory and provenance/attestation strategy required by [`VERSIONING.md`](VERSIONING.md).

### Install, upgrade, rollback, and durable state

- [ ] At least one supported reference topology has a documented first-install path that validates artifact/configuration/trust before activation and ends in healthy diagnostics.
- [ ] Normal upgrade uses version-paired Hub/Agent/`v2_maint`/recovery/Handoff components and preserves the durable one-shot maintenance transaction where applicable.
- [ ] A release that changes wire or durable-state compatibility proves the admitted previous-version upgrade path and documents the safe rollback boundary.
- [ ] Rollback never restores an old binary over newer incompatible state, clears quarantine, or turns ambiguity into retry authority.
- [ ] Mixed/incompatible versions and unsupported state representations fail closed.

### First run and operator diagnostics

- [ ] The supported reference path is easy to locate from the documentation map and reaches install -> healthy `v2_status`/`v2_doctor` -> read-only semantic call -> explicitly authorized effectful call -> documented recovery.
- [ ] Missing/unsafe secrets, trust anchors, permissions, capacity, backend, topology, and maintenance-state failures produce bounded actionable diagnostics without exposing sensitive values.
- [ ] `v2_status`/`v2_doctor` remain diagnostic/composition surfaces only; they do not become authorization, recovery, replay, or mutation authority.

### Operational readiness

- [ ] Service start/stop/drain, restart/reconnect, TLS/key expiry, storage pressure, quarantine/recovery, incident response, and backup/restore boundaries are documented for supported reference deployments.
- [ ] Backup/restore preserves exact paired runtime identity, durable state, mutation authority, and unresolved quarantine; restore never hand-edits checkpoints or manufactures settlement.
- [ ] Common `operator_action_required` states have a privacy-bounded next step that does not require normal operators to inspect raw checkpoint files.

### Reliability and compatibility

- [ ] Required deterministic tests, restart/reconnect/fault-injection evidence, and relevant resource/concurrency checks are green for the admitted change class.
- [ ] Supported CUMG minor line, OS/Cua/backend/deployment compatibility, schema mismatch behavior, and migration/deprecation guidance are current and evidence-backed.
- [ ] EN/JA normative documentation is synchronized for operator-critical behavior.

### Security and privacy invariants

- [ ] Exact principal/device/capability authorization is unchanged or deliberately strengthened.
- [ ] Ambiguous effectful work remains `Indeterminate`/quarantined until an existing authoritative settlement path succeeds; no automatic replay is introduced.
- [ ] Artifact checksums, signing, SBOMs, provenance, diagnostics, and LLM/observational evidence never become execution or recovery authority.
- [ ] Default logs/telemetry/release metadata exclude credentials, raw desktop content, commands/results, private identity material, and other payload-bearing data outside the documented bounded audit contract.

## Evidence map

- Distribution scope / first install / artifact upgrade: [`v2/V2_RELEASE_ARTIFACTS.md`](v2/V2_RELEASE_ARTIFACTS.md).
- Single-Mac lifecycle / diagnostics / effectful-path acceptance: [`v2/V2_SINGLE_MAC_PRODUCTION.md`](v2/V2_SINGLE_MAC_PRODUCTION.md).
- Backup/restore: [`v2/V2_BACKUP_RESTORE.md`](v2/V2_BACKUP_RESTORE.md).
- OS/Cua compatibility and automated evidence: [`TESTING.md`](TESTING.md), especially the Real-Cua compatibility matrix.
- Durable/wire compatibility and release support rules: [`VERSIONING.md`](VERSIONING.md).
- Operator recovery and common action-required states: [`TROUBLESHOOTING.md`](TROUBLESHOOTING.md) and [`v2/STATUS.md`](v2/STATUS.md).

## Initial #213 closeout baseline — 2026-08-31

The initial post-v0.3 Product Readiness gate is complete on the following evidence:

- #224 established the closed release-candidate manifest/checksum and fresh-extraction smoke boundary.
- #226/#233/#234/#235/#109/#236 established lane-scoped readiness, privacy-bounded incident review, durable one-shot maintenance status, unified operator status, exact durable online-recovery confirmation, and Human-guided recovery without replay.
- #237 / PR #247 added the source-free single-Mac artifact install/upgrade path, exact CUMG/Handoff pairing, recovery helper packaging, fail-closed artifact verification, clean-install orchestration tests, paired rollback, and installed doctor/status success gates. Linux and Windows remain candidate evidence rather than official binary-installer claims.
- [`v2/V2_RELEASE_ARTIFACTS.md`](v2/V2_RELEASE_ARTIFACTS.md) defines distribution scope and the SBOM/provenance/notarization boundary without overstating CI artifacts.
- [`v2/V2_SINGLE_MAC_PRODUCTION.md`](v2/V2_SINGLE_MAC_PRODUCTION.md) defines lifecycle, diagnostics, recovery, and rollback for the reviewed single-Mac profile; [`v2/V2_BACKUP_RESTORE.md`](v2/V2_BACKUP_RESTORE.md) defines the coherent backup/restore boundary.
- [`TESTING.md`](TESTING.md), [`VERSIONING.md`](VERSIONING.md), [`DEPLOYMENT.md`](DEPLOYMENT.md), and [`v2/STATUS.md`](v2/STATUS.md) provide the compatibility, release, operational, and acceptance evidence referenced by this gate.

Closing #213 does not mean every future-platform or hosted-deployment issue is complete. #104/#111/#115 and the bounded #96 investigation are complete. #215 Cloud Run design is complete but hosted support remains a future NO-GO implementation/acceptance track. The active `0.4.0` candidate now integrates Recovery & Reconciliation with multi-principal identity and semantic authorization; #217/#227/#228 and #139 retain explicit support-claim acceptance boundaries as documented in the roadmap.
