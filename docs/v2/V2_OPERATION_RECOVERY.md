# V2 durable effectful operation recovery

Status: **active V2 contract for v0.4 Recovery & Reconciliation**.

This contract provides read-only durable recovery for effectful operations whose northbound MCP response may be lost. `execute_process` and `shell` retain their bounded caller-visible output recovery; effectful Desktop/Browser calls add status-only recovery with a payload-free terminal marker. It does not weaken the existing no-replay, quarantine, exact owner/capability authorization, or `retry_safe:false` rules.

## Stable operation reference

Every effectful northbound tool accepts an optional `operation_id` with the exact form `op_` followed by 32 lowercase hexadecimal characters (128 random bits). This includes process/shell, effectful Desktop operations, and effectful Browser operations; observation-only tools do not accept the field. For every new effectful execution, callers that supply this field must generate a **fresh cryptographically secure random 128-bit value before the call** and retain it locally for `get_operation` if the response is lost. Do not hand-author memorable IDs, use counters/patterns, or reuse an earlier operation ID. The server validates the wire shape and replay identity; it deliberately does not apply heuristic entropy scoring to guess whether a syntactically valid 128-bit value was generated randomly.

Representative CSPRNG generation:

```text
Python:  "op_" + secrets.token_hex(16)
Node.js: "op_" + crypto.randomBytes(16).toString("hex")
```

Generate and retain the value before invoking the effectful tool. A caller cannot rely on learning a server-generated ID if the entire response is lost. The patterned `op_69696969696969696969696969696969` value observed during 2026-09-22 dogfood is an explicit example of what **not** to generate.

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

### Hub-authoritative pre-enqueue non-delivery (v0.8 / #377)

Execution-safety schema **v15** adds one deliberately narrow automatic non-execution proof after the Hub has already durably committed an effectful operation as `Dispatched`. The proof is valid only when the exact first Hub outbound attempt fails **before the per-session Hub→Agent queue accepts the encoded command**: either `encode_hub_frame()` fails before enqueue, or Tokio `mpsc::Sender::send()` returns the unsent frame because the receiver is closed. These are Hub-local facts about the exact dispatch attempt; provider logs, missing Agent evidence, timeouts, later transport/writer failures, or silence after successful enqueue are not equivalent evidence.

The Hub binds the proof to the exact operation/owner/device generation/capability revision/one-shot dispatch grant, creates a payload-free terminal `Cancelled` receipt with `hub_encode_failed_before_enqueue` or `hub_outbound_closed_before_enqueue`, appends the bounded auto-resolution audit record, and commits the candidate checkpoint **before** replacing live state. If that checkpoint save fails, the terminal proof is not published in memory; the already-durable `Dispatched` record remains fail-closed and ordinary session cleanup/restart rules preserve ambiguity rather than manufacturing non-execution.

Northbound reports the immediate failure as `confirmed_not_executed` with `execution_may_have_occurred=false`; a later `get_operation` returns `state=cancelled` plus `resolution=confirmed_not_executed` and the same bounded guidance. Both keep `retry_safe=false` and `blind_replay_safe=false`: the old operation ID remains a terminal replay tombstone. If the caller still needs the effect, `next_action=retry_with_new_operation_id_if_still_needed` and `follow_up_effectful_operation=new_operation_id_required` require a fresh operation identity. A failure after the outbound queue accepted the frame can never use this path and remains subject to the existing `Indeterminate`/quarantine/reconciliation contract.

Historical schema-v14 checkpoints remain readable. A checkpoint containing v15 pre-enqueue non-delivery evidence cannot be lossily downgraded to v14 because doing so would erase the evidence class that distinguishes authoritative non-delivery from post-dispatch ambiguity.

Operator surfaces stay bounded: `inspect-quarantine` includes recent automatic resolutions even after active quarantine is gone, `audit-reconciliation <operation_id>` reports `authoritative_hub_protocol` / `no_recovery_required` for this settled proof without consulting Agent state, and `get_operation` exposes `resolution=confirmed_not_executed`. Incident brief remains intentionally scoped to unresolved quarantine incidents.

## Durable result boundary

