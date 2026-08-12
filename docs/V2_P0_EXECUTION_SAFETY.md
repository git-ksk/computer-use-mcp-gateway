# V2 P0 execution-safety core

Status: **P0 accepted on 2026-08-12; P1 follow-ups remain open.**

Canonical product boundary: [`V2_POSITIONING.md`](V2_POSITIONING.md). Primary tracker: [Issue #24](https://github.com/git-ksk/computer-use-mcp-gateway/issues/24).

This document records the reference-model gap analysis that was required before changing the post-M1 state machine, the invariants adopted from ROSClaw and Agent libOS, the desktop-specific deviations, the resulting CUMG state machine, and the acceptance evidence. It is intentionally limited to the P0 uncertainty-aware desktop execution core. Fleet product work, generic authorization, a new device registry, a new policy language, another native GUI backend, and a long-lived ROSClaw fork remain out of scope.

Reference snapshots reviewed for this pass:

- ROSClaw `ros-claw/rosclaw` commit `3a2cd9c` (2026-08-12 checkout);
- Agent libOS `yingqi-z20/Agent-libOS` commit `74acb4a` (2026-08-12 checkout).

These projects are design references, not dependencies. CUMG reuses the applicable invariants without importing their unrelated runtime/product scope.

## 1. ROSClaw gap analysis

ROSClaw's `rosclawd` boundary is stronger than the pre-P0 CUMG design in several places: exact per-action authorization, finite action deadlines/leases, durable action transitions and terminal receipts, generation-wide `DISARMED` startup, and explicit operator recovery for interrupted REAL actions. CUMG adopts the parts that improve an interactive desktop without pretending that a desktop is a robot body.

| CUMG P0 concept | ROSClaw analogue | P0 decision |
| --- | --- | --- |
| stable desktop/device | Body / physical resource | **Adopt conceptually.** The quarantine unit is the whole interactive desktop because GUI focus, pointer/keyboard state, foreground application state, and shell-started applications share one user session. |
| exact `DeviceCapability` plus short-lived southbound grant | capability + exact Permit | **Keep/adopt.** Northbound authorization is reduced to exact desktop capability; the Agent consumes a one-shot, short-lived exact grant before local execution. The generic permit/authorization protocol itself is not CUMG differentiation. |
| `OperationOwner { issuer, subject }` | Agent Session / actor identity | **Strengthen.** The authenticated northbound principal is now copied into the authoritative operation record. Cancel/finalize paths are fenced against a competing owner. |
| `operation_id` | Action ID | **Match and strengthen.** One immutable operation ID follows shell/process or GUI work through dispatch, cancellation, ambiguity, quarantine, resolution, receipt, replay tombstone, and audit. The old operation ID is never reused to resume work. |
| authoritative per-desktop active ownership | Action/resource/body Lease | **Desktop-specific deviation.** The legacy M0 `LeaseManager` remains a primitive/reference, but P0 does not use TTL expiry as permission to reuse a dispatched desktop. After dispatch, ownership ends only with evidence-bearing terminal settlement or explicit resolution. Time passing cannot prove that a GUI effect stopped. |
| device/session generation fencing | daemon generation / generation-scoped arming | **Adopt fencing; deliberately do not copy blanket clean-reconnect disarming.** A stale Agent generation cannot finalize newer work. If dispatched work becomes uncertain, quarantine survives every reconnect/generation. A clean reconnect with no ambiguous operation does not require a human arm step because CUMG controls an already-authorized desktop session rather than an embodied REAL actuator. |
| durable `Indeterminate` | interrupted REAL action with unknown outcome | **Adopt strongly.** Cancel, disconnect, provider timeout, or lost result is not ordinary failure unless non-execution/termination is actually proven. |
| `DesktopQuarantine` | recovery-required / `DISARMED` boundary | **Adopt and specialize.** Quarantine binds exact device, operation ID, generation, owner, reason, and timestamp. All normal desktop work is refused until explicit resolution. |
| `resolve_indeterminate` | daemon-UID `acknowledge-recovery` | **Adopt semantics.** Resolution is explicit, durable, auditable, and never resumes the old operation. The resolver can be a separate trusted recovery principal; it is not silently inherited from reconnect. |
| `ExecutionReceipt` | `ExecutionReceipt` | **Adopt conceptually.** CUMG receipts record operation identity, owner, capability, terminal state, evidence class, generation through `OperationRef`, and finalization time without raw command/result payloads. |

### Stronger ROSClaw invariants adopted

1. **Recovery is a state transition, not a liveness event.** A connection becoming healthy cannot clear uncertainty.
2. **Old work is not resumed after recovery.** When a desktop becomes indeterminate, all already-queued, not-yet-dispatched work for that desktop is cancelled with `CancelledBeforeDispatch` evidence. Resolution reopens the desktop only for a new operation ID.
3. **Generation is a fence, not ownership.** A new generation cannot inherit or settle an old operation merely because the Agent reconnected.
4. **Recovery is auditable.** Resolver, exact ambiguous operation, generation, decision, bounded evidence metadata, and timestamp are persisted.
5. **Receipts carry evidence class.** A terminal status without evidence is not enough to collapse an ambiguous effect.

### Deliberate desktop deviations

CUMG does not copy ROSClaw's entire REAL/SHADOW/SIMULATION physical-control model, body-specific deadman policy, operator broker, hardware E-Stop, robot resource hierarchy, or blanket `DISARMED` startup. Those solve a broader embodied-runtime problem.

The important desktop-specific rule is stricter in another dimension: GUI and native shell/process work share one **desktop execution boundary**. A shell command can launch or mutate an application that Cua immediately manipulates; therefore CUMG must not maintain independent GUI and shell ownership models.

## 2. Agent libOS gap analysis

Agent libOS has a stronger durable external-effect lifecycle than pre-P0 CUMG. Its important pattern is: reauthorize and reserve finite authority together with a durable pending effect intent, cross the provider boundary only after that durable preparation, and conditionally finalize the same effect identity. Ambiguous provider failures retain an `unknown` effect instead of restoring authority or inventing a normal failure.

CUMG does not copy Agent libOS's process/object/resource OS abstraction. It adopts the effect-ordering invariants at the desktop operation boundary.

| Effect phase | Agent libOS reference invariant | CUMG P0 ordering |
| --- | --- | --- |
| authority/admission | authority is checked and finite-use authority is reserved before provider side effect | authenticated principal is bound to exact device/capability; Hub admission creates the operation/owner record before dispatch; Agent later consumes the exact one-shot grant before local execution |
| pending effect | durable pending/unknown intent exists before provider | Hub persists `ActiveNotDispatched`/queued intent; immediately before transport emission it transitions to `Dispatched` and checkpoints that fact |
| provider boundary | provider executes only after durable intent/authority reservation | signed command bytes are emitted only after Hub `Dispatched` checkpoint; Agent independently verifies/consumes grant and checkpoints its active operation before spawning process/shell or invoking Cua |
| ambiguous failure | authority is not restored and effect remains `unknown` | CUMG converges to durable `Indeterminate`; desktop quarantine prevents a retry, competing principal, reconnect, or old queue from causing implicit replay |
| finalization | guarded finalization of the same `effect_id` prevents duplicate settlement | Hub finalization requires exact operation ID + owner + device generation and a legal terminal transition; duplicate/nonterminal finalization is rejected |
| crash/restart | post-provider crash cannot erase uncertainty | restart normalization converts any dispatched/cancel-requested operation to `Indeterminate` + quarantine; queued/non-dispatched work becomes cancelled, never runnable |

### Stronger Agent libOS invariants adopted

- **Persist intent before effect.** The Hub records the operation before dispatch and records `Dispatched` before writing the command to the Agent transport.
- **Consume authority before effect.** The Agent persists consumed one-shot grant state before local execution proceeds.
- **Persist active local execution before provider/spawn.** Agent operation replay state is checkpointed before the local side-effect boundary.
- **Guard finalization by causal identity.** Operation ID, owner, and generation must all match; only `Completed`, `Failed`, or proven `Cancelled` may finalize normally.
- **Unknown remains unknown.** Transport/provider ambiguity cannot be rewritten as an ordinary failure merely to make the API convenient.
- **No duplicate settlement.** A second finalization or stale/late ambiguity signal cannot replace a terminal receipt or clear quarantine.

## 3. Authoritative CUMG state machine

`src/v2_execution_safety.rs` is the reviewed Hub-side state-machine boundary. `HubAdmissionController` remains the bounded queue mechanism underneath it, but is no longer the complete safety model.

```text
                          +----------------------+
                          |        Queued        |
                          +----------+-----------+
                                     |
                                     | capacity
                                     v
+----------------------+   +---------+------------+
| ActiveNotDispatched |<--+      StartNow        |
+----------+-----------+   +----------------------+
           |
           | checkpoint Dispatched BEFORE command bytes
           v
+----------+-----------+
|      Dispatched      |
+----+-------------+---+
     |             |
     | cancel      | verified terminal evidence
     v             v
+----+-----------+ +------------------------------+
| CancelRequested| | Completed / Failed / Cancelled|
+----+-----------+ +------------------------------+
     |                              ^
     | proof exists                 |
     +------------------------------+
     |
     | effect may have happened / result lost / disconnect
     v
+----------------------+        explicit audited resolution
|    Indeterminate     |-----------------------------------+
| DesktopQuarantine    |                                   |
+----------------------+                                   |
              old operation is NEVER replayed <------------+
```

Accepted ordinary terminal states are:

- `Completed` — verified Agent result proving the command completed according to its backend contract;
- `Failed` — verified remote error, or a process/shell timeout for which CUMG has proof the process tree was terminated and waited;
- `Cancelled` — cancellation before dispatch, or process/shell cancellation with proven process-tree termination.

`Indeterminate` is intentionally outside the ordinary terminal set. It means CUMG cannot prove enough about the side effect to reuse the desktop automatically.

## 4. Evidence taxonomy

`ExecutionEvidence` is intentionally compact and payload-free:

| Evidence | Meaning |
| --- | --- |
| `VerifiedAgentResult` | signed, generation-bound Agent result matched the original typed command/result contract |
| `VerifiedRemoteError` | signed Agent result reported an explicit command/backend error rather than a lost outcome |
| `ProvenProcessTermination` | native process/shell executor terminated and waited the controlled process tree, so cancellation/timeout is not an unknown running child |
| `CancelledBeforeDispatch` | operation was removed before command bytes crossed the Hub dispatch boundary |
| `OperatorResolution` | a trusted recovery actor explicitly reconciled an indeterminate operation; the separate resolution record contains the decision and bounded metadata |

The evidence string supplied to resolution is **metadata only**. It is bounded to 1 KiB and must not contain screenshots, raw command payloads, raw result payloads, credentials, or unrelated desktop content.

## 5. Ownership, generation, and stale-result fencing

P0 uses three separate identities that must not be conflated:

1. **operation ID** — immutable causal identity;
2. **operation owner** — northbound issuer + subject that started the work;
3. **device generation** — authenticated Agent session generation used as a stale transport/executor fence.

The Hub refuses dispatch/cancel/finalize when owner or generation does not match the authoritative operation record. The Agent also tags worker completion with the generation in which it began and rejects completion from an older worker/session generation.

A result must additionally pass the existing connection-bound signature, device, operation ID, capability revision, and typed result checks before it can reach guarded finalization.

## 6. Durable quarantine and explicit resolution

`DesktopQuarantine` is persisted independently from the live Hub-Agent connection and contains:

- stable device ID;
- exact ambiguous operation ID;
- device generation;
- operation owner;
- indeterminate reason;
- quarantine timestamp.

On Hub restart, dispatched/cancel-requested work is normalized to `Indeterminate`, and the quarantine survives. Reconnect or a higher Agent generation does not clear it.

`HubHandle::resolve_indeterminate` is the semantic resolution API. It requires the exact operation ID, an explicit resolver identity, a decision (`ConfirmedCompleted` or `ConfirmedNotExecuted`), bounded evidence metadata, and a timestamp supplied by the Hub. It records `ResolutionRecord` and produces an `OperatorResolution` receipt. It never re-dispatches the old command.

The resolution transition is **persistence-gated**: if its checkpoint cannot be written, CUMG rolls the in-memory controller back to the quarantined snapshot and returns an error. Reuse is therefore not authorized by a resolution that failed to become durable.

This pass deliberately does **not** add another generic admin-auth protocol. `HubHandle` is a trusted in-process API. A future standalone remote operator/recovery surface must authenticate and authorize the resolver before calling it; merely possessing a northbound MCP principal must not imply recovery authority.

## 7. Cancellation, timeout, disconnect, and lost-result rules

- **cancel before dispatch:** proven not sent; terminal `Cancelled`;
- **native process/shell cancel after dispatch:** executor terminates descendants and waits; terminal `Cancelled` with `ProvenProcessTermination`;
- **native process/shell timeout:** executor terminates/waits; terminal `Failed` with `ProvenProcessTermination`;
- **Cua cancellation after provider dispatch:** cancellation propagation does not prove whether the desktop action already happened; `Indeterminate` + quarantine;
- **Cua/provider timeout with unknown effect:** never becomes ordinary failure; Agent reconnect forces the Hub connection-loss path to persist unknown execution;
- **connection loss after dispatch/result delivery uncertainty:** `Indeterminate` + quarantine;
- **late/duplicate signed cancellation acknowledgement:** if the operation is already terminal or indeterminate, it is logged and ignored; it cannot clear/replace quarantine;
- **reconnect:** may advance generation, but does not transfer owner or resolve old ambiguity.

A race discovered by the mixed shell+Cua E2E was fixed during this pass: the Agent previously reconnected immediately when a Cua cancellation worker returned indeterminate, which could close the gRPC stream before the already-queued signed `IndeterminateAfterPropagation` acknowledgement was flushed. Cancellation-propagated ambiguity now keeps that session alive after the signed quarantine acknowledgement; autonomous provider timeout still reconnects to force unknown-result handling.

## 8. One desktop boundary for shell/process and Cua

The P0 model intentionally does not create `ShellOwner` and `GuiOwner` concepts. All state-changing commands enter the same `start_command_as(owner, DeviceCommand)` path and therefore share:

- one operation ID namespace;
- one authenticated principal owner;
- one per-desktop admission/exclusion boundary;
- one generation fence;
- one ambiguity/quarantine state;
- one no-auto-replay rule;
- one receipt/resolution audit model.

The deterministic E2E proves:

```text
Alice shell -> launches/mutates application state
Alice Cua   -> begins GUI drag under same desktop boundary
Bob         -> cannot steal/cancel Alice operation
Alice cancel-> provider outcome unknown
Hub         -> durable desktop quarantine
Bob         -> normal GUI work rejected
Alice/operator -> explicit resolution
Bob         -> new operation may use desktop
old drag    -> observed exactly once, never replayed
```

The real-Cua macOS acceptance now uses CUMG's native shell executor to launch TextEdit before performing the Cua operation, so the representative scenario is exercised on the actual desktop path as well as with the deterministic backend fixture.

## 9. Persistence compatibility

The Hub checkpoint schema is now **v5** because durable owner, operation-state, quarantine, receipt, and resolution semantics cannot be reconstructed safely from the previous v4 Hub checkpoint.

There is intentionally no automatic v4 -> v5 migration in this pass. Loading an incompatible checkpoint fails closed instead of guessing ownership or ambiguity. Operators upgrading an existing development deployment must treat the v5 state boundary as a reviewed migration/re-enrollment event rather than silently reusing old in-flight state.

## 10. P0 test matrix

| Requirement | Evidence |
| --- | --- |
| legal/illegal state transitions and duplicate finalize | `v2_execution_safety` unit/invariant tests |
| owner + generation fencing | deterministic tests plus `proptest` randomized stale owner/generation cases |
| quarantine blocks arbitrary principals/generations | `proptest` randomized competing-principal/generation cases |
| queued work never resumes after ambiguity | quarantine queue-cancellation invariant test |
| crash while dispatched | restart snapshot converts dispatch to indeterminate quarantine |
| crash before/after resolution | restart tests on both sides of explicit resolution; old operation replay remains rejected |
| durable resolution failure | mixed E2E makes Hub checkpoint directory unwritable; failed resolution rolls back to quarantine |
| duplicate/late ambiguity signal | invariant test plus Hub late-ack guard |
| network partition/result-loss + Hub/Agent restart race | `tests/v2_m1_partition_recovery.rs` aborts Agent after dispatch while the local side effect can still complete, restarts `SingleDeviceHub` from the durable quarantine checkpoint, then reconnects a newer Agent generation |
| shell + GUI same ownership boundary | `tests/v2_m1_desktop_boundary_e2e.rs` full Hub-Agent TLS/gRPC path |
| real Cua regression | `tests/v2_m1_cua_cancellation_e2e.rs` on operator-controlled macOS with Cua Driver |

Final acceptance on the P0 tree passed:

```bash
git diff --check
cargo fmt --check
cargo check --locked --all-targets
cargo test --locked --all-targets
cargo clippy --locked --all-targets
python3 scripts/check_docs.py
CUMG_V2_CUA_CANCEL_E2E_ACK=1 CUMG_V2_CUA_COMMAND="$HOME/.local/bin/cua-driver" \
  cargo test --locked --test v2_m1_cua_cancellation_e2e \
  real_cua_cancel_is_propagated_and_quarantined_indeterminate -- --ignored --exact --nocapture
```

The repository-wide test run contains 112 library tests plus all normal integration targets; the real-Cua opt-in acceptance separately passed on the operator-controlled macOS desktop. The mixed shell+Cua E2E was stress-run 20 consecutive times after fixing the cancellation-ack/session race, and the partition + Hub/Agent restart E2E was stress-run 10 consecutive times.

## 11. Security review

The P0 implementation was reviewed against these failure classes:

- **competing principal:** owner mismatch rejects cancel/finalize; queued work is independently identified and is cancelled if the active desktop becomes ambiguous;
- **stale Agent/worker:** connection signature + device generation + worker generation + operation identity fence finalization;
- **lost response/network partition:** dispatch is durable before send, so missing result cannot revert to not-started; Hub quarantines;
- **Hub restart:** runnable in-flight work is never restored; ambiguous dispatch becomes quarantine;
- **Agent restart:** consumed grants and terminal/active replay barriers remain persisted; old worker completion cannot be accepted into a new generation;
- **late cancellation acknowledgement:** cannot mutate terminal/quarantined state;
- **duplicate finalization:** illegal after first terminal transition;
- **resolution persistence failure:** in-memory state rolls back to quarantine;
- **privacy:** receipts contain typed evidence metadata, not command/result payloads; operator evidence must remain bounded metadata;
- **compromised endpoint/backend:** still outside cryptographic proof. A compromised Agent/backend can lie or act outside the protocol; this core limits delegation/replay/ambiguity but does not sandbox a fully compromised desktop account.

## 12. Deliberately remaining work

Do not mark the following complete as part of this P0 pass:

- **P1 multi-device invariant proof.** The authoritative controller remains exercised through the existing single-device Hub runtime; independent Device A/Device B quarantine behavior still requires a later minimal multi-device proof before fleet work.
- **P1 second materially different backend.** The deterministic Cua-shaped fixture proves the Cua adapter execution boundary and cancellation races, but it is not a second backend with a different evidence contract.
- **Remote recovery administration.** The semantic resolution API exists and is auditable, but a standalone remotely exposed operator UI/API with its own authentication/authorization has not been introduced; that must reuse an existing auth system rather than create a generic CUMG auth protocol.
- **Provider-timeout reason precision.** An autonomous Cua timeout is conservatively surfaced to the Hub through connection-loss ambiguity, so the safety state is correct but the persisted reason may be `ConnectionLost` rather than a provider-specific timeout reason. Do not weaken quarantine merely to improve diagnostics.
- **v4 checkpoint migration.** Automatic migration is intentionally absent because v4 cannot prove the new owner/quarantine fields.

These residual items are not blockers to the P0 invariant implemented here; they remain explicit follow-up work in Issue #24 rather than being silently closed.
