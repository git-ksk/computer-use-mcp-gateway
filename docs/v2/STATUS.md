# V2 status

> English is the canonical documentation. [日本語版 / Japanese translation](STATUS.ja.md)

Status as of 2026-08-26:

- **Desktop semantic path:** complete and accepted, including same-context native element click/type/key targeting and real-Cua background AX element-action evidence.
- **Browser core semantic path:** complete and accepted for prepare, bind, inspect, navigate, click, type, dialog, and pointer semantics.
- **Browser transfer:** complete and accepted. Upload/download use scoped CUMG refs plus Agent-private bounded staging; no arbitrary host path is exposed northbound. Agent startup now preserves privacy while reporting the exact bounded staging-init stage and I/O class locally when private staging cannot be established.
- **Optional first-class Human Handoff:** accepted in CUMG for bounded Window and Terminal/PTY coordination. Window uses upstream `WindowHandoffAdapter`; Terminal uses upstream `TerminalHandoffAdapter` while CUMG retains PTY/process containment and content-free verification. The controlled Agent owns the canonical Handoff runtime/checkpoint and Human surface; the Hub keeps CUMG authorization, ledger, quarantine, and conservative pre-dispatch fencing. Runtime/transport loss fails closed rather than bypassing Handoff. Upstream Window #85 still tracks its own first-class same-LAN direct physical rerun; CUMG #152 remains closed.
- **Current execution-safety boundary:** durable execution-safety schema is **v8**. Ambiguous effectful work remains `Indeterminate`, quarantines the device, and is never automatically retried or replayed. Schema v6 distinguishes text/input delivery that was applied but not committed from true no-effect resolution. Schema v7 adds a payload-free, privacy-preserving text-input evidence envelope with optional keyed candidate matching. Schema v8 adds the closed recovery-evidence read lane: explicitly allowlisted non-mutating evidence reads may run while quarantine exists, while generic shell/process and every mutation/activation/write capability remain blocked.
- **Operator inspection and reconciliation:** `v2_maint inspect-quarantine` is read-only and reports the exact earlier `blocking_operation_id` plus bounded recovery metadata without raw payloads or owner identity. `v2_maint audit-reconciliation` correlates the latest Hub/Agent durable state and explicitly distinguishes authoritative terminal evidence from legacy/non-authoritative markers and observational correlation. Neither command clears quarantine or authorizes replay. `compare-quarantine-request` remains correlation-only and returns only `same_request`, `different_request`, or `unavailable`.
- **Authoritative self-reconciliation and retirement:** execution-safety schema v4 persists an exact effectful dispatch fence and can self-resolve only an exact signed Agent terminal proof after a fresh authenticated generation; v5 adds a separate offline retirement path for narrowly allowlisted permanently unknowable legacy ambiguity while keeping the exact operation ID permanently non-replayable. Automatic settlement, manual resolution, and retirement are deliberately distinct states and all remain persistence-gated.
- **Offline recovery compatibility:** authority-bearing `v2_maint resolve` requires the Hub to be stopped and preserves the authoritative checkpoint's existing durable writer contract. The maintenance binary validates representability before publication and must be installed as a version-paired artifact with the deployed Hub. A newer arbitrary maintenance checkout is not a supported way to mutate state owned by an older deployed Hub. Issue #100 remains the one explicit `v0.3.0` release blocker because its local-user-authorized online recovery implementation still needs trusted physical-macOS Secure Enclave/user-presence acceptance and a real ambiguous-operation no-replay proof.
- **Privacy-safe northbound failures:** live control schema is **v9**. Expected execution-policy/runtime failures preserve bounded client-visible categories such as working-directory denial, timeout, program/environment policy, spawn failure, cancellation, `agent_offline`, and `device_indeterminate` while raw paths, commands, environment values, device identity, provider text, and OS error strings remain excluded. Unknown internal failures still collapse fail closed.
- **Host reliability and diagnostics:** `v2_doctor` distinguishes a proven in-band diagnostic self-observation from real blocking quarantine without changing restart-safety state. Browser staging startup failures emit bounded local stage/I/O diagnostics. Controlled `StorageFull` injection proves Agent checkpoint persistence exhaustion can terminate the Agent fail closed and therefore surface remotely as `agent_offline`; the prior committed checkpoint/replay barriers remain authoritative, normal service-manager reconnect works after capacity returns, and `v2_doctor` exposes only coarse read-only state/temp capacity signals.
- **Process/shell response-loss recovery:** `execute_process` and `shell` accept a stable caller-retained `operation_id`, and the Hub exposes read-only `get_operation` for owner/capability-scoped recovery without replay or Agent liveness. Proven terminal output is persisted before northbound delivery and survives Agent generation rollover in a bounded recovery archive (8 entries / 256 KiB encoded total). Unknown/evicted references never make the original operation retry-safe.
- **V1 compatibility:** V1 remains available for regression/reference and existing deployments. V1 regression/conformance coverage remains required during V2 work; the remaining #14/#15 observations are explicitly blocked on upstream Cua rather than treated as active CUMG release blockers.

