# V2 process and shell operation recovery

Status: **active V2 contract for v0.3 production hardening**.

This contract provides read-only durable recovery for bounded `execute_process` and `shell` operations whose northbound MCP response may be lost. It does not weaken the existing no-replay, quarantine, or `retry_safe:false` rules. Browser/Desktop semantic operations are not made recoverable by this slice.

## Stable operation reference

`execute_process` and `shell` accept an optional `operation_id` with the exact form `op_` followed by 32 lowercase hexadecimal characters (128 random bits). For long-running or mutating work, callers should generate a fresh cryptographically random ID **before** the call and retain it locally. CUMG-generated IDs are returned with normal process/shell responses, but a caller cannot rely on learning a server-generated ID if the entire response is lost.

An accepted operation ID is the existing authoritative replay identity. Reusing it for another execution is rejected as `operation_replay`; status lookup never turns that rejection into a replay or resume.

## Process lifetime and background descendants

`execute_process` and `shell` are bounded operations, not service launchers. On Unix the Agent places the launched operation in its own supervised process group; on Windows it uses a Job Object. Cancellation, timeout, and ordinary parent completion clean up descendants that remain in that supervision domain. A plain shell background job, including `nohup ... &`, therefore must not be used as a persistence mechanism: when it remains in the supervised process group it is terminated as the operation reaches its terminal state.

This is a lifecycle contract, not an OS-wide sandbox against an already-authorized Dangerous process/shell caller. In particular, the current Unix process-group primitive cannot guarantee cleanup after a descendant deliberately creates a different session/process group (for example by calling `setsid()`), reparents through an external service manager, or otherwise leaves the supervised group. Such detachment is unsupported and **must not be relied on** to create persistent work. The stricter Unix containment gap is tracked in GitHub issue #96; CUMG does not paper over it with shell-text filtering or heuristic PID killing.

Long-running builds/releases should remain inside the bounded operation and use a caller-retained `operation_id` plus `get_operation` to recover a lost northbound result. If persistent managed jobs are added in the future, they require a separate capability/API with explicit start/status/cancel lifecycle and authorization rather than weakening this process boundary.

## `get_operation`

`get_operation(operation_id)` is a Hub-local read-only MCP tool. It does not require the Agent to be online and never dispatches a device command. The lookup is scoped to the authenticated issuer+subject that created the original operation, and current authorization for the original `ExecuteProcess` or `Shell` capability is checked again before returning data. Wrong-owner and unknown IDs have the same not-found behavior so the reference cannot be used as a cross-principal existence oracle.

Public states are:

- `running` — queued, active-not-dispatched, dispatched, or cancellation-requested;
- `succeeded` — the Agent produced a verified ordinary process/shell result;
- `failed` — a verified error result or other proven failed terminal state;
- `cancelled` — process-tree cancellation was proven;
- `timed_out` — the bounded process/shell timeout fired and process-tree termination was proven;
- `indeterminate` — completion cannot be proven; the existing quarantine/no-replay rules remain authoritative.

`original_retry_safe` is always `false`. Recovery is the safe alternative to blindly retrying a mutating command.

## Durable result boundary

The Hub stores only the bounded caller-visible process/shell terminal result needed for recovery: the existing `ProcessOutput` fields, or a stable `DeviceErrorCode`. stdout and stderr keep the existing 16 KiB-per-stream bound and truncation flags. This v0.3 slice deliberately does not add retrieval for bytes beyond the inline cap; that is a separate output-reference capability.

The recovery record never accepts or persists the original command text, argv, cwd, or environment entries. These values are also not added to telemetry. Recovered stdout/stderr are intentionally caller-visible result data and therefore remain sensitive local Hub checkpoint data subject to the existing state-directory protections.

The recoverable result is first embedded in the authoritative execution-safety operation record, so terminal state, owner, generation, receipt, and result are persisted together before northbound delivery is attempted. When an Agent generation rollover compacts ordinary terminal admission records, only recoverable process/shell results move into a bounded recovery archive inside the same execution-safety snapshot. The archive is capped at **8 entries and 256 KiB total encoded bytes**, evicting the oldest result first. This preserves recovery across generation/reconnect while keeping persistence bounded. Execution-safety snapshot schema v2 can restore the previous v1 result-less form, but a v1 snapshot cannot claim to contain a recoverable result or archive.

If an old reference is eventually evicted and `get_operation` returns `operation_not_found`, that does **not** make the original operation retry-safe. Callers must not reuse the old random operation ID; reconcile external state before deciding whether to start new work with a fresh ID.

## Failure and ambiguity rules

Losing the northbound response after a proven terminal result does not change the durable terminal state. A later `get_operation` returns that result without contacting the Agent. If the Hub cannot prove terminal completion, the operation remains `indeterminate`; the lookup reports that fact and does not authorize a retry. Operator resolution of an indeterminate operation remains a separate trusted recovery action and does not synthesize a missing process/shell result.

After process spawn, local supervision failures are classified by **proof**, not merely by low-level error type. A pipe/reader setup failure, polling failure, cancellation/timeout termination failure, or wait failure may be returned as an ordinary terminal failure only when the Agent has independently proved the supervised process domain terminal. If the local process worker itself panics or disappears, the Agent conservatively treats the spawn/terminal boundary as unproven. If terminality cannot be proved, the Agent sends no normal result and reconnects; the Hub's existing connection-loss path durably records `indeterminate` + quarantine. A stdout/stderr reader I/O failure discovered only after process-domain termination is proven remains a terminal failure, not an ambiguity. The reconnect path may persist `ConnectionLost` as the conservative public indeterminate reason; that diagnostic coarseness never permits replay.
