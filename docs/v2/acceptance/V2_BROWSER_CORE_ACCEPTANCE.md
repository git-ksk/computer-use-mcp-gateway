# V2 browser core acceptance

## Scope

This document records the closeout gate for the **core** browser semantic path. The separate transfer closeout is now complete and is recorded in [`V2_BROWSER_TRANSFER_ACCEPTANCE.md`](V2_BROWSER_TRANSFER_ACCEPTANCE.md).

The core path is:

```text
open context
-> prepare when explicitly required
-> exact native-window bind
-> semantic_v2 inspect
-> navigate / click / type / dialog / pointer
-> fresh inspect and verification
-> close context
```

The production V1 route is not changed by this work.

## Required invariants

1. The public MCP surface contains only CUMG semantic request/result types and CUMG opaque refs.
   Raw Cua target ids, tab ids, page refs, dialog ids, CDP ids, websocket endpoints, profile approval
   artifacts, and provider payloads are not northbound authority.
2. Browser state is owned by one `InteractionContext` and remains bound to principal, stable device,
   Agent generation, and capability revision. A context id is state identity, not a bearer credential.
3. Browser core operations require a window-scoped context. A context that was explicitly expanded
   to desktop scope is not silently downgraded; the caller closes it and opens a fresh context.
4. Bind is exact-or-refuse. `binding_quality != exact` or `mutation_allowed != true` cannot mint
   actionable CUMG target/tab refs.
5. A fresh semantic snapshot invalidates the prior snapshot/action/content/continuation refs for that
   exact tab. Dialog refs are tab/document-bound rather than snapshot-pagination-bound: a fresh dialog
   inspection replaces the prior dialog ref, successful resolution consumes it, and navigation
   invalidates it. Other bound tabs are not invalidated merely because one tab navigated.
6. Page action refs carry the exact CUMG action set observed in the fresh snapshot. A `type`-only ref
   cannot authorize click; a content ref cannot authorize any mutation.
7. Unknown backend actions never become CUMG authority. They remain observation-only until a reviewed
   semantic capability exists.
8. `trusted` and explicit `dom_event` remain distinct input semantics. CUMG never changes trust class,
   foregrounds the browser, or escalates to desktop automatically after a provider refusal.
9. Existing-profile preparation never manufactures or forwards a CUMG approval token. Cua/operator
   authorization remains authoritative and may refuse.
10. Browser cancellation/timeout after a possible side effect remains indeterminate and uses the same
    quarantine/no-auto-replay boundary as desktop operations.
11. Browser provider refusal messages are not returned or logged verbatim. Only reviewed semantic
    refusal codes may cross the adapter boundary.
12. Control schema version 7 and capability-advertisement schema version 4 are exact fail-closed boundaries; incompatible peers are rejected rather than mixed.
13. Browser core requires a fresh `WindowScoped` InteractionContext. A context monotonically expanded
    to `DesktopScoped` cannot be silently downgraded for browser use.
14. Browser bind/snapshot observations and the explicitly bounded 16 MiB transfer carriers may use the reviewed bounded-large result allowance; ordinary Browser core mutations retain the ordinary carrier bound.

## Current core advertisement

The core set remains eight northbound browser tools when the matching live capability and policy are present: `browser_prepare`, `browser_bind`, `browser_inspect`, `browser_navigate`, `browser_click`, `browser_type`, `browser_dialog`, and `browser_pointer`. The completed transfer surface adds `browser_stage_upload_file`, `browser_upload_file`, and `browser_download_file` only when the exact live transfer capability and policy permit them.

## Core real-Cua acceptance

Run against the pinned Cua 0.19.3 shadow Agent on the trusted Mac without changing Cloudflare/V1:

- open a fresh window-scoped context;
- bind one exact Chromium-family native window;
- confirm only CUMG target/tab refs are returned;
- inspect one tab with `semantic_v2` and mint CUMG action/content refs;
- prove a content ref cannot be used as click/type authority;
- prove a wrong-action ref fails at the CUMG boundary;
- perform one safe navigate and prove prior page refs are stale;
- perform one ref-targeted click using an explicitly chosen route and verify from a fresh snapshot;
- perform one ref-targeted type and verify from a fresh snapshot;
- inspect a page-owned dialog, prove only a CUMG dialog ref is returned, resolve it explicitly, and
  prove the resolved ref is consumed;
