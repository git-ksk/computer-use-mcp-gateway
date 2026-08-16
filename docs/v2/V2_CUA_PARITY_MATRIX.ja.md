# V2 Cua parity matrix

> この日本語版は [`V2_CUA_PARITY_MATRIX.md`](V2_CUA_PARITY_MATRIX.md) の翻訳です。**英語版を canonical（正典）とし、解釈に差がある場合は英語版を優先します。**

## 目的

CUMG V2 の parity は**振る舞いの parity**であり、すべての Cua MCP tool 名をそのまま複製することを約束するものではありません。
CUMG の product contract は、境界が明確な backend-neutral semantic vocabulary のままです。
Cua tool は、次のいずれかに分類されて初めて disposition が確定したものとみなします。

- **semantic**: exact CUMG capability または typed control primitive で表現する。
- **integrated**: 既存の CUMG semantic capability に統合する。
- **backend lifecycle**: `ComputerUseBackendAdapter` の背後に保持し、backend tool 名そのものでは認可しない。
- **operator-only**: 通常の northbound MCP ではなく、信頼された local/operator plane でのみ利用する。
- **intentionally excluded**: unsafe / legacy behavior を除外し、より安全な typed replacement を使う。

cutover gate は、レビュー対象の Cua 0.19.3 tool がすべて分類され、現在の V1/Cua 経路で正当な workflow のすべてに V2 semantic path または明示的に文書化された operator-only replacement が存在することです。generic `call_tool`、`raw_cua`、任意の backend method を呼べる escape hatch は許可しません。

## 横断ルール

1. backend 固有の名前と payload は adapter で終端します。
2. authorization は exact `principal -> stable device -> DeviceCapability` のままです。
3. interaction state は HTTP/MCP transport session ではなく CUMG `InteractionContext` に scope されます。
4. context と backend ref は principal、device、Agent generation、capability revision に bind されます。
5. Agent reconnect / generation change は backend session mapping とすべての scoped ref を無効化します。
6. window から desktop への escalation は明示的に行います。automatic fallback にはしません。
7. Cua 自身の backend authorization/refusal は defense in depth として authoritative であり、CUMG が迂回しません。
8. upload/download は独立した data-transfer boundary を越えるため、それぞれ専用の exact capability が必要です。
9. recording/replay は通常の production northbound operation ではありません。replay が CUMG の operation identity、generation fence、quarantine、no-auto-replay invariant を迂回してはなりません。
10. operator setup/configuration は通常の device capability ではなく、別 control plane です。

## Cua 0.19.3 の分類

