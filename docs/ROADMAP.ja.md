# ロードマップ

> この日本語版は [`ROADMAP.md`](ROADMAP.md) の翻訳です。**英語版を canonical（正典）とし、解釈に差がある場合は英語版を優先します。**

2026-09-22 時点の status: **V1 は legacy/regression surface、推奨 runtime は V2、`v0.6.0` Managed Developer Execution が released baseline です。現在OPENのbacklogは `v0.10.0` まで明示的にversion割当済みです。**

この roadmap は、現在の maintenance priority、将来の public-contract work を採用するための rule、stable 1.x contract へ進む条件を定義します。candidate feature がすべて ship するという約束ではなく、roadmap section が存在するだけで release number を割り当てることもありません。

version の選択は [`VERSIONING.ja.md`](VERSIONING.ja.md)、project/change governance は [`PROJECT_GOVERNANCE.ja.md`](PROJECT_GOVERNANCE.ja.md) に従います。V2 product boundary の canonical document は引き続き [`v2/V2_POSITIONING.md`](v2/V2_POSITIONING.md) です。

## Product boundary

CUMG 固有の core は次の通りです。

> **stateful interactive desktop の delegated control に対する uncertainty-aware execution safety**

将来の変更が維持しなければならない invariant は次です。

```text
specific authenticated principal
        |
specific desktop + exact capability
        |
operation ID + exclusive ownership + generation/capability fencing
        |
state-changing action dispatched
        |
completion provable?
   yes -> terminal
   no  -> indeterminate -> durable quarantine -> explicit resolution
```

曖昧な state-changing operation は、client、Hub、Agent、transport、backend、device が reconnect したことを理由に automatic retry / replay しません。

完了済みの V1/V2 implementation history と acceptance evidence は [`V1_ACCEPTANCE.md`](V1_ACCEPTANCE.md)、[`v2/STATUS.md`](v2/STATUS.md)、[`v2/acceptance/`](v2/acceptance/)、[`archive/`](archive/) に残しています。この file は V2 closeout 後も relevant な work に意図的に絞ります。

## Released baseline: `0.6.x`

`v0.6.0` が released **Managed Developer Execution** baseline です。#106 managed jobs、#114 sandboxed Playwright/E2E、#267 optional Linux cgroup-v2 hardening、final #335 schema/artifact/upgrade/readiness integrationを完了します。exact release scopeは [`v2/V2_060_RELEASE_SCOPE.ja.md`](v2/V2_060_RELEASE_SCOPE.ja.md) に記録します。#139/#217/#228 のremaining acceptanceは **`v0.6.1 — Support Claim Expansion`** に割り当て、older support contractを遡及拡大しません。

`0.4.0` release は、これまで旧 `0.4.0 Recovery & Reconciliation` と `0.5.0 Multi-principal Identity` に分けていた work を統合しました。accepted support boundary は release-scope / status docs に記録します。

### `0.4.0` released scope

`0.4.0` は released **Recovery, Identity & Semantic Authorization** baseline です。canonical released scope / support-claim matrix は [`v2/V2_040_RELEASE_SCOPE.ja.md`](v2/V2_040_RELEASE_SCOPE.ja.md) に固定します。recovery/reconciliation foundation、provider-neutral multi-principal identity plumbing、typed semantic authorization boundary をまとめつつ、optional platform/hosted support claim は compiled surface より狭く保ちます。

| `0.4.0` track | Issues / PR | Status | Release role |
| --- | --- | --- | --- |
| Core recovery semantics | #103, #137, #136 | Complete | Required baseline |
| Shared WebAuthn/CTAP verifier | #256 / PR #258 | Complete | Required shared recovery dependency。単独ではplatform support claimを意味しない |
| Recovery dogfood hardening | #253, #254, #115, #255 | Complete | Included compatibility/hardening evidence |
| Multi-principal OIDC/JWT identity | #139 / PR #269 | implementation + CI complete、physical signed-token dogfood pending | implementation は含める。provider-specific support claim は acceptance 待ち |
| Typed semantic authorization | #221 / PR #271 | Complete / merged / CI green | Included |
| Windows Hello recovery | #227 / PR #252 | implementation + CI green、physical Windows interactive-desktop acceptance PASS（2026-09-19） | `v0.4.0`でrelease済み・support acceptance済み |
| Linux FIDO2 UV recovery | #228 / PR #259 | implementation + CI complete、physical acceptance deferred | non-blocking support-claim gate。physical Linux + real UV-capable FIDO2 acceptanceまでLinux online recoveryはunsupported |
| Cross-platform recovery parity umbrella | #217 | physical acceptance dependent | 実際にsupport claimするplatform setのevidenceが揃うまでOPEN可 |
| Hosted Cloud Run Hub | #215 | design complete、implementation/acceptance pending | **`0.4.0` support claimではない**。future hosted-deployment track |
| Hosted Handoff composition | #275 / #276 / #277 | architecture documented、pin adoption / hosted operator-routing implementation pending | #215 の future hosted-deployment dependency。canonical Handoff authority は Agent に維持 |

