# V2-M1 progress — single secure remote Agent

Status: **in progress**. V2-M0 is GO; V2-M1 is not yet accepted or production-ready.

## Implemented foundation

The current `v2-m1-secure-agent` implementation adds these M1 building blocks while preserving the M0 application-layer identity and capability controls:

- TLS 1.3-only Hub↔Agent transport configuration using a pinned trust root and the dedicated `cumg-hub-agent/1` ALPN;
- application-layer Ed25519 Hub/Agent authentication and signed session/command/result/cancellation messages remain above TLS rather than being replaced by transport identity;
- signed Agent heartbeat and Hub acknowledgement messages bound to the authenticated connection transcript;
- monotonically increasing heartbeat sequence enforcement, generation matching, timeout/offline detection, and bounded exponential reconnect policy;
- one-device routing that rejects offline, wrong-device, stale-generation, stale-capability, and unsupported-capability commands;
- restart snapshots for device registry, grant verifier/revocation/consumption state, Hub operation state, and Agent terminal-operation replay barriers;
- append-only checkpoint files with bounded size, `create_new`, flush/fsync, symlink rejection, and restrictive Unix directory/file permission checks;
- restart conversion of pre-dispatch work to `cancelled` and dispatched/cancel-requested work to `indeterminate`, so process restart never makes ambiguous work runnable again.

The persisted checkpoint intentionally contains **no private signing keys**. Hub transport, Agent device, grant-signing, and TLS private key material still require an explicit production secret-storage/provisioning design. Persisting public trust and replay state is necessary for fail-closed restart semantics, but is not a substitute for private-key custody.

## Evidence currently covered by tests

The repository tests prove the TLS wrapper negotiates TLS 1.3 with the dedicated ALPN and rejects an untrusted server certificate. Runtime tests also cover heartbeat replay/generation/timeout handling, capped reconnect backoff, one-device routing, consumed/revoked grant persistence, revoked device/generation persistence, and crash/restart operation replay barriers.

The existing M0 live-Cua PoC continues to prove the semantic path from an authorized client principal through a short-lived grant and bounded Hub/Agent execution to the Cua adapter. The next M1 integration slice must compose that protocol over the encrypted TLS channel rather than treating TLS and the control protocol as separate tests.

## Still required before V2-M1 acceptance

- compose TLS + Ed25519 Hub/Agent handshake + heartbeat + typed command/result in one end-to-end connection;
- package an actual long-lived outbound Agent connection loop using bounded reconnect and heartbeat timeout behavior;
- define production certificate/private-key provisioning and rotation without committing secrets to repository state;
- integrate a real northbound authenticated identity source with `AuthenticatedClientPrincipal` rather than constructing the principal inside a PoC;
- prove live cancellation against backend operations, including the explicit fail-closed behavior for operations that cannot be interrupted safely;
- add deployment-level connection/rate limits and operational observability for the M1 service boundary.

Do not start V2-M2 multi-machine routing until the single-device M1 acceptance path is complete.