- exercise a bounded pointer action and verify from fresh browser state;
- prove wrong-context, closed-context, wrong-generation, wrong-revision, wrong-target/tab, and stale
  snapshot refs fail closed;
- restart only the shadow Agent and prove the old browser context/refs are invalidated;
- close the context and prove backend session state is removed.

Provider safety refusals are valid outcomes. Acceptance must not turn an unsupported background trusted
input route into a foreground action simply to obtain a pass.

### Trusted-Mac evidence on 2026-08-14

A disposable loopback V2 shadow using the reviewed branch and the installed Cua 0.19.3 completed the
following signed northbound -> Hub -> Agent -> Cua checks without changing the persistent V1/V2
listeners or Cloudflare route:

- discovery exposed Desktop 29 + Browser core 8 tools and no browser transfer tools;
- existing-profile preparation without provider approval returned only the safe
  `browser_consent_required` code;
- isolated preparation, exact native-window bind, navigation, and semantic-v2 inspect succeeded with
  no raw target/tab/page ids in the northbound result;
- navigation and fresh snapshots made prior page refs stale, while wrong-kind/content and wrong-action
  refs were rejected at the CUMG boundary;
- ref-targeted type was verified by fresh semantic value readback;
- explicit DOM click returned `effect=unverifiable` plus `verification_required=true`, and a fresh
  inspect verified the expected DOM state without automatic replay or route/foreground escalation;
- pointer scroll used a ref carrying scroll authority and a fresh inspect observed the target move from
  `near_viewport` to `in_viewport`;
- dialog tracking was armed before the page-owned dialog opened, then inspect minted only an opaque
  CUMG dialog ref, dismiss consumed it, reuse failed closed, and fresh page state showed dismissal;
- Agent reconnect invalidated an older-generation browser context before backend dispatch;
- an intentionally noisy exact-tab PNG produced more than 3.2 million base64 characters and traversed
  the signed bounded-large result path while keeping screenshot metadata and refs backend-neutral.

The live run also caught and closed four adapter-shape gaps before merge: refusal outcomes with no MCP
`isError`, object-shaped semantic states, Cua's closed action-result projection for click/type/pointer,
and integral floating-point viewport dimensions. This evidence does **not** make the PR ready to leave
draft by itself; final-head CI, reproducible local acceptance, cleanup/session plateau checks, and the
remaining closeout bullets still apply.

## Transfer boundary closeout

The transfer boundary completed on 2026-08-15. Upload uses bounded northbound bytes -> a one-shot scoped CUMG file ref -> Agent-private regular-file staging -> real Cua `browser_set_input_files`. Download accepts no host destination path: it uses an exact click-capable page ref, a logical path-safe name, a mandatory 16 MiB-or-smaller bound, and explicit overwrite semantics; the Agent supplies its own private per-operation root to Cua `browser_download`, revalidates the direct regular-file result, and returns only a scoped result ref plus bounded bytes/metadata.

Symlink/path escape, stale/cross-session/generation/revision refs, file replacement, collision/overwrite, partial/oversized completion, definite backend failure, and cancellation/timeout behavior are covered by dedicated tests. Cancellation/timeout after dispatch remains indeterminate and uses the existing quarantine/no-replay state machine.

Trusted-Mac acceptance against installed Cua 0.19.3 passed with a harmless upload, deterministic download, and stale-ref refusal without changing V1/Cloudflare or bypassing Cua/TCC authorization. See [`V2_BROWSER_TRANSFER_ACCEPTANCE.md`](V2_BROWSER_TRANSFER_ACCEPTANCE.md).

**Browser transfer complete.**

## PR closeout

The browser PR is ready to leave draft only after:

- Rust format/check/test are green on the final head;
- docs and V1 passthrough jobs are green;
- Cua smoke remains green on Linux/macOS/Windows;
- core browser unit/contract tests are green;
- trusted-Mac real-Cua core acceptance is green with provider refusals preserved;
- no raw Cua/CDP escape hatch is present;
- production V1/Cloudflare has not changed.

The final V1 -> V2 cutover remains a later repository-level gate after the complete Cua 0.19.3 parity matrix and actual GatewayMCP/ChatGPT path smoke are complete. PR #44 remains Draft and unmerged for this closeout.
