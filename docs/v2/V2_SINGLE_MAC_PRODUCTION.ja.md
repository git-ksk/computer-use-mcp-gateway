# V2 single-Mac production profile

この profile は、信頼済みのログイン中 macOS 開発端末 1 台で V2 Hub、external grant signer、Agent、Cua backend を同居させる構成です。Linux/systemd deployment を置き換えるものではなく、CUMG の network trust boundary も広げません。

## Security boundary

review 済み構成は次です。

```text
reviewed proxy/tunnel
        |
127.0.0.1 northbound MCP
        |
V2 Hub (LaunchAgent, loopback Hub transport)
        |
private Unix socket
        |
external grant signer (LaunchAgent)

V2 Agent (logged-in user LaunchAgent)
        |
Cua Driver (pinned version)
```

Hub transport と MCP listener は loopback-only です。public TLS / origin policy は reviewed proxy/tunnel が引き続き担当します。Hub configuration には grant private-key path/material を渡さず、Hub が受け取るのは external signer が発行した署名済み exact-capability grant だけです。この profile に in-process fallback はありません。ただし各 LaunchAgent は同じ logged-in macOS user で動くため、これは process/configuration separation であり、侵害された same-user Hub process に対する OS 強制の key-custody boundary ではありません。別 signer user を使う Linux/systemd profile の方が強い custody boundary です。

secret と durable state は repository 外の以下に置きます。

```text
~/Library/Application Support/computer-use-mcp-gateway/
```

state、secret、trust material、rollback asset を含む directory は owner-private (`0700`) とします。signer runtime directory は macOS の Unix-domain socket path 長制限に十分余裕を持たせるため、短い `~/Library/Caches/cumg-v2/` に分離し、ここも `0700` とします。secret file は `0600` とします。secret value を plist、runtime manifest、command output、Issue log に入れてはいけません。

## Reviewed LaunchAgents

Template は `packaging/launchd/single-mac/` にあります。

- `com.github.git-ksk.cumg-v2-grant-signer.plist`
- `com.github.git-ksk.cumg-v2-hub.plist`
- `com.github.git-ksk.cumg-v2-agent.plist`

`@HOME@`、`@ROOT@`、`@RUN_ROOT@`、`@BINARY_DIR@`、`@HANDOFF_CONTROL_SOCKET@`、`@HANDOFF_RUNTIME_COMMAND@`、`@HANDOFF_RUNTIME_SCRIPT@`、`@HANDOFF_RUNTIME_ENV_FILE@` を置換し、明示的な `REPLACE_*` を deployment の reviewed resource URI、trusted-proxy identity、stable device ID に置換します。Hub template が持つ Handoff 機能は private operator-control relay のみで、canonical Handoff runtime / WebRTC / capture / Human input / private transport env は Agent template が所有します。single-Mac profile は意図的に loopback-only なので、Hub/MCP bind を public address に変更してはいけません。

signer を load する前に runtime directory を作成します。

```bash
ROOT="$HOME/Library/Application Support/computer-use-mcp-gateway"
RUN_ROOT="$HOME/Library/Caches/cumg-v2"
install -d -m 700 "$RUN_ROOT"
```

起動順は signer -> Hub -> Agent です。Cua と Agent-owned Handoff の capture/input は操作対象端末の macOS TCC attribution を必要とするため、Agent は logged-in user session の LaunchAgent として動かします。

signer policy example は意図的に最小です。review 済み northbound policy が必要とする exact `DeviceCapability` だけを追加してください。wildcard capability も signer fallback もありません。

## Safe runtime upgrade

`scripts/v2-single-mac-upgrade.sh` は、すでに導入済み single-Mac profile 用の reviewed upgrade helper です。以下では fail closed します。

- source checkout が clean な `main == origin/main` ではない;
- installed paired `v2_maint` で authoritative state を inspect できない;
- live quarantine が存在する;
- 必須 LaunchAgent / state directory がない;
- reviewed Cua version が明示されていない;
- 有効な Apple code-signing identity と 10 文字の Team ID が明示されていない;
- Agent Handoff env/helper path が存在しない、または unsafe;
- external signer profile が不完全。

