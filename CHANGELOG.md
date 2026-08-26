# Changelog

## v0.3.0 — Unreleased

Production-hardening and operational-readiness candidate. This version is **not released yet**. Final release remains gated on issue #100 trusted physical-macOS Secure Enclave/user-presence acceptance and proof that a real ambiguous desktop operation is resolved without replay. The older release PR #99 predates substantial merged-main work and must be refreshed or replaced from current `main` after that acceptance.

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
- the remaining `v0.3.0` release acceptance is intentionally the #100 physical local-user online-recovery flow, not the deferred stabilization/enhancement backlog.

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
