# V2 interaction context and scoped backend references

## Why this exists

Computer Use backends often keep short-lived state: Cua sessions, accessibility snapshots, element
handles, browser target/tab refs, capture scope, and cursor/perception state. CUMG must retain the
useful continuity without making an HTTP connection, MCP transport session, PID/window ID, or
backend token into an authorization credential.

An `InteractionContext` is therefore CUMG-owned workflow state. It is **not** an execution owner,
capability grant, quarantine resolver, or durable operation ledger.

## Binding

Every context is bound to exactly:

```text
authenticated principal (issuer + subject)
stable CUMG device ID
Agent device generation
CapabilityAdvertisement revision
```

A context identifier is random and opaque. Possession of the identifier alone grants nothing: every
use must also match the authenticated principal and exact device binding.

The context manager is not an authorizer. Creating a context or expanding its scope is allowed only after the ordinary northbound authorization/approval path has independently permitted the relevant operation. Context validation can narrow an already-authorized request; it can never create permission.

The context is deliberately independent from `Mcp-Session-Id` or any HTTP connection. Transport
reconnect/recreation must not silently merge two chats or clients into one Computer Use session.

## Lifetime

Contexts are non-durable workflow state. They are bounded by:

- maximum contexts per principal/device;
- idle expiry;
- absolute lifetime;
- explicit close;
- Agent generation change;
- capability revision change when stale backend refs could otherwise survive a changed surface.

Hub/Agent restart must never resurrect an old backend session mapping. A caller may create a new
context after reconnect, but the old context/ref set fails closed.

## Execution scope

Initial context scope is `window_scoped`.

A backend may internally use background accessibility/pixels/typed browser routes while preserving
that scope. Expanding to `desktop_scoped` is a monotonic, explicit CUMG control transition. Backend
helpers such as Cua `escalate_session` may implement the transition only after CUMG has authorized
and recorded it. The adapter must never broaden scope automatically because a narrower route failed.

Closing a context or creating a fresh context is the way to return from desktop scope to window
scope; a backend-specific in-session downgrade is not assumed.

## Scoped refs

Backend handles are never long-lived CUMG identity. Future element/browser actions use CUMG-minted
opaque refs that map to backend refs inside an in-memory scoped registry.

Each public ref is bound to:

```text
InteractionContext ID
device generation
capability revision
ref kind (snapshot, element, browser target, tab, page element, file handle, ...)
```

Resolution checks every field and fails closed on a mismatch. Generation/context invalidation drops
all mappings. Backend refs are not written to the durable safety checkpoint and are not logged.

Current `inspect_window` observation refs are not authorization credentials and no current V2 action
accepts them as an input. Element-targeted actions must switch to the scoped CUMG ref registry before
they are introduced.

## Coordinate model

New semantic pointer/scroll/capture operations must declare a coordinate space. Backends may use
other internal coordinates, but the CUMG contract must not rely on hidden `from_zoom` state.

Initial planned spaces are:

- `desktop_physical`: physical display pixels; multi-display origins may be negative;
- `window_physical`: physical pixels relative to the exact bound window image;
- `browser_viewport`: CSS pixels in the exact bound browser tab viewport.

Conversion requires explicit geometry/scale evidence. Ambiguous conversion fails closed.

## Browser file transfer

File upload/download is not ordinary clicking. Upload can exfiltrate local data; download writes
local data. They therefore use separate exact capabilities.

Uploads must accept a CUMG-issued bounded file handle rather than an arbitrary backend/local path.
Downloads must have bounded byte count, controlled destination roots, explicit overwrite behavior,
and no symlink escape. Backend browser authorization remains an additional refusal layer.

## Invariants

- `OperationOwner` remains authenticated principal identity; context never changes ownership.
- Context loss does not clear quarantine or settle an operation.
- A context or ref from another principal/device/generation/revision is rejected.
- Scope expansion is explicit and monotonic for one context.
- Backend session/config/credential material never crosses the Hub-to-Agent semantic contract.
- A context/ref is not logged as a metric label and backend-ref payload is not logged at all.
- No context mechanism can authorize replay of an ambiguous operation.
