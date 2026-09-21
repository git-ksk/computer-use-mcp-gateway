# V2 Managed Developer Jobs

Issue #106 adds an explicitly managed lifecycle for longer-running development processes. It does not turn `execute_process` or `shell` into service launchers and does not add a background-shell compatibility escape.

## Authority boundary

Managed jobs use two exact capabilities:

- `ManagedJobControl` — Dangerous. Required for `managed_job_start`, `managed_job_renew`, and `managed_job_stop`.
- `ManagedJobObserve` — Observe. Required for `managed_job_status` and `managed_job_output`.

Neither capability is implied by `ExecuteProcess`, `Shell`, a capability class, cwd roots, filesystem roots, or browser authority. Northbound authorization remains exact principal + stable device + exact capability and is intersected with the live Agent advertisement.

`managed_job_start` accepts only structured program + argv + cwd + allowlisted environment entries. There is no free-form shell input. `nohup`, `setsid`, daemonization, service-manager escape, or shell backgrounding is not a supported persistence mechanism.

## Identity and fencing

The Agent owns a private random job locator. The Hub never exposes that locator northbound. Before dispatch, the Hub reserves capacity for an opaque random `job_ref`; after a successful Agent start result it binds that public ref to the exact authenticated owner, stable device ID, Agent generation, capability revision, start operation ID, and private Agent locator.

Unknown refs, wrong owners, stale generations or revisions, and expired refs all fail closed without cross-owner existence disclosure. Hub restart invalidates the in-memory public-ref registry. Agent session loss triggers managed-job cleanup.

## Lifetime and revocation

Each job has a hard lifetime bounded to 6 hours and a renewable control lease. The default lease is 2 minutes and one renewal is bounded to 5 minutes. Every `managed_job_renew` call passes through a fresh `ManagedJobControl` authorization check. Revoked authority therefore cannot renew, and the Agent starts cleanup at the last issued lease deadline.

States are `starting`, `running`, `stop_requested`, `completed`, `stopped`, `expired`, and `indeterminate_termination`. `stop_requested` is never reported as `stopped`. Completed, stopped, and expired states require terminal proof for the supervised process domain.

## Process-control guarantee

Managed jobs reuse the structured-process policy and supervised process-control primitive used by bounded process execution:

- macOS and other Unix platforms: a dedicated process group.
- Windows: a Job Object.

This is a lifecycle guarantee, not a filesystem or network sandbox. Deliberate escape from the Unix process group remains unsupported. Stronger Linux cgroup-v2 containment is reserved for #267 and must not be inferred from this feature.

## Ambiguous termination and recovery

If explicit stop cannot prove terminality, the effectful stop operation follows the existing Indeterminate, no-replay, and quarantine path.

If asynchronous lease expiry or session/shutdown cleanup cannot prove terminality, there is no active northbound operation that can safely represent that ambiguity. The Agent therefore persists `managed_job_fail_closed = true`, stops accepting work, and reports a doctor error. A dedicated 100 ms safety poll detects this independently of heartbeat configuration.

This state never auto-clears. Offline operator recovery is:

1. stop the Agent;
2. independently prove that no orphan managed process remains;
3. inspect with `v2_maint inspect-managed-job-safety --agent-state-dir ...`;
4. explicitly clear with `v2_maint clear-managed-job-fail-closed --agent-state-dir ... --evidence "..."`.

The clear operation takes the Agent state-directory lock and therefore cannot run against a live Agent. Evidence text is a bounded operator gate and must not contain argv, cwd, environment values, output, or secrets.

## Output and privacy

Each stream retains at most 4 MiB in a rolling buffer. One read returns at most 64 KiB and includes absolute offsets plus `earliest_available_offset`, `next_offset`, `total_bytes`, `eof`, `gap_before_range`, and `history_truncated`.

Output is returned northbound as base64. Default telemetry never records raw output, job refs, Agent locators, argv, cwd, environment values, PIDs, or host output paths.

## Replay and response loss

Managed control calls use the existing operation ledger. Reusing the same effectful operation ID is rejected; no automatic replay is introduced.

If a start response is lost after Agent execution, the caller does not receive a `job_ref` and must not retry the same operation ID. The job remains bounded by the already-issued lease and hard lifetime. `get_operation` can inspect durable operation status without replaying start.

## Schema compatibility

Issue #106 uses control schema 11 and capability schema 7. Hub-Agent envelope schema remains 6 because the outer signed envelope shape is unchanged; the new typed command/result values are carried inside it. Control and capability mismatches fail closed. Agent checkpoint schema remains 5; the managed-job fail-closed field is additive and defaults safely for historical checkpoints.
