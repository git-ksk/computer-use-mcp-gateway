# computer-use-mcp-gateway

> この日本語版は [`README.md`](README.md) の翻訳です。**英語版を canonical（正典）とし、解釈に差がある場合は英語版を優先します。**

`computer-use-mcp-gateway`（CUMG）は、ポリシー制御された computer use のための Rust 製 MCP ゲートウェイです。推奨される V2 ランタイムでは、リモートから到達可能な **Hub** とデスクトップ側の **Agent** を分離し、生のバックエンドツール名や識別子を northbound 契約に含めるのではなく、境界が明確でバックエンド非依存のセマンティック capability を公開します。

> **ランタイム状況:** V2 Hub + Agent が推奨される開発・実行経路です。V1 は回帰試験・参照用途および既存の本番運用向けに `v1_gateway` として引き続き利用できます。Browser core と境界付き Browser transfer（upload/download）は実装・acceptance 済みです。

## 概要

CUMG は実行権限をゲートウェイ内に保持しつつ、network-edge TLS や認証などの一般的な責務は保守されたインフラストラクチャに任せられるようにします。中核となる安全規則は、状態変更を伴う操作の結果が曖昧になった場合、client、Hub、Agent、transport、backend、または device が再接続しても、その操作を**自動 replay しない**ことです。

