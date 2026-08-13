# V2-M1 progress — single secure remote Agent

Status: **accepted (2026-08-12)**. V2-M0 was GO; the V2-M1 single secure remote Agent acceptance gate is complete. See [`V2_M1_ACCEPTANCE.md`](V2_M1_ACCEPTANCE.md).

## Transport migration: gRPC bidi production candidate

The M1 transport strategy now keeps the protocol boundary flexible while selecting **gRPC bidirectional streaming over TLS** as the production candidate. The earlier raw TLS + bounded JSON framing implementation remains in the repository as a regression/reference transport instead of being deleted. This allows direct behavioral comparison while the new transport matures.

The first gRPC slice deliberately minimizes simultaneous change: Protobuf defines the bidirectional `AgentControl/OpenSession` RPC and bounded `AgentFrame`/`HubFrame` carriers, while the payload is the existing independently signed V2 application message. gRPC therefore replaces custom stream framing and supplies HTTP/2 streaming/flow-control semantics without weakening or duplicating Ed25519 identity, signed session/command/result messages, short-lived grants, generation checks, replay barriers, leases, or cancellation semantics. Native Protobuf fields for the application envelope can be migrated later without making that rewrite a prerequisite for validating the transport.

A TLS-enabled integration test proves one gRPC bidirectional session carrying the existing V2 authentication/heartbeat protocol and an Agent-native structured `git status --short` execution. The command is executed by the Rust process executor directly, with no Cua or Terminal GUI path. The repo still runs the equivalent raw-TLS process E2E alongside it. A second service-lifecycle E2E deliberately terminates the first authenticated stream, verifies outbound reconnect advances the device generation, then starts a long-running direct process and cancels it over the same live gRPC stream; the Agent emits a signed cancellation acknowledgement and a signed cancelled process result only after the process tree has been terminated and waited. A third deployment-oriented E2E uses the real `SingleDeviceHub` runtime rather than a fixture and proves Hub↔Agent TLS/gRPC, `git status --short`, free-form shell pipelines, bounded `ReadFile`/`ListDirectory`, symlink-escape rejection without session teardown, and live cancellation for both structured processes and shell commands.

The initial deployment candidate for this transport is a small always-on VM Hub, with Agents making outbound connections. `v2_hub` now implements that single-device Agent-facing service boundary. SessionAccepted anchors a monotonic Hub clock used for grant issuance; grant `issued_at` is conservatively backdated by five seconds to absorb the one-way session-acceptance network hop, which only shortens effective grant life rather than extending it. Serverless/WebSocket transports remain possible future adapters rather than constraints on the application protocol.

## Shell-first Agent direction

The M1 Agent is now explicitly planned as a self-owned remote execution agent, not merely a secure transport wrapper around Cua. The next implementation priority is **direct process/shell execution inside the Agent**. This lets common developer workflows such as `git`, `cargo`, `npm`, `xcodebuild`, and `fastlane` run without opening Terminal, synthesizing GUI input, or scraping terminal output through a computer-use backend.

The intended capability layering is:

```text
Hub
  |
  v
self-owned Agent
  +-- direct process executor      <- implemented in M1
  +-- explicit shell executor      <- implemented, separate Dangerous capability
  +-- bounded filesystem read/list <- implemented read-only in M1
  +-- GUI/computer-use adapter
       +-- Cua                     <- transition/default GUI backend
       +-- future native backends  <- later
```

Structured process execution is the safer **API shape**: an explicit executable, argument vector, working directory, bounded environment, output limits, timeout, and cancellation semantics, with no implicit `sh -c`. It is still a `Dangerous` capability, not a sandbox: executable scripts/interpreters can run arbitrary code, and argv may name files outside the allowed cwd. The Agent therefore requires an exact `DeviceCapability::ExecuteProcess` grant instead of accepting a class-only `Dangerous` grant. Free-form shell syntax is implemented as a separate exact `DeviceCapability::Shell` grant and distinct `Shell` command/result surface. The Agent uses a fixed OS shell (`/bin/sh -c` on Unix, `cmd.exe /D /S /C` on Windows), caps the command at 16 KiB, reuses the structured executor's cwd/environment/output/timeout/process-tree supervision, and supports live cancellation. A separate read-only filesystem observation surface now provides exact-capability `ReadFile` and `ListDirectory` operations. Paths are canonicalized against operator-approved roots, symlink escapes fail closed, reads/listings are bounded, and failures return coarse command-level error codes instead of raw paths or OS errors. This does **not** turn either execution surface into a filesystem sandbox: process argv and shell syntax may still address the wider host, and both capabilities remain `Dangerous`.

This ordering changes implementation priority, not the trust model. Existing TLS, identity, grants, leases, replay protection, admission control, cancellation state, and audit rules remain the security boundary for shell/process capabilities. GUI support is not removed: Cua remains available behind the adapter contract while the Agent gains direct shell utility first.

## Implemented foundation

The current `v2-m1-secure-agent` implementation adds these M1 building blocks while preserving the M0 application-layer identity and capability controls:

