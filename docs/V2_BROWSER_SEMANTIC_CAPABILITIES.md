# V2 browser semantic capabilities

## Purpose

CUMG V2 browser parity is a backend-neutral semantic surface. Cua 0.19.3 is the first backend, but
Cua tool names, raw CDP target ids, CDP session ids, browser profile authorization artifacts, and
provider refs do not become part of the CUMG northbound contract.

The stable route remains:

```text
northbound MCP tool
  -> exact DeviceCapability
  -> typed CUMG browser command
  -> InteractionContext + scoped CUMG refs
  -> ComputerUseBackendAdapter
  -> backend-specific browser operation
```

Browser state does not create a new authorization identity. `OperationOwner` remains the authenticated
principal, and every command still uses the ordinary exact device-capability grant, generation fence,
capability-revision fence, quarantine, and no-auto-replay semantics.

## Canonical workflow

The browser workflow is deliberately observation-first and exact-or-refuse:

```text
open interaction context
-> discover exact native browser window
-> optional explicit browser prepare
-> bind exact (process, window)
-> inspect selected tab and mint CUMG refs
-> navigate / click / type / pointer / dialog
-> fresh inspect for verification
-> [later transfer closeout: explicit upload / download]
-> close interaction context
```

The Cua backend also models browser targets, tabs, and page refs as session-scoped capabilities. CUMG
terminates those backend identifiers at the adapter and exposes only its own opaque `ref_...` values.

## Semantic capabilities

The planned browser runtime surface is:

| Northbound semantic | Exact capability | Contract |
|---|---|---|
| browser bind/inspect | `BrowserInspect` | read-only exact native-window bind and bounded semantic snapshot |
| browser prepare | `BrowserPrepare` | explicit setup only; never a hidden read side effect |
| browser navigate | `BrowserNavigate` | exact target/tab navigation; `http`, `https`, or `about` only |
| browser click | `BrowserClick` | current page ref preferred; typed viewport CSS coordinates only on the trusted route |
| browser type | `BrowserType` | current editable ref; bounded insert/keystroke mode and explicit replace semantics |
| browser dialog | `BrowserDialog` | inspect page-owned dialog, mint opaque ref, then explicit accept/dismiss with bounded prompt text |
| browser pointer | `BrowserPointer` | hover/right-click/double-click/scroll/drag through current semantic refs |
| browser upload | `BrowserUploadFile` | exact upload capability; CUMG-issued file refs only |
| browser download | `BrowserDownload` | exact download capability; bounded destination root, size, and overwrite policy |

No generic `page`, `call_tool`, `raw_cua`, raw CDP method, CSS selector escape hatch, or JavaScript
evaluation surface is introduced.

The current core runtime advertises only prepare, bind/inspect, navigate, click, type, dialog, and
pointer. `BrowserUploadFile` and `BrowserDownload` are schema-visible for rolling compatibility but are
not live-advertised until the independent transfer closeout is complete.

## Exact browser binding

Binding starts from an exact native `(process_id, window_id)` that CUMG already observed through the
native window surface. The backend may classify or correlate that window, but northbound mutation is
allowed only after an exact binding result.

Heuristic title matching is read-only evidence, never action authority. A moved tab, process restart,
endpoint-owner mismatch, stale geometry, ambiguous native window, Agent reconnect, or capability
revision change invalidates the binding. CUMG never selects a similar-looking target as fallback.

The Hub maps backend browser identifiers into the browser-specific `BrowserRefRegistry`. At minimum:

```text
BrowserTarget
BrowserTab
BrowserElement
```

must remain bound to the same `InteractionContext`, stable device, Agent generation, and capability
revision. Raw backend ids never cross northbound.

## Browser inspection

CUMG exposes one semantic snapshot format rather than the backend's legacy compatibility formats.
The normalized observation may contain:

- bounded page outline;
- action refs and read-only content refs;
- backend-neutral role/name/value/state/action fields;
- frame/visibility classification;
- completion/omission metadata;
- an opaque continuation capability;
- optional bounded exact-tab screenshot and viewport CSS dimensions.

A fresh snapshot invalidates older snapshot/action/content/continuation refs for that tab. A
page-owned dialog ref is independent of semantic snapshot pagination and survives a mere snapshot
refresh; navigation, a fresh dialog inspection, successful dialog resolution, or context fencing
invalidates it. Unknown, stale, cross-context, wrong-generation, wrong-revision, wrong-tab, and
wrong-kind refs fail closed. A content ref is not automatically an action ref.

