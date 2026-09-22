# V2 v0.7.0 release scope / acceptance

> English版がcanonicalです: [V2_070_RELEASE_SCOPE.md](V2_070_RELEASE_SCOPE.md)

Release: **v0.7.0 — Operator Ergonomics & Recovery Evidence**

## release scope

- #342: caller が `operation_id` を指定する場合、新しい effectful call ごとに call 前の fresh CSPRNG-generated 128-bit ID を要求する guidance を tool schema / EN/JA docs に追加。pattern/counter/hand-authored value は shape-valid でも生成方法としてunsupported。
- #295: reviewed V2 CLI binary が bounded `--version` package/source identity を公開。release-candidate smoke は fresh extraction 後の表示を closed artifact manifest と照合し、authority は引き続き manifest/hash verification に置く。
- #304: first-class read-only `cumg_status` が既存 unified operator-status model を MCP consumer へ公開。shell authority や別の health/authorization state machine は作らない。
- #289: execution-safety durable schema v14 が ambiguous launch/termination recovery 用の privacy-bounded application target identity を保持し、reviewed local recovery surface のみで扱う。
- #290: Agent M1 persistence schema v6 が reviewed operation-bound backend receipt を保持でき、exact authoritative evidence だけを既存 signed self-reconciliation path へ渡す。unsupported/weak evidence は operator-required のまま。
- #347: macOS release candidate に verified single-Mac disaster-recovery backup/restore を同梱。external manifest anchoring、exact runtime/Handoff identity、mutation-authority preservation、staged restore、explicit activation、quarantine/replay preservation、corruption/mismatch fail-closed を実装。

v0.7.0 milestone に含まれる #359 / #360 / #361 は **not planned** でcloseされており、admitted implementation ではありません。#359 は #347 と重複する backup/restore track、#360 は managed job への Linux cgroup-v2 拡張、#361 は SBOM/provenance-ready candidate evidence です。milestone上でclosedでも shipped support claim とはみなしません。

## release identity / compatibility

v0.7.0 snapshot:

- crate/package **0.7.0**
- live control schema **12**
- capability schema **8**
- persisted device registry **8**
- Hub-Agent transport **6**
- execution-safety durable schema **14**
- Agent M1 persistence schema **6**

live control/capability/registry/Hub-Agent pairing は released v0.6.0 と同じ **12/8/8/6** です。ただし rolling mixed-version compatibility を意味しません。packaged Hub / Agent / maintenance tool / runtime manifest / Handoff generation は exact source/package identity でversion-pairedのままです。

v0.7.0 では v0.6.0 から durable-state compatibility boundary が2点進みます。

1. execution-safety v13 state はreadableですが、private recovery-target metadataを持つv14 stateはv13へlossy downgradeできません。
2. historical Agent M1 schema v5 はreceipt無しならreadableですが、receipt-bearing v6 stateはv5へlossy representationできません。

pre-1.0 northbound input migration が1点あります。`terminate_application` は `process_id` に加えて bounded `application` identity を必須とし、後のlocal recoveryでcaller-selected targetを識別可能にします。`process_id`だけを送るv0.6 clientは、このeffectful operationを使う前にMCP tool discovery/schemaをrefreshする必要があります。Agent control commandはPID-onlyのままでlive control schema numberも変わりません。

`cumg_status`、bounded `--version`、operation-ID generation guidance、verified backup/restore は additive/operator-facing change であり、それ自体が execution authority を増やすことはありません。

## upgrade / rollback boundary

v0.6.0 -> v0.7.0 はreviewed artifact/maintenance pathを使う **paired runtime upgrade** です。live 12/8/8/6 identity が同じでも rolling compatibility を推測せず、exact package/source/runtime identity でactivationを判定します。

rollbackはupgrade前に取得したversion-paired rollback materialだけを使います。v0.7がtarget-bearing execution-safety v14 stateまたはreceipt-bearing Agent M1 v6 stateを書いた後に、v0.6 runtimeへそのnewer authoritative stateを再解釈させてはいけません。pre-upgrade v0.6 checkpoint + paired v0.6 binary/configを復元します。checkpoint hand-edit、recovery metadata/receiptの削除、tag移動でcompatibilityを捏造しません。

verified backup/restore は disaster-recovery path であり upgrade rollback の代替ではありません。release-paired rollback はpre-upgrade runtime/state boundaryを保持し、#347 backup/restoreはexact reviewed snapshotとmutation/quarantine/replay truthを保持してexternal manifest digestを別保管します。どちらもsettlementやsnapshot freshnessを捏造しません。

## acceptance evidence

