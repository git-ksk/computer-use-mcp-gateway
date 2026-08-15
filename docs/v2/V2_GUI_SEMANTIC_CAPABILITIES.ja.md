# V2 GUI セマンティック能力

> **注意:** この文書は [V2_GUI_SEMANTIC_CAPABILITIES.md](V2_GUI_SEMANTIC_CAPABILITIES.md) の日本語訳です。英語版が正典（canonical）です。

## 目的（Purpose）

V2 は、設定された Computer Use バックエンドの MCP ツール名を CUMG プロダクト契約の一部にはしません。安定した境界は、バックエンド非依存（backend-neutral）のセマンティック語彙（semantic vocabulary）です:

```text
northbound MCP tool
  -> exact DeviceCapability
  -> typed DeviceCommand / DeviceResult
  -> ComputerUseBackendAdapter
  -> backend-specific operation
```

Cua が最初の GUI バックエンドですが、Cua のツール名、AX ロール、セッションヘルパー、プロバイダーペイロードは `CuaMcpAdapter` で終端します。別のバックエンドが Cua の API をコピーせずに同じ CUMG セマンティクスを実装できます。

これは移植可能な最小公分母（minimum-common-denominator）ルールで**はありません**。CUMG は、一部のバックエンドだけが実装するセマンティック能力を定義できます。Agent は正確なライブ `DeviceCapability` セットを広告し、northbound のディスカバリは、その広告と認証済みプリンシパルの正確なデバイス能力ポリシーの積集合のみを公開します。

## デスクトップセマンティックサーフェス

V2 デスクトップサーフェスは、既存のランタイム能力に加えて、デスクトップパリティ拡張を含みます:

| Northbound tool | `DeviceCapability` | Contract |
|---|---|---|
| `list_apps` | `ListApplications` | 境界付きアプリケーション観察 |
| `get_screen_size` | `ScreenGeometry` | 物理/論理ディスプレイジオメトリ |
| `screenshot` | `Screenshot` | 境界付きデスクトップ PNG。文脈上のデスクトップ利用には明示的なスコープ拡張が必要 |
| `click` | `PointerClick` | 型付き座標クリック、または境界付きセマンティックアクションを持つ同一コンテキスト要素 refs |
| `drag` | `PointerDrag` | 型付きエンドポイント、ボタン/修飾子、境界付き持続時間/ステップ |
| `type_text` | `TypeText` | 同一コンテキストの要素 ref ターゲティングを含む境界付きテキスト。明示的な配信付き |
| `list_windows` | `ListWindows` | 境界付きバックエンド非依存のトップレベルウィンドウレコード |
| `launch_application` | `LaunchApplication` | 境界付きターゲットを持つ不透明な（opaque）識別子/名前による起動 |
| `inspect_window` | `InspectWindow` | 境界付き正規化 UI スナップショット。CUMG スコープ付き refs を鋳造する |
| `verify_ui_state` | `VerifyUiState` | 境界付き述語。`unknown` は決して成功ではない |
| `terminate_application` | `TerminateApplication` | 正確なプロセス終了。危険な能力 |
| `activate_window` | `ActivateWindow` | 検証エビデンス付きでプロセスまたは正確なウィンドウをアクティブ化 |
| `set_window_frame` | `SetWindowFrame` | 正確なトップレベルウィンドウジオメトリの設定と検証 |
| `invoke_menu` | `InvokeMenu` | 生のバックエンドセレクタなしで境界付きセマンティックメニューパスを呼び出す |
| `keyboard_input` | `KeyboardInput` | 同一コンテキストの要素 ref ターゲティングを含む、境界付きセマンティックキー/修飾子 |
| `scroll` | `Scroll` | 明示的なターゲットに対する境界付き方向/粒度/量 |
| `clipboard_read` | `ClipboardRead` | 境界付き型とオプションのプライバシー機微テキスト |
| `clipboard_write` | `ClipboardWrite` | 境界付きプレーンテキスト置換 |
| `get_pointer_position` | `PointerPosition` | 実際のデスクトップポインタ観察 |
| `move_pointer` | `MovePointer` | 型付きデスクトップ座標での実際のポインタ移動 |
| `set_ui_value` | `SetUiValue` | CUMG が鋳造した要素 ref を通じて境界付き値を設定 |
| `capture_region` | `CaptureRegion` | 隠れたズーム状態なしの境界付きウィンドウローカルキャプチャ |
| `expand_interaction_scope` | `DesktopScope` | 明示的な単調なウィンドウからデスクトップへのスコープ拡張 |

`open_interaction_context` と `close_interaction_context` は、デバイス能力ではなくワークフローコントロールです。これらは境界付き CUMG ワークフロー状態を作成または無効化します。コンテキスト ID の所持は、いかなるデバイス操作も認可しません。

