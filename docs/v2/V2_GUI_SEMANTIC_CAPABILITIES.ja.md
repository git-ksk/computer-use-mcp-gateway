# V2 GUI semantic capabilities

> この日本語版は [`V2_GUI_SEMANTIC_CAPABILITIES.md`](V2_GUI_SEMANTIC_CAPABILITIES.md) の翻訳です。**英語版を canonical（正典）とし、解釈に差がある場合は英語版を優先します。**

## 目的

V2 は、設定された Computer Use backend の MCP tool 名を CUMG product contract の一部にはしません。安定した境界は backend-neutral な semantic vocabulary です。

```text
northbound MCP tool
  -> exact DeviceCapability
  -> typed DeviceCommand / DeviceResult
  -> ComputerUseBackendAdapter
  -> backend-specific operation
```

Cua は最初の GUI backend ですが、Cua tool 名、AX role、session helper、provider payload は `CuaMcpAdapter` で終端します。別 backend は Cua API を複製せずに同じ CUMG semantics を実装できます。

これは portable な「最小共通分母」ルールではありません。CUMG は一部 backend のみが実装する semantic capability を定義できます。Agent は現在利用可能な exact `DeviceCapability` set を advertise し、northbound discovery はその advertisement と、認証済み principal に対する exact device-capability policy の intersection のみを公開します。

## Desktop semantic surface

V2 Desktop surface には、既存 runtime capability に加えて desktop parity extension が含まれます。

| Northbound tool | `DeviceCapability` | Contract |
|---|---|---|
| `list_apps` | `ListApplications` | bounded application observation |
| `get_screen_size` | `ScreenGeometry` | physical/logical display geometry |
| `screenshot` | `Screenshot` | bounded desktop PNG; contextual desktop use には explicit scope expansion が必要 |
| `click` | `PointerClick` | typed coordinate click または bounded semantic action を持つ same-context element ref |
| `drag` | `PointerDrag` | typed endpoint、button/modifier、bounded duration/step |
| `type_text` | `TypeText` | same-context element-ref targeting と explicit delivery を含む bounded text |
| `list_windows` | `ListWindows` | bounded backend-neutral top-level window record |
| `launch_application` | `LaunchApplication` | opaque identifier/name と bounded target による起動 |
| `inspect_window` | `InspectWindow` | bounded normalized UI snapshot; CUMG scoped ref を mint |
| `verify_ui_state` | `VerifyUiState` | bounded predicate; `unknown` を success とみなさない |
| `terminate_application` | `TerminateApplication` | exact process termination; dangerous capability |
| `activate_window` | `ActivateWindow` | verification evidence を伴う process または exact window の activate |
| `set_window_frame` | `SetWindowFrame` | exact top-level window geometry を set + verify |
| `invoke_menu` | `InvokeMenu` | raw backend selector を使わない bounded semantic menu path |
| `keyboard_input` | `KeyboardInput` | same-context element-ref targeting を含む bounded semantic key/modifier |
| `scroll` | `Scroll` | explicit target に対する bounded direction/granularity/amount |
| `clipboard_read` | `ClipboardRead` | bounded type と optional privacy-sensitive text |
| `clipboard_write` | `ClipboardWrite` | bounded plain-text replacement |
| `get_pointer_position` | `PointerPosition` | real desktop pointer observation |
| `move_pointer` | `MovePointer` | typed desktop coordinate 上の real pointer movement |
| `set_ui_value` | `SetUiValue` | CUMG-minted element ref 経由の bounded value 設定 |
| `capture_region` | `CaptureRegion` | hidden zoom state を持たない bounded window-local capture |
| `expand_interaction_scope` | `DesktopScope` | explicit monotonic window-to-desktop scope expansion |

`open_interaction_context` と `close_interaction_context` は device capability ではなく workflow control です。bounded CUMG workflow state を作成・無効化しますが、context ID を持っているだけでは device operation は認可されません。

process、shell、bounded filesystem tool は独立した non-GUI V2 capability のままです。Browser と file-transfer parity はこの Desktop phase とは分離されています。

process/shell の environment policy failure は、stable かつ coarse な code のみで Hub/Agent boundary を通過します。`environment_key_denied`、`invalid_environment`、`too_many_environment_entries` は northbound caller が remediation に利用できる public reason ですが、environment value は error に返しません。unknown/internal executor failure は引き続き coarse かつ fail closed です。この reviewed error-contract addition により live control schema は version 8 とし、persisted registry / grant-ledger schema は独立 versioning のままです。

## Capability advertisement と discovery

`CapabilityAdvertisement` は backend portability boundary です。`tools/list` は次の exact intersection です。

```text
principal/device policy allow
AND
current live Agent CapabilityAdvertisement
```

Agent が offline の場合は live advertisement がないため、semantic device tool は公開されません。reconnect によって新しい device generation または capability revision になる場合があります。stateful request は両方に対して fence されるため、discovery/dispatch race は fail closed します。

control schema は version 8、capability-advertisement schema は version 4 のままです。Hub と Agent の control-schema mismatch は fail closed し、別 version の capability advertisement も曖昧な rolling-upgrade compatibility と解釈せず拒否します。

## Interaction context と backend lifecycle

`InteractionContext` は HTTP/MCP transport session から独立した CUMG workflow state です。authenticated principal、stable device、Agent generation、capability revision に bind されます。context ID は opaque state identity であり、bearer credential や authorization token ではありません。

Cua では adapter が CUMG context ID を backend session identifier として使います。Desktop expansion では明示的に `start_session(capture_scope=auto)` を成立させ、その後 `escalate_session` を呼びます。narrower route が失敗した後に automatic escalation は行いません。

