# computer-use-mcp-gateway

> **注意:** この文書は [README.md](README.md) の日本語訳です。英語版が正典（canonical）です。

`computer-use-mcp-gateway` (CUMG) は、ポリシー制御されたコンピュータ操作（computer use）のための Rust 製 MCP ゲートウェイです。推奨される V2 ランタイムは、リモートから到達可能な **Hub** とデスクトップ側の **Agent** を分離し、生のバックエンドツール名や識別子を northbound 契約の一部にするのではなく、境界が定められたバックエンド非依存のセマンティックな能力を公開します。

> **ランタイムの状況:** V2 Hub + Agent が推奨される開発・ランタイム経路です。V1 は回帰・リファレンス用および既存の本番運用向けとして `v1_gateway` が引き続き利用可能です。ブラウザコアと境界付きブラウザ転送（アップロード/ダウンロード）は実装済みで受容されています。

## 概要

CUMG は実行権限をゲートウェイに保持しつつ、維持管理されたインフラストラクチャがネットワークエッジの TLS や認証などの一般的な関心事を担うことを可能にします。その中核となる安全規則は、あいまいな状態変更操作が、クライアント、Hub、Agent、トランスポート、バックエンド、またはデバイスの再接続後に自動的に再実行（リプレイ）されることが決してないということです。