- #342 / #295 / #304 / #289 / #290 / #347 はrelease gate前にcomplete。v0.7.0 admitted scopeのopen issueは0。
- #289/#290 regressionはdurable target/receipt compatibility、exact binding、restart persistence、mismatch/downgrade fail-closed、authoritative evidence不在時no-replayを検証。
- #295 RC regressionはfresh extraction後のpackaged `--version`を実行し、artifact manifestと異なるpackage/source identityを拒否。
- #304は既存unified status compositionを再利用し、MCP surfaceがread-only / privacy-bounded / quarantine-aware / non-authoritativeであることをdeterministic testで検証。
- #347 regressionはwriter/authority-lock refusal、quarantine/replay/mutation-authority exact round trip、external digest、corrupt/missing/extra/symlink/permission/newer-schema rejection、Handoff mismatch、clean-profile enforcement、staged tamper、bounded remediation、signer -> Hub -> Agent order、restart後authoritative reconciliationを検証。
- running production profileに対するnon-destructive trusted-macOS smokeで、service稼働中backupが `effectful_service_loaded / stop_reviewed_services` で拒否されoutputを作らないことを確認。
- local release gateはPython harness regression、Rust all-target fmt/check/clippy/test、docs/link、`git diff --check`を要求。
- protected CIはCodeQL、dependency review、docs links、backend passthrough、Linux/macOS/Windows Cua smoke、Rust CI、Release Candidate Artifacts build/verify/smokeを要求。

## Product Readiness checklist result

standing [Product Readiness checklist](../PRODUCT_READINESS.ja.md)をexact release scopeへ適用します。

- [x] distributionはsource-only GitHub pre-release。CI release-candidate archiveはevidenceでofficial binary Release assetではない。
- [x] package/source/schema identityはrelease manifest、installed runtime manifest、`--version` cross-check、checksum、closed allowlistでexact/bounded。
- [x] clean install/upgrade/rollbackはversion-paired / fail closed。v0.6で表現できないv0.7 durable stateをin-place downgradeしない。
- [x] mixed/incompatible/future stateはfail closed。同じschema numberでもhistorical v0.6 identityをpackage/sourceから独立して扱う。
- [x] `v2_status` / `v2_doctor` / `cumg_status` はdiagnostic/read-only composition surfaceでauthorization/recovery authorityにならない。
- [x] operation-ID guidanceはCSPRNG-specificだがserver-side entropy heuristicでcaller randomnessを証明したふりをしない。
- [x] recovery target metadata / backend receiptはprivacy-boundedかつauthority-classified。observational evidenceはquarantineをclearできない。
- [x] verified backup/restoreはexact paired runtime、durable state、mutation owner/epoch、unresolved quarantine、replay barrier、reviewed in-root trust/secretを保持し、ephemeral/lock/log/socket/cache/stale-keyを除外。
- [x] restoreはactivation前にstageし、external manifest digestとclean exact profileを要求し、fresh coordination lock/generationを作る。checkpoint hand-editやsettlement捏造はしない。
- [x] ambiguous effectful workは`Indeterminate`/quarantine/no-auto-replayを維持し、quarantine減少は既存authoritative reconciliation contractだけに限定。
- [x] EN/JA operator-critical docs、release note、support-claim boundary、migration/rollback guidanceを同期。
- [x] deterministic regression + exact release commitのprotected CI/RC artifact greenをtag前に要求。

## support-claim boundary

v0.7.0は、treeにimplementationが存在するだけのunfinished support claimを取り込みません。

- #139 provider-neutral OIDC/JWT plumbingはcompiledでもprovider-specific physical signed-token supportはacceptance完了までwithheld。
- #217 cross-platform recovery parityはopen。blanket Windows/Linux recovery parity claimは作らない。
- #228 Linux FIDO2 UV recoveryはtrusted physical Linux + real UV-capable FIDO2 acceptance完了までwithheld。
- open `v0.6.1 — Support Claim Expansion` milestoneは別のevidence trackで、**v0.7.0のprerequisiteではありません**。未完claimは本releaseへimportしません。
- #222 second-real-backend semantic-neutrality proofはfuture portability evidenceでv0.7 support claimではない。
- Hosted Cloud Run Hub/Handoff (#215/#275-#284) はunsupported。
- #360 managed-job cgroup-v2 extensionと#361 SBOM/provenance-ready evidenceは本releaseではshipしない。

## publication boundary

`v0.7.0` は **source-only GitHub pre-release** です。CI release-candidate archiveはrelease evidenceのみでofficial binary assetとして添付しません。#361は未実装なのでofficial binary publicationに必要なSBOM/license/provenance evidenceをclaimせず、CI candidateをApple-notarized public installerとも表現しません。

dedicated release PR merge後、exact resulting `main` commitのrequired protected checksとRelease Candidate Artifactsがgreenになった場合だけimmutable annotated `v0.7.0` tagとmatching GitHub pre-releaseを作成します。published tagは移動・再利用しません。
