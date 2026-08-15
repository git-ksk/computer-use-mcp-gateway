# V2 status

> **Note:** This is the canonical English V2 status. A Japanese translation is available at [STATUS.ja.md](STATUS.ja.md).

Status as of 2026-08-15:

- **Desktop semantic path:** complete and accepted, including same-context native element click/type/key targeting and real-Cua background AX element-action evidence.
- **Browser core semantic path:** complete and accepted for prepare, bind, inspect, navigate, click, type, dialog, and pointer semantics.
- **Browser transfer:** complete and accepted. Upload/download use scoped CUMG refs plus Agent-private bounded staging; no arbitrary host path is exposed northbound.
- **Post-dispatch ambiguity hardening:** after dispatch of a mutating command, a generic backend error, malformed/unprovable completion, or response loss is classified at the adapter/Agent boundary as `BackendOutcomeIndeterminate`, persisted by the Hub as durable `Indeterminate` with reason `BackendOutcomeUnproven`, cancels queued work for that desktop, and keeps the device quarantined until explicit persistence-gated resolution. It is never automatically retried or replayed. Read-only backend failures may remain definite. Northbound operational failures use bounded MCP `CallToolResult` errors with `isError=true` and closed CUMG codes rather than leaking transport/provider/`ExceptionGroup` shapes. Real-Cua browser-alert acceptance covers the observable-side-effect case from issue #47.
- **V1 production:** unchanged by the V2 development branch. V1 regression and conformance coverage remains required during V2 work.

## Active contracts

- [`V2_POSITIONING.md`](V2_POSITIONING.md) — canonical product boundary.
- [`V2_P0_EXECUTION_SAFETY.md`](V2_P0_EXECUTION_SAFETY.md) — uncertainty-aware execution and no-auto-replay invariants.
- [`V2_INTERACTION_CONTEXT.md`](V2_INTERACTION_CONTEXT.md) — scoped interaction state and backend-reference ownership.
- [`V2_GUI_SEMANTIC_CAPABILITIES.md`](V2_GUI_SEMANTIC_CAPABILITIES.md) — Desktop semantic surface.
- [`V2_BROWSER_SEMANTIC_CAPABILITIES.md`](V2_BROWSER_SEMANTIC_CAPABILITIES.md) — Browser semantic surface and transfer boundary.
- [`V2_CUA_PARITY_MATRIX.md`](V2_CUA_PARITY_MATRIX.md) — Cua compatibility/parity classification.
- [`V2_THREAT_MODEL.md`](V2_THREAT_MODEL.md) — security claims and non-claims.
- [`V2_STANDARDIZATION.md`](V2_STANDARDIZATION.md) and [`V2_P2_REPLACEMENT_SEAMS.md`](V2_P2_REPLACEMENT_SEAMS.md) — maintained-OSS/standards replacement boundaries.
- [`V2_USAGE_ACCOUNTING.md`](V2_USAGE_ACCOUNTING.md) — optional accounting integration.

## Acceptance evidence

- [`acceptance/V2_BROWSER_CORE_ACCEPTANCE.md`](acceptance/V2_BROWSER_CORE_ACCEPTANCE.md) — Browser core closeout.
- [`acceptance/V2_BROWSER_TRANSFER_ACCEPTANCE.md`](acceptance/V2_BROWSER_TRANSFER_ACCEPTANCE.md) — Browser transfer contract, threat controls, automated coverage, and trusted-Mac real-Cua evidence.
- [`acceptance/V2_LOCAL_DESKTOP_ACCEPTANCE.md`](acceptance/V2_LOCAL_DESKTOP_ACCEPTANCE.md) — trusted physical Desktop acceptance procedure/evidence.
- [`acceptance/V2_M1_ACCEPTANCE.md`](acceptance/V2_M1_ACCEPTANCE.md) — earlier secure-Agent milestone acceptance retained as evidence.

## Historical records

Early prototype and progress records are archived under [`../archive/v2/`](../archive/v2/). They preserve design provenance but are no longer executable setup instructions.
