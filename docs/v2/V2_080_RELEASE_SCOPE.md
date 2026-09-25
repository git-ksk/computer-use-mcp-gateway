# V2 v0.8.0 release scope and acceptance

> English is canonical. Japanese translation: [V2_080_RELEASE_SCOPE.ja.md](V2_080_RELEASE_SCOPE.ja.md).

Release: **v0.8.0 — Backend Portability & Recovery Safety**

## Admitted release scope

- **#377 — authoritative pre-enqueue non-delivery:** execution-safety schema v15 settles confirmed-not-executed only when the Hub proves the exact first outbound attempt failed before enqueue: encoding failed before enqueue, or the bounded Hub-to-Agent channel returned the unsent frame. Successful enqueue followed by transport/session loss remains ambiguous and quarantined. The old operation is never replayed.
- **#379 — structured semantic-refusal remediation:** existing stable safe codes remain canonical while reviewed refusals may return bounded `required_actor`, `next_action`, `same_operation_replay_safe=false`, `remediation_is_authority=false`, and `fresh_call_required` guidance. Hints do not become authorization, recovery, scope, consent, route, foreground, or replay authority.
- **#380 — constrained execution environment/PATH contract:** process and shell execution keep `env_clear()` plus a reviewed allowlist of Agent service environment keys. `PATH` is inherited only from the service environment when present, login/interactive profiles are not sourced, caller `env.PATH` override remains denied, and reviewed absolute executable paths are the deterministic out-of-PATH route.
- **#222 — second real-backend portability evidence:** macos-mcp 0.4.0 implements a narrow second `ComputerUseBackendAdapter` slice for CUMG `ListApplications`, `MovePointer`, and `PointerClick`. Provider tool names/state remain private; CUMG operation identity, fencing, ambiguity, quarantine, reconciliation, and replay policy remain authoritative.

This is bounded portability/recovery-safety work, not a generic MCP proxy, provider marketplace, VM/fleet scheduler, or alternate Handoff/settlement system.

## Release identity and compatibility

The v0.8.0 release snapshot pins:

- crate/package: **0.8.0**;
- live control schema: **12**;
- capability-advertisement schema: **8**;
- persisted device-registry schema: **8**;
- Hub-Agent transport schema: **6**;
- execution-safety durable schema: **15**;
- Agent M1 persistence schema: **6**.

The live 12/8/8/6 protocol identity and Agent M1 v6 writer contract are unchanged from v0.7.0. The durable Hub execution-safety writer advances from v14 to v15 for #377. Historical v14 state remains readable, but v15-only pre-enqueue non-delivery evidence cannot be lossily represented as v14.

## Upgrade and rollback boundary

Upgrade from v0.7.0 to v0.8.0 remains a **version-paired runtime upgrade** through the reviewed artifact/maintenance path. Equal live schema numbers are not a rolling mixed-version guarantee.

Before upgrading, retain the reviewed pre-upgrade v0.7 rollback checkpoint and paired v0.7 binaries/configuration. Once v0.8 writes execution-safety v15-only evidence, do not run v0.7 Hub or maintenance writers against that newer state. Roll back by restoring the captured pre-upgrade v0.7 state plus the paired v0.7 runtime. Do not hand-edit checkpoints, strip v15 evidence, or move release tags.

#379 and #380 are additive caller/operator contracts and do not weaken authorization. #222 is additional portability evidence; it does not make macos-mcp a feature-parity replacement for Cua.

## Acceptance evidence

- The v0.8.0 milestone has all four admitted issues closed: #377, #379, #380, and #222.
- #377 / PR #382 proves exact pre-enqueue settlement, persistence-before-live-release, bounded reporting, and the unsafe neighbor where successful enqueue/session loss remains `Indeterminate` / quarantined.
- #379 / PR #383 proves deterministic provider-neutral remediation, unknown-code fail-safe behavior, actor separation, and no implicit foreground/route/scope/replay escalation.
- #380 / PR #384 makes the constrained execution environment discoverable without exposing raw PATH values or inheriting ambient interactive-shell authority.
- #222 / PR #385 records trusted-Mac physical macos-mcp 0.4.0 acceptance and proves stale generation/revision has zero dispatch, post-dispatch unproven provider result creates durable quarantine, restart/reconnect retains it, and the old operation cannot replay. See [V2_SECOND_BACKEND_PORTABILITY_ACCEPTANCE.md](acceptance/V2_SECOND_BACKEND_PORTABILITY_ACCEPTANCE.md).
- PR #385 protected CI passed Rust, backend passthrough, CodeQL, dependency/docs checks, Linux/macOS/Windows release-candidate bundles, and native Cua smoke on Linux/macOS/Windows.

## Product Readiness checklist result

The standing [Product Readiness checklist](../PRODUCT_READINESS.md) is applied to this exact release scope:

- [x] Distribution remains source-only GitHub pre-release; verified CI archives are evidence, not official binary assets.
- [x] Candidate archives retain closed manifest/checksum verification and existing signing/notarization boundaries.
- [x] The v0.7 -> v0.8 upgrade is version-paired and the durable v15 rollback boundary is fail closed.
- [x] Mixed/incompatible state never becomes replay or settlement authority.
- [x] Existing single-Mac install, diagnostics, recovery, backup/restore, and paired rollback paths remain the supported reference topology.
- [x] `v2_status`, `v2_doctor`, `cumg_status`, and refusal remediation remain observational/composition guidance only.
- [x] #377 preserves quarantine on every post-enqueue uncertainty and requires a fresh operation ID for later effectful work.
- [x] #379 separates remediation from replay safety and prohibits implicit fallback/escalation.
- [x] #380 documents service-environment PATH behavior without exposing raw values or importing login-shell authority.
- [x] #222 keeps provider provenance distinct from caller/Hub/Agent/Human authority and proves real provider-backed operation plus ambiguity handling.
- [x] Existing Cua regressions are green on Linux, macOS, and Windows.
- [x] EN/JA operator-critical release documentation is synchronized.
- [x] Exact release-commit protected CI and Release Candidate Artifacts must be green before tagging.

## Support-claim boundary

- macos-mcp 0.4.0 is a deliberately narrow portability-evidence adapter slice, not a feature-parity/default replacement for Cua or a generic provider platform.
- #139 signed-token dogfood, #217 cross-platform recovery parity, and #228 physical Linux FIDO2 UV remain on the separate v0.6.1 support-claim evidence track.
- Hosted Cloud Run Hub/Handoff remains assigned to v0.9.0 and unsupported until its acceptance gate closes.
- `v1_gateway` remains legacy/regression-only; deliberate retirement is assigned to v0.10.0.

## Publication boundary

`v0.8.0` is a **source-only GitHub pre-release**. Verified CI release-candidate archives remain evidence only and are not official binary assets. The immutable annotated `v0.8.0` tag and matching GitHub pre-release are created only after the dedicated release PR merges and the exact resulting `main` commit has required protected checks plus Release Candidate Artifacts green.
