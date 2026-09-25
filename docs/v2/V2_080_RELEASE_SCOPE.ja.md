# V2 v0.8.0 release scope / acceptance

> この日本語版は [V2_080_RELEASE_SCOPE.md](V2_080_RELEASE_SCOPE.md) の翻訳です。**英語版を canonical（正典）とし、解釈に差がある場合は英語版を優先します。**

Release: **v0.8.0 — Backend Portability & Recovery Safety**

## Admitted release scope

- **#377 — authoritative pre-enqueue non-delivery:** execution-safety schema v15では、Hubがexact first outbound attemptのenqueue前failureを証明できる場合だけconfirmed-not-executedとしてsettleします。successful enqueue後のtransport/session lossは引き続きambiguous / quarantineで、old operationをreplayしません。
- **#379 — structured semantic-refusal remediation:** existing stable safe codeをcanonicalのまま維持し、review済みrefusalにboundedな `required_actor`、`next_action`、`same_operation_replay_safe=false`、`remediation_is_authority=false`、`fresh_call_required` guidanceを返せます。hintはauthorization / recovery / scope / consent / route / foreground / replay authorityにはなりません。
- **#380 — constrained execution environment / PATH contract:** process/shellは`env_clear()`とreview済みAgent service environment allowlistを維持します。`PATH`はservice environmentに存在するときだけinheritし、login/interactive profileはsourceせず、caller `env.PATH` overrideはdenyしたままです。service PATH外のtoolはreview済みabsolute executable pathを使います。
- **#222 — second real-backend portability evidence:** macos-mcp 0.4.0をnarrowなsecond `ComputerUseBackendAdapter`として、CUMG `ListApplications` / `MovePointer` / `PointerClick` sliceだけにadaptします。provider tool名/stateはprivateのまま、operation identity / fencing / ambiguity / quarantine / reconciliation / replay policyはCUMGがauthoritativeです。

これはboundedなportability / recovery-safety workで、generic MCP proxy、provider marketplace、VM/fleet scheduler、別Handoff/settlement systemには拡張しません。

## Release identity / compatibility

v0.8.0 release snapshotは以下をpinします。

- crate/package: **0.8.0**;
- live control schema: **12**;
- capability-advertisement schema: **8**;
- persisted device-registry schema: **8**;
- Hub-Agent transport schema: **6**;
- execution-safety durable schema: **15**;
- Agent M1 persistence schema: **6**.

live 12/8/8/6 protocol identityとAgent M1 v6 writer contractはv0.7.0から変更しません。Hub durable execution-safety writerだけを#377のためv14からv15へ進めます。historical v14 stateはreadableですが、v15-only pre-enqueue non-delivery evidenceはv14へlossyに表現できません。

## Upgrade / rollback boundary

v0.7.0からv0.8.0へのupgradeはreview済みartifact/maintenance pathによる**version-paired runtime upgrade**です。live schema numberが同じでもrolling mixed-version guaranteeではありません。

upgrade前にreview済みpre-upgrade v0.7 rollback checkpointとpaired v0.7 binaries/configurationを保持します。v0.8がexecution-safety v15-only evidenceを書いた後にv0.7 Hub/maintenance writerをそのstateへ直接当ててはいけません。rollbackはcaptured pre-upgrade v0.7 stateとpaired v0.7 runtimeをrestoreします。checkpoint hand-edit、v15 evidence除去、release tag移動は禁止です。

#379/#380はadditive caller/operator contractで、authorizationを弱めません。#222はadditional portability evidenceであり、macos-mcpをCua feature-parity replacementにはしません。

## Acceptance evidence

- v0.8.0 milestoneのadmitted 4 issue（#377 / #379 / #380 / #222）はすべてCLOSEDです。
- #377 / PR #382でexact pre-enqueue settlement、persist-before-live-release、bounded reporting、successful enqueue/session lossが`Indeterminate` / quarantineに残るunsafe-neighborを証明しています。
- #379 / PR #383でdeterministic provider-neutral remediation、unknown-code fail-safe、actor separation、implicit foreground/route/scope/replay escalation禁止を証明しています。
- #380 / PR #384でraw PATHを公開せず、ambient interactive-shell authorityをinheritしないexecution environmentをmachine-readableにしています。
- #222 / PR #385でtrusted-Mac physical macos-mcp 0.4.0 acceptanceを記録し、stale generation/revisionはdispatch 0件、post-dispatch unproven provider resultはdurable quarantine、restart/reconnect後もquarantine維持、old operation replay禁止を証明しています。詳細は [V2_SECOND_BACKEND_PORTABILITY_ACCEPTANCE.md](acceptance/V2_SECOND_BACKEND_PORTABILITY_ACCEPTANCE.md) を参照してください。
- PR #385 protected CIはRust / backend passthrough / CodeQL / dependency・docs checks / Linux・macOS・Windows release-candidate bundle / Linux・macOS・Windows native Cua smokeがすべてgreenです。

## Product Readiness checklist result

standing [Product Readiness checklist](../PRODUCT_READINESS.ja.md)をこのexact release scopeへ適用します。

- [x] Distribution scopeはsource-only GitHub pre-releaseです。verified CI archiveはrelease evidenceでofficial binary assetではありません。
- [x] candidate archiveはclosed manifest/checksum verificationと既存signing/notarization boundaryを維持します。
- [x] v0.7 -> v0.8 upgradeはversion-pairedで、durable v15 rollback boundaryをfail-closedに明記します。
- [x] mixed/incompatible stateはreplay/settlement authorityになりません。
- [x] 既存single-Mac install / diagnostics / recovery / backup-restore / paired rollback pathをsupported reference topologyとして維持します。
- [x] `v2_status` / `v2_doctor` / `cumg_status` / refusal remediationはobservational/composition guidanceだけです。
- [x] #377はpost-enqueue uncertaintyをquarantineに残し、later effectful workにはfresh operation IDを要求します。
- [x] #379はremediationとreplay safetyを分離し、implicit fallback/escalationを禁止します。
- [x] #380はraw valueを公開せず、login-shell authorityをimportせずにservice-environment PATH semanticsを明示します。
- [x] #222はprovider provenanceとcaller/Hub/Agent/Human authorityを分離し、real provider-backed operationとambiguity handlingを証明しています。
- [x] existing Cua regressionはLinux / macOS / Windowsですべてgreenです。
- [x] EN/JA operator-critical release documentationを同期します。
- [x] tag前にexact release commitのprotected CI / Release Candidate Artifacts greenを必須とします。

## Support-claim boundary

- macos-mcp 0.4.0はdeliberately narrowなportability-evidence adapter sliceで、Cuaのfeature-parity/default replacementやgeneric provider platformではありません。
- #139 signed-token dogfood、#217 cross-platform recovery parity、#228 physical Linux FIDO2 UVはseparate v0.6.1 support-claim evidence trackのままです。
- hosted Cloud Run Hub/Handoffはv0.9.0へ割当済みで、acceptance gate完了までunsupportedです。
- `v1_gateway`はlegacy/regression-onlyのままで、deliberate retirementはv0.10.0へ割当済みです。

## Publication boundary

`v0.8.0` は **source-only GitHub pre-release** です。verified CI release-candidate archiveはevidenceのままでofficial binary assetとして添付しません。immutable annotated `v0.8.0` tagとmatching GitHub pre-releaseはdedicated release PR merge後、exact resulting `main` commitのrequired protected checksとRelease Candidate Artifactsがgreenになってから作成します。
