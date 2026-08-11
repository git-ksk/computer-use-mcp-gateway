# V2-M0 control-plane PoC

Status: **in progress — no GO decision yet**.

This PoC deliberately tests the control semantics before selecting or building a Hub↔Agent transport. V1 remains unchanged.

## Competitor overlap and candidate gap

Current overlap is substantial:

- Cua Driver already supplies a cross-platform computer-use backend, daemon/session policy, bounded permission modes, explicit browser-profile authorization, and MCP/CLI/SDK integration.
- Desktop Commander already supplies a cloud Remote MCP path, device-side outbound connectivity, OAuth 2.0 with PKCE, reconnection, and multi-device support.
- Desktop Commander's public issue tracker currently includes reports of intermittent wrong-device routing and process/search session device-affinity failures in multi-device mode (#587 and #602). Those reports are evidence of a control-plane class of problem, not proof that Desktop Commander lacks every mitigation.

The candidate gap for this project is therefore narrower than "remote computer use":

> A backend-neutral delegated device capability control plane in which Hub-issued, cryptographically signed, short-lived grants authorize one semantic capability on one enrolled device; operation leases prevent conflicting ownership; stale generation/capability state, replay, revocation, expiry, and reconnect all fail closed; and audit evidence excludes raw screenshots, tool arguments, and tool results.

Primary references reviewed on 2026-08-11:

- Cua Driver: https://github.com/trycua/cua/blob/main/libs/cua-driver/README.md
- Desktop Commander Remote MCP release notes: https://github.com/wonderwhy-er/DesktopCommanderMCP/releases
- Desktop Commander issue tracker: https://github.com/wonderwhy-er/DesktopCommanderMCP/issues

## Implemented prototype

`src/v2_m0.rs` implements an isolated control-plane model:

- Ed25519 device identity and enrollment challenge proof;
- device connection generation counters;
- backend/platform/version capability advertisements with capability revision;
- Ed25519-signed short-lived grants;
- one-shot grant consumption plus expiry, revocation, and replay rejection;
- exact semantic capability enforcement (`observe` does not authorize `interact`);
- per-device operation leases tied to device generation;
- reconnect generation changes that do not transfer an existing lease;
- typed backend-neutral command envelopes and typed result envelopes;
- audit evidence containing IDs, policy outcome, and reason but no raw command/result/screenshot fields.

The unit tests are intentionally transport-independent so these semantics survive a later WebSocket, QUIC, gRPC, or other transport choice.

`src/v2_m0_transport.rs` adds a separate transport-facing PoC layer with:

- Agent-initiated outbound connectivity with no Agent listener;
- a pinned Ed25519 Hub identity authenticated by the Agent;
- proof of the enrolled Ed25519 device identity authenticated by the Hub;
- fresh Agent/Hub nonces that bind each handshake transcript and reject proof replay;
- signed session acceptance, commands, and results bound to those connection nonces, so post-handshake payload tampering fails closed;
- a grant-signing key that is separate from the Hub transport identity;
- bounded 64 KiB length-prefixed JSON frames;
- versioned typed Hub↔Agent messages;
- Agent-side short-lived grant validation before backend execution.

The network PoC deliberately binds only to loopback TCP. It proves the outbound authentication and application-message integrity semantics, not production remote confidentiality. A remote implementation must add authenticated TLS or an equivalently reviewed secure tunnel without changing the backend-neutral command/grant contract.

## One-device live backend proof

Run:

```bash
CUMG_BACKEND_COMMAND="$HOME/.local/bin/cua-driver" cargo run --bin v2_m0_poc
```

The PoC:

1. generates a cryptographic device identity;
2. proves possession during enrollment;
3. connects the device with a versioned Cua capability advertisement;
4. issues a 60-second `observe` grant;
5. authorizes a backend-neutral `ListApplications` command;
6. acquires the device operation lease;
7. executes a real `cua-driver call list_apps {}` operation;
8. releases the lease;
9. rejects replay of the consumed grant;
10. rejects `PointerClick` under a separate `observe` grant before any backend action;
11. rejects revoked and expired grants;
12. reconnects the same device with a new generation and proves it cannot take over a still-live lease from the old generation;
13. emits only summary/audit evidence, never the backend's raw app list.

## Outbound authenticated Agent proof

Run:

```bash
CUMG_BACKEND_COMMAND="$HOME/.local/bin/cua-driver" cargo run --locked --bin v2_m0_network_poc
```

This slice starts a loopback Hub listener, then starts an Agent that initiates the connection outbound. The Agent does not expose a listening socket. The peers perform a fresh-nonce mutual Ed25519 handshake: the Agent verifies a pre-pinned Hub identity, and the Hub verifies the already-enrolled device identity. The Hub then signs the accepted session and each command against the connection nonces, while the Agent signs each result against the same connection context. Only after those checks does the Hub establish the device generation, issue a short-lived `observe` grant, acquire the operation lease, and send a typed `ListApplications` command. The Agent validates the session and grant locally before executing the real Cua backend call and returning only a typed application count.

The frame codec rejects payloads over 64 KiB before allocation of the declared payload, and the PoC emits no raw backend app list, screenshots, arguments, or result bodies.

## What this PoC does not prove yet

These PoCs do **not** yet satisfy the complete V2-M0 GO gate. In particular, it does not yet implement or prove:

- production remote transport confidentiality/integrity beyond the loopback-only authenticated TCP PoC;
- Hub identity/key rotation and Agent credential rotation;
- MCP-client→Hub authorization mapping to device/capability grants;
- distributed cancellation/backpressure behavior across a real transport;
- compromised-Hub/Agent/backend/client threat-model analysis;
- backend-adapter conformance across more than Cua on this one machine.

Until those remaining items are designed and reviewed, the V2-M0 decision remains **PENDING**, not GO.

## Recorded local run — 2026-08-11

The first operator-controlled run passed on macOS arm64 against Cua Driver 0.19.3. The real observe operation returned an app-array count of 77; the raw list was not emitted by the PoC. The same run confirmed:

- cryptographic enrollment: PASS;
- short-lived observe grant + real backend call: PASS;
- observe grant used for interact: REJECTED;
- consumed grant replay: REJECTED;
- revoked grant: REJECTED;
- expired grant: REJECTED;
- reconnect generation attempting to take a live prior-generation lease: REJECTED.

This is evidence for the local control semantics only. It does not change the V2-M0 GO/NO-GO state from **PENDING**.

## Recorded outbound network run — 2026-08-11

The first outbound network run passed on the same macOS arm64 machine against Cua Driver 0.19.3. The Agent initiated a loopback TCP connection to the Hub, authenticated the pinned Hub identity, proved the enrolled device identity back to the Hub, validated a short-lived `observe` grant locally, and executed a real `ListApplications` command. The typed result reported an application count of 77; the raw application list was not logged or returned by the PoC.

Additional transport tests confirmed:

- unpinned Hub identity: REJECTED;
- forged Agent proof: REJECTED;
- Agent proof replay against a fresh Hub nonce: REJECTED;
- signed command payload tampering: REJECTED;
- signed result payload tampering: REJECTED;
- oversized declared frame: REJECTED before payload allocation/read;
- bounded typed frame round-trip: PASS.

This proves the outbound authenticated control hop on loopback only. It does not claim encrypted remote transport or change the V2-M0 GO/NO-GO state from **PENDING**.
