# V2-M1 progress — single secure remote Agent

Status: **in progress**. V2-M0 is GO; V2-M1 is not yet accepted or production-ready.

## Implemented foundation

The current `v2-m1-secure-agent` implementation adds these M1 building blocks while preserving the M0 application-layer identity and capability controls:

- TLS 1.3-only Hub↔Agent transport configuration using a pinned trust root and the dedicated `cumg-hub-agent/1` ALPN;
- application-layer Ed25519 Hub/Agent authentication and signed session/command/result/cancellation messages remain above TLS rather than being replaced by transport identity;
- signed Agent heartbeat and Hub acknowledgement messages bound to the authenticated connection transcript;
- monotonically increasing heartbeat sequence enforcement, generation matching, timeout/offline detection, and bounded exponential reconnect policy;
- a reusable outbound lifecycle runner that bounds consecutive connection/session failures, resets the failure streak after an established session, and reconnects without transferring prior session generation state;
- one-device routing that rejects offline, wrong-device, stale-generation, stale-capability, and unsupported-capability commands;
- restart snapshots for device registry, grant verifier/revocation/consumption state, Hub operation state, and Agent terminal-operation replay barriers;
- append-only checkpoint files with bounded size, `create_new`, flush/fsync, symlink rejection, and restrictive Unix directory/file permission checks;
- restart conversion of queued/pre-dispatch work to `cancelled` and dispatched/cancel-requested work to `indeterminate`, so process restart never makes ambiguous work runnable again;
- an asynchronous Cua MCP semantic adapter that reuses the V1 request-level cancellation path, normalizes `list_apps`/`get_screen_size`, and classifies propagated cancellation or timeout as `indeterminate` rather than claiming the desktop action definitely stopped;
- Hub-side device quarantine for `indeterminate` operations: a different operation on the same device is rejected until an explicit resolution records the ambiguous operation as confirmed completed or confirmed not executed.

The persisted checkpoint intentionally contains **no private signing keys**. Hub transport, Agent device, grant-signing, and TLS private key material still require an explicit production secret-storage/provisioning design. Persisting public trust and replay state is necessary for fail-closed restart semantics, but is not a substitute for private-key custody.

## Evidence currently covered by tests

The repository tests prove the TLS wrapper negotiates TLS 1.3 with the dedicated ALPN and rejects an untrusted server certificate. Runtime tests also cover heartbeat replay/generation/timeout handling, capped reconnect backoff, one-device routing, consumed/revoked grant persistence, revoked device/generation persistence, queued/in-flight crash recovery, and operation replay barriers. A second encrypted integration test runs two outbound TLS sessions from the same Agent identity, proves the Hub advances device generation from 1 to 2, and confirms a command bound to generation 1 is rejected after generation 2 becomes current. The deterministic MCP backend fixture also proves that V2 cancellation propagation references the exact in-flight downstream MCP request ID; the resulting state is deliberately classified `indeterminate` and quarantines that device.

The existing M0 live-Cua PoC continues to prove the semantic path from an authorized client principal through a short-lived grant and bounded Hub/Agent execution to the Cua adapter. M1 now also has an end-to-end integration test that composes the TLS channel with the application protocol in one outbound connection: TLS 1.3 + dedicated ALPN, Ed25519 Hub/Agent authentication, signed session acceptance, signed heartbeat/ack, one-device routing, bounded admission and lease ownership, short-lived grant validation on the Agent, and a signed typed result.

## Still required before V2-M1 acceptance

- package the reusable lifecycle into an operator-facing long-lived Agent process/service and wire heartbeat-timeout detection to that service lifecycle; the lifecycle runner and two-session encrypted reconnect/generation test are implemented;
- define production certificate/private-key provisioning and rotation without committing secrets to repository state;
- integrate a real northbound authenticated identity source with `AuthenticatedClientPrincipal` rather than constructing the principal inside a PoC;
- run cancellation acceptance against real Cua desktop operations; the deterministic MCP fixture already proves exact-request propagation and indeterminate quarantine, but it cannot prove a real desktop action stopped;
- add deployment-level connection/rate limits and operational observability for the M1 service boundary.

Do not start V2-M2 multi-machine routing until the single-device M1 acceptance path is complete.