For `execute_process` and `shell`, the Hub stores only the bounded caller-visible terminal result needed for recovery: the existing `ProcessOutput` fields, or a stable `DeviceErrorCode`. stdout and stderr keep the existing 16 KiB-per-stream bound and truncation flags. For every other effectful Desktop/Browser capability, the durable recovery record stores only a payload-free `effectful_status` marker alongside the already-authoritative terminal state and execution receipt. It never copies screenshots, typed text, URLs, clipboard content, browser/backend result payloads, or GUI state merely to support status lookup.

The recovery record never accepts or persists the original command text, argv, cwd, or environment entries. These values are also not added to telemetry. Recovered stdout/stderr are intentionally caller-visible result data and therefore remain sensitive local Hub checkpoint data subject to the existing state-directory protections.

Recovery material is first embedded in the authoritative execution-safety operation record, so terminal state, owner, generation, receipt, and either bounded process/shell output or the payload-free effectful marker are persisted together before northbound delivery is attempted. When an Agent generation rollover compacts ordinary terminal admission records, recoverable records move into the same bounded recovery archive. The archive remains capped at **8 entries and 256 KiB total encoded bytes**, evicting the oldest detailed record first. Execution-safety schema v9 introduced the effectful status-only marker; schema **v14** adds bounded private recovery-target metadata for application-targeted operations. Earlier reviewed schemas remain readable within their representational limits, while downgrade fails closed whenever it would discard v14 recovery-target state.

### Local-only application recovery target identity (v0.7 / #289)

`launch_application` records only its already-bounded application `identifier` / `name` selector as private recovery metadata. `terminate_application` continues to dispatch only the exact PID to the Agent, but the northbound v0.7 tool schema additionally requires a bounded `application` identity selected by the caller from prior observation (for example `list_windows`). The Hub persists `{process_id, application}` only inside the authoritative checkpoint so a later local operator knows which application needs independent verification if termination becomes `Indeterminate`. A missing, malformed, capability-mismatched, or corrupted target on a newly admitted v14 operation fails closed. Historical v13 checkpoints without target metadata remain readable as legacy state; the system does not fabricate an identity that was never captured.

Recovery-target metadata is **not execution evidence**. It cannot establish `confirmed_completed` or `confirmed_not_executed`, clear quarantine, authorize replay, switch mutation authority, or bypass Human presence. `get_operation`, normal MCP results/tooling, generic `inspect-quarantine` JSON, unified `cumg_status`, logs, metrics, and ordinary telemetry omit the raw target. The reviewed local `v2_maint incident-brief` surface may disclose the bounded target under state-directory/operator filesystem authorization so a Human can perform independent verification; its JSON explicitly marks the target `recovery_metadata_only` with `settlement_authority=false` and `replay_authority=false`.

**v0.7 client migration:** `terminate_application` now requires both `process_id` and `application`. Clients using the v0.6 `{process_id}` shape must refresh MCP discovery and pass the bounded application identity associated with the selected PID. This is a northbound pre-1.0 minor-version break only; the Agent `DeviceCommand::TerminateApplication` remains PID-only and does not require a live control-schema bump.

If an old reference is eventually evicted and `get_operation` returns `operation_not_found`, that does **not** make the original operation retry-safe. Callers must not reuse the old random operation ID; reconcile external state before deciding whether to start new work with a fresh ID.

### Authoritative backend execution receipts (v0.7 / #290)

A backend may opt into a reviewed **durable execution-receipt** contract for an effectful operation. This is not a generic interpretation of provider text or logs. The Agent asks the adapter only for one already-recorded receipt after the ordinary backend call became ambiguous; the lookup must not replay the mutation, probe current GUI state, or infer success from a heuristic. A receipt is accepted only when its bounded schema validates and it matches the exact stable device ID, original device generation, operation ID, `DeviceCapability`, capability revision, one-shot dispatch grant, backend/provider identity and version, monotonically increasing provider sequence, and the command target binding where one exists. For application launch/termination, that target binding must also correlate with the private #289 recovery target; the private human-readable `application` label remains recovery metadata and never becomes backend evidence.

