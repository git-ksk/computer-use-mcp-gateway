# V2 Cua パリティ行列

> **注意:** この文書は [V2_CUA_PARITY_MATRIX.md](V2_CUA_PARITY_MATRIX.md) の日本語訳です。英語版が正典（canonical）です。

## 目的（Goal）

CUMG V2 のパリティは挙動（behavioral）であり、すべての Cua MCPツール名をミラーリングするという約束ではありません。
CUMG のプロダクト契約は、境界付き（bounded）かつバックエンド非依存（backend-neutral）なセマンティック語彙（semantic vocabulary）のままです。
Cua ツールは、以下のいずれかに分類されたときのみ「完成（complete）」とみなされます:

- **semantic**: 正確な CUMG 能力または型付きコントロールプリミティブによって表現される;
- **integrated**: 既存の CUMG セマンティック能力に吸収される;
- **backend lifecycle**: `ComputerUseBackendAdapter` の背後に保持され、バックエンドのツール名では決して認可されない;
- **operator-only**: 信頼されたローカル/オペレーター基盤（plane）を介してのみ利用可能であり、通常の northbound MCP ではない;
- **intentionally excluded**: 危険またはレガシーな挙動であり、より安全な型付き置き換え（replacement）が存在する。

カットオーバー（cutover）ゲートは: レビュー済みの Cua 0.19.3 のすべてのツールにちょうど1つの分類があり、現在の V1/Cua パスを通じて利用可能な正当なワークフローすべてに、V2 のセマンティックパスまたは明示的に文書化された operator-only の置き換えがあることです。汎用の `call_tool`、`raw_cua`、または任意のバックエンドメソッドのエスケープハッチがあってはなりません。

## 横断的ルール（Cross-cutting rules）

1. バックエンド固有の名前とペイロードは、アダプターで終端します。
2. 認可は、正確な `principal -> stable device -> DeviceCapability` のままです。
3. インタラクション状態は、HTTP/MCP トランスポートセッションではなく、CUMG の `InteractionContext` によってスコープされます。
4. コンテキストとバックエンド refs は、principal、device、Agent 世代（generation）、能力リビジョン（capability revision）に束縛されます。
5. Agent の再接続/世代変更は、バックエンドセッションマッピングとすべてのスコープ付き refs を無効化します。
6. ウィンドウからデスクトップへのエスカレーションは明示的です。自動フォールバックになることは決してありません。
7. Cua 自身のバックエンド認可/拒否は、多層防御（defense in depth）として残り、CUMG によって決してバイパスされません。
8. アップロード/ダウンロードは、別個のデータ転送境界を横断し、専用の正確な能力を必要とします。
9. 記録/リプレイは、通常の本番 northbound 運用の一部ではありません。リプレイは、CUMG の操作アイデンティティ（operation identity）、世代フェンス（generation fences）、隔離（quarantine）、または自動リプレイなし（no-auto-replay）の不変条件を決してバイパスしてはなりません。
10. オペレーターによるセットアップ/設定は、別個の制御基盤（control plane）であり、通常のデバイス能力ではありません。

## Cua 0.19.3 の分類