| # | Cua tool | V2 disposition | Planned/Current CUMG semantic |
|---:|---|---|---|
| 1 | `list_apps` | semantic, current | `ListApplications` / `list_apps` |
| 2 | `list_windows` | semantic, current | `ListWindows` / `list_windows` |
| 3 | `get_window_state` | semantic, current | `InspectWindow` / `inspect_window` |
| 4 | `verify_state` | semantic, current | `VerifyUiState` / `verify_ui_state` |
| 5 | `launch_app` | semantic, current | `LaunchApplication` / `launch_application`; V2 は現在、V1/Cua の `additional_arguments` と `webkit_inspector_port` を公開していない |
| 6 | `kill_app` | semantic, current | `TerminateApplication` |
| 7 | `bring_to_front` | semantic, current | exact-window verification evidence を伴う `ActivateWindow` |
| 8 | `set_window_frame` | semantic, current | `SetWindowFrame` |
| 9 | `invoke_menu` | semantic, current | exact bounded `InvokeMenu` |
| 10 | `click` | semantic, current | `PointerClick`; typed coordinate または semantic AX action を伴う scoped native element ref |
| 11 | `double_click` | integrated | scoped native element target を含む `PointerClick { click_count: 2 }` |
| 12 | `right_click` | integrated | scoped native element target を含む `PointerClick { button: right }` |
| 13 | `drag` | semantic, current | `PointerDrag`; typed endpoint、modifier、duration、step |
| 14 | `type_text` | semantic, current | `TypeText`; bounded contextual coordinate/window/element target semantics |
| 15 | `press_key` | semantic, current | scoped native element targeting を含む `KeyboardInput` |
| 16 | `hotkey` | integrated | `KeyboardInput` chord |
| 17 | `set_value` | semantic, current | scoped CUMG element ref を使う `SetUiValue` |
| 18 | `scroll` | semantic, current | explicit coordinate space/target を持つ `Scroll` |
| 19 | `clipboard_read` | semantic, current | bounded output と sensitive-data treatment を持つ `ClipboardRead` |
| 20 | `clipboard_write` | semantic, current | bounded **plain-text-only** input の `ClipboardWrite`; V1/Cua の image/file clipboard write parity は未実装 |
| 21 | `get_screen_size` | semantic, current | `ScreenGeometry` / `get_screen_size` |
| 22 | `get_desktop_state` | integrated, current | explicit `DesktopScope` expansion 後の contextual `Screenshot` |
| 23 | `get_cursor_position` | semantic, current | `PointerPosition` |
| 24 | `move_cursor` | semantic, current | `MovePointer` |
| 25 | `set_agent_cursor_enabled` | backend lifecycle | interaction visualization policy |
| 26 | `set_agent_cursor_motion` | backend lifecycle | interaction visualization policy |
| 27 | `set_agent_cursor_theme` | backend lifecycle | interaction visualization policy |
| 28 | `get_agent_cursor_state` | backend lifecycle/diagnostic | 通常の northbound capability なし |
| 29 | `check_permissions` | operator-only | prompt-capable setup と分離した read-only diagnostic |
| 30 | `health_report` | operator-only | backend diagnostic |
| 31 | `get_config` | operator-only | backend configuration inspection |
| 32 | `set_config` | operator-only | backend configuration mutation |
| 33 | `get_accessibility_tree` | integrated | `ListApplications` + `ListWindows`; raw backend tree contract は公開しない |
| 34 | `zoom` | semantic, current | bounded window-local `CaptureRegion`; hidden `from_zoom` state なし |
| 35 | `page` | intentionally excluded/replaced | typed browser surface; arbitrary JS は standard parity ではない |
| 36 | `get_browser_state` | semantic, core-wired | `BrowserInspect` |
| 37 | `browser_prepare` | semantic, core-wired | `BrowserPrepare`; Cua/backend authorization/refusal を保持 |
| 38 | `browser_navigate` | semantic, core-wired | `BrowserNavigate` |
| 39 | `browser_click` | semantic, core-wired | scoped page ref を使う `BrowserClick` |
| 40 | `browser_type` | semantic, core-wired | scoped page ref を使う `BrowserType` |
| 41 | `browser_dialog` | semantic, core-wired | `BrowserDialog` |
| 42 | `browser_set_input_files` | semantic, core-wired | `BrowserUploadFile`; one-shot CUMG file ref は Agent-private staging の regular file のみに解決される |
| 43 | `browser_download` | semantic, core-wired | `BrowserDownload`; exact click ref + Agent-private destination root + bounded result ref/data |
| 44 | `browser_pointer` | semantic, core-wired | explicit browser viewport coordinate を持つ `BrowserPointer` |
| 45 | `start_recording` | operator/test-only | local acceptance/regression tooling |
| 46 | `stop_recording` | operator/test-only | local acceptance/regression tooling |
| 47 | `get_recording_state` | operator/test-only | local acceptance/regression tooling |
| 48 | `replay_trajectory` | intentionally excluded from production | test-only; production replay authority にはしない |
| 49 | `install_ffmpeg` | operator-only | local dependency/setup plane |
| 50 | `start_session` | backend lifecycle, current | CUMG context ID を backend session ID として使用 |
| 51 | `escalate_session` | explicit CUMG control, current | monotonic `WindowScoped -> DesktopScoped` |
| 52 | `get_session_state` | backend lifecycle/diagnostic | context health を公開する場合も bounded CUMG state のみ |
| 53 | `end_session` | backend lifecycle, current | close/expiry/generation/revision cleanup で backend session を終了 |
| 54 | `check_for_update` | operator-only | local update plane |

