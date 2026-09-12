# V2 Cloud Run Hub support gate

Status: **design complete, implementation/acceptance pending** for Issue #215.

Cloud Run is **not a supported CUMG Hub deployment** yet. The existing single-host/VM Hub profile remains the supported model. This document defines the architecture and evidence required before that support claim can change.

## Re-verified Cloud Run facts

The platform assumptions below were re-verified against current Google Cloud documentation on 2026-09-03. They are deployment facts, not CUMG invariants, and must be re-checked again at physical acceptance time.

- Cloud Run service request timeout defaults to 5 minutes and can be configured up to 60 minutes. When the request timeout expires, the network request is closed and the caller receives a timeout; the serving container instance is **not necessarily terminated**. A reconnect is a new request and is not guaranteed to reach the same instance.
- Before service-instance shutdown, Cloud Run sends `SIGTERM` and documents a 10-second graceful-shutdown window before `SIGKILL`. Even minimum instances can be restarted.
- The writable container filesystem is disposable/in-memory and does not survive instance replacement. It cannot hold authoritative checkpoint, quarantine, replay-barrier, or recovery state.
- A service has one ingress container listening on the configured `PORT`. Native gRPC requires HTTP/2; for end-to-end HTTP/2 the container receives `h2c` after Google's frontend terminates public TLS.
- Session affinity is best effort and can break when an instance terminates or becomes unavailable. It is not execution-safety authority.
- Minimum/maximum instance settings are capacity controls, not fencing. Minimum instances can restart, and revision rollout can leave old and new revision instances alive concurrently; maximum-instance settings must not be treated as proof that only one authoritative writer exists.

Authoritative references:

- <https://docs.cloud.google.com/run/docs/configuring/request-timeout>
- <https://docs.cloud.google.com/run/docs/container-contract>
- <https://docs.cloud.google.com/run/docs/configuring/http2>
- <https://docs.cloud.google.com/run/docs/configuring/session-affinity>
- <https://docs.cloud.google.com/run/docs/configuring/min-instances>
- <https://docs.cloud.google.com/run/docs/configuring/max-instances-limits>

## Current CUMG incompatibilities

The current Hub cannot be called Cloud-Run-ready by changing deployment flags alone.

1. `CheckpointStore` is explicitly local-filesystem based. Its commit proof depends on private pending files, flush/fsync, no-clobber publication, and directory fsync.
2. `v2_hub` currently exposes separate Agent gRPC/TLS and northbound MCP HTTP listeners. Cloud Run supplies one ingress port.
3. Current Hub authority is process-local around one `HubHandle`, including in-memory pending-operation coordination. Session affinity cannot make that state authoritative across replacement/concurrent instances.
4. The default planned shutdown drain is 30 seconds, longer than Cloud Run's documented 10-second instance-shutdown grace period.
5. The current 3600-second Agent session lifetime exactly matches Cloud Run's maximum request timeout. That leaves no platform headroom for a controlled stream rotation.

Therefore `min-instances=1` / `max-instances=1` is acceptable only for a non-authoritative PoC. It is not a support argument.

## Required hosted architecture

### 1. Provider-neutral durable authoritative state

Introduce a durable Hub-state seam before a hosted profile is supported. The implementation does not need to make every provider interchangeable at runtime, but the CUMG execution model must no longer assume POSIX filesystem publication semantics.

The durable backend must provide an equivalent or stronger contract for the complete authoritative Hub snapshot, including device generation, operation ownership, dispatch state, terminal receipts, `Indeterminate` quarantine, permanent replay tombstones, recovery state, and compatibility metadata.

At minimum it must support:

- exact current-state read with a monotonic revision/version;
- transactional compare-and-commit against the expected revision;
- allocation of a monotonically increasing writer/fencing epoch;
- every authoritative mutation conditioned on both the expected state revision and current writer epoch;
- atomic publication of the complete new authoritative state or no publication at all;
- durable read-after-commit semantics before authority is considered changed;
- no automatic fallback to an older committed state after a failed/latest read;
- bounded, explicit backup/restore and schema migration;
- fail-closed behavior for unavailable, ambiguous, or partially failed persistence.

A Cloud Storage or mounted-filesystem implementation is not presumed equivalent. It needs its own proof if proposed.

### 2. Fencing before effect dispatch

A stale Cloud Run instance must be unable to dispatch an effect merely because it still owns an old Agent stream.

For every effectful operation, the authoritative admission/dispatch transition must be durably committed **under the current writer epoch immediately before southbound dispatch**. If that compare-and-commit fails because another instance advanced the writer epoch or state revision, the stale instance must not dispatch.

The same epoch/revision condition applies to terminal settlement, quarantine/recovery mutation, generation change, and replay-barrier mutation. Process-local `pending` data may remain a cache/coordination aid but cannot be the source of truth.

This is the key reason Cloud Run session affinity or `max-instances=1` is insufficient.

### 3. Hosted Agent stream profile

Cloud Run request timeout and CUMG session lifetime remain separate clocks.

The initial hosted acceptance profile should use:

- Cloud Run request timeout: `3600s`;
- CUMG maximum Agent session lifetime: **`3300s` (55 minutes)**;
- existing pre-expiry reauthentication drain: **`30s`**.

This intentionally closes the CUMG stream about five minutes before the platform request deadline. A normal rotation must still perform a fresh authenticated handshake and advance generation through existing semantics.

The five-minute headroom is operational margin, not a safety proof. Cloud Run can still disconnect earlier. Any platform timeout, instance replacement, network loss, or unexpected stream closure remains ordinary transport loss: never success, never replay authority, and never automatic quarantine clear.

### 4. Hosted shutdown contract

A Cloud Run profile cannot rely on the local default 30-second shutdown drain completing after `SIGTERM`.