- the raw-TLS regression transport is TLS 1.3-only with pinned trust and the dedicated `cumg-hub-agent/1` ALPN; the gRPC production candidate uses TLS + HTTP/2 with pinned certificate trust/domain validation and keeps Ed25519 application identity above the transport;
- application-layer Ed25519 Hub/Agent authentication and signed session/command/result/cancellation messages remain above TLS rather than being replaced by transport identity;
- signed Agent heartbeat and Hub acknowledgement messages bound to the authenticated connection transcript;
- monotonically increasing heartbeat sequence enforcement, generation matching, timeout/offline detection, and bounded exponential reconnect policy;
- a reusable outbound lifecycle runner that bounds consecutive connection/session failures, resets the failure streak after an established session, and reconnects without transferring prior session generation state;
- an operator-facing `v2_agent` binary that loads the separate device/Hub/grant/TLS trust material, opens the outbound gRPC/TLS session, maintains signed heartbeats, reconnects with bounded exponential backoff, handles Ctrl-C shutdown, and keeps the receive loop responsive while direct process work runs on a blocking worker;
- an operator-facing `v2_hub` binary for the small always-on VM deployment target. It loads separate Hub/grant/TLS credentials plus the enrolled Agent public key, persists Hub registry/admission state, marks restored devices offline until fresh authentication, advances and fsyncs generation before session acceptance, keeps a bounded single-device queue, persists `Dispatched` before network send, verifies signed results/cancellation acknowledgements, and converts lost dispatched work to `indeterminate`;
- an optional standard-first northbound MCP Authorization boundary on `v2_hub`: the Hub acts only as an OAuth protected resource, publishes RFC 9728 Protected Resource Metadata, accepts bearer tokens only in the `Authorization` header, validates them through a configured RFC 7662 introspection endpoint with issuer/resource audience checks, emits MCP-compatible 401/403 challenges, and removes the bearer header before rmcp handler dispatch. The validated identity is reduced to `AuthenticatedClientPrincipal`, then the existing local principal -> device -> exact `DeviceCapability` policy filters discovery and gates every call. OAuth bearer tokens are never included in Hub-to-Agent messages or Agent grants;
- `v2_agent` persists Agent replay/trust checkpoints in its operator-selected state directory: consumed grants are fsynced before execution proceeds, an active operation is checkpointed before the child is spawned, and terminal operation IDs survive process restart; startup restores the latest checkpoint and fails closed on device/trust-anchor mismatch;
- replay state is generation-bounded: after a fresh authenticated generation becomes current, stale-generation commands fail session validation so completed/cancelled tombstones from prior generations can be pruned. Hub `indeterminate` operations are never pruned by generation and continue to quarantine the device until explicit resolution;
- live Agent-native process cancellation over the gRPC session: a signed `Cancel` flips the process cancellation token without blocking stream receive; Unix process groups and Windows Job Objects terminate the process tree, background descendants are also cleaned up when the top-level process exits, the operation ID becomes terminal, and signed cancellation/result evidence is returned;
- free-form Agent-native shell execution as its own exact `DeviceCapability::Shell` Dangerous capability: shell syntax/pipelines are accepted only through the dedicated command surface, the command body is capped at 16 KiB, cwd/environment/output/timeout remain bounded, and cancellation terminates the supervised shell process tree;
- bounded Agent-native filesystem observation: `ReadFile` is capped to a wire-safe 8 KiB payload and `ListDirectory` is capped by entry count; both canonicalize existing targets under approved roots and reject symlink escapes. Filesystem errors are normalized to coarse signed `DeviceErrorCode` values and do not tear down the authenticated session;
- one-device routing that rejects offline, wrong-device, stale-generation, stale-capability, and unsupported-capability commands;
- restart snapshots for device registry, grant verifier/revocation/consumption state, Hub operation state, and Agent terminal-operation replay barriers;
- crash-safe checkpoint files with bounded per-file size, `create_new`, flush/fsync, symlink rejection, restrictive Unix directory/file permission checks, and a bounded 64-checkpoint retention window; consumed-grant tombstones are pruned after their enforced grant expiry;
- restart conversion of queued/pre-dispatch work to `cancelled` and dispatched/cancel-requested work to `indeterminate`, so process restart never makes ambiguous work runnable again;
- an optional asynchronous Cua MCP semantic adapter integrated into the long-lived Agent runtime. It exposes typed `ListApplications`, `ScreenGeometry`, `PointerClick`, and bounded `PointerDrag`, reuses request-level MCP cancellation, and classifies propagated cancellation or timeout as `indeterminate` rather than claiming a desktop side effect definitely stopped;
- Hub-side device quarantine for `indeterminate` operations: a different operation on the same device is rejected until an explicit resolution records the ambiguous operation as confirmed completed or confirmed not executed;
- a separate key-material boundary for Agent device, Hub application identity, grant-signing identity, and TLS material: secret files are created with restrictive permissions, symlinks and weak permissions fail closed, Hub/device key replacement requires signed continuity, grant verifier rotation supports a bounded old/new overlap, and TLS certificate renewal remains an ACME/service-manager concern rather than a custom protocol;
- overload controls at the M1 service boundaries: Agent session starts and active sessions are bounded with standard gRPC resource-exhaustion errors, northbound MCP requests are shed with HTTP 429/503 before OAuth work, and these guards compose with the existing per-device operation admission controller;
- OpenTelemetry/OTLP traces and metrics using standard OTel endpoint/protocol/header/timeout environment variables, with command payloads, argv, file contents, screenshots, clipboard data, bearer tokens, and private credentials excluded from default telemetry;
- OS-native service packaging: a hardened systemd Hub service uses encrypted systemd credentials for long-lived application keys, Linux Agents have a user-service template, and macOS Agents use a LaunchAgent so Cua/TCC remains in the interactive user session.

