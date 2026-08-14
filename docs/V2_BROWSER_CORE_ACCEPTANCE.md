# V2 browser core acceptance

## Scope

This document defines the closeout gate for the **core** browser semantic path. It does not mark the
separate upload/download transfer boundary complete.

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
12. Control and capability schema v4 mixing fails closed against pre-v4 peers.
13. Browser core requires a fresh `WindowScoped` InteractionContext. A context monotonically expanded
    to `DesktopScoped` cannot be silently downgraded for browser use.
14. Only Browser bind/snapshot observations receive the reviewed bounded-large result allowance;
    transfer capabilities remain unadvertised and mutations retain the ordinary carrier bound.

## Current core advertisement

The core branch advertises exactly eight northbound browser tools when the matching live capability
and policy are present: `browser_prepare`, `browser_bind`, `browser_inspect`, `browser_navigate`,
`browser_click`, `browser_type`, `browser_dialog`, and `browser_pointer`. Browser upload/download are
not present in tool discovery.

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

## Transfer boundary remains separate

`BrowserUploadFile` and `BrowserDownload` may exist in schema v4 so grants, signed transport, rolling
upgrade, and exact capability identities are reviewable, but they must **not** be advertised live until
the transfer implementation is complete.

Upload completion requires:

- a CUMG-issued file ref;
- local canonical regular-file validation without symlink traversal;
- explicit staging/lifetime cleanup;
- exact `BrowserUploadFile` authorization;
- no arbitrary public path forwarded to Cua.

Download completion requires:

- a CUMG-issued destination-root ref;
- an existing canonical local root;
- a caller-chosen path-safe destination basename, never an untrusted server filename;
- a hard byte ceiling enforced during transfer, not only checked afterward;
- explicit overwrite policy enforced before destination mutation;
- exact `BrowserDownload` authorization and reviewed mapping to Cua's host-approval proof;
- deterministic cleanup/restoration on success, refusal, timeout, cancellation, and disconnect.

Until those gates pass, upload/download are unsupported rather than silently falling back to raw Cua
paths or unbounded download behavior.

## PR closeout

The browser PR is ready to leave draft only after:

- Rust format/check/test are green on the final head;
- docs and V1 passthrough jobs are green;
- Cua smoke remains green on Linux/macOS/Windows;
- core browser unit/contract tests are green;
- trusted-Mac real-Cua core acceptance is green with provider refusals preserved;
- no raw Cua/CDP escape hatch is present;
- production V1/Cloudflare has not changed.

The final V1 -> V2 cutover remains a later repository-level gate after the complete Cua 0.19.3 parity
matrix, browser transfer closeout, and actual GatewayMCP/ChatGPT path smoke are complete.