release closeout は次の順で完了しました。

1. **#221 / PR #271をmerge/verify済み**。typed backend-neutral semantic constraint boundary、full regression/CI、EN/JA normative docsを完了。
2. standing [`PRODUCT_READINESS.ja.md`](PRODUCT_READINESS.ja.md) gateで **`0.4.0` release closeoutを完了済み**。version/durable-schema compatibility、source-free candidate artifact、clean install/upgrade/rollback、doctor/status、recovery/no-replay、dependency/CodeQL、docs、release noteを確認しました。
3. **#139 implementation は released historical baseline に含まれ、remaining signed-token dogfood は Post-v0.5 acceptance work。** provider-specific support claim は evidence 完了まで accepted と表現しません。
4. **#227 physical Windows acceptance は完了済み。** 2026-09-19 の trusted interactive-desktop run で cancel -> approve -> exact durable resolution -> restart/no-replay まで PASS しました。remaining #217/#228 parity/Linux evidence は `v0.5.0` より後ろで追跡し、`v0.4.0` を再オープンしません。
5. **#215 implementationをrelease gateへ引き込まない。** Cloud Run designはmain上の有用なevidenceだが、hosted Hub supportは別contractのdurable-state/fencing/ingress/acceptanceが実装されるまでNO-GOのままにする。

これはrelease scopeの統合であり、acceptanceの弱体化ではありません。artifactに実装が含まれても、**support claimをcompiled surfaceより狭くする**ことがあり、その境界はrelease note/statusで明示します。

### Legacy V1 retirement candidate

`v1_gateway` は regression/reference と、まだ存在する可能性のある legacy deployment のため `main` に保持します。独立した `0.1.x` maintenance line ではなく、routine backport もしません。推奨 runtime は V2 Hub + Agent です。

V1 retirement は今後の simplification candidate として妥当ですが、通常 maintenance のついでに削除してはいけません。削除前に次を満たします。

- supported production deployment が `v1_gateway` に依存していないことを確認する;
- V1 regression/conformance fixture のうち backend-contract test として価値が残るものを判断し、意図的に migrate または archive する;
- #14/#15 のような V1-only upstream-blocked issue は、surface retirement 時に no-longer-applicable として resolve/close する;
- V1 configuration/deployment documentation と compatibility claim を一貫して削除する;
- removal を pre-1.0 の incompatible public-contract change と分類し、適切な MINOR release と migration/release note を通してのみ ship する。

これらを満たすまでは V1 を narrow / regression-only に保ち、新しい capability は追加しません。

## Post-v0.6 の numbered release sequence

`v0.6.0` は released / frozen の **Managed Developer Execution** baseline です。remaining OPEN backlog は unnumbered future bucket のままにせず、明示的な numbered release boundary を割り当てます。これらのversionはscope boundaryでありcalendar promiseではありません。issueを別versionへ移す場合はroadmapとmilestoneを明示的に更新します。