最初にレビューされたコンピュータ操作バックエンドは [Cua Driver](https://github.com/trycua/cua) で、CI では **0.19.3** に固定されています。Cua 固有の MCP 名、生のブラウザ/CDP 識別子、アクセシビリティハンドル、スクリーンショット、プロバイダーのレスポンス形状は、安定した CUMG API サーフェスになるのではなく、アダプタ境界で終端します。

## アーキテクチャ

```text
MCP client
    |
    | authenticated MCP
    v
V2 Hub
    |  exact principal -> device -> capability authorization
    |  operation ownership / generation fencing / quarantine
    |
    | gRPC bidirectional stream over TLS
    v
V2 Agent
    |  direct process / shell / bounded filesystem capabilities
    |  backend-neutral Desktop + Browser semantic adapter
    v
Computer-use backend (Cua Driver today)
```

Hub は admission（受け入れ）、authorization（認可）、操作状態、リプレイ障壁、永続的な `indeterminate` 隔離（quarantine）を担います。Agent は認証済みデバイスセッションとローカルな実行境界を担います。オプションの使用量課金（usage accounting）は独立した会計権限であり、実行の認可、隔離の解除、リプレイの許可を行うことはできません。

[`docs/ARCHITECTURE.ja.md`](docs/ARCHITECTURE.ja.md) と、V2 の境界の正典である [`docs/v2/V2_POSITIONING.ja.md`](docs/v2/V2_POSITIONING.ja.md) を参照してください。

## はじめに

クリーンインストールについては [`docs/GETTING_STARTED.ja.md`](docs/GETTING_STARTED.ja.md) に従ってください。前提条件、OS の権限、バックエンドの検証、Hub/Agent の設定、ローカル MCP 接続、リモートアクセスへの安全な経路を網羅しています。

現在のすべてのターゲットをビルドします:

```bash
git clone https://github.com/git-ksk/computer-use-mcp-gateway.git
cd computer-use-mcp-gateway
cargo build --locked
```

デフォルトのバイナリは V2 Hub です:

```bash
cargo run --locked -- --help
```

デスクトップ側の Agent は別です:

```bash
cargo run --locked --bin v2_agent -- --help
```

V1 はレガシー/回帰運用のために引き続き利用可能です:

```bash
cargo run --locked --bin v1_gateway -- --allow-tools list_apps,list_windows,get_accessibility_tree,get_screen_size
```

バックエンドのトランスポートを直接公開したり、文書化された信頼/TLS 境界を認証されていない公開リスナーに置き換えたりしないでください。パッケージ化されたサービス例は `packaging/` 配下にあります。クライアント例とトラブルシューティングは [`docs/CLIENTS.md`](docs/CLIENTS.md) と [`docs/TROUBLESHOOTING.md`](docs/TROUBLESHOOTING.md) にあります。

## セキュリティ

CUMG は、デスクトップを変更し得る能力について fail-closed（失敗時閉鎖）です。特に:

- northbound の認可は、正確な認証済みプリンシパル、安定したデバイス、正確な能力（capability）に限定されます;
- 古いデバイス世代、古い能力リビジョン、誤ったコンテキストの参照、消費済みの参照、不正な操作は拒否されます;
- あいまいな状態変更作業は、再実行される代わりに `indeterminate` となり、明示的な解決までデバイスを隔離します;
- 生のバックエンド ID や汎用のバックエンド脱出ハッチは V2 の northbound セマンティックサーフェスの一部ではありません;
- バックエンドのリクエスト引数/結果、スクリーンショット、クリップボードデータ、ベアラートークン、プライベート資格情報は、デフォルトのテレメトリ/ログから除外されます;
- リモートデプロイでは、Hub のリスナーをレビュー済みの TLS/認証エッジの背後に置き、外向きの Agent 接続モデルを維持します。

これらの制御は、OS の権限、エンドポイントの強化、資格情報の管理、ネットワーク制御、デプロイ固有の監視に取って代わるものではありません。機密性の高いデスクトップをリモートに公開する前に、[`docs/SECURITY.ja.md`](docs/SECURITY.ja.md) と [`docs/v2/V2_THREAT_MODEL.ja.md`](docs/v2/V2_THREAT_MODEL.ja.md) を読んでください。

## V2 ステータス

アクティブな実装は、内部のマイルストーン名ではなく能力（capability）単位で追跡されます:

| 領域 | ステータス |
| --- | --- |
| Desktop semantic path | Complete / accepted |
| Browser core | Complete / accepted |
| Browser transfer (upload/download) | Complete / accepted |
| V1 regression/conformance | Required and preserved |

ブラウザコアは、型付けされた prepare、bind、inspect、navigate、click、type、dialog、pointer の各経路を網羅しつつ、不透明な CUMG 参照と exact-or-refuse（完全一致または拒否）の実行セマンティクスを維持します。ブラウザ転送は、コンテキストスコープの参照、Agent プライベートなファイルシステムステージング、正確な能力チェックを備えた境界付きステージングアップロード/ダウンロードを追加し、古い参照、パスエスケープ、部分完了、タイムアウト、キャンセルを fail-closed に処理します。

現在のマップについては [`docs/v2/STATUS.ja.md`](docs/v2/STATUS.ja.md)、ブラウザコアのエビデンスについては [`docs/v2/acceptance/V2_BROWSER_CORE_ACCEPTANCE.md`](docs/v2/acceptance/V2_BROWSER_CORE_ACCEPTANCE.md)、ブラウザ転送のエビデンスについては [`docs/v2/acceptance/V2_BROWSER_TRANSFER_ACCEPTANCE.md`](docs/v2/acceptance/V2_BROWSER_TRANSFER_ACCEPTANCE.md)、アクティブな仕様・受容エビデンス・アーカイブ済み意思決定記録の整理方法については [`docs/README.md`](docs/README.md) を参照してください。

## テストとデプロイ

リポジトリの変更は、CI に頼る前にローカルで同じ警告なし（warning-free）ベースラインを通過する必要があります:

```bash
cargo fmt --all -- --check
cargo check --locked --all-targets
cargo test --locked
cargo clippy --locked --all-targets -- -D warnings
python3 scripts/check_docs.py
git diff --check
```

V1 互換性は明示的な回帰境界のままです:

```bash
python3 scripts/v1_quality_gate.py
python3 scripts/v1_conformance.py
```

通常の CI は、Linux、macOS、Windows 上の固定された Cua リリース、選択された MCP 適合性シナリオ、キャンセル動作、リソース/ソークチェック、バックエンドパススルー契約も実行します。信頼された物理デスクトップの受容テストはオペレータ制御であり、信頼されていないホスト型ランナーに GUI アクセスを許可するものではありません。

正確な保証と制限については [`docs/TESTING.md`](docs/TESTING.md) を参照してください。デプロイ、サービスの監視、TLS/認証エッジ要件、資格情報の取り扱い、V1 のレガシー設定は [`docs/DEPLOYMENT.md`](docs/DEPLOYMENT.md) に文書化されています。

## ドキュメント

[`docs/README.md`](docs/README.md) から始めてください。以下のものを区別しています:

- オペレータ/コントリビュータ向けガイド;
- アクティブな V2 仕様;
- 受容エビデンス;
- 歴史的な PoC と意思決定記録。

リポジトリ内のドキュメントリンクチェッカーは、これらのディレクトリを再帰的に検証するため、アーカイブ済みまたはネストされた文書に壊れたローカルリンクが静かに蓄積されることはありません。

## ライセンス

MIT. これは独立したプロジェクトであり、Cua AI や Model Context Protocol プロジェクトとは提携していません。