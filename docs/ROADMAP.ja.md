# ロードマップ

> この日本語版は [`ROADMAP.md`](ROADMAP.md) の翻訳です。**英語版を canonical（正典）とし、解釈に差がある場合は英語版を優先します。**

2026-08-27 時点の status: **V1 implementation は closed で legacy/regression surface としてのみ保持し、推奨 runtime は V2、current released version は `v0.3.0` です。**

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

## Current maintenance line: `0.3.x`

`0.3.x` は released V2 Production Hardening / Operational Readiness direction を維持します。その contract と互換な work は、新しい milestone number を作るのではなく patch line に残します。

現在の priority:

- authoritative operation / quarantine / resolution / no-auto-replay state machine を維持する;
- live control schema v9 と capability-advertisement schema v5 の behavior を明示した状態で維持する。capability schema v5 は signed / payload-free reconciliation-report boundary を追加する reviewed change で、mixed version は fail closed する;
- Cua Driver upgrade を pinned / repeatable evidence を伴う reviewed compatibility change として扱う;
- security、dependency、documentation、packaging、CI、conformance、soak、resource-regression quality を維持する;
- V1 固有で残る compatibility observation（#14 / #15）は active CUMG release blocker とせず、対応する upstream Cua issue に blocked された状態を明示する;
- compatible な runtime/security/reliability defect は PATCH candidate として修正する;
- docs-only/editorial work は、immutable な corrected release snapshot が operationally 必要な場合を除き version-neutral とする。

`v0.3.0` 後に merge された compatible fix は将来の `0.3.1` に含められますが、maintenance commit が存在するだけで release を必須にはしません。

### 直近の Product Readiness 実行順

現在は、**追加 desktop platform の recovery parity より先に single-Mac の operator/product path を完成させる**ことを優先します。cross-platform recovery は重要ですが、新しい platform-backed recovery provider を追加しても、すでに supported な deployment に残る最大の usability gap は解消しません。

`0.3.x` Product Readiness closeout は完了済みです。以下の表は completed gate と、明示的に non-blocking な stabilization / future work を記録します。

| Closeout track | Issues | Status | 現在の `0.3.x` closeout を block? |
| --- | --- | --- | --- |
| Operator/recovery foundation | #226, #233, #234, #235, #109 | Complete | No |
| Guided quarantine recovery | #236 | Complete | No |
| Artifact-backed install/upgrade | #237 | Complete | No — closeout evidence 実装済み |
| Cross-cutting Product Readiness closeout | #213 | Complete | No — standing gate は [`PRODUCT_READINESS.ja.md`](PRODUCT_READINESS.ja.md) |
| Compatible stabilization backlog | #215, #115, #104, #111, bounded #96 work | Parallel/deferred | released regression evidence が出ない限り No |
| Cross-platform recovery parity | #217, #227, #228 | `0.4.0` planned | No |

released safety regression を示す新しい evidence がない限り、次の順序を使います。

1. **operator/recovery workflow 完了:** #226 lane-scoped readiness、#233 operator-ready incident brief、#234 durable / inspectable single-Mac upgrade transaction、#235 unified operator status、#109 exact durable online-recovery completion confirmation、#236 guided quarantine recovery は完了済みです。
2. **product workflow を完了:** #237 は exact CUMG/Handoff pairing、fail-closed verification、one-shot upgrade、paired rollback、installed doctor/status verification を備えた reviewed single-Mac source-free artifact install/upgrade path を提供します。
3. **横断 gate 完了:** #213 で single-Mac product path を end-to-end review し、恒久 per-release gate として [`PRODUCT_READINESS.ja.md`](PRODUCT_READINESS.ja.md) を確立しました。compatible stabilization backlog は、その evidence が release invariant を壊さない限り non-blocking のままです。
4. **Recovery & Reconciliation を dependency 順に拡張:** #103 durable effectful operation identity/status と #137 reviewed local-Human current-state acceptance は完了済みです。次に #136 の permanent replay tombstone / bounded detailed retirement history 分離、その後 #217/#227/#228 の cross-platform user-presence recovery parity を進めます。

これは Windows/Linux recovery の de-scope ではなく、実行順の決定です。`0.3.x` Product Readiness path は完了済みです。途中まで実装済みの #227 work は保持しますが、#103/#137/#136 の immediate dependency ではありません。platform-provider implementation は dedicated parity slot で再開し、concrete production evidence により Windows/Linux online recovery がより高い safety/operability priority になった場合だけ前倒しします。

### Legacy V1 retirement candidate

`v1_gateway` は regression/reference と、まだ存在する可能性のある legacy deployment のため `main` に保持します。独立した `0.1.x` maintenance line ではなく、routine backport もしません。推奨 runtime は V2 Hub + Agent です。

V1 retirement は今後の simplification candidate として妥当ですが、通常 maintenance のついでに削除してはいけません。削除前に次を満たします。