The persisted replay/trust checkpoint intentionally contains **no private signing keys**. File-based key provisioning is now defined and tested, but a production deployment may still choose an OS keychain, HSM/KMS, or another reviewed secret store instead of filesystem secrets. Persisting public trust and replay state is necessary for fail-closed restart semantics, but is not a substitute for private-key custody.

## Evidence currently covered by tests

The repository tests prove the TLS wrapper negotiates TLS 1.3 with the dedicated ALPN and rejects an untrusted server certificate. Runtime tests also cover heartbeat replay/generation/timeout handling, capped reconnect backoff, one-device routing, consumed/revoked grant persistence, revoked device/generation persistence, queued/in-flight crash recovery, and operation replay barriers. A second encrypted integration test runs two outbound TLS sessions from the same Agent identity, proves the Hub advances device generation from 1 to 2, and confirms a command bound to generation 1 is rejected after generation 2 becomes current. The deterministic MCP backend fixture also proves that V2 cancellation propagation references the exact in-flight downstream MCP request ID; the resulting state is deliberately classified `indeterminate` and quarantines that device.

The existing M0 live-Cua PoC continues to prove the semantic path from an authorized client principal through a short-lived grant and bounded Hub/Agent execution to the Cua adapter. M1 now also has an end-to-end integration test that composes the TLS channel with the application protocol in one outbound connection: TLS 1.3 + dedicated ALPN, Ed25519 Hub/Agent authentication, signed session acceptance, signed heartbeat/ack, one-device routing, bounded admission and lease ownership, short-lived grant validation on the Agent, and a signed typed result.

Northbound tests now cover RFC 9728 path-inserted metadata, HTTPS-only resource/issuer/introspection configuration, malformed or duplicate bearer rejection, query-token rejection, invalid-token 401 and insufficient-scope 403 challenges, audience binding, exact wrong-device/capability denial, and an actual MCP `2026-07-28` `tools/list` request that exposes only the principal's authorized exact capability. The auth middleware strips the `Authorization` header after verification before rmcp captures HTTP request parts, so the bearer token cannot enter the Hub command path.

A separate operator-controlled M1 backend run on 2026-08-11 connected the asynchronous `CuaMcpAdapter` to Cua Driver 0.19.3 through its MCP transport. `ListApplications` normalized to an application count of 77 and `ScreenGeometry` normalized to 1920×1080 points with scale factor 1.0; the PoC did not emit the raw application list. This proves the asynchronous semantic adapter against the real backend for observe operations, not real-Cua cancellation.

## V2-M1 acceptance result

The remaining M1 blockers were closed on 2026-08-12. The final gate and command-level evidence are recorded in [`V2_M1_ACCEPTANCE.md`](V2_M1_ACCEPTANCE.md).

The production boundary now follows the standard-first decision:

- ordinary Hub TLS certificate renewal is delegated to ACME; the deploy hook validates the certificate/private-key pair and atomically installs regular files before service restart;
- Linux Hub application keys use systemd encrypted credentials, while Agent key files remain behind strict local file permissions and signed continuity rules; no private key is stored in replay checkpoints or repository configuration;
- Hub/device identity replacement uses the existing signed continuity proofs, and grant signing uses a 5-minute-maximum old/new verifier overlap before retirement;
- Agent session starts/active sessions and northbound MCP requests have bounded overload shedding with standard gRPC/HTTP errors; pre-TLS handshake flood control remains the responsibility of the standard network edge/firewall/reverse proxy rather than a custom transport;
- OTLP traces/metrics are opt-in through standard OpenTelemetry variables and omit sensitive operation payloads by default;
- launchd/systemd own restart/config/log lifecycle;
- real Cua Driver 0.19.3 cancellation was exercised through the actual V2 Hub↔Agent gRPC/TLS runtime. A 10-second desktop drag was cancelled in flight; the downstream MCP cancellation was propagated, the result remained `IndeterminateAfterPropagation`, the originating operation became `DeviceIndeterminate`, and subsequent work stayed quarantined instead of being replayed.

V2-M1 acceptance is a milestone claim, not a claim that every deployment is automatically secure. Operators still need a reviewed authorization server configuration, a TLS/network edge with appropriate handshake/rate controls, protected secret custody, OS permissions/TCC, and deployment-specific monitoring. Multi-machine routing, SPIFFE/SPIRE adoption, native GUI backends, and fleet-wide workload identity remain V2-M2/later work.