成功する upgrade は次の順序です。

1. source/state/service preflight;
2. 1 commit から `v2_hub`、`v2_agent`、`v2_maint`、`v2_doctor`、`v2_grant_signer` を build し、`v2_agent` を stable Agent identifier / Team-ID designated requirement で sign;
3. old binary と service config を保存し、drain 後に authoritative な停止済み Hub/Agent state を rollback asset として保存;
4. Agent 接続を維持したまま Hub に先に signal を送り、新規 admission を閉じて既存 work を drain;
5. Hub shutdown 後に Agent と signer を unload;
6. 停止済み authoritative state を再確認し、drain 中に quarantine が生成されていれば binary replacement 前に拒否;
7. configured Agent-local Handoff host helper を stable Team-ID designated requirement で sign した後、version-paired runtime binaries を atomic replace;
8. package version、source commit、binary name、SHA-256 だけを含む `runtime-manifest.json` を作成;
9. signer -> Hub -> Agent の順で起動;
10. `v2_doctor` を実行。post-start doctor が失敗した場合は profile を fail closed で停止し、新しい state に old binary だけを自動的に組み合わせません。

例:

```bash
CUMG_V2_EXPECTED_CUA_VERSION=0.19.3 \
CUMG_V2_MACOS_CODESIGN_IDENTITY="Apple Development: Example Name (TEAMMEMBER)" \
CUMG_V2_MACOS_TEAM_ID=ABCDEFGHIJ \
  scripts/v2-single-mac-upgrade.sh --preflight-only

CUMG_V2_EXPECTED_CUA_VERSION=0.19.3 \
CUMG_V2_MACOS_CODESIGN_IDENTITY="Apple Development: Example Name (TEAMMEMBER)" \
CUMG_V2_MACOS_TEAM_ID=ABCDEFGHIJ \
  scripts/v2-single-mac-upgrade.sh
```

helper は rollback asset directory を表示します。これは old-binary / old-state の明示的な pair です。new runtime が進めた state に old binary だけを戻してはいけません。recovery は常に明示的 operator action です。

## `v2_doctor`

`v2_doctor` は read-only です。quarantine resolve、work dispatch、secret content read、raw command/result/desktop data 出力は行いません。

standard profile では次を確認します。

- runtime manifest schema、source commit、`v2_hub` / `v2_agent` / `v2_maint` / `v2_doctor` / `v2_grant_signer` の exact SHA-256 identity;
- authoritative Hub checkpoint の readability と current registry/capability schema;
- enrolled single-Mac device が 1 台だけであることと current generation;
- Agent checkpoint readability と exact Hub/Agent generation pairing;
- live quarantine count;
- Hub/Agent/external-signer LaunchAgent の running state と Agent -> loopback Hub transport の established 状態;
- private signer socket と parent permission;
- server certificate と pinned Agent trust root の validity;
- 実 Cua Driver version と explicit reviewed pin の一致。

JSON output は local operator automation に利用できます。

```bash
"$HOME/Library/Application Support/computer-use-mcp-gateway/bin/v2_doctor" \
  --expected-cua-version 0.19.3 \
  --json
```

exit status は healthy=`0`、degraded/warning=`1`、unsafe/error=`2` です。non-zero result を indeterminate operation の replay / quarantine clear 許可として扱ってはいけません。

## Acceptance

single-Mac upgrade を healthy とする前に最低限次を満たします。

- `v2_doctor` が `overall=healthy`;
- restart 後に fresh authenticated Agent generation がある;
- live quarantine が 0 のまま;
- runtime manifest が installed paired binary をすべて verify;
- harmless northbound semantic smoke が durable terminal `Completed` に到達;
- operator-selected bake period が終わるまで old binary/state rollback pair を保持。

さらに dirty/diverged source や live quarantine など、少なくとも 1 つの unsafe upgrade を拒否させ、binary replacement が起きないことを確認します。
