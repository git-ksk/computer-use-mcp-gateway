# V2 GUI semantic capabilities

> English is the canonical documentation. [日本語版 / Japanese translation](V2_GUI_SEMANTIC_CAPABILITIES.ja.md)

## Purpose

V2 does not make a configured Computer Use backend's MCP tool names part of the CUMG product
contract. The stable boundary is a backend-neutral semantic vocabulary:

```text
northbound MCP tool
  -> exact DeviceCapability
  -> typed DeviceCommand / DeviceResult
  -> ComputerUseBackendAdapter
  -> backend-specific operation
```

Cua is the first GUI backend, but Cua tool names, AX roles, session helpers, and provider payloads
terminate at `CuaMcpAdapter`. Another backend may implement the same CUMG semantics without copying
Cua's API.

This is **not** a portable minimum-common-denominator rule. CUMG may define a semantic capability
that only some backends implement. The Agent advertises its exact live `DeviceCapability` set, and
northbound discovery exposes only the intersection of that advertisement and the authenticated
principal's exact device-capability policy.

## Desktop semantic surface

The V2 desktop surface now includes the existing runtime capabilities plus the desktop parity
extension:

| Northbound tool | `DeviceCapability` | Contract |
|---|---|---|
| `list_apps` | `ListApplications` | bounded application observation |
| `get_screen_size` | `ScreenGeometry` | physical/logical display geometry |
| `screenshot` | `Screenshot` | bounded desktop PNG; contextual desktop use requires explicit scope expansion |
| `click` | `PointerClick` | typed coordinate clicks or same-context element refs with bounded semantic actions |
| `drag` | `PointerDrag` | typed endpoints, button/modifiers, bounded duration/steps |
| `type_text` | `TypeText` | bounded text, including same-context element-ref targeting, with explicit delivery |
| `list_windows` | `ListWindows` | bounded backend-neutral top-level window records |
| `launch_application` | `LaunchApplication` | launch by opaque identifier/name with bounded targets |
| `inspect_window` | `InspectWindow` | bounded normalized UI snapshot; mints CUMG scoped refs |
| `verify_ui_state` | `VerifyUiState` | bounded predicates; `unknown` is never success |
| `terminate_application` | `TerminateApplication` | exact process termination; dangerous capability |
| `activate_window` | `ActivateWindow` | activate a process or exact window with verification evidence |
| `set_window_frame` | `SetWindowFrame` | set and verify exact top-level window geometry |
| `invoke_menu` | `InvokeMenu` | invoke a bounded semantic menu path without raw backend selectors |
| `keyboard_input` | `KeyboardInput` | bounded semantic key/modifiers, including same-context element-ref targeting |
| `scroll` | `Scroll` | bounded direction/granularity/amount against an explicit target |
| `clipboard_read` | `ClipboardRead` | bounded types and optional privacy-sensitive text |
| `clipboard_write` | `ClipboardWrite` | bounded plain-text replacement |
| `get_pointer_position` | `PointerPosition` | real desktop pointer observation |
| `move_pointer` | `MovePointer` | real pointer movement in typed desktop coordinates |
| `set_ui_value` | `SetUiValue` | set a bounded value through a CUMG-minted element ref |
| `capture_region` | `CaptureRegion` | bounded window-local capture without hidden zoom state |
| `expand_interaction_scope` | `DesktopScope` | explicit monotonic window-to-desktop scope expansion |

`open_interaction_context` and `close_interaction_context` are workflow controls rather than device
capabilities. They create or invalidate bounded CUMG workflow state; possession of a context ID does
not authorize any device operation.

Process, shell, and bounded filesystem tools remain separate non-GUI V2 capabilities. Browser and
file-transfer parity are deliberately excluded from this desktop phase.

