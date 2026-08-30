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

## Single mutating authority

物理 desktop / Cua backend 1つを local mutation-authority domain 1つとして扱います。standard single-Mac profile は owner state を `@ROOT@/mutation-authority`（通常 `~/Library/Application Support/computer-use-mcp-gateway/mutation-authority`）に置き、その backend に到達できる supported control plane すべてへ同じ directory を渡します。state が保持するのは closed owner role (`v1` / `v2`) と単調増加 epoch だけです。effectful backend call と authority transition は private OS file lock で直列化します。process が死ねば一時 lock は kernel により解放されますが、durable owner role は自動移譲されません。

V1 の direct-Cua startup は、policy が effectful tool を1つでも公開し得る場合 `CUMG_MUTATION_AUTHORITY_DIR` を必須とします。V2 も Cua または Agent-owned Human Handoff input を構成する場合に同じ directory を必須とします。non-owner でも observe-class Cua call は diagnostics 用に利用できますが、effectful call は durable owner でない限り backend dispatch 前に拒否されます。Human Handoff の begin/recovery も V2 owner gate を通るため、Human input が Cua-side fence を迂回することもありません。

authority change は `v2_maint mutation-authority-switch` による明示的 CAS transition だけです。live mutation と同じ exclusive lock を使い、current owner 不一致、durable V2 quarantine 存在、lock 保持中に Agent-owned Handoff が idle と証明できない場合は拒否します。quarantine resolve、operation replay、process liveness からの owner 推測は行いません。`v2_doctor` は privacy-bounded な current owner / epoch を表示します。

legacy -> V2 の supported cutover は次です。

1. 同じ backend に対して旧 unfenced Legacy Gateway と V2 Agent が同時に loaded ならそこで停止します。upgrade preflight は `legacy_gateway_unfenced` を返し、writer の自動停止や安全な retire の推測は行いません。
2. 旧 Legacy Gateway を先に retire するか、同じ authority directory を使う authority-aware V1 へ更新します。legacy writer を残す必要がある場合は domain を `owner=v1` で初期化します。
3. V2 quarantine が空で Handoff が idle であることを証明してから、明示的な `v1 -> v2` CAS switch を実行します。以後 authority-aware V1 を read-only diagnostics 用に loaded のまま残しても、effectful Cua call は dispatch 前に拒否されます。不要なら unload して構いません。
4. authority-aware V1 へ rollback する場合は、V1 mutation を有効にする前に逆向きの `v2 -> v1` switch を明示的に行います。旧 unfenced Legacy Gateway を V2 と同時に再起動してはいけません。

pre-authority の V2-only installation だけは狭い automatic migration lane を持ちます。通常 preflight で Legacy Gateway が loaded でないことを証明した後、reviewed upgrade helper が V2 を停止し、新しい private authority domain を `owner=v2` で作成して Agent configuration を追加し、その後 restart します。参照されていない既存 authority directory がある場合は採用せず拒否します。

## Safe runtime upgrade

`scripts/v2-single-mac-upgrade.sh` は導入済み single-Mac profile 用の reviewed upgrade helper です。CUMG checkout が clean な `main == origin/main` でない、review 済み Handoff checkout が clean な `main == origin/main` かつ exact `CUMG_V2_EXPECTED_HANDOFF_COMMIT` でない、live quarantine がある、Handoff が active/recovery/faulted、必須 state/service がない、Cua/signing input が不足している場合は replacement 前に拒否します。

exact target の `runtime-<cumg>-<handoff>` generation がすでに存在する場合、helper はそれを上書きしたり in-place repair したりしません。owner-private path、exact source commit pair、manifest schema と完全な file set、記録済み SHA-256 の全一致、symlink 不在、必須 runtime import/dependency、Handoff helper の stable code signature をすべて検証できた場合に限り、その既存 generation を再利用できます。1つでも不一致なら service shutdown 前に拒否します。failure cleanup が削除できるのは現在の invocation が新規作成した generation だけで、既存の verified generation は削除しません。この bounded reuse path は、generation staging だけが先に完了した状態から paired binary/manifest cutover を再開するためのもので、state edit、replay、quarantine 変更を許可するものではありません。