1. **`v0.6.1 — Support Claim Expansion`** — #139 signed-token physical dogfood、#217 cross-platform recovery parity、#228 physical Linux FIDO2 UV acceptance。compatibility-preservingなsupport-claim patchに限定し、`v0.6.0`やolder releaseを遡及拡大しません。required physical evidenceが不足するclaimは、`v0.6.1`を切る前にissueを明示的に後続versionへ移します。
2. **`v0.7.0 — Operator Ergonomics & Recovery Evidence`** — #342 operation-ID guidance、#295 bounded `--version` identity、#304 unified read-only runtime status、#289 privacy-bounded ambiguous-target identity、#290 durable backend execution receipts、#347 verified single-Mac backup/restore。#289でreviewed target/evidence boundaryを定義し、#290がdurable receiptをauthoritative reconciliationへ利用し、#347がexact runtime/quarantine/replay/mutation-authority preservation と staged activation を閉じます。
3. **`v0.8.0 — Backend Portability Evidence`** — #222で既存CUMGのGUI authority / ambiguity / recovery semanticsを、materially differentなsecond real backendに対して実証します。generic provider / VM / fleet productには拡張しません。
4. **`v0.9.0 — Hosted Hub & Handoff`** — #215、#275、#276、#277、#282、#283、final acceptance #284。#276/#277でreviewed Handoff dependencyとhosted operator routeを固め、#282/#283はその後並行可能、#284をreplacement / partition / physical-Handoffのrequired final gateとします。ここを通るまでhosted supportはadvertiseしません。
5. **`v0.10.0 — Legacy V1 Retirement`** — `v1_gateway`をdeliberateにretireし、V1-only upstream-blocked #14/#15もresolve/closeします。supported deploymentがV1に依存しないこと、価値の残るregression fixtureを意図的にmigrate/archiveすることをremoval条件とします。

各patch/minorは引き続きstanding [`PRODUCT_READINESS.ja.md`](PRODUCT_READINESS.ja.md) gateを満たす必要があります。optional provider/platform supportはassigned releaseでrequired evidenceが揃うまでwithholdし、version割当を理由にquarantine/no-auto-replay semanticsを弱めません。

### v0.6.0 released working order

1. **#106 — managed long-running jobs を foundation とする。** separately authorized な job lifecycle、owner/device binding、bounded output、expiry/concurrency、proven stop semantics を先に定義します。nohup、setsid、generic shell escape で persistent work を復活させません。
2. **#114 — sandboxed Playwright/E2E は #106 の lifecycle が安定してから積む。** lifecycle/output/process-containment primitive は適切な範囲で再利用しますが、browser profile、filesystem、environment、artifact、network の制約は独立した sandbox boundary なので Playwright authority は分離したままにします。
3. **#267 — optional Linux cgroup-v2 containment は実装済みplatform hardening。** reviewed dedicated delegationがあるLinuxだけbounded process/shell descendant cleanupを強化し、Playwright sandboxの代替にはせず、macOS/portable-Unix claimも変更しません。
4. **#335 — final integration / acceptance gate で release を閉じる。** schema/capability/config change、artifact identity、v0.5.0 からの upgrade/rollback、physical dogfood、EN/JA docs、platform support claim、standing PRODUCT_READINESS.md checklist を統合確認してから v0.6.0 complete とします。

#106/#114/#267 のいずれかを evidence に基づいて defer する場合は、#335 と support boundary を明示的に更新します。milestone に残したまま、欠けた guarantee を含むように見える release を silent に ship しません。

| v0.6.0 track | Issue | Role |
| --- | --- | --- |
| Managed job lifecycle | #106 | Core foundation。最初の implementation dependency |
| Sandboxed Playwright/E2E | #114 | Dedicated developer-test capability。stable managed lifecycle に依存 |
| Linux cgroup-v2 containment | #267 | bounded process/shell cleanup向け実装済みoptional hardening。Linux-only reviewed delegation |
| Release integration / acceptance | #335 | schema、artifact、upgrade/rollback、dogfood、docs、support claim を閉じる required final gate |