プロセス、シェル、境界付きファイルシステムツールは、別個の非 GUI V2 能力のままです。ブラウザとファイル転送のパリティは、このデスクトップフェーズから意図的に除外されています。

## 能力広告とディスカバリ

`CapabilityAdvertisement` は、バックエンドの移植性境界です。`tools/list` は以下の正確な積集合です:

```text
principal/device policy allow
AND
current live Agent CapabilityAdvertisement
```

Agent がオフラインの場合、ライブ広告はなく、セマンティックデバイスツールは公開されません。再接続は、新しいデバイス世代または能力リビジョンを生じることがあります。ステートフルな（状態を持つ）リクエストはその両方に対してフェンスされるため、ディスカバリ/ディスパッチの競合（race）はフェイルクローズします。

コントロールスキーマはバージョン 7、能力広告スキーマはバージョン 4 のままです。Hub と Agent のコントロールスキーマ不一致はフェイルクローズします。別のスキーマバージョンを持つ能力広告も、曖昧なローリングアップグレード互換モードとして解釈されるのではなく、拒否されます。

## インタラクションコンテキストとバックエンドライフサイクル

`InteractionContext` は、HTTP または MCP トランスポートセッションから独立した CUMG ワークフロー状態です。認証済みプリンシパル、安定したデバイス、Agent 世代、能力リビジョンに束縛されます。コンテキスト ID は不透明な状態アイデンティティであり、ベアラ資格情報（bearer credential）や認可トークンではありません。

Cua の場合、アダプターは CUMG コンテキスト ID をバックエンドセッション識別子として使用します。デスクトップ拡張は、明示的に `start_session(capture_scope=auto)` を確実にし、次に `escalate_session` を呼び出します。より狭いルートが失敗した後の自動エスカレーションはありません。

Cua の `start_session` と `end_session` は、生の northbound 能力ではなく、バックエンドライフサイクルのままです。コンテキストのクローズ、失効、世代フェンシング、能力リビジョンフェンシングは、CUMG refs を無効化し、署名付きの Hub から Agent へのライフサイクルコントロールを通じてバックエンドセッションのクリーンアップを要求します。そのコントロールは、`DeviceCommand`、グラント、`OperationOwner`、リプレイアイデンティティ、または隔離遷移を作成しません。

スコープは単調（monotonic）です。`window_scoped` コンテキストは明示的に `desktop_scoped` に拡張できますが、その場でウィンドウスコープに戻ることはできません。拡張後、ウィンドウ専用コマンドは CUMG 境界で失敗します。呼び出し側はコンテキストを閉じて新しいものを開かなければなりません。これは、Cua 0.19.3 セッション契約の拒否挙動をバイパスするのではなく、その契約に一致します。

## スコープ付きバックエンド参照

生のバックエンドスナップショット/要素ハンドルは、northbound のアクション権限になることは決してありません。`inspect_window` は観察を正規化し、`ref_...` のような CUMG 不透明 refs を鋳造します。各マッピングはメモリ内に保持され、以下に束縛されます:

```text
InteractionContext ID
device generation
capability revision
ref kind
```

`set_ui_value`、`click`、`type_text`、`keyboard_input` は、同じライブコンテキストからの CUMG 要素 ref を消費できます。Hub は、コンテキスト/デバイス/世代/リビジョン/種類のチェック後にのみ、その不透明な ref を解決します。コマンドは依然として正確なプロセス/ウィンドウターゲットをバックエンドに運び、その要素トークンはそのウィンドウと一致しなければなりません。未知、古い（stale）、クロスコンテキスト、間違った世代、間違ったリビジョン、間違った種類、またはプロバイダーに拒否されたウィンドウ/トークンの組み合わせは、フェイルクローズします。CUMG は変更を自動更新（auto-refresh）してリプレイすることはありません。

コンテキストとスコープ付き ref レジストリは、`OperationOwner` を置き換えません。隔離の所有権と不確定（indeterminate）な解決は、認証済みプリンシパルに束縛されたままです。

## バックエンド非依存の UI と座標モデル

`InspectWindow` は、AX/UIA/AT-SPI ツリーをそのまま転送しません。アダプターはバックエンドフィールドを、`UiRect`、`WindowInfo`、`UiElement`、セマンティックな `UiRole` 値などの境界付き CUMG データに還元します。未知のプロバイダー固有ロールは、バックエンド語彙に漏洩するのではなく、`other` に正規化されます。`ListWindows` はすべてのプロバイダーレコードを検証しますが、V2 ウィンドウ契約ではターゲットにできないため、ゼロ面積のヘルパー/エージェントウィンドウを省略します。正確なウィンドウターゲットとスナップショットは、引き続き厳密に正のジオメトリを必要とします。不正な（malformed）非ジオメトリフィールドは、それでもフェイルクローズします。

