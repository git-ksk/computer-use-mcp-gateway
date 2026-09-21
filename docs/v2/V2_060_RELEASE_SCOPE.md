# V2 v0.6.0 release scope and acceptance

> English is canonical. Japanese translation: [V2_060_RELEASE_SCOPE.ja.md](V2_060_RELEASE_SCOPE.ja.md).

Release: **v0.6.0 — Managed Developer Execution**

## Admitted release scope

- #106: separately authorized managed long-running developer jobs with bounded lease, hard lifetime, concurrency, output, stable opaque refs, explicit stop, and fail-closed asynchronous termination handling.
- #114: separately authorized sandboxed Playwright/E2E execution through an operator-configured Docker/Podman-compatible provider with digest-pinned image identity, network none, read-only workspace, private writable artifacts, sanitized environment, resource ceilings, provider-absence terminal proof, and no replay after ambiguity.
- #267: optional Linux-only cgroup-v2 process/shell hardening. It activates only with an explicitly reviewed delegated root, pre-exec placement, kernel `nsdelegate` cgroup-namespace containment, `cgroup.kill`, and `populated 0` terminal proof. Portable Unix/macOS process-group and Windows Job Object claims are unchanged.
- #335: this final integration, compatibility, artifact, upgrade, dogfood, documentation, and Product Readiness gate.

Managed jobs remain a distinct capability family and are not a persistence escape from generic shell/process. Playwright authority remains separate from managed jobs. Linux cgroup containment is process-lifecycle hardening, not a Playwright/filesystem/network sandbox.

## Release identity and compatibility

The v0.6.0 release snapshot pins:

- crate/package: **0.6.0**;
- control schema: **12**;
- capability-advertisement schema: **8**;
- persisted device-registry schema: **8**;
- Hub-Agent transport schema: **6**.

Historical v0.5.0 is control 10 / capability 6 / registry 8 / Hub-Agent 6. Historical #106 development used live control 11 / capability 7. Registry restore accepts the exact reviewed historical registry/capability pairings, discards stale historical capability advertisements, marks devices offline, and requires a fresh current Agent advertisement. Live old/new control or capability schemas do not roll; they fail closed.

Upgrade from v0.5.0 is therefore a **paired Hub/Agent/maintenance/runtime upgrade**, not a rolling mixed-version deployment. New optional Playwright and Linux-cgroup configuration is absent by default, so a valid v0.5.0 configuration does not silently gain authority on upgrade.

Rollback after v0.6.0 has written current registry/capability state requires the pre-upgrade v0.5.0 rollback checkpoint plus the version-paired v0.5.0 binaries/configuration. A v0.5.0 binary is not expected to reinterpret a v0.6.0 capability-8 checkpoint. Do not hand-edit state or move release tags.

## Acceptance evidence

- #106, #114, and #267 are closed before this gate.
- Local deterministic release-gate checks include all-target format/check/clippy/tests, docs/link validation, schema migration/mixed-version tests, release-candidate manifest tests, artifact install/upgrade/rollback tests, and V1 regression/conformance preservation.
- #267 Linux CI executes against a real delegated cgroup-v2 subtree and proves detached `setsid()` cleanup, fork-race cleanup, `nsdelegate` migration containment, explicit unavailable behavior for bad delegation, and unchanged portable process-group regressions.
- Managed-job lifecycle is dogfooded on the trusted physical macOS host using the real process supervision domain; start/renew/stop and lease/output lifecycle tests do not use background-shell escape.
- Playwright real-provider acceptance passed on the trusted Apple-silicon Mac using Docker 29.5.3, `@playwright/test` 1.63.0, and digest-pinned arm64 image `sha256:a0f4498920a5dbac63196d9140ed738ef00470f27e2e74029abd8850b7bd5717`. A real offline Chromium test completed with the fixed `network none` profile and terminal success was exposed only after provider cleanup. The compiled surface still never implies that an unavailable provider is supported.
- Release Candidate Artifacts build/verify/smoke the exact release identity on Linux, macOS, and Windows. CI archives remain evidence, not official binary installers.
- Existing macOS/Windows process/shell, Handoff, recovery, quarantine, and no-auto-replay regressions remain required.