`v0.6.0` freeze時点では Hosted Cloud Run Hub/Handoff (#215/#275-#284)、recovery evidence expansion (#289/#290)、second-real-backend semantic-neutrality proof (#222)、operator/consumer ergonomics (#295/#304) を意図的に除外していました。現在は上記のpost-v0.6 numbered releaseへ割当済みで、この記述はv0.6.0 release boundaryのhistorical recordとして維持します。

released v0.6.0 baseline は control schema 12、capability schema 8、registry schema 8、Hub-Agent schema 6 を pin します。released v0.5.0 は 10/6/8/6、historical #106 development は 11/7 でした。mixed live version は fail closed で、persisted v0.5 registry state は reviewed historical pairingだけをmigrationし、stale capability advertisementを捨ててからfresh v0.6 advertisementを要求します。workspace writable root は operator/device configuration、exact mutation capability は principal 単位です。

minor number は working release boundary であり calendar promise ではありません。implementation が artifact に含まれていても、optional platform/provider support claim は explicit acceptance まで保留します。

### 横断 Product Readiness track

初回 umbrella [#213](https://github.com/git-ksk/computer-use-mcp-gateway/issues/213) は完了済みです。future release preparation は恒久 [`PRODUCT_READINESS.ja.md`](PRODUCT_READINESS.ja.md) checklist を使い、gate が actionable gap を見つけた場合は narrower issue に分割します。

source-tree dogfood で capability が動くだけでは製品化完了とはしません。Post-v0.5 の各 milestone は次の product-level foundation を改善または維持します。

1. **Distribution / release integrity.** Source release は引き続き有効ですが、installable product path は最終的に reviewed な platform 別 artifact、deterministic checksum、provenance / attestation、SBOM と third-party license / notice inventory、適用可能な platform signing / notarization を提供します。release artifact に credential / private endpoint を含めません。source checkout だけでなく clean-machine artifact install smoke で user が受け取る実物を検証します。
2. **Install / upgrade / rollback.** 初回 install、Hub / Agent / maintenance / helper の coordinated upgrade、durable-state migration、rollback の supported path を明示します。version-paired component と checkpoint compatibility を明確にし、incompatible mixed version は silent rolling compatibility を試さず fail closed します。durable / wire state を変更する release は previous supported minor からの upgrade と safe rollback boundary を証明します。
3. **First-run / configuration UX.** supported platform ごとに明確な reference deployment を保ち、可能な範囲で effectful service start 前に configuration を validate します。missing / unsafe secret・trust anchor を actionable にし、`v2_doctor` / preflight 系チェックで新規 operator が internal state file を読まず configuration、permission、capacity、trust、backend failure を区別できるようにします。safe default は least-privilege / fail-closed のままです。
4. **Operational readiness.** service lifecycle、health/readiness、quarantine、recovery、TLS/key expiry、storage pressure、restart/drain、backup/restore、incident runbook を product behavior として扱います。operator signal は bounded / privacy-safe とし、raw desktop、command、credential、identity content を公開せず「operator action が必要」を検出できるようにします。
5. **Reliability / performance / resource budget.** deterministic regression、soak、concurrency、restart/reconnect、fault injection、capacity evidence を維持します。#111 で再現可能な informational latency / throughput distribution harness を確立し、future release でも safe operation に関係する CPU / RSS / disk / output / concurrency ceiling を明示します。workstation measurement は regression evidence であり production capacity の marketing claim にはしません。
6. **Security / privacy / supply chain.** threat model、exact capability authorization、no-auto-replay、secret isolation、key/certificate rotation、private vulnerability reporting、Dependency Review、CodeQL、content-minimizing telemetry を維持します。distribution automation が source/dependency provenance を弱めたり、signing infrastructure を runtime authority に変えたりしてはいけません。
7. **Compatibility / support / deprecation.** supported CUMG minor line、tested OS / Cua / backend / deployment matrix、schema mismatch behavior、migration / deprecation note を公開します。1.0 前は latest released minor のみ actively supported とし、compatibility claim は近い version だからではなく evidence に基づけます。
8. **Onboarding / documentation.** supported reference path では repository archaeology を要求せず、install → healthy diagnostics → 最初の read-only call → 明示 authorize した effectful call → documented recovery path まで進める状態を目指します。security / deployment / versioning / operator-critical behavior の EN/JA normative docs を同期します。

これらは横断 gate であり、hosted dashboard、account system、auto-updater、generic device fleet、remote-desktop product を作る約束ではありません。product feature は既存 CUMG boundary に収まる場合、または evidence 付きで boundary を deliberate に改訂する場合だけ admit します。

### `0.3.0` closeout: V2 Production Hardening

当初の `0.3.0` Production Hardening / Operational Readiness baseline は実装済みです。`#64`〜`#73` はすべて closed で、authoritative operation / quarantine / no-auto-replay model を弱めず、production recovery、shutdown、persistence、audit、local-abuse、trust-lifecycle、signing-authority の基盤を確立しました。

最後の明示的な `0.3.0` runtime release blocker だった **#100 — local-user-authorized online quarantine recovery** は complete です。trusted physical macOS の Secure Enclave/user-presence acceptance で実 ambiguous desktop operation を replay せず resolve し、authorization は user presence 後にだけ publish、quarantine は verified resolution 後にだけ clear、Hub restart 後も terminal resolution が維持され旧 operation は復活しないことを確認しました。

したがって `0.3.0` は、その後の dogfood で見つかったすべての issue を自動的には取り込みません。新しい issue が release blocker になるのは、既に約束した `0.3.0` safety/operability invariant を破る、または #100 acceptance を無効にする evidence がある場合だけです。それ以外は issue-driven な follow-up hardening として扱い、release scope を bounded に保ちながら fail-closed semantics を維持します。

完了済み baseline は次です。

- recovery / restart safety: `#64`、`#65`、`#72`;
- persistence / incident closure: `#69`、`#73`;
- audit / local caller protection: `#70`、`#71`;
- trust lifecycle / signing authority: `#68`、`#67`、`#66`。

stale release PR #99 は、その後 main に substantial work が merge される前の snapshot だったため、#100 acceptance 後の current `main` から作った fresh release snapshot で supersede します。

### First-class Human Handoff: integrated, now in dogfood hardening

physical CUMG + `mcp-execution-handoff` では、exact macOS Window に対する Agent -> Human -> verifying -> explicit Agent resume、direct/TURN fallback、fresh exact-window verification、restart/context-expiry/generation-rollover recovery、no-auto-replay、quarantine 0 まで受入済みです。acceptance-only Unix bridge は長期 runtime architecture にはしません。

Issue [#152](https://github.com/git-ksk/computer-use-mcp-gateway/issues/152) でこの integration と merged-main physical OS-window acceptance は完了しました。構成原則は **first-class だが optional** です。通常の CUMG capability は Handoff を必須としませんが、Handoff を有効化した deployment では authority decision を best-effort な外付け判定ではなく execution boundary の一部として扱います。操作対象 Agent が canonical Handoff FSM/checkpoint、WebRTC/TURN、capture、Human input、local verification を所有し、Hub は CUMG authorization / ledger / quarantine と conservative な pre-dispatch fence、signed operator-control relay のみを保持します。Hub/Agent に二重の Handoff state machine は作りません。generation rollover は fresh same-surface observation を伴う explicit `rebind_live` とし、Agent は Cua 直前に signed authority binding と実 command surface を再検証します。有効化後に runtime/transport が unavailable になった場合は coordinator を迂回せず fail closed します。


Hosted extension は [`v2/V2_HOSTED_HANDOFF_TOPOLOGY.ja.md`](v2/V2_HOSTED_HANDOFF_TOPOLOGY.ja.md) で定義します。Cloud/hosted routing は Human session の発行・routingを担当できますが、canonical physical mutation authority/checkpoint は controlled Agent に残し、CUMG authoritative operation/quarantine/replay state は Hub に残します。hosted routing state を2つ目のHandoff FSMやreplay authorityとして扱いません。

当初の依存順はcomponent migrationまで完了しました。

1. `#152` — first-class CUMG HandoffCoordinator / OS-window regression acceptance — **closed**
2. `mcp-execution-handoff#48` — bounded PTY semantic dogfood — **closed**
3. `mcp-execution-handoff#47` — reusable bounded OS/window primitive — **closed**
4. CUMG `#176` / `#177` — Window / Terminal runtime compositionをupstream `WindowHandoffAdapter` / `TerminalHandoffAdapter` へ移行 — **merged**
5. `#157` — legacy/current launchd coexistenceをfail closed — **closed**
6. `#168` — production cutover前にdependency-complete / import-provenなHandoff runtime packagingを保証 — **closed**

以前ここで参照していた upstream Handoff closeout（#45、#46、#85、#91）は完了し、その後の LocalAuthentication lifecycle fix #147/#149 も完了しています。CUMG は upstream first-class component を継続利用し、semantic を fork しません。`mcp-execution-handoff#150` のような残る upstream UX polish は独立した Handoff 課題であり、CUMG の safety / operability invariant を破る evidence がない限り CUMG release blocker にはしません。

### Production baseline 後の operational dogfood follow-up

当初の production-hardening baseline 完了後も、CUMG + Handoff の sustained dogfood で実際の failure/recovery path を継続して試しました。その結果、core authority model を変更せず追加の課題が見つかっています。これらは `0.3.0` gate を暗黙に拡張せず、stabilization queue として追跡します。

- execution/recovery semantics: `#179` partial input effect、`#180` quarantine-safe evidence lane、`#181` privacy-preserving evidence envelope、`#133` first-class reconciliation readiness audit、および `#115`/`#136`/`#137` recovery/retirement UX;
- Handoff/operator lifecycle: `#184` in-band Handoff begin の self-interference、`#185` explicit one-shot single-Mac maintenance job;
- diagnostics / host reliability: `#141` privacy-safe structured execution error、`#143` privacy-safe browser staging startup stage/I/O diagnostics、`#112` disk/temp exhaustion の fail-closed 診断・回復、`#194` `v2_doctor` self-observation;
- 各 issue は独立した severity、compatibility、test、acceptance boundary を維持する。follow-up は PATCH-compatible、将来 minor への admission、または defer のいずれもあり得るが、backlog を減らすために quarantine/no-replay semantics を弱めない。

この queue は初期 `0.3.0` gate 後も Handoff integration / physical dogfood を継続した結果です。historical follow-up は現在、`0.4.0` included、explicit support-claim gate、またはlater roadmap trackのいずれかへ整理済みです。

### 現在の open issue inventory

すべての OPEN issue は、下記 bucket または別の explicit roadmap section のどこかに現れる必要があります。この inventory は admission / ordering を表し、すべての OPEN issue が次 release に入ることを意味しません。

- **`v0.6.1 — Support Claim Expansion`:** #139 signed-token physical dogfood、#217 cross-platform recovery parity、#228 physical Linux FIDO2 UV acceptance。
- **`v0.7.0 — Operator Ergonomics & Recovery Evidence`:** #342 / #295 / #304 / #289 / #290 / #347 で planned scope は完了し、#347 merge 後の milestone OPEN issue は **0**。
- **`v0.8.0 — Backend Portability Evidence`:** #222 second-real-backend semantic-neutrality proof。
- **`v0.9.0 — Hosted Hub & Handoff`:** #215 Cloud Run support gate、#275 Agent-owned Handoff topology、#276 reviewed Handoff consumer adoption、#277 hosted operator routing、#282 closed one-port ingress、#283 durable Hub state / writer-epoch fencing、#284 final replacement / partition / physical-Handoff acceptance。
- **`v0.10.0 — Legacy V1 Retirement`:** #14/#15 はV1が存在する間はupstream-blockedのまま扱い、V1を拡張するのではなくdeliberateな`v1_gateway` retirementの一部としてresolve/closeします。

Hosted sequencing も明示します。#276/#277 が reviewed Handoff dependency、interface が安定すれば #282/#283 は並行可能、#284 が deployment acceptance を閉じます。#215 を NO-GO から変更するのはその evidence が揃った後だけです。

OPEN issue がこの inventory または別の explicit roadmap section に現れない場合は roadmap stale として、release closeout 前に修正します。

Cua authorization/product-boundary research #219 は [v2/V2_AUTHORIZATION_CAPABILITY_REVIEW.ja.md](v2/V2_AUTHORIZATION_CAPABILITY_REVIEW.ja.md) で完了し、admitした follow-up は #221/#222 です。

### `0.4.0` identity / semantic-authorization component

Issue [#139](https://github.com/git-ksk/computer-use-mcp-gateway/issues/139) の implementation history は integrated `0.4.0` baseline にあります。verified external identityを既存`AuthenticatedClientPrincipal`へ落とし込み、exact principal/device/capability authorizationは変更しません。ただし still-open の physical signed-token dogfood は unfinished `0.4.0` work ではなく、`v0.6.1 — Support Claim Expansion` milestone で追跡し、advertised support claim は v0.6 release以降でのみ有効化します。

```text
external OAuth/OIDC identity provider
        |
verified signed token (provider boundary)
        |
generic OIDC/JWT adapter
        |
AuthenticatedClientPrincipal { issuer, subject }
        |
DeviceCapabilityAuthorizer
        |
principal -> stable device -> exact DeviceCapability
```

adapterはprovider-neutral / fail closedのままです。signature、issuer、audience、time claim、subject、asymmetric algorithm policy、bounded JWKS cache/rotationを検証してから既存CUMG principalを生成します。caller-supplied identity headerとMCP `clientInfo`はaudit metadataでありauthorization authorityにはなりません。

Issue [#221](https://github.com/git-ksk/computer-use-mcp-gateway/issues/221) がcandidateの残るauthorization implementation gateです。finalized command boundaryへtyped/backend-neutral/narrow-only semantic constraintを追加し、CUMGをgeneric policy engineにはしません。exact capability authorization、grant signing、Handoff、recovery authority、quarantine、no-auto-replayは独立authorityのままです。

このintegrated releaseはCUMGをidentity provider、account database、session manager、generic policy engine、token issuerにはしません。RFC 7662 introspectionとexplicit single-principal trusted-proxyも引き続きdeployment choiceで、optional signed-token/platform support claimはacceptance-gatedです。

### Remaining semantic parity decisions

current Cua parity matrix では、次の legitimate gap を意図的に明示しています。

- `ClipboardWrite` は plain text のみ対応し、image/file clipboard write parity は未実装;
- `LaunchApplication` は Cua の `additional_arguments` と `webkit_inspector_port` を公開していない。

gap は、具体的な workflow need があり、generic backend passthrough を公開せず bounded backend-neutral contract を定義できる場合にのみ実装します。gap を explicit unsupported のまま維持する判断も有効です。

[`v2/V2_CUA_PARITY_MATRIX.ja.md`](v2/V2_CUA_PARITY_MATRIX.ja.md) を参照してください。

### Additional backend or native GUI adapter

Issue #222 が current P1 portability-evidence candidate を所有します。materially different な second real computer-use/native-GUI backend で small overlapping semantic slice を実証し、real side effect と ambiguity semantics を exercise します。compile-time compatibility や deterministic reference executor だけを cross-GUI-backend claim の evidence にはしません。

second real Computer Use backend または native GUI adapter は、具体的な operational、portability、support、security benefit がある場合にのみ candidate とします。

すべての adapter は同じ CUMG authority boundary の下に残さなければなりません。

- second operation lifecycle / settlement authority を作らない;
- backend-specific ID を permanent northbound capability identifier にしない;
- unsupported または post-dispatch outcome を prove できない場合は indeterminate のまま扱う;
- principal/device/capability/generation fencing を弱めない;
- automatic replay を追加しない。

real desktop side effect を起こせる backend では、compile-time interface compatibility だけを acceptance evidence としません。

### Pluggable external capability providers

CUMG 自身を agent 化したり upstream implementation を重複実装したりせず、有用な execution surface を追加できる場合、optional な external capability provider を candidate として検討できます。代表例は developer-workspace provider です。DevSpace-class provider は project/worktree context、repository instruction、file editing、patch、shell execution、Git-aware state などの Codex-like workspace primitive を提供でき、Serena-class provider は semantic code navigation や symbol-aware workspace intelligence を提供できます。どちらも reviewed CUMG adapter の背後に配置でき、CUMG core 自体へ取り込む必要はありません。

integration boundary は generic MCP proxy ではなく capability-oriented のまま維持します。

- planning、tool selection、project reasoning、multi-step agent loop は upstream chat/agent harness の責務のままとする;
- provider が higher-level tool を提供していても、CUMG 内部に second autonomous coding/operations agent loop を持ち込まない;
- provider tool は bounded input/result と read-only / state-changing classification を持つ explicit CUMG semantic capability に map する;
- provider-specific tool name、opaque authority、arbitrary passthrough を permanent northbound contract にしない;
- state-changing provider work は native capability と同じ authenticated principal、exact-capability grant、operation ownership、fencing、ambiguity、quarantine、cancellation、no-auto-replay rule の下に置く;
- external provider は CUMG core product boundary を再定義せず、置換または省略可能でなければならない。

これは extensibility direction であり、特定 provider の bundle を約束するものではありません。採用には具体的な workflow benefit と、adapter が CUMG execution-safety invariant を維持する evidence が必要です。

### Higher-risk capability surfaces

explicit filesystem mutation、richer clipboard data、application launch argument、その他 consequential surface は、それぞれ separate exact capability として検討できます。既存 shell、GUI、browser、backend integration から暗黙に authority を継承しません。

採用には reviewed threat boundary、bounded input/result、fail-closed authorization、ambiguity handling、test が必要で、behavior が real desktop/provider に依存する場合は physical acceptance も必要です。

### Replaceable infrastructure

transport、identity、policy、device-fabric、backend implementation は、具体的な benefit があり CUMG safety invariant を維持できる場合、maintained standards/OSS へ置換・統合できます。

architecture 上の流行だけを理由に infrastructure を採用しません。既存の reviewed implementation は、replacement evidence が migration cost/risk を上回るまで有効です。[`v2/V2_STANDARDIZATION.md`](v2/V2_STANDARDIZATION.md) を参照してください。

## Minor-release acceptance gate

future minor release を切る前に public-contract scope を明示し、applicable な gate をすべて pass する必要があります。

1. feature/compatibility boundary を implementation より前、または同じ change で document する;
2. [`PROJECT_GOVERNANCE.ja.md`](PROJECT_GOVERNANCE.ja.md) の change class を特定する;
3. security/execution-safety change は threat-model と targeted regression を更新する;
4. control/capability schema change は explicit、fail-closed で、upgrade/mismatch behavior を含む;
5. backend parity/status docs で implemented、unsupported、intentionally excluded behavior を正確に示す;
6. semantics を変更する場合、English canonical と paired Japanese normative docs を同期する;
7. deterministic CI を pass し、Class D change は trusted physical acceptance も pass する;
8. incompatible pre-1.0 change では migration/deprecation note を用意する;
9. final release は merged `main` から [`VERSIONING.ja.md`](VERSIONING.ja.md) の release process に従って準備する。

これらの boundary を bypass した prototype の成功だけでは release evidence としません。

## Path to `1.0.0`

`1.0.0` に target date や required feature count は設定しません。

1.0 の判断は compatibility commitment です。[`VERSIONING.ja.md`](VERSIONING.ja.md) の criteria が実運用でも成立した時点で readiness とします。特に:

- supported northbound semantic surface と execution-safety invariant を explicitly stable と指定している;
- control/capability schema の upgrade / mismatch behavior を document している;
- supported backend/deployment compatibility を document し、repeatable acceptance がある;
- governance、release、security、support、deprecation rule が書かれているだけでなく実際に運用されている;
- 少なくとも1つの documented install / upgrade / rollback path が ad-hoc な source-tree assembly ではなく reviewed release artifact を使い、release integrity / provenance を再現できる;
- first-run diagnostics、supported compatibility matrix、operational recovery、resource / health signal が十分明確で、通常 deployment が maintainer だけの repository 知識に依存しない;
- maintainer が 1.x 内の backward compatibility を維持する準備ができている。

remaining parity gap は **自動的には 1.0 blocker ではありません**。各 gap を supported、intentionally unsupported、deferred、deprecated のいずれかに分類し、stable boundary を user が理解できることを要求します。

`0.9.x` は countdown ではありません。compatibility commitment が正当化されるまで `0.10.0`、`0.11.0`、それ以降の pre-1.0 minor を利用できます。

## Explicit non-goals

product boundary を evidence とともに deliberate に再検討しない限り、次は NO-GO by default のままです。

- 新しい screenshot/input computer-use engine の構築;
- screen streaming または general remote-desktop product;
- 別の generic delegated-authorization protocol;
- maintained infrastructure が既に必要性を満たす場合の別 generic physical-device fabric/registry;
- multiple machine が技術的に可能という理由だけで fleet dashboard、broad discovery、failover、orchestration を作ること;
- arbitrary backend-tool passthrough、または raw backend identifier を public API にすること;
- parity shortcut として arbitrary browser JavaScript execution を公開すること;
- blanket long-lived device-control credential;
- ambiguous state-changing work の automatic replay;
- reconnect、heartbeat、backend restart、device liveness を unresolved operation が安全に忘れられる proof とみなすこと。

## Re-evaluation rule

roadmap candidate は implementation 前に、current standard、maintained OSS、backend capability、user workflow、accepted CUMG invariant と照合して review します。

maintained OSS が将来、同等以上の per-desktop operation ownership、fencing、durable indeterminate quarantine、explicit resolution、no-auto-replay semantics を提供する場合は、sunk cost を守るのではなく integration または retirement を再評価します。