## Active contracts

- [`V2_POSITIONING.md`](V2_POSITIONING.md) — canonical product boundary.
- [`V2_P0_EXECUTION_SAFETY.md`](V2_P0_EXECUTION_SAFETY.md) — uncertainty-aware execution and no-auto-replay invariants.
- [`V2_OPERATION_RECOVERY.md`](V2_OPERATION_RECOVERY.md) — durable bounded process/shell result recovery after northbound response loss.
- [`V2_INTERACTION_CONTEXT.md`](V2_INTERACTION_CONTEXT.md) — scoped interaction state and backend-reference ownership.
- [`V2_GUI_SEMANTIC_CAPABILITIES.md`](V2_GUI_SEMANTIC_CAPABILITIES.md) — Desktop semantic surface.
- [`V2_BROWSER_SEMANTIC_CAPABILITIES.md`](V2_BROWSER_SEMANTIC_CAPABILITIES.md) — Browser semantic surface and transfer boundary.
- [`V2_CUA_PARITY_MATRIX.md`](V2_CUA_PARITY_MATRIX.md) — Cua compatibility/parity classification.
- [`V2_THREAT_MODEL.md`](V2_THREAT_MODEL.md) — security claims and non-claims.
- [`V2_STANDARDIZATION.md`](V2_STANDARDIZATION.md) and [`V2_P2_REPLACEMENT_SEAMS.md`](V2_P2_REPLACEMENT_SEAMS.md) — maintained-OSS/standards replacement boundaries.

## Acceptance evidence

- [`acceptance/V2_BROWSER_CORE_ACCEPTANCE.md`](acceptance/V2_BROWSER_CORE_ACCEPTANCE.md) — Browser core closeout.
- [`acceptance/V2_BROWSER_TRANSFER_ACCEPTANCE.md`](acceptance/V2_BROWSER_TRANSFER_ACCEPTANCE.md) — Browser transfer contract, threat controls, automated coverage, and trusted-Mac real-Cua evidence.
- [`acceptance/V2_LOCAL_DESKTOP_ACCEPTANCE.md`](acceptance/V2_LOCAL_DESKTOP_ACCEPTANCE.md) — trusted physical Desktop acceptance procedure/evidence.
- [`acceptance/V2_M1_ACCEPTANCE.md`](acceptance/V2_M1_ACCEPTANCE.md) — earlier secure-Agent milestone acceptance retained as evidence.
- [Issue #100](https://github.com/git-ksk/computer-use-mcp-gateway/issues/100) / draft PR #101 — pending trusted physical-macOS acceptance for the remaining `v0.3.0` blocker.

## Historical records

Early prototype and progress records are archived under [`../archive/v2/`](../archive/v2/). They preserve design provenance but are no longer executable setup instructions.