Cua の `start_session` / `end_session` は backend lifecycle のままで、raw northbound capability ではありません。context close、expiry、generation fencing、capability-revision fencing は CUMG ref を無効化し、signed Hub-to-Agent lifecycle control を通じて backend-session cleanup を要求します。この control は `DeviceCommand`、grant、`OperationOwner`、replay identity、quarantine transition を作成しません。

scope は monotonic です。`window_scoped` context は明示的に `desktop_scoped` へ拡張できますが、その場で window scope に戻せません。拡張後の window-only command は CUMG boundary で失敗し、caller は context を閉じて新しい context を開く必要があります。これは Cua 0.19.3 session contract の refusal behavior を迂回しません。

## Scoped backend reference

raw backend snapshot/element handle を northbound action authority にしません。`inspect_window` は observation を normalize し、`ref_...` のような CUMG opaque ref を mint します。各 mapping は memory 内に保持され、次に bind されます。

```text
InteractionContext ID
device generation
capability revision
ref kind
```

`set_ui_value`、`click`、`type_text`、`keyboard_input` は同じ live context の CUMG element ref を利用できます。Hub は context/device/generation/revision/kind check 後にのみ opaque ref を resolve します。command は exact process/window target も backend に渡し、backend の element token はその window と一致しなければなりません。unknown、stale、cross-context、wrong-generation、wrong-revision、wrong-kind、provider-rejected な window/token 組み合わせは fail closed します。CUMG は mutation を auto-refresh して replay しません。

context registry と scoped-ref registry は `OperationOwner` を置き換えません。quarantine ownership と indeterminate resolution は authenticated principal に bind されたままです。

## Backend-neutral UI と coordinate model

`InspectWindow` は AX/UIA/AT-SPI tree をそのまま転送しません。adapter は backend field を `UiRect`、`WindowInfo`、`UiElement`、semantic `UiRole` など bounded CUMG data に縮約します。unknown provider-specific role は backend vocabulary を外に漏らさず `other` に normalize します。

`ListWindows` はすべての provider record を検証しますが、V2 window contract で target にできない zero-area helper/agent window は省略します。exact window target と snapshot は引き続き strictly positive geometry を要求し、malformed non-geometry field も fail closed します。

Desktop action は `DesktopPhysical`、`WindowPhysical`、scoped native element target、`InputTarget`、`ScrollTarget` など typed coordinate/target を使います。element target は Cua の `element_token`、`element_index`、`snapshot_id` を northbound に公開しません。CUMG は Cua の hidden `from_zoom` coordinate state も公開しません。

`CaptureRegion` は window-local です。explicit expansion 後の desktop observation は contextual desktop screenshot path を使います。

semantic command が許可する場合、background delivery を最初の選択肢とします。backend safety refusal は authoritative のままです。たとえば、同じ PID に複数の eligible window があり exact delivery を証明できないとき、Cua は process-scoped background key を拒否できます。CUMG がそれを黙って foreground action に変換することはありません。

execution-safety classification では、semantic command 自体が read-only かどうかを定義します。mutating command を dispatch した後の generic backend error、response loss、malformed/unprovable completion は terminal failure evidence ではありません。adapter/Agent はこの uncertainty を `BackendOutcomeIndeterminate` と分類し、Hub は reason `BackendOutcomeUnproven` を持つ durable `Indeterminate` として永続化します。affected desktop に queue 済みの work は cancel され、explicit かつ persistence-gated な resolution が完了するまで device は quarantine されます。explicitly read-only command で同等の event が起きた場合は definite backend error のまま扱える場合があります。reviewed semantic refusal は typed refusal code を維持します。ambiguous な mutating operation は automatic retry / replay しません。

## Bounded carrier と privacy

通常の signed Hub/Agent application message は 64 KiB bound のままです。explicitly bounded large observation result では、reviewed large-result carrier allowance を使います。対象は screenshot、UI snapshot、clipboard observation、region capture です。image payload、UI element count、dimension、label、query、menu path、modifier、text、その他の argument/result には引き続き explicit limit があります。

clipboard plain text は 1 MiB が上限です。clipboard content は telemetry ではなく user data です。text 自体が不要なら operator は type-only observation を優先してください。

## Browser と data-transfer boundary

Browser semantic parity は、raw Cua/CDP method を公開せず、backend-neutral inspect/bind、navigate、click、type、dialog、upload、download、pointer contract として別途実装されています。

upload は local-data exfiltration boundary であり、任意の local path ではなく CUMG-issued file ref を使う必要があります。download は local-write boundary であり、destination、size、overwrite behavior を独立して bind する必要があります。これらの capability は Desktop PR の scope とは分離されています。

operator configuration、diagnostic、update check、recording、test/replay control は、通常の user-facing northbound MCP surface ではなく operator plane に置きます。

## Security invariant は不変

semantic extension は authoritative V2 safety model を維持します。

- authentication は capability authorization より前に `AuthenticatedClientPrincipal` へ縮約されます。
- authorization は exact `principal -> stable device -> DeviceCapability` のままです。
- generation fencing と capability-revision fencing は authoritative のままです。
- southbound execution は bearer/proxy credential を転送するのではなく、短命な exact grant を使います。
- effect を証明できない mutating cancellation/timeout は indeterminate のままとなり device を quarantine します。
- ambiguous operation を automatic replay しません。
- desktop escalation は explicit のみです。
- raw Cua passthrough は禁止したままです。
- backend-specific credential、tool 名、ref、payload contract は adapter で終端します。

Cua 0.19.3 全体の disposition は [`V2_CUA_PARITY_MATRIX.ja.md`](V2_CUA_PARITY_MATRIX.ja.md) を参照してください。stateful workflow rule は [`V2_INTERACTION_CONTEXT.md`](V2_INTERACTION_CONTEXT.md) で定義しています。