Process/shell policy failures cross the Hub/Agent boundary only as stable coarse codes. In addition to
`environment_key_denied`, `invalid_environment`, and `too_many_environment_entries`, callers may
receive `working_directory_denied`, `working_directory_invalid`, `invalid_timeout`, `invalid_program`,
`program_denied`, `too_many_arguments`, or `process_spawn_failed`. Shell preserves the safe category
of its wrapped process error. No path, program/argv, environment key/value, or raw OS error is returned;
unknown/internal executor failures remain coarse and fail closed. This reviewed wire-contract addition
is why the live control schema is version 9; persisted registry and grant-ledger schemas remain
independently versioned.

## Capability advertisement and discovery

`CapabilityAdvertisement` is the backend portability boundary. `tools/list` is the exact intersection
of:

```text
principal/device policy allow
AND
current live Agent CapabilityAdvertisement
```

If the Agent is offline, there is no live advertisement and no semantic device tool is exposed. A
reconnect may produce a new device generation or capability revision; stateful requests are fenced
against both, so a discovery/dispatch race fails closed.

The control schema is version 9 and the capability-advertisement schema is version 5. Capability schema v5 coordinates the signed payload-free reconciliation-report frame used after a fresh authenticated generation. Hub and
Agent control-schema mismatches fail closed; capability advertisements with another schema version are
also rejected rather than interpreted as an ambiguous rolling-upgrade compatibility mode.

## Interaction context and backend lifecycle

An `InteractionContext` is CUMG workflow state, independent of HTTP or MCP transport sessions. It is
bound to authenticated principal, stable device, Agent generation, and capability revision. The
context ID is opaque state identity, not a bearer credential or authorization token.

For Cua, the adapter uses the CUMG context ID as the backend session identifier. Cua 0.19.3's
default backend-session idle TTL (300 seconds) is shorter than the CUMG InteractionContext idle
lifetime, so an otherwise-valid context can outlive Cua session state during Human Handoff. A
contextual `verify_ui_state` therefore idempotently ensures `start_session(capture_scope=auto)` after
ordinary context/Handoff admission and before `verify_state`; refresh failure stops verification
fail-closed. This only restores window-scoped backend lifecycle state and cannot revive a stale CUMG
context or grant desktop scope. Desktop expansion separately ensures `start_session(capture_scope=auto)`
and then invokes `escalate_session`. There is no automatic escalation after a narrower route fails.

Cua's `start_session` and `end_session` remain backend lifecycle, not raw northbound capabilities.
Context close, expiry, generation fencing, and capability-revision fencing invalidate CUMG refs and
request backend-session cleanup through a signed Hub-to-Agent lifecycle control. That control does
not create a `DeviceCommand`, grant, `OperationOwner`, replay identity, or quarantine transition.

Scope is monotonic. A `window_scoped` context may be explicitly expanded to `desktop_scoped`, but it
cannot return to window scope in place. After expansion, window-only commands fail at the CUMG
boundary; the caller must close the context and open a fresh one. This matches the Cua 0.19.3 session
contract instead of bypassing its refusal behavior.

## Scoped backend references

Raw backend snapshot/element handles never become northbound action authority. `inspect_window`
normalizes observations and mints CUMG opaque refs such as `ref_...`. Each mapping is held in memory
and is bound to:

```text
InteractionContext ID
device generation
capability revision
ref kind
```

`set_ui_value`, `click`, `type_text`, and `keyboard_input` can consume a CUMG element ref from the
same live context. The Hub resolves that opaque ref only after context/device/generation/revision/kind
checks; the command still carries the exact process/window target to the backend, whose element token
must agree with that window. Unknown, stale, cross-context, wrong-generation, wrong-revision,
wrong-kind, or provider-rejected window/token combinations fail closed. CUMG does not auto-refresh
and replay a mutation.

The context and scoped ref registries do not replace `OperationOwner`. Quarantine ownership and
indeterminate resolution remain bound to the authenticated principal.

## Backend-neutral UI and coordinate model

