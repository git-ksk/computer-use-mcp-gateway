# V2 process and shell operation recovery

Status: **active V2 contract for v0.3 production hardening**.

This contract provides read-only durable recovery for bounded `execute_process` and `shell` operations whose northbound MCP response may be lost. It does not weaken the existing no-replay, quarantine, or `retry_safe:false` rules. Browser/Desktop semantic operations are not made recoverable by this slice.

## Stable operation reference

`execute_process` and `shell` accept an optional `operation_id` with the exact form `op_` followed by 32 lowercase hexadecimal characters (128 random bits). For long-running or mutating work, callers should generate a fresh cryptographically random ID **before** the call and retain it locally. CUMG-generated IDs are returned with normal process/shell responses, but a caller cannot rely on learning a server-generated ID if the entire response is lost.

An accepted operation ID is the existing authoritative replay identity. Reusing it for another execution is rejected as `operation_replay`; status lookup never turns that rejection into a replay or resume.

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
