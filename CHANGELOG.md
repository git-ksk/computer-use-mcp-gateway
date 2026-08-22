# Changelog

## v0.3.0 — 2026-08-16

V2 production hardening.

- graceful shutdown, bounded persistence, crash-loop handling, and quarantine alerting/resolution hardening;
- trusted-proxy abuse guards plus Agent reauthentication/device rotation, enrollment/trust-anchor, and external grant-signer hardening;
- durable process/shell operation-result recovery with caller-supplied operation IDs and read-only `get_operation`, without replay or automatic retry;
- explicit locale environment support and stable coarse northbound environment-policy rejection codes without value leakage;
- bounded background-process lifetime semantics, including cleanup of ordinary descendants that remain inside the supervised Unix process group / Windows Job Object;
- atomic checkpoint publication using private pending files, fsync, and no-clobber publication so ENOSPC or mid-write failures cannot supersede the last committed checkpoint;
- expanded restart, recovery, ownership, persistence, compatibility, and cross-platform CI regression coverage.

Known follow-ups remain outside the v0.3 release scope, including retrievable references for truncated output (#83) and stronger Unix containment for deliberately detached `setsid()` descendants (#96).

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