## Desktop parity の状況

Desktop-only semantic baseline は Cua 0.19.3 shadow Agent 上で 29 の northbound tool を公開します。対応する live advertisement と policy が許可する場合、Browser core がさらに 8 個の typed tool を追加します。generic Cua call/proxy tool は存在しません。`tools/list` は exact policy と live advertisement の intersection であり、Agent が offline の場合は semantic device tool を公開しません。

現在の Desktop runtime acceptance は次をカバーします。

- device generation と capability revision に atomic に bind された context open。
- contextual window inspection と CUMG snapshot/element ref の mint。
- same-context CUMG element ref を使う `set_ui_value`、native click、text input、keyboard input。
- raw backend handle を公開しない native element `press/open/show_menu/pick/confirm/cancel` mapping。
- stale、unknown、cross-context、wrong-generation、wrong-revision、wrong-kind、provider-stale ref の拒否。
- background AX element press が exact window を変更することを証明する trusted-Mac Calculator acceptance。
- verified exact-window activation。
- unambiguous target に対する代表的な background keyboard/scroll。
- privacy-safe な clipboard type-only observation。
- bounded window-local region capture。
- explicit `WindowScoped -> DesktopScoped` expansion 後の desktop screenshot。
- desktop expansion 後の window-only command を CUMG pre-dispatch で拒否。
- context close と generation fencing により CUMG ref と Cua session state の両方を除去。

Cua safety refusal は引き続き authoritative です。同一 PID が複数の対象 window を持ち、provider が process-scoped input の配送先を証明できない場合、background key/scroll は拒否されることがあります。CUMG はその refusal を automatic foreground action や desktop escalation に変換しません。

既知の parity gap は passthrough behavior で隠さず、明示します。

- `ClipboardWrite` が現在対応するのは plain text のみです。V1/Cua の image clipboard write と file clipboard write は **V2 では未実装**です。
- `LaunchApplication` は現在、V1/Cua の `additional_arguments` と `webkit_inspector_port` を公開していません。これらは **未parity** です。

`CONTROL_SCHEMA_VERSION` は version 8、`CAPABILITY_SCHEMA_VERSION` は引き続き version 4 です。control schema mismatch と capability-advertisement schema mismatch は fail closed します。通常の signed Hub/Agent message は 64 KiB application bound を維持し、bounded image/UI/clipboard/region observation はレビュー済み large-result allowance を使用します。clipboard plain text の上限は 1 MiB です。

## 必須の実装順序

1. **Parity foundations** — `InteractionContext`、generation/revision binding、explicit execution scope、scoped opaque ref、TTL/limit、rolling-upgrade test。
2. **Desktop semantic parity** — application/window management、keyboard、UI value、scrolling、clipboard、desktop observation、pointer movement、region capture。
3. **Browser semantic parity** — exact browser binding、inspect/navigate/interact/dialog、その後に独立した upload/download transfer capability。
4. **Operator-plane separation** — diagnostic/setup/config/update/recording は通常 MCP の外に置く。
5. **Behavioral acceptance** — trusted Mac 上の real-Cua workflow E2E と backend-portability test。
6. **Cutover** — matrix に unclassified tool がなく、正当な V1 workflow gap がなくなってから実施。

## Cutover workflow gate

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

どちらの workflow も CUMG operation ID、exact capability authorization、generation fencing、indeterminate quarantine、explicit resolution、および ambiguous operation の no replay を維持しなければなりません。