Page text, labels, URLs, attributes, and dialog text are untrusted application content. They cannot
grant approval, change policy, expand scope, select a capability, or override the caller's request.

## Preparation boundary

Inspection is read-only and never performs browser setup implicitly. When the backend reports that
setup is required, the caller must invoke the exact `BrowserPrepare` capability.

CUMG supports semantic profile intent such as:

- isolated new profile;
- isolated named profile;
- existing profile attachment.

Existing-profile attachment is an elevated backend resource boundary. The CUMG request does **not**
accept or forward Cua approval tokens, launch grants, bearer credentials, proxy credentials, CDP
credentials, or host authorization artifacts. Backend/operator authorization remains behind the
adapter and may refuse the request even after CUMG capability authorization succeeds.

CUMG never edits browser profile files, copies a personal profile into an isolated directory,
terminates/restarts a personal browser as hidden setup, or automates a generic consent dialog.

## Navigation

`BrowserNavigate` accepts only bounded `http:`, `https:`, and `about:` destinations. `file:`, `data:`,
`javascript:`, custom schemes, leading/trailing whitespace, and control characters are rejected before
backend dispatch.

Navigation is mutating and never auto-replayed after an ambiguous cancellation or timeout. It
invalidates existing page refs, so the next ref-based action requires a fresh snapshot.

## Input trust and background-first semantics

Browser input preserves the backend's explicit trust distinction rather than silently changing
routes:

- `trusted`: browser-engine trusted input where the backend can preserve the requested posture;
- `dom_event`: explicit synthetic DOM-event route when its semantics are acceptable.

A synthetic click requires a current action ref. CUMG does not offer synthetic coordinate clicks or
arbitrary JavaScript execution. If the backend refuses trusted input because background posture cannot
be preserved, CUMG returns the refusal. It does not silently foreground the browser, switch to a DOM
event, or expand desktop scope.

Completion proves dispatch according to the selected route, not application-level success. Callers
must verify the expected page state with a fresh browser snapshot.

## Cua 0.19.3 result normalization

Cua 0.19.3 has two reviewed result shapes at the browser adapter boundary. Bind, inspect, navigate,
and dialog retain their browser-specific structured results. `browser_click`, `browser_type`, and
`browser_pointer` are projected through Cua's closed action-result chokepoint before CUMG sees them.
CUMG accepts that projection only when the observed route exactly matches the requested semantic:
`trusted_input` for trusted input, `dom` for explicit DOM events, and `background` as the browser
delivery posture. Unknown routes, unknown fields, partial delivery, suspected no-op, or actual
foreground/pixel/session escalation fail closed.

For an explicit DOM action, Cua may return `effect=unverifiable` with the narrow recommendation
`target=page, reason=effect_unconfirmed`. This is not evidence that Cua performed another action or
changed input route; it is a request to verify the already-dispatched page action. CUMG therefore
returns `verification_required=true` and requires a fresh inspect rather than replaying, foregrounding,
or escalating automatically.

Cua semantic-ref `states` are provider objects, not northbound authority. CUMG reduces only the
reviewed state keys and values into a small backend-neutral string vocabulary and ignores unknown
provider state keys. Browser refusal outcomes are likewise normalized before the generic MCP error
path: Cua may return `status=refused` without setting MCP `isError`, and only the closed refusal code
crosses the adapter boundary.

## Dialog boundary

`BrowserDialog action=inspect` observes only a page-owned JavaScript dialog on one exact bound tab.
The backend dialog id terminates at the Hub and, when a dialog is present, is replaced by a fresh
opaque CUMG dialog ref. Inspect carries no dialog ref, prompt text, or resolution authority. Browser
permission UI, authentication sheets, native file pickers, save panels, browser chrome, and other
native dialogs remain on the native window semantic path.

Accept/dismiss require the current dialog ref. Prompt text is accepted only with an explicit accept
action and remains bounded. Successful resolution consumes the ref. A fresh dialog inspection
replaces the prior dialog ref; navigation invalidates it. Delivery posture is explicit. A backend
refusal for background dialog resolution leaves the current ref available for an explicit caller
decision but is not permission for an automatic foreground retry.

## Upload boundary

Browser upload is a local-data exfiltration boundary and therefore has its own exact capability.
Northbound requests contain only CUMG-issued file refs:

```text
BrowserUploadFile {
  context,
  target_ref,
  tab_ref,
  element_ref,
  file_refs[]
}
```

There is no arbitrary local path field. CUMG resolves each file ref only after principal/device
policy authorization and scoped-ref validation. The upload set is bounded to 32 unique files. The
backend may apply stricter regular-file, symlink, directory, or platform checks and its refusal remains
authoritative.