The initial hosted profile should cap application drain at **8 seconds**, immediately close new admission, and use the remaining platform grace only for stream/server teardown. More importantly, safety must not depend on the full 8 seconds being available.

- work not durably marked dispatched must never dispatch after restart;
- work durably marked dispatched but lacking authoritative terminal proof may conservatively restore as `Indeterminate`/quarantined;
- forced termination must never synthesize completion or authorize replay;
- restart must recover exact replay barriers and quarantine from the external durable backend.

Extra quarantine is an acceptable conservative failure mode; lost ambiguity is not.

### 5. One-port protocol multiplexing

A supported Cloud Run Hub needs one reviewed ingress service on Cloud Run's `PORT`. The recommended direction is one application-level HTTP/2/h2c listener that routes only closed protocol surfaces:

- Agent gRPC service methods -> existing Agent application authentication/device identity and signed protocol semantics;
- northbound MCP paths -> existing OAuth/trusted-proxy principal authentication and exact CUMG authorization;
- explicitly documented health/metadata paths -> their existing coarse/read-only policy.

There must be no generic pass-through route.

Cloud Run terminates public TLS. The hosted ingress therefore cannot depend on the current private `v2_hub` TLS listener shape. This does **not** weaken Agent identity: Agent application-level Ed25519 identity/enrollment remains independent of transport TLS. The hosted profile must explicitly document Google frontend trust, h2c inside the service boundary, and northbound HTTPS resource identity.

### 6. Hosted Handoff composition

A Handoff-enabled hosted profile must also satisfy the Agent-owned composition in [`V2_HOSTED_HANDOFF_TOPOLOGY.md`](V2_HOSTED_HANDOFF_TOPOLOGY.md). The Hub-local Unix operator socket remains valid for single-host/VM deployment, but it is not the hosted operator interface. Hosted lifecycle control must be separately authenticated/authorized from normal MCP tool discovery, must not let callers manufacture PID/window authority, and must relay only bounded fenced control to the Agent-owned canonical Handoff runtime.

Human media/input and STUN/TURN/provider credentials remain outside CUMG authoritative state. Viewer/transport generations are separate from Agent generation and Handoff epoch, and Hub replacement never restores Human/Agent authority from hosted routing metadata. The same writer-epoch/revision fence required above must deny stale hosted instances before they can dispatch an effect even if they retain an old Agent stream or stale permissive Handoff cache.

### 7. Instance count and concurrency are not authority

The initial operational profile may use `min-instances=1` and `max-instances=1` for cost/predictability, but acceptance must deliberately prove safety with **two concurrently alive Hub revisions/instances** because rollout and replacement can create that condition.

The exact tested Cloud Run concurrency value must be recorded in the acceptance artifact. It is a capacity/latency setting, not a security boundary. Changing it must not alter single-writer fencing or no-replay behavior.

### 8. Secrets, observability, and recovery

A supported profile must also document and accept:

- managed secret/key provisioning without secret-value logging;
- coarse health plus alerting for persistence failure, writer-fence loss, Agent disconnect, quarantine, and repeated failed stream rotation;
- OTLP behavior under instance replacement;
- durable-state backup/restore and schema migration;
- revision rollout and rollback when old/new binaries coexist;
- cost guidance separately from all security/recovery invariants.

## Support gate

Cloud Run remains **NO-GO for support** until all rows below have evidence.

| Gate | Current status |
| --- | --- |
| Current Cloud Run limits re-verified | Design evidence complete (2026-09-03) |
| Ephemeral filesystem excluded from authoritative state | Design decision complete; implementation pending |
| Provider-neutral durable Hub-state backend | Pending |
| Monotonic writer fencing and stale-writer dispatch denial | Pending |
| One-port h2c gRPC + MCP ingress with separate auth boundaries | Pending |
| 3300s proactive Agent stream rotation acceptance | Pending |
| <=8s hosted drain plus forced-kill fail-closed acceptance | Pending |
| Concurrent old/new revision fencing test | Pending |
| Durable quarantine/replay-barrier restore after replacement | Pending |
| Hosted deploy/upgrade/rollback/backup/alerting runbook | Pending |
| Hosted Handoff operator/routing + Agent-owned authority composition | #275 design / #276 pin / #277 operator-routing; implementation/acceptance pending |
| Physical Agent + real Cua interrupted-effect acceptance | Pending |

The existing VM/single-host deployment remains unchanged and supported while these hosted gates are open.

## Acceptance scenarios

Before #215 can close, acceptance must include at least:

1. start two Hub instances against the same durable state and prove only the current writer epoch can commit or dispatch;
2. terminate the current writer before dispatch commit and prove no later dispatch occurs;
3. terminate it immediately after durable dispatch but before terminal proof and prove restart restores exact `Indeterminate` quarantine without replay;
4. recover that quarantine through the existing reviewed recovery path and prove the old operation remains non-replayable;
5. rotate the Agent stream before 3600 seconds and prove fresh handshake/generation semantics;
6. force request/transport loss and prove it is not interpreted as successful session completion;
7. roll from revision A to B while both are alive and prove stale A cannot mutate or dispatch;
8. route Agent gRPC and northbound MCP through the single hosted ingress and prove each authentication boundary rejects the other's credentials/routes;
9. backup/restore the durable backend and prove exact quarantine and replay barriers survive;
10. repeat the deliberately interrupted effect with a physical Agent and real Cua.
11. enable Handoff through the hosted operator/routing path and prove Hub replacement, viewer reconnect, and transport fallback cannot restore Agent/Human authority; then complete Human active -> Agent deny -> Done -> fresh verification -> explicit resume on a physical Agent.

No hosted availability improvement may weaken the existing commit-before-authority-change, `Indeterminate`, quarantine, or no-auto-replay contracts.