最初にレビューされた computer-use backend は [Cua Driver](https://github.com/trycua/cua) で、CI では **0.19.3** に固定されています。Cua 固有の MCP 名、生の browser/CDP 識別子、Accessibility handle、screenshot、provider response shape は adapter 境界で終端し、安定した CUMG API surface にはなりません。

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

Hub は admission、authorization、operation state、replay barrier、永続的な `indeterminate` quarantine を所有します。Agent は認証済み device session とローカル実行境界を所有します。任意の usage accounting は独立した accounting authority であり、実行を認可したり、quarantine を解除したり、replay を許可したりする権限はありません。

詳しくは [`docs/ARCHITECTURE.ja.md`](docs/ARCHITECTURE.ja.md) と、V2 の canonical boundary を定義する [`docs/v2/V2_POSITIONING.ja.md`](docs/v2/V2_POSITIONING.ja.md) を参照してください。

## はじめに

クリーンな環境から導入する場合は [`docs/GETTING_STARTED.ja.md`](docs/GETTING_STARTED.ja.md) に従ってください。前提条件、OS 権限、backend 検証、Hub/Agent 設定、ローカル MCP 接続、リモート公開までの安全な経路を説明しています。

現在の全ターゲットをビルドするには:

```bash
git clone https://github.com/git-ksk/computer-use-mcp-gateway.git
cd computer-use-mcp-gateway
cargo build --locked
```

デフォルト binary は V2 Hub です:

```bash
cargo run --locked -- --help
```

デスクトップ側 Agent は別プロセスです:

```bash
cargo run --locked --bin v2_agent -- --help
```

V1 は legacy / regression 用として引き続き利用できます:

```bash
cargo run --locked --bin v1_gateway -- --allow-tools list_apps,list_windows,get_accessibility_tree,get_screen_size
```

backend transport を直接公開したり、文書化された trust/TLS 境界を unauthenticated public listener に置き換えたりしないでください。service packaging の例は `packaging/` に、client 例と troubleshooting は [`docs/CLIENTS.md`](docs/CLIENTS.md) と [`docs/TROUBLESHOOTING.md`](docs/TROUBLESHOOTING.md) にあります。

## セキュリティ

CUMG は、デスクトップを変更できる capability について fail-closed に動作します。特に:

- northbound authorization は、認証済みの正確な principal、stable device、exact capability にまで縮約されます。
- stale device generation、stale capability revision、wrong-context ref、consumed ref、unauthorized operation は拒否されます。
- 状態変更操作の結果が曖昧になった場合、その操作は `indeterminate` となり、explicit resolution が行われるまで device は quarantine されます。自動 replay は行いません。
- raw backend ID や generic backend escape hatch は V2 northbound semantic surface に含めません。
- backend request arguments/results、screenshot、clipboard data、bearer token、private credential はデフォルトの telemetry/logging から除外します。
- remote deployment では Hub listener をレビュー済み TLS/authentication edge の背後に置き、Agent からの outbound connection model を維持します。

これらの制御は OS 権限、endpoint hardening、secret custody、network control、deployment 固有の監視を代替するものではありません。機密性の高い desktop をリモート公開する前に、[`docs/SECURITY.ja.md`](docs/SECURITY.ja.md) と [`docs/v2/V2_THREAT_MODEL.ja.md`](docs/v2/V2_THREAT_MODEL.ja.md) を確認してください。

## V2 の状況

現在の実装状況は、内部 milestone 名ではなく capability 単位で追跡します:

| 領域 | 状況 |
| --- | --- |
| Desktop semantic path | Complete / accepted |
| Browser core | Complete / accepted |
| Browser transfer (upload/download) | Complete / accepted |
| V1 regression/conformance | Required and preserved |

Browser core は、opaque CUMG reference と exact-or-refuse 実行 semantics を維持したまま、型付きの prepare、bind、inspect、navigate、click、type、dialog、pointer 経路を提供します。Browser transfer は、context-scoped ref、Agent-private filesystem staging、exact capability check、および stale ref、path escape、partial completion、timeout、cancellation に対する fail-closed 処理を備えた bounded staged upload/download を追加します。

現在の全体像は [`docs/v2/STATUS.ja.md`](docs/v2/STATUS.ja.md) を参照してください。Browser core の evidence は [`docs/v2/acceptance/V2_BROWSER_CORE_ACCEPTANCE.md`](docs/v2/acceptance/V2_BROWSER_CORE_ACCEPTANCE.md)、Browser transfer の evidence は [`docs/v2/acceptance/V2_BROWSER_TRANSFER_ACCEPTANCE.md`](docs/v2/acceptance/V2_BROWSER_TRANSFER_ACCEPTANCE.md) にあります。active spec、acceptance evidence、archive 済み decision record の整理方法は [`docs/README.md`](docs/README.md) を参照してください。

## テストとデプロイ

repository の変更は、CI に任せる前に同じ warning-free baseline をローカルで通すことを推奨します:

```bash
cargo fmt --all -- --check
cargo check --locked --all-targets
cargo test --locked
cargo clippy --locked --all-targets -- -D warnings
python3 scripts/check_docs.py
git diff --check
```

V1 compatibility は明示的な regression boundary のままです:

```bash
python3 scripts/v1_quality_gate.py
python3 scripts/v1_conformance.py
```

通常 CI では、固定された Cua release を Linux、macOS、Windows で実行し、選択された MCP conformance scenario、cancellation behavior、resource/soak check、backend passthrough contract も検証します。信頼された物理 Desktop acceptance は operator-controlled であり、信頼されていない hosted runner に GUI 権限を与える方式ではありません。

保証内容と制約の詳細は [`docs/TESTING.md`](docs/TESTING.md) を参照してください。deployment、service supervision、TLS/authentication edge 要件、credential handling、V1 legacy configuration は [`docs/DEPLOYMENT.md`](docs/DEPLOYMENT.md) にあります。

## ドキュメント

入口は [`docs/README.md`](docs/README.md) です。文書は次の区分に整理されています:

- operator/contributor guide と project governance/versioning
- active V2 specification
- acceptance evidence
- historical PoC / decision record

project の運営方針は [`docs/PROJECT_GOVERNANCE.ja.md`](docs/PROJECT_GOVERNANCE.ja.md)、version selection / release は [`docs/VERSIONING.ja.md`](docs/VERSIONING.ja.md) に定義しています。community participation は [`CODE_OF_CONDUCT.ja.md`](CODE_OF_CONDUCT.ja.md)、support routing は [`SUPPORT.ja.md`](SUPPORT.ja.md)、脆弱性報告は [`SECURITY.ja.md`](SECURITY.ja.md) に従います。

repository-local documentation link checker はこれらの directory を再帰的に検証し、archive や nested document に壊れた local link が蓄積しないようにします。

## ライセンス

MIT。これは独立したプロジェクトであり、Cua AI または Model Context Protocol project とは提携していません。
