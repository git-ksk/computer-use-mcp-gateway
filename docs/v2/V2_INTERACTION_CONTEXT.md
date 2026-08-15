# V2 interaction context and scoped backend references

## Why this exists

Computer Use backends keep short-lived state such as Cua sessions, accessibility snapshots, element
handles, browser refs, capture scope, and cursor/perception state. CUMG retains useful continuity
without making an HTTP connection, MCP transport session, PID/window ID, or backend token into an
authorization credential.

An `InteractionContext` is CUMG-owned workflow state. It is **not** an execution owner, capability
grant, bearer credential, quarantine resolver, or durable operation ledger.

## Binding and authorization

Every context is bound to exactly:

```text
authenticated principal (issuer + subject)
stable CUMG device ID
Agent device generation
CapabilityAdvertisement revision
```

The context identifier is random and opaque. Possession of it grants nothing: every use must still
match the authenticated principal/device and pass the ordinary exact capability policy.

The context manager can only narrow an already-authorized request. It cannot create permission.
`OperationOwner` remains the authenticated principal; context state never replaces ownership of an
operation or quarantine.

The context is independent from `Mcp-Session-Id`, HTTP connections, and the MCP transport lifetime.
Transport reconnect/recreation must not silently merge separate callers into one Computer Use
workflow.

## Lifetime and backend cleanup

Contexts are non-durable and bounded by:

- maximum contexts per principal/device;
- idle expiry;
- absolute lifetime;
- explicit close;
- Agent generation change;
- capability-revision change.

Invalidation removes all CUMG scoped refs for the context. For backends with session state, CUMG also
requests backend-session cleanup. The current Cua adapter maps that cleanup to `end_session` through a
signed Hub-to-Agent lifecycle control rather than a northbound `DeviceCommand`.

The lifecycle control is intentionally outside operation admission: it does not mint a grant, change
`OperationOwner`, resolve quarantine, create a replayable operation ID, or carry a northbound bearer
credential. Cleanup failure never resurrects a locally invalid context; the request remains failed
closed and provider idle cleanup is an additional backstop.

Hub/Agent restart or Agent reconnect must never resurrect an old context/backend-session mapping. A
new context may be opened against the new generation, but the old context/ref set is invalid.

## Cua lifecycle mapping

For Cua, the context ID is used as the backend session identifier for contextual desktop commands.
The adapter keeps Cua tool names and lifecycle payloads south of the CUMG semantic boundary.

The desktop expansion path is explicit:

```text
CUMG WindowScoped context
  -> ensure Cua start_session(session=context_id, capture_scope=auto)
  -> explicit CUMG DesktopScope authorization/control
  -> Cua escalate_session(session=context_id)
  -> CUMG DesktopScoped context
```

There is no automatic fallback from a failed window route to desktop scope.

## Execution scope

Every context starts `window_scoped`. Window-scoped actions may use background accessibility,
window pixels, or other provider routes while preserving that scope.

Expansion to `desktop_scoped` is explicit and monotonic. Once expanded, window-only commands are
rejected at the CUMG boundary because Cua 0.19.3 also treats the effective session scope as desktop.
The caller must close that context and open a fresh one to regain window scope. CUMG never emulates a
backend-specific downgrade or silently starts a replacement context.

Desktop-scoped commands such as contextual desktop screenshot and desktop pointer operations are
rejected until the explicit expansion has completed.

## Scoped refs

Backend handles are not long-lived CUMG identity. `inspect_window` normalizes backend observation
refs and mints CUMG opaque refs backed by an in-memory `ScopedBackendRefRegistry`.

Each public ref is bound to:

```text
InteractionContext ID
device generation
capability revision
ref kind (snapshot, element, browser target, tab, page element, file handle, ...)
```

Resolution checks every field. Unknown, stale, cross-context, wrong-generation, wrong-revision, and
wrong-kind refs fail closed. Generation/revision/context invalidation drops all associated mappings.
Backend-ref payloads are not persisted in the durable execution checkpoint and are not logged.

`set_ui_value`, native `click`, `type_text`, and `keyboard_input` consume scoped CUMG element refs
minted by `inspect_window` in the same live context. The backend element token itself is never a
northbound action argument. Each element action also supplies the exact process/window target; a
provider rejection of a stale or mismatched window/token pair remains authoritative.

A newer provider snapshot may invalidate an older backend token even inside an otherwise live CUMG
context. That provider stale-ref refusal is preserved; CUMG does not auto-refresh and replay a
mutation.

## Coordinate model

Desktop semantic pointer, scroll, text-input, and capture commands use typed coordinate/target forms.
Current desktop forms include:

- `DesktopPhysical`: physical desktop screenshot pixels;
- `WindowPhysical`: physical pixels relative to the exact window image;
- `InputTarget`: desktop, exact window, exact window point, or same-context scoped element;
- `ScrollTarget`: exact window, exact window point, or desktop point.

Backends may use other internal coordinate systems, but CUMG does not depend on hidden provider state
such as Cua `from_zoom`. Ambiguous conversion fails closed.

A future browser phase will add an exact browser viewport coordinate contract bound to browser target
and tab identity.

## Browser file transfer

File upload/download is not ordinary clicking. Upload can exfiltrate local data; download writes
local data. They therefore remain separate exact capabilities in the browser phase.

Uploads must accept a CUMG-issued bounded file handle rather than an arbitrary backend/local path.
Downloads must bind byte limits, controlled destination roots, overwrite behavior, and symlink
handling. Backend browser authorization remains an additional refusal layer rather than something an
adapter may bypass.

## Invariants

- Context ID is workflow state, never authorization identity or a bearer credential.
- `OperationOwner` and quarantine ownership remain authenticated principal identity.
- Context loss does not clear quarantine or settle an operation.
- A context/ref from another principal/device/generation/revision is rejected.
- Scope expansion is explicit and monotonic for one context.
- Window-only commands do not run after that context becomes desktop-scoped.
- Context close/expiry/generation/revision invalidates refs and requests backend-session cleanup.
- Backend session/config/credential material never becomes the northbound semantic contract.
- Context/ref values are not metric labels and backend-ref payloads are not logged.
- No context mechanism can authorize or auto-replay an ambiguous operation.
