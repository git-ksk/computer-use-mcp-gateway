# はじめに

> この日本語版は [`GETTING_STARTED.md`](GETTING_STARTED.md) の翻訳です。**英語版を canonical（正典）とし、解釈に差がある場合は英語版を優先します。**

このガイドでは、クリーンな環境から推奨される **V2 Hub + V2 Agent** ランタイムを使える状態まで進めます。V1 は regression/reference 用に `v1_gateway` として引き続き利用できますが、現在の `cargo run` は V2 Hub を起動し、trust/TLS material を明示的に要求します。

## 必要なもの

- Git
- Rust 1.88 以降
- interactive desktop session
- Cua Driver

この pre-alpha repository は現在 source 配布のため、Git、Rust、各 platform の通常の native build tool も contributor だけでなく user setup の一部です。

repository の CI は現在 **Cua Driver 0.19.3** を対象にテストしています。新しい Cua release でも動作する可能性はありますが、repository が別 version を pin / test するまでは 0.19.3 が reproducible compatibility target です。

gateway は localhost-first / deny-by-default です。以下の例で公開するのは小さな non-mutating inspection surface のみです。ただし inspection でも sensitive な window title、application name、screen dimension、accessibility text が見える可能性があるため、personal desktop で使う前に allowlist を確認してください。

## 1. Git と native build prerequisite をインストールする

Git の official platform installer は次にあります。

```text
https://git-scm.com/install/
```

### macOS

Apple の Command Line Tools には、Rust build に必要な Git と native linker/toolchain が含まれます。

```bash
xcode-select --install
```

インストール後に確認します。

```bash
git --version
```

### Windows

Git for Windows をインストールします。official Git page で案内されている方法の1つは `winget` です。

```powershell
winget install --id Git.Git -e --source winget
```

新しい PowerShell window を開いて確認します。

```powershell
git --version
```

後述の Rust setup では Microsoft C++ build/linker prerequisite も必要です。`rustup-init` からインストールを提案される場合があります。または Visual Studio Build Tools の **Desktop development with C++** をインストールしてください。

### Linux

distribution の package manager を使います。Debian/Ubuntu 系では、setup command で使う basic native build toolchain と `curl` も次で導入できます。

```bash
sudo apt update
sudo apt install git curl build-essential
```

続けて確認します。

```bash
git --version
```

Fedora、Arch、openSUSE、その他 distribution では、その distribution が提供する Git package または上記 official Git install page を利用してください。

## 2. Rust 1.88+ をインストールする

Rust の official installer は `rustup` です。

```text
https://www.rust-lang.org/tools/install
```

### macOS / Linux

