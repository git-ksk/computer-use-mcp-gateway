# V2 Cua parity matrix

## Goal

CUMG V2 parity is behavioral, not a promise to mirror every Cua MCP tool name.
The CUMG product contract remains a bounded, backend-neutral semantic vocabulary.
A Cua tool is complete only when it is classified as one of:

- **semantic**: represented by an exact CUMG capability or typed control primitive;
- **integrated**: absorbed into an existing CUMG semantic capability;
- **backend lifecycle**: kept behind `ComputerUseBackendAdapter` and never authorized by backend tool name;
- **operator-only**: available only through a trusted local/operator plane, not normal northbound MCP;
- **intentionally excluded**: unsafe or legacy behavior with a safer typed replacement.

The cutover gate is: every reviewed Cua 0.19.3 tool has one classification, and every legitimate
workflow available through the current V1/Cua path has a V2 semantic path or an explicit documented
operator-only replacement. There must be no generic `call_tool`, `raw_cua`, or arbitrary backend
method escape hatch.

## Cross-cutting rules

1. Backend-specific names and payloads terminate at the adapter.
2. Authorization remains exact `principal -> stable device -> DeviceCapability`.
3. Interaction state is scoped by a CUMG `InteractionContext`, not by an HTTP/MCP transport session.
4. Contexts and backend refs are bound to principal, device, Agent generation, and capability revision.
5. Agent reconnect/generation change invalidates backend session mappings and all scoped refs.
6. Window-to-desktop escalation is explicit. It is never an automatic fallback.
7. Cua's own backend authorization/refusal remains defense in depth and is never bypassed by CUMG.
8. Upload/download cross a separate data-transfer boundary and require dedicated exact capabilities.
9. Recording/replay is not part of ordinary production northbound operation; replay must never bypass
   CUMG's operation identity, generation fences, quarantine, or no-auto-replay invariant.
10. Operator setup/configuration is a separate control plane, not a normal device capability.

## Cua 0.19.3 classification

| # | Cua tool | V2 disposition | Planned/Current CUMG semantic |
|---:|---|---|---|
| 1 | `list_apps` | semantic, current | `ListApplications` / `list_apps` |
| 2 | `list_windows` | semantic, current | `ListWindows` / `list_windows` |
| 3 | `get_window_state` | semantic, current | `InspectWindow` / `inspect_window` |
| 4 | `verify_state` | semantic, current | `VerifyUiState` / `verify_ui_state` |
| 5 | `launch_app` | semantic, current | `LaunchApplication` / `launch_application` |
| 6 | `kill_app` | semantic, current | `TerminateApplication` |
| 7 | `bring_to_front` | semantic, current | `ActivateWindow` with exact-window verification evidence |
| 8 | `set_window_frame` | semantic, current | `SetWindowFrame` |
| 9 | `invoke_menu` | semantic, current | exact bounded `InvokeMenu` |
| 10 | `click` | semantic, current | `PointerClick`; typed coordinates or scoped native element ref with semantic AX action |
| 11 | `double_click` | integrated | `PointerClick { click_count: 2 }`, including scoped native element targets |
| 12 | `right_click` | integrated | `PointerClick { button: right }`, including scoped native element targets |
| 13 | `drag` | semantic, current | `PointerDrag`; typed endpoints, modifiers, duration, and steps |
| 14 | `type_text` | semantic, current | `TypeText`; bounded contextual coordinate/window/element target semantics |
| 15 | `press_key` | semantic, current | `KeyboardInput`, including scoped native element targeting |
| 16 | `hotkey` | integrated | `KeyboardInput` chord |
| 17 | `set_value` | semantic, current | `SetUiValue` using scoped CUMG element refs |
| 18 | `scroll` | semantic, current | `Scroll` with explicit coordinate space/target |
| 19 | `clipboard_read` | semantic, current | `ClipboardRead` with bounded output and sensitive-data treatment |
| 20 | `clipboard_write` | semantic, current | `ClipboardWrite` with bounded input |
| 21 | `get_screen_size` | semantic, current | `ScreenGeometry` / `get_screen_size` |
| 22 | `get_desktop_state` | integrated, current | contextual `Screenshot` after explicit `DesktopScope` expansion |
| 23 | `get_cursor_position` | semantic, current | `PointerPosition` |
| 24 | `move_cursor` | semantic, current | `MovePointer` |
| 25 | `set_agent_cursor_enabled` | backend lifecycle | interaction visualization policy |
| 26 | `set_agent_cursor_motion` | backend lifecycle | interaction visualization policy |
| 27 | `set_agent_cursor_theme` | backend lifecycle | interaction visualization policy |
| 28 | `get_agent_cursor_state` | backend lifecycle/diagnostic | no normal northbound capability |
| 29 | `check_permissions` | operator-only | read-only diagnostics separate from prompt-capable setup |
| 30 | `health_report` | operator-only | backend diagnostics |
| 31 | `get_config` | operator-only | backend configuration inspection |
| 32 | `set_config` | operator-only | backend configuration mutation |
| 33 | `get_accessibility_tree` | integrated | `ListApplications` + `ListWindows`; no raw backend tree contract |
| 34 | `zoom` | semantic, current | bounded window-local `CaptureRegion`; no hidden `from_zoom` state |
| 35 | `page` | intentionally excluded/replaced | typed browser surface; arbitrary JS is not standard parity |
| 36 | `get_browser_state` | semantic, core-wired | `BrowserInspect` |
| 37 | `browser_prepare` | semantic, core-wired | `BrowserPrepare`; preserve Cua/backend authorization/refusal |
| 38 | `browser_navigate` | semantic, core-wired | `BrowserNavigate` |
| 39 | `browser_click` | semantic, core-wired | `BrowserClick` using scoped page refs |
| 40 | `browser_type` | semantic, core-wired | `BrowserType` using scoped page refs |
| 41 | `browser_dialog` | semantic, core-wired | `BrowserDialog` |
| 42 | `browser_set_input_files` | semantic, core-wired | `BrowserUploadFile`; one-shot CUMG file refs resolve only to Agent-private staged regular files |
| 43 | `browser_download` | semantic, core-wired | `BrowserDownload`; exact click ref + Agent-private destination root + bounded result ref/data |
| 44 | `browser_pointer` | semantic, core-wired | `BrowserPointer` with explicit browser viewport coordinates |
| 45 | `start_recording` | operator/test-only | local acceptance/regression tooling |
| 46 | `stop_recording` | operator/test-only | local acceptance/regression tooling |
| 47 | `get_recording_state` | operator/test-only | local acceptance/regression tooling |
| 48 | `replay_trajectory` | intentionally excluded from production | test-only; never a production replay authority |
| 49 | `install_ffmpeg` | operator-only | local dependency/setup plane |
| 50 | `start_session` | backend lifecycle, current | CUMG context ID is the backend session ID |
| 51 | `escalate_session` | explicit CUMG control, current | monotonic `WindowScoped -> DesktopScoped` |
| 52 | `get_session_state` | backend lifecycle/diagnostic | context health may be exposed only as bounded CUMG state |
| 53 | `end_session` | backend lifecycle, current | close/expiry/generation/revision cleanup ends backend session |
| 54 | `check_for_update` | operator-only | local update plane |


