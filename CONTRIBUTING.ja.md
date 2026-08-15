# コントリビューション

> この日本語版は [`CONTRIBUTING.md`](CONTRIBUTING.md) の翻訳です。**英語版を canonical（正典）とし、解釈に差がある場合は英語版を優先します。**

## 原則

1. public MCP boundary は backend-agnostic に保ちます。
2. 必要な capability を backend が提供できない場合を除き、desktop automation を再実装しません。
3. proxy 時は backend MCP の content/result envelope を保持します。
4. security check は backend execution より前に行います。
5. idempotency contract が証明されていない failed / timed-out / cancelled computer-use call に automatic replay を追加しません。
6. 1つの物理 desktop は shared mutable state として扱い、concurrency model を明示的に変更しない限り operation serialization を維持します。
7. raw tool arguments、results、screenshots、clipboard contents、credentials はデフォルトでログに記録しません。
8. semantic tool class と exact-name authorization を分離します。unknown tool は保守的な分類のまま扱います。
9. idle resource usage と deterministic soak behavior は V1 regression gate です。
10. newcomer 向け command は copy-paste 可能な状態を保ち、repository-local Markdown link を有効に保ちます。

## PR の前に

通常 CI で使用する deterministic check を実行します:

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

Node 20+ と network access が利用できる場合は、さらに次を実行します:

```bash
python3 scripts/v1_conformance.py
```

conformance script は `npx` 経由で正確に pin された official runner package を取得します。検証対象は [`docs/TESTING.md`](docs/TESTING.md) に記載された、V1 に適用可能な official server-boundary scenario のみです。これを full MCP conformance certification と表現しないでください。

`Cargo.lock` は binary application の reproducibility contract の一部です。dependency change では意図的に更新し、通常の validation で `--locked` を迂回しないでください。

docs link checker は external website を取得せず、repository-local Markdown target を検証します。external installation/client/deployment command を変更する場合は、review の一環として現在の authoritative upstream documentation と照合してください。

## 互換性に関わる変更

MCP lifecycle handling、cancellation、Cua integration、policy filtering/classification、Host/Origin validation、reconnect behavior、health telemetry、CI supply-chain pin を変更する場合は、該当する deterministic または real-Cua coverage を追加・更新してください。詳細は [`docs/TESTING.md`](docs/TESTING.md) を参照してください。

TCC 権限を付与した self-hosted desktop runner 上で、信頼されていない pull-request code を実行しないでください。desktop E2E workflow は意図的に manual かつ `main`-only です。
