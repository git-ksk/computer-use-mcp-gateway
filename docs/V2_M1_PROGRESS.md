# V2-M1 progress — single secure remote Agent

Status: **in progress**. V2-M0 is GO; V2-M1 is not yet accepted or production-ready.

## Transport migration: gRPC bidi production candidate

The M1 transport strategy now keeps the protocol boundary flexible while selecting **gRPC bidirectional streaming over TLS** as the production candidate. The earlier raw TLS + bounded JSON framing implementation remains in the repository as a regression/reference transport instead of being deleted. This allows direct behavioral comparison while the new transport matures.

The first gRPC slice deliberately minimizes simultaneous change: Protobuf defines the bidirectional `AgentControl/OpenSession` RPC and bounded `AgentFrame`/`HubFrame` carriers, while the payload is the existing independently signed V2 application message. gRPC therefore replaces custom stream framing and supplies HTTP/2 streaming/flow-control semantics without weakening or duplicating Ed25519 identity, signed session/command/result messages, short-lived grants, generation checks, replay barriers, leases, or cancellation semantics. Native Protobuf fields for the application envelope can be migrated later without making that rewrite a prerequisite for validating the transport.

A TLS-enabled integration test now proves one gRPC bidirectional session carrying the existing V2 authentication/heartbeat protocol and an Agent-native structured `git status --short` execution. The command is executed by the Rust process executor directly, with no Cua or Terminal GUI path. The repo still runs the equivalent raw-TLS process E2E alongside it. A second service-lifecycle E2E deliberately terminates the first authenticated stream, verifies outbound reconnect advances the device generation, then starts a long-running direct process and cancels it over the same live gRPC stream; the Agent emits a signed cancellation acknowledgement and a signed cancelled process result only after the child has been killed and waited.

The initial deployment candidate for this transport is a small always-on VM Hub, with Agents making outbound connections. Serverless/WebSocket transports remain possible future adapters rather than constraints on the application protocol.

## Shell-first Agent direction

The M1 Agent is now explicitly planned as a self-owned remote execution agent, not merely a secure transport wrapper around Cua. The next implementation priority is **direct process/shell execution inside the Agent**. This lets common developer workflows such as `git`, `cargo`, `npm`, `xcodebuild`, and `fastlane` run without opening Terminal, synthesizing GUI input, or scraping terminal output through a computer-use backend.

The intended capability layering is:

```text
Hub
  |
  v
self-owned Agent
  +-- direct process executor      <- implemented in M1
  +-- explicit shell executor      <- separate, higher-risk capability
  +-- bounded filesystem surface   <- follows shell/process needs
  +-- GUI/computer-use adapter
       +-- Cua                     <- transition/default GUI backend
       +-- future native backends  <- later
```

Structured process execution is the safer **API shape**: an explicit executable, argument vector, working directory, bounded environment, output limits, timeout, and cancellation semantics, with no implicit `sh -c`. It is still a `Dangerous` capability, not a sandbox: executable scripts/interpreters can run arbitrary code, and argv may name files outside the allowed cwd. The Agent therefore now requires an exact `DeviceCapability::ExecuteProcess` grant instead of accepting a class-only `Dangerous` grant. A future free-form shell command must receive its own exact capability scope. Filesystem authority remains a separate M1 gap.

This ordering changes implementation priority, not the trust model. Existing TLS, identity, grants, leases, replay protection, admission control, cancellation state, and audit rules remain the security boundary for shell/process capabilities. GUI support is not removed: Cua remains available behind the adapter contract while the Agent gains direct shell utility first.

## Implemented foundation

The current `v2-m1-secure-agent` implementation adds these M1 building blocks while preserving the M0 application-layer identity and capability controls:

- the raw-TLS regression transport is TLS 1.3-only with pinned trust and the dedicated `cumg-hub-agent/1` ALPN; the gRPC production candidate uses TLS + HTTP/2 with pinned certificate trust/domain validation and keeps Ed25519 application identity above the transport;
- application-layer Ed25519 Hub/Agent authentication and signed session/command/result/cancellation messages remain above TLS rather than being replaced by transport identity;
- signed Agent heartbeat and Hub acknowledgement messages bound to the authenticated connection transcript;
- monotonically increasing heartbeat sequence enforcement, generation matching, timeout/offline detection, and bounded exponential reconnect policy;
- a reusable outbound lifecycle runner that bounds consecutive connection/session failures, resets the failure streak after an established session, and reconnects without transferring prior session generation state;
- an operator-facing `v2_agent` binary that loads the separate device/Hub/grant/TLS trust material, opens the outbound gRPC/TLS session, maintains signed heartbeats, reconnects with bounded exponential backoff, handles Ctrl-C shutdown, and keeps the receive loop responsive while direct process work runs on a blocking worker;
- `v2_agent` persists Agent replay/trust checkpoints in its operator-selected state directory: consumed grants are fsynced before execution proceeds, an active operation is checkpointed before the child is spawned, and terminal operation IDs survive process restart; startup restores the latest checkpoint and fails closed on device/trust-anchor mismatch;
- live Agent-native process cancellation over the gRPC session: a signed `Cancel` flips the process cancellation token without blocking stream receive; Unix process groups and Windows Job Objects terminate the process tree, background descendants are also cleaned up when the top-level process exits, the operation ID becomes terminal, and signed cancellation/result evidence is returned;
- one-device routing that rejects offline, wrong-device, stale-generation, stale-capability, and unsupported-capability commands;
- restart snapshots for device registry, grant verifier/revocation/consumption state, Hub operation state, and Agent terminal-operation replay barriers;
- crash-safe checkpoint files with bounded per-file size, `create_new`, flush/fsync, symlink rejection, restrictive Unix directory/file permission checks, and a bounded 64-checkpoint retention window; consumed-grant tombstones are pruned after their enforced grant expiry;
- restart conversion of queued/pre-dispatch work to `cancelled` and dispatched/cancel-requested work to `indeterminate`, so process restart never makes ambiguous work runnable again;
- an asynchronous Cua MCP semantic adapter that reuses the V1 request-level cancellation path, normalizes `list_apps`/`get_screen_size`, and classifies propagated cancellation or timeout as `indeterminate` rather than claiming the desktop action definitely stopped;
- Hub-side device quarantine for `indeterminate` operations: a different operation on the same device is rejected until an explicit resolution records the ambiguous operation as confirmed completed or confirmed not executed;
- a separate key-material boundary for Agent device, Hub transport, and grant-signing Ed25519 secrets plus public trust anchors: secret files are created with restrictive permissions, symlinks and weak permissions fail closed, and TLS root material is loaded separately from replay checkpoints.

