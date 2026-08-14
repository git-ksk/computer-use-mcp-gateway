# V2 GUI semantic capabilities

## Purpose

V2 does not make the configured Computer Use backend's MCP tool names part of the CUMG product contract. The stable boundary is a backend-neutral semantic vocabulary:

```text
northbound MCP tool
  -> exact DeviceCapability
  -> typed DeviceCommand / DeviceResult
  -> ComputerUseBackendAdapter
  -> backend-specific operation
```

Cua is the first GUI backend, but `list_windows`, `launch_app`, `get_window_state`, `verify_state`, AX role names, and other Cua wire details terminate at `CuaMcpAdapter`. A future native GUI, OpenClaw-style Computer Use runtime, or another maintained backend may implement the same semantics without copying Cua's API.

This is **not** a minimum-common-denominator rule. CUMG may add a semantic capability that only some backends implement. The Agent advertises the exact `DeviceCapability` set supported by its active backend, and the Hub keeps authorization exact to `principal -> stable device -> DeviceCapability`.

## Current semantic surface

The original portable/runtime surface remains:

- `list_apps` -> `ListApplications`
- `get_screen_size` -> `ScreenGeometry`
- `screenshot` -> `Screenshot`
- `click` -> `PointerClick`
- `drag` -> `PointerDrag`
- `type_text` -> `TypeText`
- `execute_process` -> `ExecuteProcess`
- `shell` -> `Shell`
- `read_file` -> `ReadFile`
- `list_directory` -> `ListDirectory`

The GUI semantic extension adds:

| Northbound tool | `DeviceCapability` | Class | Current Cua adapter | Contract |
|---|---|---|---|---|
| `list_windows` | `ListWindows` | Observe | `list_windows` | bounded top-level window records with backend-neutral IDs, process IDs, titles, bounds and visibility |
| `launch_application` | `LaunchApplication` | System | `launch_app` | launch by opaque application identifier or display name, with bounded targets and optional new-instance request |
| `inspect_window` | `InspectWindow` | Observe | `get_window_state` | bounded snapshot of one exact process/window as normalized UI elements, optionally with a PNG window image |
| `verify_ui_state` | `VerifyUiState` | Observe | `verify_state` | bounded predicates over one exact window; `unknown` remains distinct from success |

The northbound names intentionally do not mirror every backend name. For example, CUMG exposes `launch_application`, not Cua's `launch_app`, because the semantic operation belongs to CUMG while the backend spelling does not.

## Capability advertisement and discovery

`CapabilityAdvertisement` is the portability boundary, not a promise that every backend implements every semantic capability.

When an Agent is online, V2 northbound discovery is the intersection of:

```text
authorized exact DeviceCapability
AND
live Agent CapabilityAdvertisement
```

A connected backend that does not advertise `InspectWindow`, for example, does not expose `inspect_window` through `tools/list` even if the principal's policy contains that capability. Dispatch independently validates the Agent generation, capability revision, and exact advertised capability, so a discovery/dispatch race still fails closed.

When the Agent is offline, `tools/list` keeps the authorized semantic contract visible rather than collapsing to an empty list. The call itself fails as `AgentOffline` until a new live session exists. This avoids making the usable MCP schema depend permanently on one transient disconnect while the current server does not advertise a tool-list-changed notification.

## Backend-neutral UI model

`InspectWindow` does not forward an AX/UIA/AT-SPI tree verbatim. The adapter reduces backend fields into bounded CUMG types:

- `UiRect` for geometry;
- `WindowInfo` for top-level windows;
- `UiElement` for snapshot elements;
- `UiRole` for common roles such as `button`, `text_field`, `menu_item`, `row`, and `cell`;
- opaque `snapshot_ref` and `element_ref` values for backend-scoped handles.

Cua AX roles are normalized inside the Cua adapter. For example, `AXButton` becomes `button`, `AXTextField` becomes `text_field`, and `AXMenuItem` becomes `menu_item`. Unknown backend roles become `other` in observations; `other` is not accepted as an input selector because it has no portable matching semantics.

The opaque element reference is observation data, not a new authorization credential. A future element-targeted action must define its own typed semantic command and stale-reference behavior rather than accepting arbitrary backend arguments.

## Verification semantics

`VerifyUiState` carries typed predicates rather than a backend query language. The initial contract supports:

- window existence;
- window bounds with bounded tolerance;
- element existence by semantic role and/or bounded label substring;
- element enabled/selected/value state.

The normalized result is `satisfied`, `unsatisfied`, or `unknown`. `unknown` never implies success. Backend-specific diagnostic payloads such as Cua's raw `observed_json` do not cross the adapter boundary.

Predicate count, wait duration, stability samples, labels, values, UI element counts and screenshots are bounded. Optional screenshots use the same bounded PNG trust boundary as other V2 image results.

## Lifecycle is not a device capability

Backend session helpers such as Cua `start_session`, `end_session`, capture-scope escalation, cursor themes, and recording lifecycle are not northbound `DeviceCapability`s merely because the backend exposes them as tools.

Those operations describe the executor's local lifecycle or perception machinery, not an end-user semantic permission such as “inspect this window” or “launch this application.” They stay behind `ComputerUseBackendAdapter` unless CUMG later defines a backend-neutral lifecycle primitive with an independent security reason to expose it.

This prevents CUMG authorization policy from becoming a mirror of one backend's implementation API.

## Adding another GUI semantic capability

A backend feature should be promoted into the CUMG contract only when all of these are true:

1. The operation has a backend-neutral meaning that can be named without referring to Cua, AX, UIA, AT-SPI, CDP, or another implementation API.
2. Inputs and outputs can be represented as bounded typed CUMG data rather than an arbitrary JSON/tool passthrough.
3. Its capability class and read-only/mutating behavior are explicit.
4. Cancellation/timeout ambiguity maps into the existing CUMG execution-safety and quarantine model.
5. A backend can omit the capability from its advertisement without weakening another capability.
6. The adapter has normalization/conformance tests, and a real backend acceptance path exists for behavior that compile-time fixtures cannot prove.

Likely future candidates include semantic key input, scrolling, window-frame management, and element-targeted actions. They should be added one semantic primitive at a time; a generic `call_tool`, `raw_cua`, or arbitrary backend method escape hatch remains out of scope.

## Security invariants unchanged

The GUI extension does not change the authoritative safety model:

- northbound authentication reduces to `AuthenticatedClientPrincipal` before authorization;
- authorization remains exact `principal -> stable device -> DeviceCapability`;
- Hub/Agent generation and capability-revision fencing remain authoritative;
- short-lived exact grants remain southbound;
- a mutating operation with unproven cancellation/timeout effect remains `indeterminate` and quarantines the device;
- backend-specific credentials, session state and raw identity/tool headers are not forwarded through the command contract;
- a malicious authenticated backend remains outside CUMG's ability to attest and may still lie or act outside the requested operation.

The semantic layer improves replaceability and reduces accidental backend coupling; it does not turn the backend into a trusted execution environment.