An accepted receipt is persisted in the Agent checkpoint before reconnect handling proceeds and is converted into the existing payload-free `AgentTerminalEvidence`. A fresh authenticated Agent session then sends the **existing signed reconciliation report**, and the Hub uses the existing #124 `reconcile_authoritative_terminal()` state machine. The Hub persists the candidate terminal checkpoint before live quarantine disappears. There is no second receipt settlement state machine and no new replay authority. Missing, malformed, stale, duplicate-conflicting, cross-operation, wrong-generation, wrong-capability/revision/grant/target, unsupported-schema, or provider-mismatched evidence leaves the original outcome `Indeterminate`; quarantine and permanent no-auto-replay remain in force.

Receipt provenance is deliberately provider-specific. The default `ComputerUseBackendAdapter::recover_execution_receipt()` returns no evidence, so a backend is **not receipt-authoritative merely because it can execute a command**. The current Cua MCP adapter has no reviewed durable provider-receipt lookup contract; therefore a Cua response lost before the Agent obtains a definite result still remains `Indeterminate` / operator-required. If the Agent already received a definite normal result and only later lost Hub transport, the pre-existing Agent terminal-evidence journal continues to cover that case without a backend receipt. Process/shell execution likewise keeps its existing terminal-result path; #290 does not reinterpret process output, backend logs, current application state, browser state, or OS journals as settlement evidence.

The receipt itself is payload-free: it carries only bounded operation/dispatch/provider provenance, sequence, target binding, and terminal outcome/evidence class. It never stores raw argv, typed text, URLs, clipboard contents, credentials, screenshots, GUI payloads, provider response text, stdout, or stderr. Generic MCP results, `get_operation`, `cumg_status`, logs, metrics, and ordinary telemetry do not expose receipt payloads or private recovery targets. The local operator `v2_maint audit-reconciliation` / `incident-brief` surface may show bounded receipt provenance (provider/version/contract schema/sequence and target-match status) with an explicit CUMG-authority label while keeping external diagnostics `observational_only`.

#290 changes only Agent-local durable state: M1 checkpoint schema **v6** adds the bounded receipt journal while historical schema v5 remains readable when it carries no receipt state. A receipt-bearing v6 checkpoint cannot be represented as v5 and therefore fails closed on lossy downgrade. The authoritative Hub execution-safety schema is **v15** in v0.8 development because #377 adds Hub-local pre-enqueue non-delivery evidence; #290 itself still changes only Agent-local durable state, and `CONTROL_SCHEMA_VERSION`, capability-advertisement schema, and Hub-Agent wire schema are unchanged because the live signed reconciliation message still carries the existing `AgentTerminalEvidence` shape.

## Failure and ambiguity rules

Losing the northbound response after a proven terminal result does not change the durable terminal state. A later `get_operation` returns the durable state without contacting the Agent; process/shell may also return bounded output, while Desktop/Browser recovery intentionally remains status-only. If the Hub cannot prove terminal completion, the operation remains `indeterminate`; the lookup reports that fact and does not authorize a retry. Operator resolution of an indeterminate operation remains a separate trusted recovery action and does not synthesize a missing process/shell result.

After process spawn, local supervision failures are classified by **proof**, not merely by low-level error type. A pipe/reader setup failure, polling failure, cancellation/timeout termination failure, or wait failure may be returned as an ordinary terminal failure only when the Agent has independently proved the supervised process domain terminal. If the local process worker itself panics or disappears, the Agent conservatively treats the spawn/terminal boundary as unproven. If terminality cannot be proved, the Agent sends no normal result and reconnects; the Hub's existing connection-loss path durably records `indeterminate` + quarantine. A stdout/stderr reader I/O failure discovered only after process-domain termination is proven remains a terminal failure, not an ambiguity. The reconnect path may persist `ConnectionLost` as the conservative public indeterminate reason; that diagnostic coarseness never permits replay.

## Managed jobs (#106)

Long-running development work uses the separate managed-job lifecycle in [V2_MANAGED_JOBS.md](V2_MANAGED_JOBS.md), not `nohup`, `setsid`, shell backgrounding, or service-manager escape. Explicit stop ambiguity follows the existing Indeterminate/no-replay/quarantine path. Asynchronous lease-expiry or shutdown ambiguity persists Agent-local fail-closed state and requires explicit offline operator recovery.
