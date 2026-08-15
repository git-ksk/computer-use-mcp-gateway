# はじめに

> **注意:** この文書は [GETTING_STARTED.md](GETTING_STARTED.md) の日本語訳です。英語版が正典（canonical）です。

このガイドは、クリーンなマシンから推奨される **V2 Hub + V2 Agent** ランタイムまで、新しいユーザーを案内します。V1 は回帰・リファレンス用として `v1_gateway` のまま引き続き利用可能ですが、`cargo run` は現在 V2 Hub を起動し、明示的な信頼/TLS マテリアルを意図的に要求します。

## 必要なもの

- Git
- Rust 1.88 以降
- インタラクティブなデスクトップセッション
- Cua Driver

この pre-alpha リポジトリは現在ソースから配布されているため、Git、Rust、そしてプラットフォームの通常のネイティブビルドツールは、貢献者（contributor）だけの依存関係ではなくユーザーセットアップの一部です。

このリポジトリは現在、CI で **Cua Driver 0.19.3** に対してテストを行っています。新しい Cua リリースでも動作する可能性がありますが、リポジトリが別のバージョンを固定してテストするまでは、0.19.3 が再現可能な互換性ターゲットです。

ゲートウェイは localhost 優先かつ既定で拒否（deny-by-default）です。以下の例では、小さな非変更（non-mutating）検査サーフェスのみを公開します。検査によって機密性の高いウィンドウタイトル、アプリケーション名、画面サイズ、またはアクセシビリティテキストが明らかになる可能性があるため、個人用デスクトップで使用する前に許可リスト（allowlist）をレビューしてください。

## 1. Git とネイティブビルドの前提条件をインストールする

公式の Git プラットフォームインストーラは以下にあります:

```text
https://git-scm.com/install/
```

### macOS

Apple の Command Line Tools は、Rust ビルドに必要な Git とネイティブのリンカ/ツールチェーンを提供します:

```bash
xcode-select --install
```

インストール後:

```bash
git --version
```

### Windows

Git for Windows をインストールしてください。公式 Git ページのオプションの 1 つに `winget` があります:

```powershell
winget install --id Git.Git -e --source winget
```

新しい PowerShell ウィンドウを開いて検証します:

```powershell
git --version
```

後述の Rust の手順も、Microsoft C++ ビルド/リンカの前提条件を必要とします。`rustup-init` がそれらのインストールを提案することがあります。あるいは **Desktop development with C++** を含む Visual Studio Build Tools をインストールしてください。

### Linux

お使いのディストリビューションのパッケージマネージャを使用してください。Debian/Ubuntu 系のシステムでは、これにより以下のセットアップコマンドで使用される基本的なネイティブビルドツールチェーンと `curl` もインストールされます:

```bash
sudo apt update
sudo apt install git curl build-essential
```

次に検証します:

```bash
git --version
```

Fedora、Arch、openSUSE、その他のディストリビューションでは、ディストリビューションが提供する Git パッケージまたは上記の公式 Git インストールページを使用してください。

## 2. Rust 1.88+ をインストールする

Rust の公式インストーラは `rustup` です:

```text
https://www.rust-lang.org/tools/install
```

### macOS / Linux