| # | Cua tool | V2 disposition | Planned/Current CUMG semantic |
|---:|---|---|---|
| 1 | `list_apps` | semantic, current | `ListApplications` / `list_apps` |
| 2 | `list_windows` | semantic, current | `ListWindows` / `list_windows` |
| 3 | `get_window_state` | semantic, current | `InspectWindow` / `inspect_window` |
| 4 | `verify_state` | semantic, current | `VerifyUiState` / `verify_ui_state` |
| 5 | `launch_app` | semantic, current | `LaunchApplication` / `launch_application`; CUMG の起動は、レビュー済みの識別子/名前ターゲットに境界付けられたままであり、Cua の `additional_arguments` や `webkit_inspector_port` を公開しません |
| 6 | `kill_app` | semantic, current | `TerminateApplication` |
| 7 | `bring_to_front` | semantic, current | 正確なウィンドウ検証エビデンス付きの `ActivateWindow` |
| 8 | `set_window_frame` | semantic, current | `SetWindowFrame` |
| 9 | `invoke_menu` | semantic, current | 正確で境界付きの `InvokeMenu` |
| 10 | `click` | semantic, current | `PointerClick`; 型付き座標またはセマンティック AX アクション付きのスコープ付きネイティブ要素参照 |
| 11 | `double_click` | integrated | `PointerClick { click_count: 2 }`、スコープ付きネイティブ要素ターゲットを含む |
| 12 | `right_click` | integrated | `PointerClick { button: right }`、スコープ付きネイティブ要素ターゲットを含む |
| 13 | `drag` | semantic, current | `PointerDrag`; 型付きエンドポイント、修飾子、持続時間、ステップ |
| 14 | `type_text` | semantic, current | `TypeText`; 境界付きの文脈上の座標/ウィンドウ/要素ターゲットセマンティクス |
| 15 | `press_key` | semantic, current | `KeyboardInput`、スコープ付きネイティブ要素ターゲティングを含む |
| 16 | `hotkey` | integrated | `KeyboardInput` コード（chord） |
| 17 | `set_value` | semantic, current | スコープ付き CUMG 要素 refs を使用する `SetUiValue` |
| 18 | `scroll` | semantic, current | 明示的な座標空間/ターゲットを持つ `Scroll` |
| 19 | `clipboard_read` | semantic, current | 境界付き出力と機密データ処理を備えた `ClipboardRead` |
| 20 | `clipboard_write` | semantic, current | 境界付きプレーンテキスト入力のみを扱う `ClipboardWrite` |
| 21 | `get_screen_size` | semantic, current | `ScreenGeometry` / `get_screen_size` |
| 22 | `get_desktop_state` | integrated, current | 明示的な `DesktopScope` 拡張後の文脈上の `Screenshot` |
| 23 | `get_cursor_position` | semantic, current | `PointerPosition` |
| 24 | `move_cursor` | semantic, current | `MovePointer` |
| 25 | `set_agent_cursor_enabled` | backend lifecycle | インタラクション可視化ポリシー |
| 26 | `set_agent_cursor_motion` | backend lifecycle | インタラクション可視化ポリシー |
| 27 | `set_agent_cursor_theme` | backend lifecycle | インタラクション可視化ポリシー |
| 28 | `get_agent_cursor_state` | backend lifecycle/diagnostic | 通常の northbound 能力なし |
| 29 | `check_permissions` | operator-only | プロンプト対応セットアップとは分離された読み取り専用診断 |
| 30 | `health_report` | operator-only | バックエンド診断 |
| 31 | `get_config` | operator-only | バックエンド設定の検査 |
| 32 | `set_config` | operator-only | バックエンド設定の変更 |
| 33 | `get_accessibility_tree` | integrated | `ListApplications` + `ListWindows`; 生のバックエンドツリー契約なし |
| 34 | `zoom` | semantic, current | 境界付きウィンドウローカル `CaptureRegion`; 隠れた `from_zoom` 状態なし |
| 35 | `page` | intentionally excluded/replaced | 型付きブラウザサーフェス; 任意の JS は標準のパリティではありません |
| 36 | `get_browser_state` | semantic, core-wired | `BrowserInspect` |
| 37 | `browser_prepare` | semantic, core-wired | `BrowserPrepare`; Cua/バックエンドの認可/拒否を保持 |
| 38 | `browser_navigate` | semantic, core-wired | `BrowserNavigate` |
| 39 | `browser_click` | semantic, core-wired | スコープ付きページ refs を使用する `BrowserClick` |
| 40 | `browser_type` | semantic, core-wired | スコープ付きページ refs を使用する `BrowserType` |
| 41 | `browser_dialog` | semantic, core-wired | `BrowserDialog` |
| 42 | `browser_set_input_files` | semantic, core-wired | `BrowserUploadFile`; ワンショットの CUMG ファイル refs は、Agent プライベートなステージングされた通常ファイルのみに解決される |
| 43 | `browser_download` | semantic, core-wired | `BrowserDownload`; 正確なクリック参照 + Agent プライベートな宛先ルート + 境界付き結果参照/データ |
| 44 | `browser_pointer` | semantic, core-wired | 明示的なブラウザビューポート座標を持つ `BrowserPointer` |
| 45 | `start_recording` | operator/test-only | ローカル受容/回帰ツール |
| 46 | `stop_recording` | operator/test-only | ローカル受容/回帰ツール |
| 47 | `get_recording_state` | operator/test-only | ローカル受容/回帰ツール |
| 48 | `replay_trajectory` | intentionally excluded from production | テスト専用; 本番のリプレイ権限では決してない |
| 49 | `install_ffmpeg` | operator-only | ローカルの依存関係/セットアップ基盤 |
| 50 | `start_session` | backend lifecycle, current | CUMG コンテキスト ID がバックエンドセッション ID |
| 51 | `escalate_session` | explicit CUMG control, current | 単調な `WindowScoped -> DesktopScoped` |
| 52 | `get_session_state` | backend lifecycle/diagnostic | コンテキスト健全性は、境界付き CUMG 状態としてのみ公開されることがあります |
| 53 | `end_session` | backend lifecycle, current | クローズ/失効/世代/リビジョンのクリーンアップがバックエンドセッションを終了させる |
| 54 | `check_for_update` | operator-only | ローカル更新基盤 |


## デスクトップパリティのステータス