## Product Readiness snapshot

The standing [Product Readiness checklist](../PRODUCT_READINESS.md) is applied to this exact release scope:

- distribution identity is exact-commit/package/schema bound by release-manifest schema v3 and installed runtime-manifest schema v4;
- clean install and upgrade paths remain fail closed and require pre-provisioned trust/secrets plus the documented platform service-manager inputs;
- authoritative state, rollback, quarantine, and no-auto-replay semantics remain version-paired and are never inferred from publication metadata;
- diagnostics remain privacy-bounded and do not expose command text, host paths, secrets, Playwright grep/test bodies, provider IDs, or cgroup paths northbound;
- optional providers/platform hardening advertise only after their local startup probes succeed;
- dependency review, CodeQL, docs links, native candidate packaging, and protected CI are required on the exact release commit before tagging.

### Product Readiness checklist result

- [x] Distribution scope is explicit: source-only GitHub pre-release; macOS install-capable candidate plus Linux/Windows candidate evidence are not official binary assets.
- [x] Candidate archives use deterministic checksum + closed manifest; tamper, unexpected path/file, symlink, size/digest drift, and fresh-extraction smoke tests are green.
- [x] Candidate allowlists exclude secrets, private endpoints, repository trees, and temporary acceptance data.
- [x] Signing/notarization wording matches reality; CI candidates are not claimed as notarized public installers.
- [x] Official-binary SBOM/provenance publication is scope-N/A because v0.6.0 is source-only; promotion to binary assets remains separately gated.
- [x] Clean single-Mac artifact install orchestration reaches paired service activation plus healthy doctor/status in the deterministic installer harness.
- [x] v0.5.0 persisted registry/capability state (8/6) restores through the reviewed migration without promoting stale advertisements and preserves quarantine/resolution evidence.
- [x] Upgrade/rollback remains version-paired; single-Mac and Windows harnesses preserve rollback assets and reject unsafe/incomplete mixed runtime state.
- [x] Mixed live control/capability versions fail closed; rollback never asks v0.5 binaries to interpret v0.6 live capability state.
- [x] First-run and operator diagnostics remain read-only authority boundaries and retain the documented install -> doctor/status -> semantic/effectful -> recovery path.
- [x] Operational service drain/restart, TLS/key/storage, quarantine/recovery, incident, and backup/restore boundaries remain covered by the standing production docs and unchanged regressions.
- [x] Deterministic restart/reconnect/fault/resource tests and EN/JA operator-critical docs remain synchronized.
- [x] Exact principal/device/capability authorization is preserved; managed-job and Playwright authorities are separate exact capabilities.
- [x] Ambiguous effectful work remains `Indeterminate`/quarantined with no automatic replay.
- [x] Checksums/manifests/provider evidence remain non-authoritative; privacy-bounded telemetry excludes raw command/test/path/provider payloads.

## Support-claim boundary

The artifact may compile implementation whose support claim is narrower than the compiled surface.

- #139 provider-neutral signed OIDC/JWT verification remains compiled, but provider-specific physical signed-token support is **withheld** pending its dogfood acceptance.
- #217 cross-platform recovery parity remains **open**; no blanket cross-platform recovery claim is created by v0.6.0.
- #228 Linux FIDO2 UV recovery remains **withheld** pending trusted physical Linux + real UV-capable FIDO2 acceptance.
- Hosted Cloud Run Hub/Handoff (#215/#275-#284) remains unsupported and outside v0.6.0.
- Recovery evidence research (#289/#290), second-real-backend proof (#222), and unrelated operator UX work are outside this release gate.

## Publication boundary

`v0.6.0` remains a **source-only GitHub pre-release** unless separately reviewed binary assets, SBOM/license inventory, and provenance/attestation are explicitly attached. Verified CI release-candidate archives are release evidence only.

The immutable `v0.6.0` tag and matching GitHub pre-release are created only after this release PR is merged and required push checks plus Release Candidate Artifacts are green on the exact resulting `main` commit.
