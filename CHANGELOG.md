# Changelog

## v0.4.0 — 2026-09-03

V2 Recovery, Identity & Semantic Authorization release. This release consolidates the post-v0.3 recovery/reconciliation hardening, provider-neutral multi-principal identity, and narrow typed semantic authorization into one reviewed minor release without widening unaccepted platform/provider support claims.

### Recovery and execution safety

- durable recovery/reconciliation and operator guidance were tightened across current-state acceptance, historical Human resolution, runtime/tool skew detection, recovery-key readiness, and replay-tombstone handling (#103/#115/#136/#137/#253/#254/#255/#256);
- execution-safety durable schema v12 records only bounded semantic-authorization admission evidence (snapshot revision/digest plus constraint kind/rule ID); v11 and earlier supported state remains readable, while a downgrade that would discard v12 semantic evidence fails closed;
- permanent no-auto-replay, `Indeterminate` quarantine, exact operation ownership, and pre-dispatch cancellation semantics remain authoritative.

### Identity and authorization

- provider-neutral signed OIDC/JWT caller identity verifies exact issuer/audience, asymmetric algorithm allowlists, pinned HTTPS JWKS, bounded cache/unknown-`kid` refresh, and maps only verified `issuer + subject` into the existing exact principal/device/capability authorizer (#139 / PR #269);
- typed semantic authorization adds narrow-only constraints at the finalized command boundary: a UTF-8 byte ceiling for `TypeText` and normalized requested-origin allowlists for `BrowserNavigate` (#221 / PR #271);
- semantic decisions are bound to an immutable revision+digest snapshot, recorded without raw text/URL/policy payloads, and fenced again before provider dispatch; stale snapshot identity cancels before dispatch rather than becoming indeterminate;
- no generic expression language, regex policy escape hatch, caller-controlled hot reload, or backend-private authorization namespace is introduced.

### Product and operability

- filesystem observation roots are separated from process working-directory roots (#104);
- reproducible V1 latency/concurrency benchmarking is available as informational product evidence (#111);
- the Unix explicit-session-detachment investigation is closed with the portable process-group guarantee documented; stronger optional Linux cgroup-v2 containment remains future #267 work (#96);
- the `0.4.0` roadmap now treats Cloud Run #215 as design-complete but unsupported future hosted work rather than a release claim.

### Compatibility and support claims

- `v0.4.0` is a pre-1.0 minor compatibility boundary and must be deployed as a version-paired Hub/Agent/maintenance/recovery/Handoff set; mixed/incompatible schema or durable-state representations continue to fail closed;
- the GitHub Release remains source-only unless reviewed binary assets, SBOM/license inventory, and provenance/attestation are explicitly attached; CI release-candidate artifacts are evidence, not automatically supported installers;
- generic signed-token support remains withheld until #139 physical/dogfood acceptance is recorded; Windows Hello recovery (#227), Linux FIDO2 UV recovery (#228), and cross-platform parity (#217) remain support-claim acceptance work only;
- Cloud Run remains unsupported, and Linux/Windows CI artifacts do not become official binary-installer claims.

### Acceptance evidence

- #221 merged after local full regression (`530 passed / 0 failed`, six existing physical-only tests ignored), warning-free all-target clippy, synchronized EN/JA docs, and all 15 GitHub checks green;
- the standing Product Readiness gate is rerun by the dedicated `release/v0.4.0` PR and tracked in #272 before the immutable tag/GitHub Release is created.

## v0.3.0 — 2026-08-27

V2 Production Hardening / Operational Readiness release. The final #100 trusted physical-macOS Secure Enclave/user-presence acceptance passed on merged release-candidate code: a real ambiguous desktop operation was resolved through local-user-authorized online recovery, the durable quarantine cleared only after verified authorization, Hub restart preserved the resolution, and the old operation was never replayed. The stale release PR #99 was superseded by a fresh release snapshot from current `main`.

### Execution safety and recovery

- privacy-preserving, read-only quarantine inspection with explicit `blocking_operation_id`, plus candidate correlation that never becomes completion/replay authority (#116);
- version-paired Hub/`v2_maint` offline recovery with pre-publication durable writer-compatibility checks (#117);
- exact signed self-reconciliation for supported terminal evidence, bounded unknown-outcome retirement for reviewed low-impact legacy ambiguity, and a first-class cross-Hub/Agent reconciliation-readiness audit without raw checkpoint archaeology (#133);
- partial text/input effect resolution, privacy-preserving evidence envelopes, and the execution-safety schema-v8 restricted recovery-evidence read lane while mutation remains quarantined (#179/#181/#180);
- permanent no-auto-replay and persistence-gated quarantine semantics remain unchanged across reconnect, restart, recovery, retirement, and evidence collection.

### Human Handoff and runtime boundary

- first-class optional Handoff coordination is integrated into the controlled Agent for Window and Terminal/PTY surfaces while the Hub retains CUMG authorization/ledger/quarantine and conservative dispatch fencing (#152);
- legacy/current launchd runtime coexistence fails closed instead of allowing ambiguous double-runtime ownership (#157);
- Handoff unavailability does not silently bypass the coordinator once the deployment enables it.

### Diagnostics and operability

- live control schema v9 carries bounded privacy-safe execution failure classes through Agent -> Hub -> northbound MCP without exposing host paths, commands, environment values, device identity, or raw OS/provider errors (#141);
- `v2_doctor` distinguishes an exact in-band diagnostic self-observation from a real blocking quarantine without mutating restart-safety state (#194);
- browser staging startup reports bounded local initialization stage/I/O classes while preserving fail-closed private staging (#143);
- controlled `StorageFull` fault injection confirms durable Agent checkpoint exhaustion can surface remotely as `agent_offline`; failed publication preserves the prior committed checkpoint/replay barriers, normal service-manager restart/authenticated reconnect succeeds after writable capacity returns, and doctor exposes coarse read-only state/temp capacity warnings (#112).

### Acceptance status

- merged-main regression coverage includes warning-free Rust gates, V1 quality/conformance preservation, pinned-Cua Linux/macOS/Windows smoke, privacy/no-replay durability tests, and the previously accepted physical Desktop/Window/Terminal Handoff evidence applicable to their respective changes;
- #100 trusted physical local-user online recovery passed with Secure Enclave user presence, durable resolution across Hub restart, quarantine remaining clear, and `operation_replayed=false`; deferred stabilization/enhancement issues remain outside the v0.3.0 release gate.

## v0.2.0 — 2026-08-13

V2 complete.

- uncertainty-aware execution safety with authoritative operation ownership and generation fencing;
- durable `indeterminate` quarantine with explicit auditable resolution and no automatic replay;
- restart/reconnect-safe persistence and fixed-set multi-device invariant proof;
- backend portability/replacement seams without duplicating the execution-safety state machine;
- standard OAuth northbound and TLS/gRPC southbound boundaries;
- payload-safe structured observability and OpenTelemetry support;
- 10k-operation, reconnect/generation-churn, and RSS-plateau regression coverage;
- trusted real-Cua desktop acceptance on merged `main`.

Non-goals remain generic fleet/platform infrastructure, a generic device fabric, remote desktop, and a generic delegated-authorization protocol.

## v0.1.0 — 2026-08-11

V1 complete: hardened MCP-to-computer-use gateway with local/remote transport, deny-by-default tool policy, cancellation/serialization/resource regression coverage, and trusted real-desktop acceptance.