- supported production deployment が `v1_gateway` に依存していないことを確認する;
- V1 regression/conformance fixture のうち backend-contract test として価値が残るものを判断し、意図的に migrate または archive する;
- #14/#15 のような V1-only upstream-blocked issue は、surface retirement 時に no-longer-applicable として resolve/close する;
- V1 configuration/deployment documentation と compatibility claim を一貫して削除する;
- removal を pre-1.0 の incompatible public-contract change と分類し、適切な MINOR release と migration/release note を通してのみ ship する。

これらを満たすまでは V1 を narrow / regression-only に保ち、新しい capability は追加しません。

## Post-v0.3 の製品化シーケンス

CUMG は初期 V2 production-hardening release を終えました。Post-v0.3 では execution-safety boundary を弱めず、security-focused な source release から install / operate しやすい product へ段階的に進めます。以下の minor number は現在の作業順であり日程の約束ではありません。minor release は admitted public-contract scope と evidence が揃った場合だけ切ります。

現在の作業順:

- **`0.3.x` — Product Readiness & Stabilization:** #237 と #213 cross-cutting closeout は完了済みです。future release は恒久 [`PRODUCT_READINESS.ja.md`](PRODUCT_READINESS.ja.md) gate を再利用します。#226/#233/#234/#235/#109/#236 の operator/recovery foundation と #224 release-candidate artifact boundary は完了済みです。#215、#115、#104、#111、#96 の bounded な調査/documentation は compatible non-blocking stabilization として並行可能で、milestone を無制限な backlog-clearing exercise にはしません。
- **`0.4.0` — Recovery & Reconciliation:** #103 / #137 は完了済みです。effectful Desktop/Browser work は caller-retained durable operation identity/status を持ち、reviewed `Scroll` / `MovePointer` ambiguity は exact local-user-presence authorization 後だけ `current_state_accepted` として retire できます。historical outcome は `Indeterminate` のまま、old ID は permanent non-replayable のままです。次に #136 の permanent replay tombstone / bounded detailed retirement history 分離、その後 #217/#227/#228 の cross-platform user-presence recovery parity を進めます。
- **`0.5.0` — Multi-principal Identity:** #139 で provider-neutral OIDC/JWT caller identity を追加し、既存の exact `principal -> device -> capability` authorizer と single-principal / introspection adapter を維持。
- **`0.6.0` — Least-privilege Workspace:** #83 の bounded retrievable output、#105 の ranged / deterministic filesystem observation、#107 の明示的 writable root 下での atomic workspace mutation により Dangerous shell authority への依存を減らす。
- **`0.7.0` — Managed Developer Execution:** #106 の explicitly managed long-running job と #114 の separately sandboxed Playwright/E2E execution を追加。background-shell escape compatibility ではなく #96 の Unix containment 調査を前提にする。

implementation evidence により area の combine / split / defer、または compatible patch 扱いが妥当と分かった場合は release number を動かしてよいものとします。number より dependency / safety boundary を優先します。

### 横断 Product Readiness track