## Desktop parity status

The desktop-only semantic baseline exposes 29 northbound tools on the Cua 0.19.3 shadow Agent. The
browser core adds eight typed tools when the corresponding live advertisement and policy permit them.
There is no generic Cua call/proxy tool. `tools/list` is the exact policy/live-advertisement
intersection, and an offline Agent exposes no semantic device tools.

Current desktop runtime acceptance covers:

- context open bound atomically to device generation and capability revision;
- contextual window inspection and CUMG snapshot/element ref minting;
- `set_ui_value`, native click, text input, and keyboard input through same-context CUMG element refs;
- native element `press/open/show_menu/pick/confirm/cancel` mapping without raw backend handles;
- stale, unknown, cross-context, wrong-generation, wrong-revision, wrong-kind, and provider-stale ref rejection;
- trusted-Mac Calculator acceptance proving a background AX element press changes the exact window;
- verified exact-window activation;
- representative background keyboard/scroll on an unambiguous target;
- privacy-safe clipboard type-only observation;
- bounded window-local region capture;
- explicit `WindowScoped -> DesktopScoped` expansion followed by desktop screenshot;
- CUMG pre-dispatch rejection of window-only commands after desktop expansion;
- context close and generation fencing removing both CUMG refs and Cua session state.

Cua safety refusals remain authoritative. A background key/scroll may be refused when the provider
cannot prove which sibling window of one PID would receive process-scoped input; CUMG never turns
that refusal into an automatic foreground or desktop escalation.

`CONTROL_SCHEMA_VERSION` is version 7 and `CAPABILITY_SCHEMA_VERSION` remains version 4. Control
schema mismatches and capability-advertisement schema mismatches fail closed. Ordinary signed Hub/Agent messages retain the 64 KiB application bound, while bounded
image/UI/clipboard/region observations use the reviewed large-result allowance. Clipboard plain text
is capped at 1 MiB.

## Required implementation order

1. **Parity foundations** — `InteractionContext`, generation/revision binding, explicit execution scope,
   scoped opaque refs, TTL/limits, and rolling-upgrade tests.
2. **Desktop semantic parity** — application/window management, keyboard, UI value, scrolling,
   clipboard, desktop observation, pointer movement, region capture.
3. **Browser semantic parity** — exact browser binding, inspect/navigate/interact/dialog, then separate
   upload/download transfer capabilities.
4. **Operator-plane separation** — diagnostics/setup/config/update/recording stay out of ordinary MCP.
5. **Behavioral acceptance** — real-Cua workflow E2E on the trusted Mac plus backend-portability tests.
6. **Cutover** — only after the matrix has no unclassified tool and no legitimate V1 workflow gap.

## Cutover workflow gates

Desktop workflow:

```text
launch -> discover window -> inspect -> frame/menu/element action
-> keyboard -> scroll -> clipboard -> verify
```

Browser workflow:

```text
bind/prepare -> inspect -> navigate -> semantic interaction -> dialog -> verify
-> explicit bounded upload/download
```

Both workflows must preserve CUMG operation IDs, exact capability authorization, generation fencing,
indeterminate quarantine, explicit resolution, and no replay of an ambiguous operation.