The persisted replay/trust checkpoint intentionally contains **no private signing keys**. File-based key provisioning is now defined and tested, but a production deployment may still choose an OS keychain, HSM/KMS, or another reviewed secret store instead of filesystem secrets. Persisting public trust and replay state is necessary for fail-closed restart semantics, but is not a substitute for private-key custody.

## Evidence currently covered by tests

The repository tests prove the TLS wrapper negotiates TLS 1.3 with the dedicated ALPN and rejects an untrusted server certificate. Runtime tests also cover heartbeat replay/generation/timeout handling, capped reconnect backoff, one-device routing, consumed/revoked grant persistence, revoked device/generation persistence, queued/in-flight crash recovery, and operation replay barriers. A second encrypted integration test runs two outbound TLS sessions from the same Agent identity, proves the Hub advances device generation from 1 to 2, and confirms a command bound to generation 1 is rejected after generation 2 becomes current. The deterministic MCP backend fixture also proves that V2 cancellation propagation references the exact in-flight downstream MCP request ID; the resulting state is deliberately classified `indeterminate` and quarantines that device.

The existing M0 live-Cua PoC continues to prove the semantic path from an authorized client principal through a short-lived grant and bounded Hub/Agent execution to the Cua adapter. M1 now also has an end-to-end integration test that composes the TLS channel with the application protocol in one outbound connection: TLS 1.3 + dedicated ALPN, Ed25519 Hub/Agent authentication, signed session acceptance, signed heartbeat/ack, one-device routing, bounded admission and lease ownership, short-lived grant validation on the Agent, and a signed typed result.

A separate operator-controlled M1 backend run on 2026-08-11 connected the asynchronous `CuaMcpAdapter` to Cua Driver 0.19.3 through its MCP transport. `ListApplications` normalized to an application count of 77 and `ScreenGeometry` normalized to 1920×1080 points with scale factor 1.0; the PoC did not emit the raw application list. This proves the asynchronous semantic adapter against the real backend for observe operations, not real-Cua cancellation.

## Still required before V2-M1 acceptance

The `v2_agent` process now integrates the direct process executor, gRPC/TLS outbound lifecycle, heartbeat timeout/reconnect behavior, cancellation, the Agent-side file-based key/trust boundary, and restart-safe replay checkpoints. Remaining M1 work is narrower, but there are still production blockers:

- implement the operator-facing single-device Hub gRPC daemon for the always-on VM target; the current gRPC server is test-only, so `v2_agent` is not yet deployable end-to-end as a product service;
- bound/prune terminal operation replay tombstones inside a very long-lived device generation. Checkpoint file count and consumed-grant state are now bounded/pruned, but terminal operation IDs can still accumulate until generation rollover;
- treat `allowed_cwd_root` as working-directory policy only, not filesystem confinement. Before exposing narrower filesystem claims, add explicit path-scoped filesystem capabilities or an OS sandbox;
- define the separate higher-risk shell-command capability and the minimum bounded filesystem surface needed for practical repository/build workflows;
- add OS-specific service packaging/installation (for example launchd/systemd) around the now-runnable long-lived `v2_agent` process;
- integrate the Hub-side key boundary into the operator-facing Hub service and document the chosen production secret-store/certificate rotation procedure; the repository does not commit generated private keys;
- integrate a real northbound authenticated identity source with `AuthenticatedClientPrincipal` rather than constructing the principal inside a PoC;
- run cancellation acceptance against real Cua desktop operations; the deterministic MCP fixture already proves exact-request propagation and indeterminate quarantine, but it cannot prove a real desktop action stopped;
- add deployment-level connection/rate limits and operational observability for the M1 service boundary.

Do not start V2-M2 multi-machine routing until the single-device M1 acceptance path is complete.