公式の rustup ブートストラップコマンドで Rust をインストールします:

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
```

新しいターミナルを開き、次に検証します:

```bash
rustc --version
cargo --version
```

報告される Rust バージョンは 1.88 以降でなければなりません。すでに rustup を使用しているが、有効なツールチェーンが古い場合は、このリポジトリをビルドする前に更新してください:

```bash
rustup update stable
```

### Windows

上記の公式 Rust インストールページにある Windows 用 `rustup-init` インストーラを使用してください。Rust の既定の Windows MSVC ツールチェーンは Microsoft C++ リンカ/ライブラリを必要とします。提案された場合は前提条件のインストールを受け入れるか、**Desktop development with C++** を含む Visual Studio Build Tools をインストールしてください。

インストール後、新しい PowerShell ウィンドウを開いて検証します:

```powershell
rustc --version
cargo --version
```

報告される Rust バージョンは 1.88 以降でなければなりません。既存の rustup インストールが古い場合:

```powershell
rustup update stable
```

## 3. Cua Driver 0.19.3 をインストールする

### macOS

要件: Apple Silicon または Intel 上の macOS 14 以降です。

```bash
CUA_DRIVER_RS_VERSION=0.19.3 /bin/bash -c "$(curl -fsSL https://cua.ai/driver/install.sh)"
```

`cua-driver` がすぐに `PATH` にない場合は新しいターミナルを開き、次にインストールを検証します:

```bash
cua-driver --version
cua-driver doctor
```

Cua の macOS 権限は、任意のターミナル起動のヘルパープロセスではなく `CuaDriver.app` に属していなければなりません。アプリケーション対応のデーモンを起動し、許可（grant）を要求して、それらを検証します:

```bash
open -n -g -a CuaDriver --args serve
cua-driver permissions grant
cua-driver permissions status
```

macOS が関連するペインを開いたら、システム設定（System Settings）で CuaDriver に **Accessibility** と **Screen & System Audio Recording** を許可してください。TCC の許可を変更した場合、その変更が有効になる前に CuaDriver の再起動が必要になることがあります。

**System Settings → Privacy & Security → Screen & System Audio Recording** の下に `CuaDriver.app` が自動的に表示され**ない**場合は、その場合のみこのフォールバックを使用してください: `+` をクリックし、`/Applications/CuaDriver.app` を選択して有効化し、必要に応じて CuaDriver を再起動し、その後 `cua-driver permissions status` でもう一度検証してください。macOS が CuaDriver を通常どおり一覧表示する場合、手動での追加は不要です。

### Windows

要件: インタラクティブなデスクトップセッションと PowerShell 付きの Windows 10/11 または Windows Server です。

```powershell
$env:CUA_DRIVER_RS_VERSION = "0.19.3"
irm https://cua.ai/driver/install.ps1 | iex
cua-driver autostart kick
```

更新されたユーザー `Path` がまだ表示されない場合は新しい PowerShell ウィンドウを開き、次に検証します:

```powershell
cua-driver --version
cua-driver doctor
```

コンピュータ操作（computer-use）アクションにはインタラクティブなデスクトップセッションが必要です。非インタラクティブなサービスや SSH のみのセッションでは、通常のクリック/タイプ/ウィンドウ自動化には不十分です。

### Linux

テスト済みの Cua ターゲットは x86_64 Linux デスクトップセッションを想定しています。X11/XWayland が保守的な経路です。アクセシビリティツリーのツールは AT-SPI 2 も必要とします。Debian/Ubuntu 系の最小インストールでは:

```bash
sudo apt update
sudo apt install libxi6 at-spi2-core
CUA_DRIVER_RS_VERSION=0.19.3 /bin/bash -c "$(curl -fsSL https://cua.ai/driver/install.sh)"
```

次に検証します:

```bash
cua-driver --version
cua-driver doctor
```

ヘッドレスサーバーは操作対象となるデスクトップを提供しません。スクリーンショット/ウィンドウ/入力ツールが動作することを期待する前に、実際のデスクトップセッションを使用してください。

### 任意のテレメトリ設定

Cua Driver は、プロダクトテレメトリが既定で有効であると文書化しています。ご希望であれば無効化してください:

```bash
cua-driver telemetry disable
```

## 4. ゲートウェイを追加する前に Cua を検証する

バックエンドが無害な読み取り操作に応答できることを確認してください:

```bash
cua-driver call list_apps
```

これが失敗する場合は、まず Cua のインストール/権限を修正してください。ゲートウェイは、欠落した OS 権限や使用不能なデスクトップセッションを修復できません。

## 5. ゲートウェイをクローンしてビルドする

```bash
git clone https://github.com/git-ksk/computer-use-mcp-gateway.git
cd computer-use-mcp-gateway
cargo build --locked
```

`Cargo.lock` はコミットされており、通常のビルドでは `--locked` を使用するため、依存関係グラフは再現可能です。

## 6. V2 のアイデンティティと TLS 境界を準備する

V2 は、Hub/Agent の信頼分割が安全モデルの一部であるため、旧 V1 の 1 行起動を意図的に持っていません。`v2_keyctl` で Hub、grant、device の各アイデンティティを個別に作成してください。シークレットファイルはリポジトリの外に保持してください:

```bash
cargo run --locked --bin v2_keyctl -- generate-hub /secure/cumg/hub.key /secure/cumg/hub.pub
cargo run --locked --bin v2_keyctl -- generate-grant /secure/cumg/grant.key /secure/cumg/grant.pub
cargo run --locked --bin v2_keyctl -- generate-device /secure/cumg/device.key /secure/cumg/device.pub
```

Hub gRPC エンドポイントには通常の TLS 証明書ライフサイクルを使用してください。Linux サービスへのデプロイには、[`../packaging/README.md`](../packaging/README.md) と [`DEPLOYMENT.md`](DEPLOYMENT.md) に従ってください。それらはキーの配置、TLS ルート、状態ディレクトリ、継続性ルールを定義しています。Hub、grant、device、TLS のキーを 1 つの資格情報にまとめないでください。

## 7. V2 Hub を起動する

既定のバイナリは現在 V2 Hub です。`v2_hub` は、サービスパッケージング用の明示的な同等バイナリとして残っています:

```bash
cargo run --locked -- --help
# equivalent explicit entrypoint:
cargo run --locked --bin v2_hub -- --help
```

必要な Hub/grant/device公開鍵/TLS/state パスと、Agent 向け gRPC バインドを設定してください。northbound MCP を公開するには、ループバックの `CUMG_V2_MCP_BIND`、正規の公開 HTTPS リソース、および [`DEPLOYMENT.md`](DEPLOYMENT.md) の正確な principal -> device -> `DeviceCapability` ポリシーを設定してください。明示的に単一 principal の認証トンネルには、OAuth RFC 7662 イントロスペクションまたはパッケージ化された trusted-proxy 固定 principal モードのいずれかを選択してください。どちらも、CUMG の認可の前に同一の `AuthenticatedClientPrincipal` にアイデンティティを還元します。この 2 つのモードは相互排他的です。

northbound MCP は素の Cua プロキシではありません。現在の型付き V2 契約は、Agent ネイティブのプロセス/シェル実行と境界付きファイルシステム観測、ネイティブの同一コンテキスト要素アクションを含む Desktop セマンティック能力、そして正確なポリシーとライブ広告が許可する場合の Browser セマンティック能力と境界付きアップロード/ダウンロード転送にグループ化されています。ディスカバリは、正確な認可と、Agent がオンラインである間はそのライブ `CapabilityAdvertisement` によってフィルタリングされます。[`V2_GUI_SEMANTIC_CAPABILITIES.ja.md`](v2/V2_GUI_SEMANTIC_CAPABILITIES.ja.md) と [`V2_BROWSER_SEMANTIC_CAPABILITIES.md`](v2/V2_BROWSER_SEMANTIC_CAPABILITIES.md) を参照してください。

## 8. 管理対象デスクトップで V2 Agent を起動する

デスクトップ上で、個別の outbound（外向き）Agent を実行します:

```bash
cargo run --locked --bin v2_agent -- --help
```

Hub のエンドポイント/ドメイン、安定した device ID、device シークレット、Hub/grant 公開鍵、TLS ルート、状態ディレクトリ、許可された cwd ルートを設定してください。GUI 能力に Cua を使用するには、次を設定します:

```text
CUMG_V2_CUA_COMMAND=cua-driver
CUMG_V2_CUA_ARGS=mcp
CUMG_V2_CUA_BACKEND_VERSION=0.19.3
```

Cua は MCP stdio 経由で Agent の背後に留まります。本番環境では `CUMG_V2_CUA_BACKEND_VERSION` を、レビュー済みの正確な互換性ターゲットに設定してください。具体的な値に設定すると、Agent はすべての接続と再接続のたびに Cua MCP ハンドシェイクの `serverInfo.version` を検証し、ドリフト時にはフェイルクローズ（失敗時に閉じる）します。`external` デフォルトは、カスタムデプロイ用の明示的な未固定（unpinned）モードであり、レビュー済みの Cua 経路の推奨本番設定ではありません。macOS では、Agent/Cua をログイン中ユーザーのセッション内に留め、TCC プロンプトを迂回したり、GUI 自動化をヘッドレスシステムデーモンに移したりしないでください。

## 9. 任意のランタイム使用量クォータ

`CUMG_V2_USAGE_ENDPOINT` がない場合、V2 は `NoopUsageController` を使用し、Node は不要です。

オプションのローカル MemoryUsageStore サイドカーを有効にするには、[`V2_USAGE_ACCOUNTING.md`](v2/V2_USAGE_ACCOUNTING.md) に従ってください。Hub エンドポイントは文字どおりのループバックでなければなりません。例えば:

```text
CUMG_V2_USAGE_ENDPOINT=http://127.0.0.1:8787/
CUMG_V2_USAGE_TIMEOUT_SECS=2
```

これは永続的でないランタイム/セッションのクォータであり、課金ではありません。パッケージ化された Hub+サイドカーのライフサイクルを再起動すると使用量状態はリセットされますが、CUMG の永続的な `indeterminate` 隔離（quarantine）がクリアされることは決してありません。

## 10. リモート公開の前に検証する

northbound MCP リソースを公開する前に、以下をすべて独立に検証してください:

- `cua-driver --version` が設定された `CUMG_V2_CUA_BACKEND_VERSION` と一致する;
- `cua-driver call list_apps` がデスクトップ上で動作する;
- V2 Agent が、想定どおりの安定した device と新しいジェネレーションで接続する;
- northbound OAuth が、意図した issuer+subject の principal のみを生成する;
- `tools/list` に、正確なポリシーによって許可された能力のみが含まれる;
- `list_apps` や `get_screen_size` のような無害な V2 Cua バックエンド操作が成功する;
- オプションの使用量アカウンティングが、有効な場合のみ増加する;
- 再接続によって未解決の CUMG 隔離がクリアされない。

その後、[`DEPLOYMENT.md`](DEPLOYMENT.md) を使用して、レビュー済みのリバースプロキシ/TLS 経路を適用してください。northbound MCP リスナーはループバック限定に保ってください。

## レガシー V1 ローカル回帰パス

回帰・リファレンス用のみとして、旧単一プロセスの手順は明示的な `v1_gateway` バイナリを通じて引き続き利用できます:

```bash
cargo run --locked --bin v1_gateway -- \
  --allow-tools list_apps,list_windows,get_accessibility_tree,get_screen_size
```

その既定のエンドポイントは引き続き `http://127.0.0.1:8100/mcp` と `/healthz` です。V1 の動的な 54 ツールの Cua サーフェスと、正確な名前による allow/deny モデルは、意図的に V2 の正確な能力（exact-capability）契約にはコピーされません。

## 何かが失敗した場合

[`TROUBLESHOOTING.md`](TROUBLESHOOTING.md) を参照してください。PATH の問題、macOS の権限、Linux のディスプレイ/ランタイム依存関係、Windows のインタラクティブセッション、空のツールリスト、Host/Origin の 403、リバースプロキシ認証、バックエンドのタイムアウトをカバーしています。