official rustup bootstrap command で Rust をインストールします。

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
```

新しい terminal を開き、確認します。

```bash
rustc --version
cargo --version
```

表示される Rust version は 1.88 以降でなければなりません。既に rustup を利用していて active toolchain が古い場合は、この repository を build する前に更新します。

```bash
rustup update stable
```

### Windows

official Rust install page の Windows `rustup-init` installer を使います。Rust の default Windows MSVC toolchain には Microsoft C++ linker/library が必要です。prerequisite installation の提案を受け入れるか、Visual Studio Build Tools の **Desktop development with C++** をインストールしてください。

インストール後、新しい PowerShell window を開いて確認します。

```powershell
rustc --version
cargo --version
```

表示される Rust version は 1.88 以降でなければなりません。既存 rustup installation が古い場合は更新します。

```powershell
rustup update stable
```

## 3. Cua Driver 0.19.3 をインストールする

### macOS

要件: Apple Silicon または Intel の macOS 14 以降。

```bash
CUA_DRIVER_RS_VERSION=0.19.3 /bin/bash -c "$(curl -fsSL https://cua.ai/driver/install.sh)"
```

`cua-driver` がすぐ `PATH` に見えない場合は新しい terminal を開き、installation を確認します。

```bash
cua-driver --version
cua-driver doctor
```

Cua の macOS permission は、任意の terminal-launched helper process ではなく `CuaDriver.app` に付与する必要があります。application-backed daemon を起動し、grant を要求して確認します。

```bash
open -n -g -a CuaDriver --args serve
cua-driver permissions grant
cua-driver permissions status
```

macOS が該当 pane を開いたら、System Settings で CuaDriver に **Accessibility** と **Screen & System Audio Recording** を許可してください。TCC grant を変更した場合、反映のため CuaDriver の再起動が必要になることがあります。

**System Settings → Privacy & Security → Screen & System Audio Recording** に `CuaDriver.app` が自動表示されない場合だけ、次の fallback を使います。`+` を押し、`/Applications/CuaDriver.app` を選択して enable し、必要なら CuaDriver を再起動したうえで `cua-driver permissions status` を再確認してください。macOS が通常どおり CuaDriver を一覧表示している場合、manual addition は不要です。

### Windows

要件: interactive desktop session と PowerShell を利用できる Windows 10/11 または Windows Server。

```powershell
$env:CUA_DRIVER_RS_VERSION = "0.19.3"
irm https://cua.ai/driver/install.ps1 | iex
cua-driver autostart kick
```

更新された user `Path` がまだ見えない場合は新しい PowerShell window を開き、確認します。

```powershell
cua-driver --version
cua-driver doctor
```

computer-use action には interactive desktop session が必要です。non-interactive service や SSH-only session だけでは通常の click/type/window automation は動作しません。

### Linux

test 対象の Cua は x86_64 Linux desktop session を想定します。X11/XWayland が conservative な経路で、accessibility-tree tool には AT-SPI 2 も必要です。Debian/Ubuntu 系 minimal installation では次を実行します。

```bash
sudo apt update
sudo apt install libxi6 at-spi2-core
CUA_DRIVER_RS_VERSION=0.19.3 /bin/bash -c "$(curl -fsSL https://cua.ai/driver/install.sh)"
```

続けて確認します。

```bash
cua-driver --version
cua-driver doctor
```

headless server には操作対象 desktop がありません。screenshot/window/input tool の動作を期待する前に real desktop session を用意してください。

### Optional telemetry setting

Cua Driver の文書では product telemetry は default enable です。必要に応じて無効化できます。

```bash
cua-driver telemetry disable
```

## 4. gateway を追加する前に Cua を確認する

backend が harmless な read operation に応答できることを確認します。

```bash
cua-driver call list_apps
```

これが失敗する場合は、先に Cua installation/permission を修正してください。gateway は不足している OS permission や利用不能な desktop session を修復できません。

## 5. gateway を clone / build する

```bash
git clone https://github.com/git-ksk/computer-use-mcp-gateway.git
cd computer-use-mcp-gateway
cargo build --locked
```

`Cargo.lock` は commit 済みで、normal build は `--locked` を使って dependency graph の reproducibility を保ちます。

## 6. V2 identity と TLS boundary を provision する

Hub/Agent trust split 自体が safety model の一部なので、V2 には old V1 の one-line startup は意図的にありません。`v2_keyctl` で Hub、grant、device identity を分離して作成し、secret file は repository 外に置きます。

```bash
cargo run --locked --bin v2_keyctl -- generate-hub /secure/cumg/hub.key /secure/cumg/hub.pub
cargo run --locked --bin v2_keyctl -- generate-grant /secure/cumg/grant.key /secure/cumg/grant.pub
cargo run --locked --bin v2_keyctl -- generate-device /secure/cumg/device.key /secure/cumg/device.pub
```

Hub gRPC endpoint には通常の TLS certificate lifecycle を使います。Linux service deployment では [`../packaging/README.md`](../packaging/README.md) と [`DEPLOYMENT.md`](DEPLOYMENT.md) に従ってください。key placement、TLS root、state directory、continuity rule を定義しています。Hub、grant、device、TLS key を1つの credential にまとめないでください。

## 7. V2 Hub を起動する

現在の default binary は V2 Hub です。`v2_hub` は service packaging 向けの明示的な equivalent binary として残っています。

```bash
cargo run --locked -- --help
# equivalent explicit entrypoint:
cargo run --locked --bin v2_hub -- --help
```

必要な Hub/grant/device-public/TLS/state path と Agent-facing gRPC bind を設定します。northbound MCP を公開するには、[`DEPLOYMENT.md`](DEPLOYMENT.md) に従い、loopback `CUMG_V2_MCP_BIND`、canonical public HTTPS resource、exact principal -> device -> `DeviceCapability` policy を設定します。OAuth RFC 7662 introspection、または明示的に single-principal とする authenticated tunnel 向けの packaged trusted-proxy fixed-principal mode のどちらかを選びます。両者とも CUMG authorization より前に identity を同じ `AuthenticatedClientPrincipal` へ縮約し、同時には利用できません。

northbound MCP は raw Cua proxy ではありません。現在の typed V2 contract は、Agent-native process/shell execution + bounded filesystem observation、same-context native element targeting/action を含む Desktop semantic capability、そして exact policy と live `CapabilityAdvertisement` が許可する場合の Browser semantic capability + bounded upload/download transfer という構成です。discovery は exact authorization と Agent の live advertisement で filter され、Agent が offline の場合は semantic device tool を公開しません。詳細は [`V2_GUI_SEMANTIC_CAPABILITIES.ja.md`](v2/V2_GUI_SEMANTIC_CAPABILITIES.ja.md)、[`V2_BROWSER_SEMANTIC_CAPABILITIES.md`](v2/V2_BROWSER_SEMANTIC_CAPABILITIES.md)、[`V2_CUA_PARITY_MATRIX.ja.md`](v2/V2_CUA_PARITY_MATRIX.ja.md) を参照してください。

## 8. controlled desktop 上で V2 Agent を起動する

desktop では別の outbound Agent を実行します。

```bash
cargo run --locked --bin v2_agent -- --help
```

Hub endpoint/domain、stable device ID、device secret、Hub/grant public key、TLS root、state directory、allowed cwd root を設定します。GUI capability に Cua を使う場合は次を設定します。

```text
CUMG_V2_CUA_COMMAND=cua-driver
CUMG_V2_CUA_ARGS=mcp
CUMG_V2_CUA_BACKEND_VERSION=0.19.3
```

Cua は Agent の背後に MCP stdio で配置します。production では `CUMG_V2_CUA_BACKEND_VERSION` を exact reviewed compatibility target に設定してください。concrete value を設定すると、Agent は connection / reconnect ごとに Cua MCP handshake の `serverInfo.version` を verify し、drift した場合は fail closed します。default の `external` は custom deployment 用の explicit unpinned mode であり、reviewed Cua path の recommended production setting ではありません。macOS では Agent/Cua を logged-in user session 内に置き、TCC prompt を bypass したり、GUI automation を headless system daemon に移したりしないでください。

## 9. Optional runtime usage quota

`CUMG_V2_USAGE_ENDPOINT` を設定しない場合、V2 は `NoopUsageController` を使うため Node は不要です。

optional local MemoryUsageStore sidecar を有効化する場合は [`V2_USAGE_ACCOUNTING.md`](v2/V2_USAGE_ACCOUNTING.md) に従います。Hub endpoint は literal loopback でなければなりません。例:

```text
CUMG_V2_USAGE_ENDPOINT=http://127.0.0.1:8787/
CUMG_V2_USAGE_TIMEOUT_SECS=2
```

これは billing ではなく non-durable runtime/session quota です。packaged Hub+sidecar lifecycle を restart すると usage state は reset されますが、CUMG の durable `indeterminate` quarantine は clear されません。

## 10. remote exposure の前に確認する

northbound MCP resource を公開する前に、次をそれぞれ独立して確認します。

- `cua-driver --version` が configured `CUMG_V2_CUA_BACKEND_VERSION` と一致する。
- desktop 上で `cua-driver call list_apps` が動作する。
- V2 Agent が expected stable device と fresh generation で接続する。
- northbound OAuth が intended issuer+subject principal のみを生成する。
- `tools/list` に exact policy で grant された capability のみが含まれる。
- `list_apps` / `get_screen_size` のような harmless V2 Cua-backed operation が成功する。
- optional usage accounting は enable 時のみ increment する。
- reconnect が unresolved CUMG quarantine を clear しない。

その後、reviewed reverse-proxy/TLS path について [`DEPLOYMENT.md`](DEPLOYMENT.md) に従ってください。northbound MCP listener は loopback-only のままにします。

## Legacy V1 local regression path

regression/reference 用としてのみ、旧 single-process instruction は explicit `v1_gateway` binary で引き続き利用できます。

```bash
cargo run --locked --bin v1_gateway -- \
  --allow-tools list_apps,list_windows,get_accessibility_tree,get_screen_size
```

default endpoint は `http://127.0.0.1:8100/mcp` と `/healthz` のままです。V1 の dynamic 54-tool Cua surface と exact-name allow/deny model は、V2 exact-capability contract には意図的にコピーしていません。

## 問題が起きた場合

[`TROUBLESHOOTING.md`](TROUBLESHOOTING.md) を参照してください。PATH problem、macOS permission、Linux display/runtime dependency、Windows interactive session、empty tool list、Host/Origin 403、reverse-proxy authentication、backend timeout を扱っています。