Local paths must not appear in normal northbound results, telemetry, or audit records.

## Download boundary

Browser download is an independent local-write capability. It is not implied by navigation, click,
filesystem read, browser upload, or ordinary browser interaction.

The CUMG contract binds:

```text
BrowserDownload {
  context,
  target_ref,
  tab_ref,
  element_ref,
  destination_root_ref,
  destination_name,
  max_bytes,
  overwrite
}
```

`destination_root_ref` is a CUMG-issued storage capability, not a caller-provided arbitrary path.
`destination_name` is a caller-chosen, path-safe basename inside that root; it is never inferred
from an untrusted server filename. `max_bytes` is mandatory and bounded by the protocol-wide
absolute ceiling. Overwrite behavior applies to that exact destination name on every request.

The normal result contains only an opaque download ref and byte count. Source URL, server filename,
and resolved local destination path are not necessary northbound authority and should remain hidden
unless a separately reviewed product requirement introduces them.

## Browser chrome and unsupported engines

Typed browser semantics operate on exactly bound page content. Tabs, address bars, menus, bookmarks,
extension UI, permission prompts, browser-native file/save dialogs, and unsupported embedded engines
remain native-window concerns.

CUMG never uses `Cmd+L`/`Ctrl+L`, shell launchers, browser activation scripts, arbitrary JavaScript, or
legacy Cua `page` mutations as an implicit browser API fallback.

## Carrier and privacy

Ordinary command/control frames retain the existing 64 KiB application bound. Browser bind/snapshot
observations use the existing reviewed 28 MiB bounded-large gRPC result allowance; Browser mutations
remain on the ordinary bound. Normalized browser structured metadata is capped at 2 MiB and PNG
screenshot bytes at 16 MiB before the signed result is emitted. This leaves explicit carrier-envelope
headroom while preserving limits on:

- screenshot bytes and dimensions;
- outline bytes;
- action/content ref counts;
- role/name/value/state text;
- query and continuation material;
- typed input text and prompt text;
- upload ref count;
- download byte ceiling.

Cua reports viewport CSS dimensions as numeric values and may serialize integral dimensions as JSON
floating-point values such as `1200.0`. CUMG accepts only finite, positive, mathematically integral
values that fit the reviewed integer bounds; fractional viewport dimensions are refused rather than
silently rounded. Pixel-to-CSS scale remains serialized as integer millionths on the signed transport.

Browser page content and screenshots are user data, not telemetry.

## Lifecycle and cleanup

Browser backend state is owned by the `InteractionContext`, not the HTTP/MCP transport session.
Closing, expiring, generation-fencing, or capability-revision-fencing a context invalidates browser
bindings, tab refs, element refs, continuation/dialog/download capabilities, file-transfer refs, and
backend session state.

Agent reconnect never resurrects a prior browser target, tab, ref, or prepared backend session. The
caller must bind and inspect again.

## Acceptance gates

Browser parity is complete only when all of the following are true:

1. exact native-window bind succeeds and heuristic bind cannot mutate;
2. raw backend target/tab/CDP ids never appear northbound;
3. inspect mints scoped CUMG target/tab/page refs;
4. fresh snapshots invalidate older page refs;
5. navigate invalidates page refs;
6. click/type/pointer refuse stale, cross-context, wrong-generation, wrong-revision, and wrong-kind refs;
7. backend input-trust refusal is preserved without automatic foreground or route switching;
8. page dialogs use fresh opaque dialog refs and native dialogs remain excluded;
9. upload has no local-path northbound field and accepts only exact CUMG file refs;
10. download binds destination root, path-safe destination name, byte ceiling, and overwrite policy independently;
11. ambiguous cancellation/timeout still creates the ordinary CUMG indeterminate/quarantine state;
12. close/expiry/reconnect removes all browser refs and backend session state;
13. resource/ref counts plateau under repeated browser context cycles;
14. real Cua 0.19.3 browser workflow passes on the trusted Mac;
15. V1/Cloudflare production remains unchanged until the final V2 cutover gate.

See [`V2_CUA_PARITY_MATRIX.md`](V2_CUA_PARITY_MATRIX.md),
[`V2_GUI_SEMANTIC_CAPABILITIES.md`](V2_GUI_SEMANTIC_CAPABILITIES.md), and
[`V2_INTERACTION_CONTEXT.md`](V2_INTERACTION_CONTEXT.md) for the surrounding V2 contract.