初回 umbrella [#213](https://github.com/git-ksk/computer-use-mcp-gateway/issues/213) は完了済みです。future release preparation は恒久 [`PRODUCT_READINESS.ja.md`](PRODUCT_READINESS.ja.md) checklist を使い、gate が actionable gap を見つけた場合は narrower issue に分割します。

source-tree dogfood で capability が動くだけでは製品化完了とはしません。Post-v0.3 の各 milestone は次の product-level foundation を改善または維持します。

1. **Distribution / release integrity.** Source release は引き続き有効ですが、installable product path は最終的に reviewed な platform 別 artifact、deterministic checksum、provenance / attestation、SBOM と third-party license / notice inventory、適用可能な platform signing / notarization を提供します。release artifact に credential / private endpoint を含めません。source checkout だけでなく clean-machine artifact install smoke で user が受け取る実物を検証します。
2. **Install / upgrade / rollback.** 初回 install、Hub / Agent / maintenance / helper の coordinated upgrade、durable-state migration、rollback の supported path を明示します。version-paired component と checkpoint compatibility を明確にし、incompatible mixed version は silent rolling compatibility を試さず fail closed します。durable / wire state を変更する release は previous supported minor からの upgrade と safe rollback boundary を証明します。
3. **First-run / configuration UX.** supported platform ごとに明確な reference deployment を保ち、可能な範囲で effectful service start 前に configuration を validate します。missing / unsafe secret・trust anchor を actionable にし、`v2_doctor` / preflight 系チェックで新規 operator が internal state file を読まず configuration、permission、capacity、trust、backend failure を区別できるようにします。safe default は least-privilege / fail-closed のままです。
4. **Operational readiness.** service lifecycle、health/readiness、quarantine、recovery、TLS/key expiry、storage pressure、restart/drain、backup/restore、incident runbook を product behavior として扱います。operator signal は bounded / privacy-safe とし、raw desktop、command、credential、identity content を公開せず「operator action が必要」を検出できるようにします。
5. **Reliability / performance / resource budget.** deterministic regression、soak、concurrency、restart/reconnect、fault injection、capacity evidence を維持します。#111 で再現可能な latency / throughput distribution を確立し、future release でも safe operation に関係する CPU / RSS / disk / output / concurrency ceiling を明示します。workstation measurement は regression evidence であり production capacity の marketing claim にはしません。
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

この queue は、#100 が既知の `0.3.0` blocker だった期間にも Handoff integration / physical dogfood を進めた結果です。各 follow-up の evidence が released invariant を無効化しない限り、completed v0.3.0 runtime gate の外で追跡します。

### 現在の open issue inventory

repository の open issue は、work が roadmap の可視性から黙って抜け落ちないよう working milestone ごとに分類します。これらの milestone は ordering / admission guidance であり、evidence により split / defer が必要になった場合まで同時 ship を約束するものではありません。

- **`0.3.x — Product Readiness & Stabilization`:** #213 final cross-cutting Product Readiness gate は完了済みで、今後は [`PRODUCT_READINESS.ja.md`](PRODUCT_READINESS.ja.md) が standing gate を引き継ぎます。#226/#233/#234/#235/#109/#236 の operator/recovery foundation は完了済みで、#224 も completed release-candidate artifact boundary を提供します。artifact-backed install/upgrade #237 と #213 cross-cutting closeout は完了済みで、blocking `0.3.x` implementation/gate は残っていません。その他の compatible work（#215、#115、#104、#111、bounded #96）は、released safety/reliability invariant を壊す evidence が出ない限り closeout non-blocking です。
- **`0.4.0 — Recovery & Reconciliation`:** #103 / #137 は完了済みです。effectful Desktop/Browser call は caller-retained durable status recovery を持ち、reviewed low-impact `Scroll` / `MovePointer` ambiguity は historical completion を synthesize せず replay も許可しない local-user-presence `current_state_accepted` を利用できます。次は #136 permanent replay tombstone / bounded detailed retirement history、最後に #217 cross-platform user-presence recovery parity（#227 Windows Hello/WebAuthn / #228 Linux FIDO2 UV）です。provider work は保持しつつ、production evidence が priority を変えない限り shared recovery semantics の後に進めます。
- **`0.5.0 — Multi-principal Identity`:** #139 が既存 exact authorizer を維持した provider-neutral OIDC/JWT identity、#221 が CUMG を generic policy engine にせず narrow-only typed backend-neutral semantic constraints を追加します。
- **`0.6.0 — Least-privilege Workspace`:** #83 が bounded retrievable process/shell output、#105 が ranged / deterministic filesystem observation、#107 が unrestricted shell authority を継承しない bounded atomic workspace mutation を追加します。
- **`0.7.0 — Managed Developer Execution`:** #106 が explicit managed-job lifecycle、#114 が separately sandboxed Playwright/E2E execution を追加します。
- **Upstream-blocked V1 compatibility:** #14（`get_screen_size` session/escalation）と #15（`list_apps` live-process discovery mismatch）は upstream Cua 待ちのまま、active post-v0.3 milestone を意図的に付けません。V1 を deliberate に retire する場合は no-longer-applicable になる可能性があります。

open issue がこの inventory または他の明示的 roadmap section に存在しない場合、roadmap は stale とみなし、milestone / release closeout を宣言する前に修正します。

Cua authorization / product-boundary research #219 は [`v2/V2_AUTHORIZATION_CAPABILITY_REVIEW.ja.md`](v2/V2_AUTHORIZATION_CAPABILITY_REVIEW.ja.md) で完了し、admit した follow-up は #221 と #222 です。

### `0.5.0` candidate: multi-principal northbound identity

Issue [#139](https://github.com/git-ksk/computer-use-mcp-gateway/issues/139) は `0.3.x` stabilization / `0.4.0` Recovery & Reconciliation contract には**意図的に含めません**。distinct authenticated principal が必要な deployment 向けの provider-neutral signed-token path として、現在の作業順では `0.5.0` northbound-authentication expansion に置きます。正確なrelease numberは上記admission/evidence ruleに従います。

target architecture は次です。

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

adapter は provider-neutral / fail-closed を維持します。signature、issuer、audience、time claim、subject、algorithm policy、bounded な JWKS/metadata rotation を検証した後にだけ既存 CUMG principal を生成します。caller-supplied identity header と MCP `clientInfo` は audit metadata のままで、authorization authority にはしません。

この work により CUMG を identity provider、account database、session manager、token issuer にはしません。既存 RFC 7662 introspection と、明示的に single-principal とする trusted-proxy adapter は引き続き deployment option として維持します。signed-token deployment では、fixed-principal local proxy bridge が identity 確立だけのために存在する場合は取り除けます。一方、reverse proxy / tunnel は transport、origin hardening、rate limiting、defense in depth のために残して構いません。

acceptance では少なくとも2つの verified subject が既存 `DeviceCapabilityAuthorizer` を通じて異なる exact device/capability decision を受けることを証明し、bad signature / issuer / audience / time / key / subject / algorithm は fail closed、既存 authentication adapter に regression がないことを要求します。

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