`InspectWindow` does not forward an AX/UIA/AT-SPI tree verbatim. The adapter reduces backend fields
into bounded CUMG data such as `UiRect`, `WindowInfo`, `UiElement`, and semantic `UiRole` values.
Unknown provider-specific roles normalize to `other` rather than escaping backend vocabulary.
`ListWindows` validates every provider record but omits zero-area helper/agent windows because they
cannot be targeted by the V2 window contract. Exact window targets and snapshots continue to require
strictly positive geometry; malformed non-geometry fields still fail closed.

Desktop actions use typed coordinates/targets such as `DesktopPhysical`, `WindowPhysical`, scoped
native element targets, `InputTarget`, and `ScrollTarget`. Element targets never expose Cua
`element_token`, `element_index`, or `snapshot_id` northbound. CUMG also does not expose Cua's hidden
`from_zoom` coordinate state.
`CaptureRegion` is window-local; desktop observation after explicit expansion is performed through the
contextual desktop screenshot path.

Background delivery remains the first rung where the semantic command permits it. Backend safety
refusals remain authoritative. For example, Cua may reject a process-scoped background key when one
PID owns multiple eligible windows and exact delivery cannot be proven; CUMG does not silently turn
that into a foreground action.

For execution-safety classification, the semantic command itself defines whether it is read-only. After dispatch of a mutating command, a generic backend error, response loss, or malformed/unprovable completion is not terminal failure evidence. The adapter/Agent classifies that uncertainty as `BackendOutcomeIndeterminate`; the Hub persists durable `Indeterminate` with reason `BackendOutcomeUnproven`, cancels queued work for the affected desktop, and quarantines the device until explicit persistence-gated resolution. The corresponding event on an explicitly read-only command may remain a definite backend error. Reviewed semantic refusals keep their typed refusal codes. No ambiguous mutating operation is automatically retried or replayed.

## Bounded carrier and privacy

Ordinary signed Hub/Agent application messages remain bounded to 64 KiB. Explicitly bounded large
observation results use the reviewed large-result carrier allowance: screenshots, UI snapshots,
clipboard observations, and region captures. Image payloads, UI element counts, dimensions, labels,
queries, menu paths, modifiers, text, and other arguments/results retain explicit limits.

Clipboard plain text is capped at 1 MiB. Clipboard contents are user data, not telemetry; operators
should prefer type-only observation when the text itself is unnecessary.

## Browser and data-transfer boundary

Browser semantic parity is implemented separately through backend-neutral inspect/bind, navigate,
click, type, dialog, upload, download, and pointer contracts without exposing raw Cua/CDP methods.

Upload is a local-data exfiltration boundary and must use a CUMG-issued file ref rather than an
arbitrary local path. Download is a local-write boundary and must independently bind destination,
size, and overwrite behavior. These capabilities are intentionally absent from this desktop PR.

Operator configuration, diagnostics, update checks, recording, and test/replay controls remain on an
operator plane rather than the ordinary user-facing northbound MCP surface.

## Security invariants unchanged

The semantic extension preserves the authoritative V2 safety model:

- authentication reduces to `AuthenticatedClientPrincipal` before capability authorization;
- authorization remains exact `principal -> stable device -> DeviceCapability`;
- generation and capability-revision fencing remain authoritative;
- southbound execution still uses short-lived exact grants rather than forwarding bearer/proxy
  credentials;
- mutating cancellation/timeout with unproven effect remains indeterminate and quarantines the
  device;
- no ambiguous operation is automatically replayed;
- desktop escalation is explicit only;
- raw Cua passthrough remains forbidden;
- backend-specific credentials, tool names, refs, and payload contracts terminate at the adapter.

The complete Cua 0.19.3 disposition is tracked in
[`V2_CUA_PARITY_MATRIX.md`](V2_CUA_PARITY_MATRIX.md). Stateful workflow rules are defined in
[`V2_INTERACTION_CONTEXT.md`](V2_INTERACTION_CONTEXT.md).