既知の single-Mac Hub/Agent launchd label family は相互排他です。Hub label が2つ、Agent label が2つ、または異なる既知 family の Hub と Agent が同時に loaded なら preflight で拒否します。reviewed cutover では configured service を drain/unload した後、restart 前に alternate の既知 Hub/Agent label を bootout + disable します。rollback/forensics 用 plist は削除せず保持し、この guard は quarantine/replay state を変更しません。

single-Mac maintenance は明示的な one-shot に限定します。upgrade/recovery command に `launchctl submit` を使ってはいけません。underspecified な submitted job では launchd が persistence/relaunch behavior を推論する場合があります。upgrade helper の reviewed launchd wrapper は `scripts/v2_launchd_maintenance_job.py run-upgrade` です。owner-private な temporary plist に `RunAtLoad=true` / `KeepAlive=false` を明記し、upgrade helper が必要とする closed な non-secret environment allowlist だけを渡します。upgrade が non-zero で終了した場合も launchd の `runs` が1を超えないことを検証し、return 前に必ず job を bootout して temporary plist を削除します。failed upgrade の retry は行いません。

upgrade 前には wrapper と `v2-single-mac-upgrade.sh` の両方が current GUI launchd domain から known current/legacy CUMG maintenance label を検査します。wrapper 自身の exact current label 以外に loaded job があれば `stale_maintenance_jobs` で拒否し、active job は自動停止しません。privacy-bounded な state/runs/last-exit の確認には `scripts/v2_launchd_maintenance_job.py inspect` を使います。stale job が running でないことを確認した後だけ `cleanup-stale` で bootout し、matching private temporary plist のみ削除できます。matching maintenance job が active の間は cleanup 自体を拒否します。

signing は exact 40-hex `CUMG_V2_MACOS_CODESIGN_FINGERPRINT` を優先します。display-name の `CUMG_V2_MACOS_CODESIGN_IDENTITY` は、valid certificate が exactly one に解決できる場合だけ compatibility fallback として使えます。選択 certificate の exact Team ID を **sign 前に** 検証し、sign 後も stable identifier / Team-ID designated requirement を再検証します。ad-hoc fallback はありません。

成功する upgrade の順序は次です。

1. CUMG/Handoff source provenance、quarantine=0、loaded service、既知 Hub/Agent launchd family の競合なし、exact current one-shot wrapper 以外の stale CUMG maintenance job がないこと、Agent-owned Handoff idle を locator/owner data を出さずに確認;
2. 1つの merged CUMG commit から paired binaries を build し、exact reviewed Handoff `dist` / `package.json` / lockfile と CUMG runtime host script から private `runtime-<cumg>-<handoff>` generation を stage。lockfile-pinned production dependency だけを lifecycle script 無効で導入し、runtime generation を symlink-free に保つため npm command shim の `.bin` link を除去、残存 dependency symlink を拒否した上で、service を止める前に configured Node executable で staged entrypoint の import 成功を確認;
3. Handoff host helper を新 generation へ copy して stable sign。live helper は in-place 変更しない;
4. old binaries/config、Handoff env、helper copy、runtime dependency を含む self-contained old Handoff generation を private rollback bundle に保存。dependency が欠けた archive は external-runtime reference のまま扱い、その runtime の cleanup を許可しない。authoritative Hub/Agent state は drain 後だけ保存;
5. Hub を先に signal して admission close/drain、Hub/Agent/signer unload 後、alternate の既知 Hub/Agent label を plist を削除せず bootout + disable し、stopped quarantine を再確認;
6. stopped 状態で private Handoff env と Agent plist を staged generation へ atomic retarget し、paired CUMG binaries を atomic replace;
7. merged CUMG source commit、exact Hub/Agent application-schema version、package version、binary SHA-256 を持つ schema 3 `runtime-manifest.json` を作成;
8. signer -> Hub -> Agent で起動し、既知 launchd family の競合がないことを再確認してから read-only Handoff status を含む `v2_doctor` を実行;
9. doctor healthy の後だけ、eligible な未参照 `runtime-*` code directory を prune。active runtime、legacy external rollback reference、bounded recent generations、symlink/unsafe candidate は保護または拒否。checkpoint/key/env/audit/control/rollback data は cleanup candidate 外。

例:

```bash
export CUMG_V2_EXPECTED_CUA_VERSION=0.19.3
export CUMG_V2_MACOS_CODESIGN_FINGERPRINT=0123456789ABCDEF0123456789ABCDEF01234567
export CUMG_V2_MACOS_TEAM_ID=ABCDEFGHIJ
export CUMG_V2_HANDOFF_SOURCE_ROOT="$HOME/x-code/mcp-execution-handoff"
export CUMG_V2_EXPECTED_HANDOFF_COMMIT=<reviewed-40-hex-commit>

scripts/v2-single-mac-upgrade.sh --preflight-only
python3 scripts/v2_launchd_maintenance_job.py run-upgrade
```

preflight は service stop/restart を行わないため direct 実行で構いません。actual cutover は ad-hoc launchd job ではなく reviewed one-shot wrapper を必ず使います。upgrade が non-zero でも temporary job の bootout/plist cleanup 後にその exit を返し、retry はしません。最初の pinned cutover 後は `CUMG_V2_HANDOFF_SOURCE_ROOT` を explicit reviewed checkout として渡してください。runtime の `CUMG_V2_HANDOFF_ROOT` は immutable staged code を指し、development checkout ではありません。

rollback bundle は old-binary / old-state / Handoff-code の明示的 evidence set です。new runtime が進めた state に old binary だけを戻してはいけません。post-start failure は new profile を fail closed で停止し、recovery は explicit operator action に限定します。

expired-recovery abandonment は signed checkpoint を削除する前に private append-only JSONL audit を書きます。record は timestamp、recovery epoch、prior closed recovery status、bounded result code のみです。locator、process/window/context/intervention ID、principal、action digest、TURN credential、Human input、payload は保存しません。audit append が失敗した場合は abandonment を拒否し、recovery を authoritative のまま維持します。

## `v2_doctor`

`v2_doctor` は read-only です。quarantine resolve、work dispatch、secret content read、raw command/result/desktop data 出力は行いません。

`v2_doctor` 自身を live single-Mac Agent の `execute_process` / `shell` 経由で起動すると、live Hub では quarantine されていない実行中operationでも、restart-safe Hub checkpoint上では `HubRestartAfterDispatch` として見えます。doctor がこれを `diagnostic_self_observation=restart_safe_active_caller` と分類できるのは、doctor process が現在launchdでrunningなAgent processの子孫であること、Agent -> Hub loopback transportがestablishedであること、checkpointがenrolled device 1台かつquarantine-shaped entry 1件だけであること、そのentryがregistryのcurrent generationに属するprocess/shell operationであること、durable dispatch bindingが存在して`auto_reconciling`であることをすべて満たす場合だけです。caller-supplied operation IDは判定に使いません。条件欠落/不一致、複数entry、older generation、実際のindeterminate reasonは従来どおりblocking `live_quarantine` errorのままです。この分類はdiagnostic表示だけを変え、restart restore時の Dispatched -> durable `Indeterminate` 変換、quarantine、no-replay、state mutationのsemanticsは一切変更しません。

standard profile では次を確認します。

- runtime manifest schema 3、exact Hub/Agent application-schema version、source commit、`v2_hub` / `v2_agent` / `v2_maint` / `v2_doctor` / `v2_recover` / `v2_recovery_enclave_helper` / `v2_grant_signer` の exact SHA-256 identity;
- authoritative Hub checkpoint の readability と current registry/capability schema;
- enrolled single-Mac device が 1 台だけであることと current generation;
- Agent checkpoint readability と exact Hub/Agent generation pairing;
- live quarantine count;
- Hub/Agent/external-signer LaunchAgent の running state、privacy-bounded な current/legacy CUMG maintenance-job presence、Agent -> loopback Hub transport の established 状態;
- private signer socket と parent permission;
- server certificate と pinned Agent trust root の validity;
- 実 Cua Driver version と explicit reviewed pin の一致;
- private control socket 経由の Agent-owned Handoff status。output は bounded guidance（idle / exact recover-reissue / exact recover-rebind-or-prior-surface-absent時のabandon / active / faulted）のみで、locator や owner/intervention ID は出力しない。

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
- schema-3 runtime manifest が installed paired binary と exact Hub/Agent application schema を verify;
- Handoff が recovery/resume/fault なしの idle;
- harmless northbound semantic smoke が durable terminal `Completed` に到達;
- operator-selected bake period が終わるまで old binary/state rollback pair を保持。

さらに dirty/diverged source や live quarantine など、少なくとも 1 つの unsafe upgrade を拒否させ、binary replacement が起きないことを確認します。
