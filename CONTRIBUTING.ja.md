# コントリビューション

> **注意:** この文書は [CONTRIBUTING.md](CONTRIBUTING.md) の日本語訳です。英語版が正典（canonical）です。

## 原則

1. 公開 MCP 境界（バウンダリ）はバックエンド非依存に保つ。
2. バックエンドが必要な能力を提供できない場合を除き、デスクトップ自動化を再実装しない。
3. プロキシ時にバックエンドの MCP コンテンツ/結果エンベロープを保持する。
4. セキュリティチェックはバックエンド実行の前に実施する。
5. 実証された冪等性（idempotency）契約がなければ、失敗・タイムアウト・キャンセルされた computer-use 呼び出しの自動リプレイを決して追加しない。
6. 1 つの物理デスクトップを共有された可変状態として扱う。並行性モデルが明示的に変更されない限り、操作の直列化（serialization）を維持する。
7. デフォルトでは、生のツール引数、結果、スクリーンショット、クリップボードの内容、資格情報をログに記録しない。
8. セマンティックなツールクラスと完全名（exact-name）認可を分離したままにする。未知のツールは保守的に分類された状態に留まらなければならない。
9. アイドル時のリソース使用量と決定的なソーク（soak）動作は、V1 の回帰ゲートである。
10. 初心者が使うコマンドをコピーペースト可能なままにし、リポジトリ内ローカルの Markdown リンクを有効に保つ。

## PR の前

通常の CI が使用する決定的チェックを実行します:

```bash
cargo fmt --check
cargo check --locked --all-targets
cargo test --locked
python3 -m py_compile \
  scripts/cua_gateway_smoke.py \
  scripts/cua_desktop_e2e.py \
  scripts/mock_mcp_backend.py \
  scripts/v1_quality_gate.py \
  scripts/v1_conformance.py
cargo build --locked
python3 scripts/v1_quality_gate.py
python3 scripts/check_docs.py
```

Node 20 以上とネットワークアクセスが利用可能な場合は、以下も実行します:

```bash
python3 scripts/v1_conformance.py
```

適合性（conformance）スクリプトは、`npx` を通じて正確に固定された公式ランナーパッケージをダウンロードします。これは、[`docs/TESTING.md`](docs/TESTING.md) に文書化された V1 に適用可能な公式のサーバー境界シナリオのみを検証します。これを完全な MCP 適合性認証（conformance certification）として説明しないでください。

`Cargo.lock` はバイナリアプリケーションの再現性契約の一部です。依存関係の変更は意図的にそれを更新しなければなりません。通常の検証では `--locked` を迂回しないでください。

ドキュメントリンクチェッカーは、外部のウェブサイトを取得することなく、リポジトリ内ローカルの Markdown ターゲットを検証します。外部のインストール/クライアント/デプロイメントコマンドを変更する場合は、レビューの一環として最新の権威ある上流（upstream）ドキュメントに対して検証してください。

## 互換性の変更

MCP ライフサイクル処理、キャンセル、Cua 統合、ポリシーフィルタリング/分類、Host/Origin 検証、再接続動作、ヘルステレメトリ、または CI サプライチェーンの固定（pin）の変更には、関連する決定的または実 Cua のカバレッジを含めるか更新する必要があります。[`docs/TESTING.md`](docs/TESTING.md) を参照してください。

信頼されていないプルリクエストのコードを、TCC が許可されたセルフホスト型デスクトップランナーで実行しないでください。デスクトップ E2E ワークフローは手動であり、設計上 `main` のみです。