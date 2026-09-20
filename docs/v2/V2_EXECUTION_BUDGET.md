# V2 effectful execution budget

> English is canonical. See [V2_EXECUTION_BUDGET.ja.md](V2_EXECUTION_BUDGET.ja.md) for Japanese.

Issue #319 closes a contract gap between semantic command duration and the bounded Cua MCP tool timeout. CUMG must not knowingly dispatch an effectful request whose own reviewed timing parameters cannot fit inside the available backend deadline.

## Invariants

1. Budget validation runs before any Cua tool call, interaction-session refresh, or other backend dispatch.
2. A budget rejection is the typed execution_budget_exceeded terminal error. Because no backend effect was dispatched, it is retry-safe and does not create quarantine.
3. The configured Cua tool timeout remains the hard outer deadline. CUMG does not globally raise it for long commands.
4. CUMG reserves backend/IPC/cancellation headroom before admitting duration-bearing work:
   - reserve = max(2 seconds, 20% of tool timeout), capped at the whole timeout;
   - effective semantic budget = tool timeout - reserve;
   - required == effective is accepted; required > effective is rejected.
5. A timeout after real backend dispatch remains Indeterminate, quarantines effectful work, and is never automatically retried or replayed.
6. Payload, text, host paths, and target identity are never added to generic timeout diagnostics.

With the default 30 second Cua tool timeout, the effective semantic budget is 24 seconds.

## Type-text timing

The pinned Cua Driver 0.19.3 contract is measured in Unicode scalar values for paced text paths. CUMG therefore uses text.chars().count() rather than UTF-8 byte length.

The conservative cross-platform estimate is:

    per_scalar =
        max(
            macOS physical synthesis: 24ms + max(delay_ms, 8ms),
            Windows paced/post-message worst scalar: 28ms + delay_ms,
            Linux XTest conservative scalar: 10ms + delay_ms
        )

    required = scalar_count * per_scalar + 2000ms fixed drain/settle

The estimate deliberately over-budgets faster AX/UIA/foreground routes because route selection may fall back after admission. Legacy TypeText uses the existing 30ms default pacing for budgeting.

The 2026-09-20 production incident shape (about 1403 Unicode scalars at 30ms with a 30 second outer tool timeout) requires about 83.374 seconds under this conservative contract and is rejected before dispatch.

The existing MAX_TYPE_TEXT_BYTES remains an independent carrier/input bound; bytes are not used as timing events.

## Duration-bearing capability audit

| Surface | Duration source | Budget treatment |
| --- | --- | --- |
| TypeText | text scalar count + default pacing | conservative type-text estimate |
| TypeTextAdvanced | text scalar count + delay_ms | conservative type-text estimate |
| PointerDrag | duration_ms | exact requested duration |
| PointerDragAdvanced | duration_ms | exact requested duration |
| VerifyUiState | timeout_ms | exact requested duration |
| VerifyUiStateContextual | timeout_ms | exact requested duration |
| KeyboardInput | no caller duration/repeat parameter | no semantic duration budget |
| Browser semantic operations | no caller-controlled wait duration in the current CUMG surface | no additional semantic duration budget |
| Process / Shell | their own supervised executor timeout_ms | separate from Cua tool timeout |

Any future Cua semantic command that adds a caller-controlled duration must either join this audit or prove an independently bounded deadline before release.

## Timeout ambiguity and diagnostics

A Cua timeout after dispatch does not prove whether the OS-side effect occurred. CUMG therefore preserves the existing fail-closed model:

- the Agent records no terminal completion evidence;
- the operation is Indeterminate;
- the exact operation remains non-replayable;
- new effectful work is fenced by quarantine.

#319 additionally introduces a signed, payload-free IndeterminateAck from Agent to Hub for backend_timed_out. It is bound to the authenticated connection, device generation, and exact operation ID. It is diagnostic/quarantine metadata only: it cannot claim completion, non-execution, or authorize replay.

When that signed acknowledgement reaches the Hub, the durable quarantine reason remains backend_timed_out instead of being degraded to a later generic connection_lost. If the acknowledgement itself is lost, ordinary connection-loss handling remains the conservative fallback.

Because this adds an outer Hub-Agent wire variant, #314 owns the final HUB_AGENT_SCHEMA_VERSION compatibility bump and mixed-version release acceptance.

## Release acceptance

Automated coverage must prove:

- below/equal/above effective-budget boundaries;
- Unicode scalar timing independent of UTF-8 byte length;
- the 2026-09-20 incident shape is rejected before backend dispatch;
- ordinary short typing is unaffected;
- duration-bearing capability coverage stays explicit;
- post-dispatch backend timeout remains Indeterminate/no-replay;
- the signed timeout-cause acknowledgement is payload-free, signature-bound, and persists BackendTimedOut as quarantine reason;
- real-Cua acceptance exercises one controlled delay-bearing text-input case before v0.5 release closeout.