デスクトップアクションは、`DesktopPhysical`、`WindowPhysical`、スコープ付きネイティブ要素ターゲット、`InputTarget`、`ScrollTarget` などの型付き座標/ターゲットを使用します。要素ターゲットが Cua の `element_token`、`element_index`、`snapshot_id` を northbound に公開することは決してありません。CUMG は Cua の隠れた `from_zoom` 座標状態も公開しません。
`CaptureRegion` はウィンドウローカルです。明示的な拡張後のデスクトップ観察は、文脈上のデスクトップスクリーンショットパスを通じて実行されます。

バックグラウンド配信は、セマンティックコマンドがそれを許可する場合の最初の手段（rung）のままです。バックエンドの安全性拒否は引き続き権威（authoritative）です。たとえば、1 つの PID が複数の該当ウィンドウを所有しており、正確な配信を証明できない場合、Cua はプロセススコープのバックグラウンドキーを拒否することがあります。CUMG はそれをフォアグラウンドアクションに静かに変えることはありません。

実行安全性の分類では、読み取り専用であるかどうかはセマンティックコマンド自体が定義します。変更を伴うコマンドのディスパッチ後に汎用バックエンドエラー、応答喪失、または不正／証明不能な完了が発生しても、それは終端失敗のエビデンスではありません。adapter/Agent はその不確実性を `BackendOutcomeIndeterminate` と分類し、Hub は reason `BackendOutcomeUnproven` を持つ耐久性のある `Indeterminate` として永続化し、対象デスクトップのキュー済み作業をキャンセルし、明示的かつ永続化ゲート付きの解決までデバイスを隔離します。明示的に読み取り専用のコマンドでは、対応する事象を確定的なバックエンドエラーとして扱える場合があります。レビュー済みのセマンティック拒否は型付き拒否コードを維持します。曖昧な変更操作を自動的にリトライ／リプレイすることはありません。

## 境界付きキャリアとプライバシー

通常の署名付き Hub/Agent アプリケーションメッセージは、64 KiB に境界付けられたままです。明示的に境界付きの大きな観察結果は、レビュー済みの大きな結果（large-result）キャリア許容量を使用します: スクリーンショット、UI スナップショット、クリップボード観察、領域キャプチャ。画像ペイロード、UI 要素数、寸法、ラベル、クエリ、メニューパス、修飾子、テキスト、その他の引数/結果は、明示的な上限を保持します。

クリップボードのプレーンテキストは 1 MiB に制限されます。クリップボードの内容はユーザーデータであり、テレメトリではありません。オペレーターは、テキスト自体が不要な場合、タイプのみの観察を優先すべきです。

## ブラウザとデータ転送境界

ブラウザのセマンティックパリティは、生の Cua/CDP メソッドを公開せずに、バックエンド非依存の inspect/bind、navigate、click、type、dialog、upload、download、pointer 契約を通じて別途実装されます。

アップロードはローカルデータの外部送信（exfiltration）境界であり、任意のローカルパスではなく CUMG が発行したファイル ref を使用しなければなりません。ダウンロードはローカル書き込み境界であり、宛先、サイズ、上書き挙動を独立して束縛しなければなりません。これらの能力は、このデスクトップ PR から意図的に除外されています。

オペレーター設定、診断、更新チェック、記録、テスト/リプレイコントロールは、通常のユーザー向け northbound MCP サーフェスではなく、オペレーター基盤（plane）に残ります。

## セキュリティ不変条件は変更なし

セマンティック拡張は、権威ある V2 安全性モデルを保持します:

- 認証は、能力認可の前に `AuthenticatedClientPrincipal` に還元されます;
- 認可は、正確な `principal -> stable device -> DeviceCapability` のままです;
- 世代と能力リビジョンのフェンシングは、引き続き権威です;
- southbound の実行は、ベアラ/プロキシ資格情報を転送するのではなく、引き続き短命の正確なグラントを使用します;
- 効果が未証明の変更を伴うキャンセル/タイムアウトは、不確定（indeterminate）のままとなり、デバイスを隔離します;
- 曖昧な操作は自動リプレイされません;
- デスクトップのエスカレーションは明示のみです;
- 生の Cua パススルーは禁止されたままです;
- バックエンド固有の資格情報、ツール名、refs、ペイロード契約は、アダプターで終端します。

完全な Cua 0.19.3 の処遇（disposition）は
[`V2_CUA_PARITY_MATRIX.ja.md`](V2_CUA_PARITY_MATRIX.ja.md) で追跡されています。ステートフル（状態を持つ）ワークフロールールは
[`V2_INTERACTION_CONTEXT.md`](V2_INTERACTION_CONTEXT.md) で定義されています。
