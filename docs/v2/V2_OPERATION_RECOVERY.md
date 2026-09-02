# V2 durable effectful operation recovery

Status: **active V2 contract for v0.4 Recovery & Reconciliation**.

This contract provides read-only durable recovery for effectful operations whose northbound MCP response may be lost. `execute_process` and `shell` retain their bounded caller-visible output recovery; effectful Desktop/Browser calls add status-only recovery with a payload-free terminal marker. It does not weaken the existing no-replay, quarantine, exact owner/capability authorization, or `retry_safe:false` rules.

## Stable operation reference

Every effectful northbound tool accepts an optional `operation_id` with the exact form `op_` followed by 32 lowercase hexadecimal characters (128 random bits). This includes process/shell, effectful Desktop operations, and effectful Browser operations; observation-only tools do not accept the field. For work whose response loss matters, callers should generate a fresh cryptographically random ID **before** the call and retain it locally. A caller cannot rely on learning a server-generated ID if the entire response is lost.

An accepted operation ID is the existing authoritative replay identity. Reusing it for another execution is rejected as `operation_replay`; status lookup never turns that rejection into a replay or resume.

## Process lifetime and background descendants

`execute_process` and `shell` are bounded operations, not service launchers. On Unix the Agent places the launched operation in its own supervised process group; on Windows it uses a Job Object. Cancellation, timeout, and ordinary parent completion clean up descendants that remain in that supervision domain. A plain shell background job, including `nohup ... &`, therefore must not be used as a persistence mechanism: when it remains in the supervised process group it is terminated as the operation reaches its terminal state.

This is a lifecycle contract, not an OS-wide sandbox against an already-authorized Dangerous process/shell caller. In particular, the current Unix process-group primitive cannot guarantee cleanup after a descendant deliberately creates a different session/process group (for example by calling `setsid()`), reparents through an external service manager, or otherwise leaves the supervised group. Such detachment is unsupported and **must not be relied on** to create persistent work. The stricter Unix containment gap is tracked in GitHub issue #96; CUMG does not paper over it with shell-text filtering or heuristic PID killing.

Long-running builds/releases should remain inside the bounded operation and use a caller-retained `operation_id` plus `get_operation` to recover a lost northbound result. If persistent managed jobs are added in the future, they require a separate capability/API with explicit start/status/cancel lifecycle and authorization rather than weakening this process boundary.

## `get_operation`

`get_operation(operation_id)` is a Hub-local read-only MCP tool. It does not require the Agent to be online and never dispatches a device command. The lookup is scoped to the authenticated issuer+subject that created the original operation, and current authorization for the original exact capability is checked again before returning data. Wrong-owner and unknown IDs have the same not-found behavior so the reference cannot be used as a cross-principal existence oracle.

Public states are:

- `running` — queued, active-not-dispatched, dispatched, or cancellation-requested;
- `succeeded` — the Agent produced a verified terminal result for the original effectful capability;
- `failed` — a verified error result or other proven failed terminal state;
- `cancelled` — process-tree cancellation was proven;
- `timed_out` — the bounded process/shell timeout fired and process-tree termination was proven;
- `indeterminate` — completion cannot be proven; the existing quarantine/no-replay rules remain authoritative.

`original_retry_safe` is always `false`. Recovery is the safe alternative to blindly retrying a mutating command. A northbound `device_indeterminate` error also carries bounded actionable guidance: `execution_may_have_occurred=true`, `blind_replay_safe=false`, `next_action=get_operation_then_reconcile`, and `follow_up_effectful_operation=new_operation_id_required`. If the exact `get_operation` lookup itself remains `indeterminate`, its next action is `reconcile_indeterminate`. These fields are derived from the authoritative operation state, never from command-text heuristics. A later effectful attempt is a new operation only after reconciliation; it never replays the quarantined operation.

## Durable result boundary

For `execute_process` and `shell`, the Hub stores only the bounded caller-visible terminal result needed for recovery: the existing `ProcessOutput` fields, or a stable `DeviceErrorCode`. stdout and stderr keep the existing 16 KiB-per-stream bound and truncation flags. For every other effectful Desktop/Browser capability, the durable recovery record stores only a payload-free `effectful_status` marker alongside the already-authoritative terminal state and execution receipt. It never copies screenshots, typed text, URLs, clipboard content, browser/backend result payloads, or GUI state merely to support status lookup.

The recovery record never accepts or persists the original command text, argv, cwd, or environment entries. These values are also not added to telemetry. Recovered stdout/stderr are intentionally caller-visible result data and therefore remain sensitive local Hub checkpoint data subject to the existing state-directory protections.

Recovery material is first embedded in the authoritative execution-safety operation record, so terminal state, owner, generation, receipt, and either bounded process/shell output or the payload-free effectful marker are persisted together before northbound delivery is attempted. When an Agent generation rollover compacts ordinary terminal admission records, recoverable records move into the same bounded recovery archive. The archive remains capped at **8 entries and 256 KiB total encoded bytes**, evicting the oldest detailed record first. Execution-safety schema v9 introduces the effectful status-only marker; a checkpoint containing that marker cannot be downgraded to a v8 reader. Earlier schemas remain readable within their representational limits.

If an old reference is eventually evicted and `get_operation` returns `operation_not_found`, that does **not** make the original operation retry-safe. Callers must not reuse the old random operation ID; reconcile external state before deciding whether to start new work with a fresh ID.

## Failure and ambiguity rules

Losing the northbound response after a proven terminal result does not change the durable terminal state. A later `get_operation` returns the durable state without contacting the Agent; process/shell may also return bounded output, while Desktop/Browser recovery intentionally remains status-only. If the Hub cannot prove terminal completion, the operation remains `indeterminate`; the lookup reports that fact and does not authorize a retry. Operator resolution of an indeterminate operation remains a separate trusted recovery action and does not synthesize a missing process/shell result.

After process spawn, local supervision failures are classified by **proof**, not merely by low-level error type. A pipe/reader setup failure, polling failure, cancellation/timeout termination failure, or wait failure may be returned as an ordinary terminal failure only when the Agent has independently proved the supervised process domain terminal. If the local process worker itself panics or disappears, the Agent conservatively treats the spawn/terminal boundary as unproven. If terminality cannot be proved, the Agent sends no normal result and reconnects; the Hub's existing connection-loss path durably records `indeterminate` + quarantine. A stdout/stderr reader I/O failure discovered only after process-domain termination is proven remains a terminal failure, not an ambiguity. The reconnect path may persist `ConnectionLost` as the conservative public indeterminate reason; that diagnostic coarseness never permits replay.