デスクトップ専用セマンティックベースラインは、Cua 0.19.3 シャドウ Agent 上で 29 個の northbound ツールを公開します。ブラウザコアは、対応するライブ広告とポリシーが許可する場合、8 個の型付きツールを追加します。汎用の Cua 呼び出し/プロキシツールはありません。`tools/list` は、正確なポリシー/ライブ広告の積集合であり、オフラインの Agent はセマンティックデバイスツールを一切公開しません。

現在のデスクトップランタイム受容（acceptance）は以下をカバーします:

- デバイス世代と能力リビジョンに原子的に束縛されたコンテキストオープン;
- 文脈上のウィンドウ検査と CUMG スナップショット/要素参照の鋳造（minting）;
- 同一コンテキストの CUMG 要素 refs を通じた `set_ui_value`、ネイティブクリック、テキスト入力、キーボード入力;
- 生のバックエンドハンドルなしでのネイティブ要素 `press/open/show_menu/pick/confirm/cancel` マッピング;
- 古い（stale）、未知、クロスコンテキスト、間違った世代、間違ったリビジョン、間違った種類、プロバイダー失効（provider-stale）の参照拒否;
- バックグラウンドの AX 要素の press が正確なウィンドウを変更することを証明する、信頼された Mac の Calculator 受容;
- 検証された正確なウィンドウアクティベーション;
- 曖昧でないターゲットに対する代表的なバックグラウンドのキーボード/スクロール;
- プライバシー安全なクリップボードのタイプのみの観察;
- 境界付きウィンドウローカル領域キャプチャ;
- デスクトップスクリーンショットに続く明示的な `WindowScoped -> DesktopScoped` 拡張;
- デスクトップ拡張後のウィンドウ専用コマンドの CUMG 事前ディスパッチ拒否;
- CUMG refs と Cua セッション状態の両方を除去する、コンテキストクローズと世代フェンシング。

Cua の安全性拒否は引き続き権威（authoritative）です。プロバイダーが、1 つの PID のどの兄弟ウィンドウがプロセススコープ入力を受け取るかを証明できない場合、バックグラウンドのキー/スクロールは拒否されることがあります。CUMG は、その拒否を自動的なフォアグラウンドまたはデスクトップへのエスカレーションに変えることは決してありません。

既知のパリティギャップは残っています:

- `ClipboardWrite` はプレーンテキストのみです。V1/Cua の画像またはファイルのクリップボード書き込みは、現在の V2 パリティではありません。
- `LaunchApplication` は、Cua の `additional_arguments` や `webkit_inspector_port` を公開しません。これらは、隠れたパススルー挙動ではなく、パリティギャップのままです。

`CONTROL_SCHEMA_VERSION` はバージョン 7、`CAPABILITY_SCHEMA_VERSION` はバージョン 4 のままです。コントロールスキーマの不一致と能力広告スキーマの不一致は、フェイルクローズ（fail closed）されます。通常の署名付き Hub/Agent メッセージは 64 KiB のアプリケーションバウンドを保持しますが、境界付きの image/UI/clipboard/region 観察は、レビュー済みの大きな結果（large-result）許容量を使用します。クリップボードのプレーンテキストは 1 MiB に制限されます。

## 必要な実装順序

1. **パリティ基盤（Parity foundations）** — `InteractionContext`、世代/リビジョン束縛、明示的な実行スコープ、スコープ付き非透過参照、TTL/制限、ローリングアップグレードテスト。
2. **デスクトップセマンティックパリティ** — アプリケーション/ウィンドウ管理、キーボード、UI 値、スクロール、クリップボード、デスクトップ観察、ポインタ移動、領域キャプチャ。
3. **ブラウザセマンティックパリティ** — 正確なブラウザ束縛、inspect/navigate/interact/dialog、続いて別個のアップロード/ダウンロード転送能力。
4. **オペレーター基盤の分離** — 診断/セットアップ/設定/更新/記録は、通常の MCP の外に留まる。
5. **挙動受容（Behavioral acceptance）** — 信頼された Mac 上での実 Cua ワークフロー E2E と、バックエンド移植性テスト。
6. **カットオーバー** — 行列に未分類ツールがなく、正当な V1 ワークフローギャップがない場合にのみ。

## カットオーバーワークフローゲート

デスクトップワークフロー:

```text
launch -> discover window -> inspect -> frame/menu/element action
-> keyboard -> scroll -> clipboard -> verify
```

ブラウザワークフロー:

```text
bind/prepare -> inspect -> navigate -> semantic interaction -> dialog -> verify
-> explicit bounded upload/download
```

両方のワークフローは、CUMG 操作 ID、正確な能力認可、世代フェンシング、未確定の隔離（indeterminate quarantine）、明示的な解決、および曖昧な操作のリプレイなし、を保持しなければなりません。