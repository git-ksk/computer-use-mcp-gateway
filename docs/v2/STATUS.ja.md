# V2 ステータス

> **注意:** この文書は [STATUS.md](STATUS.md) の日本語訳です。英語版が正典（canonical）です。

2026-08-15 時点のステータス:

- **デスクトップセマンティックパス:** 完了済み・受容済み。同一コンテキストでのネイティブ要素の click/type/key ターゲティングと、実 Cua バックグラウンドの AX 要素アクションエビデンスを含みます。
- **ブラウザコアセマンティックパス:** prepare、bind、inspect、navigate、click、type、dialog、pointer の各セマンティクスについて完了済み・受容済み。
- **ブラウザ転送:** 完了済み・受容済み。アップロード/ダウンロードは、スコープ付き CUMG refs と Agent プライベートな境界付きステージングを使用します。任意のホストパスが northbound に公開されることはありません。
- **ディスパッチ後あいまい性の堅牢化:** 変更を伴うコマンドのディスパッチ後に汎用バックエンドエラー、不正／証明不能な完了、または応答喪失が発生した場合、adapter/Agent 境界で `BackendOutcomeIndeterminate` と分類し、Hub は reason `BackendOutcomeUnproven` を持つ耐久性のある `Indeterminate` として永続化します。そのデスクトップのキュー済み作業をキャンセルし、明示的かつ永続化ゲート付きの解決までデバイスを隔離します。自動リトライ／リプレイは行いません。読み取り専用のバックエンド失敗は確定的な失敗のまま扱える場合があります。northbound の運用上の失敗は、transport/provider/`ExceptionGroup` を漏らさず、`isError=true` とクローズドな CUMG コードを持つ境界付き MCP `CallToolResult` エラーとして返します。実 Cua の browser alert acceptance が issue #47 の観測可能な副作用ケースをカバーします。
- **V1 本番:** V2 開発ブランチによって変更されていません。V1 の回帰・適合性カバレッジは、V2 作業中も引き続き必要です。

## 有効な契約

- [`V2_POSITIONING.ja.md`](V2_POSITIONING.ja.md) — 正典の製品境界（原文: [V2_POSITIONING.md](V2_POSITIONING.md)）。
- [`V2_P0_EXECUTION_SAFETY.md`](V2_P0_EXECUTION_SAFETY.md) — 不確実性を考慮した実行と自動リプレイなしの不変条件。
- [`V2_INTERACTION_CONTEXT.md`](V2_INTERACTION_CONTEXT.md) — スコープ付きインタラクション状態とバックエンド参照の所有権。
- [`V2_GUI_SEMANTIC_CAPABILITIES.ja.md`](V2_GUI_SEMANTIC_CAPABILITIES.ja.md) — デスクトップセマンティックサーフェス（原文: [V2_GUI_SEMANTIC_CAPABILITIES.md](V2_GUI_SEMANTIC_CAPABILITIES.md)）。
- [`V2_BROWSER_SEMANTIC_CAPABILITIES.md`](V2_BROWSER_SEMANTIC_CAPABILITIES.md) — ブラウザセマンティックサーフェスと転送境界。
- [`V2_CUA_PARITY_MATRIX.ja.md`](V2_CUA_PARITY_MATRIX.ja.md) — Cua 互換性・パリティ分類（原文: [V2_CUA_PARITY_MATRIX.md](V2_CUA_PARITY_MATRIX.md)）。
- [`V2_THREAT_MODEL.ja.md`](V2_THREAT_MODEL.ja.md) — セキュリティの主張と非主張（原文: [V2_THREAT_MODEL.md](V2_THREAT_MODEL.md)）。
- [`V2_STANDARDIZATION.md`](V2_STANDARDIZATION.md) と [`V2_P2_REPLACEMENT_SEAMS.md`](V2_P2_REPLACEMENT_SEAMS.md) — 維持管理された OSS・標準への置換境界。
- [`V2_USAGE_ACCOUNTING.md`](V2_USAGE_ACCOUNTING.md) — オプションの会計連携。

## 受容エビデンス

- [`acceptance/V2_BROWSER_CORE_ACCEPTANCE.md`](acceptance/V2_BROWSER_CORE_ACCEPTANCE.md) — ブラウザコアのクローズアウト。
- [`acceptance/V2_BROWSER_TRANSFER_ACCEPTANCE.md`](acceptance/V2_BROWSER_TRANSFER_ACCEPTANCE.md) — ブラウザ転送契約、脅威対策、自動カバレッジ、信頼された Mac での実 Cua エビデンス。
- [`acceptance/V2_LOCAL_DESKTOP_ACCEPTANCE.md`](acceptance/V2_LOCAL_DESKTOP_ACCEPTANCE.md) — 信頼された物理デスクトップの受容手順・エビデンス。
- [`acceptance/V2_M1_ACCEPTANCE.md`](acceptance/V2_M1_ACCEPTANCE.md) — 早期のセキュア Agent マイルストーン受容をエビデンスとして保持。

## 履歴記録

初期プロトタイプと進捗記録は [`../archive/v2/`](../archive/v2/) にアーカイブされています。これらは設計の由来を保存していますが、実行可能なセットアップ手順ではなくなっています。
