# ロードマップ

> この日本語版は [`ROADMAP.md`](ROADMAP.md) の翻訳です。**英語版を canonical（正典）とし、解釈に差がある場合は英語版を優先します。**

2026-08-21 時点の status: **V1 closed、V2 execution-safety baseline complete、current released version は `v0.2.0` です。**

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

## Current maintenance line: `0.2.x`

`0.2.x` は released V2 public direction を維持します。その contract と互換な work は、新しい milestone number を作るのではなく patch line に残します。

現在の priority:

- authoritative operation / quarantine / resolution / no-auto-replay state machine を維持する;
- control schema v8 と capability-advertisement schema v5 の behavior を明示した状態で維持する。v5 は signed / payload-free reconciliation-report boundary を追加する reviewed change で、mixed version は fail closed する;
- Cua Driver upgrade を pinned / repeatable evidence を伴う reviewed compatibility change として扱う;
- security、dependency、documentation、packaging、CI、conformance、soak、resource-regression quality を維持する;
- 残っている V1 compatibility / quality issue を調査し、close または明示的に document する:
  - issue #14 — read-only `get_screen_size` の session / escalation semantics;
  - issue #15 — Cua Driver の application/process discovery identity の不整合;
  - issue #20 — Linux enforcement を弱めずに V1 idle resource quality gate の portable behavior を定義する;
- compatible な runtime/security/reliability defect は PATCH candidate として修正する;
- docs-only/editorial work は、immutable な corrected release snapshot が operationally 必要な場合を除き version-neutral とする。

`v0.2.0` 後に merge された compatible fix は将来の `0.2.1` に含められますが、maintenance commit が存在するだけで release を必須にはしません。

## Next minor: admission-driven, not number-driven

次の minor release は、accepted work が public contract を変更・有意に拡張する場合、または deliberate な pre-1.0 incompatibility が正当化される場合にのみ作成します。現時点なら通常は `0.3.0` ですが、この roadmap はその number に feature bundle を事前固定しません。

candidate area はそれぞれ独立に評価します。

### `0.3.0` candidate: V2 Production Hardening

現在の `0.3.0` candidate theme は **V2 Production Hardening / Operational Readiness** とします。継続的な V2 実運用で見つかった reliability、recoverability、observability、local-abuse、trust-lifecycle の gap を、authoritative operation / quarantine / no-auto-replay safety model を弱めずに close することが目的です。

target issue set は `#64` から `#73` です。

- recovery / restart safety: `#64` production quarantine resolution、`#65` SIGTERM + bounded operation drain、`#72` operator-visible quarantine alerting;
- persistence / incident closure: `#69` generation 内 checkpoint growth の bounded 化、`#73` persistence crash-loop root cause の確定または evidence-backed な除外;
- audit / local caller protection: `#70` northbound client correlation、`#71` loopback caller rate limiting / trust gate;
- trust lifecycle: `#68` bounded Agent session lifetime と device-key rotation procedure、`#67` repeatable enrollment / trust-anchor lifecycle、`#66` grant-signing isolation / external signer boundary。

implementation は **issue-driven / PR-isolated** のまま、dependency を考慮して次の順を優先します。

1. `#65` planned-shutdown safety;
2. `#69` / `#73` persistence boundedness と incident root cause;
3. `#64` audited production quarantine resolution;
4. `#72` / `#70` / `#71` operator visibility、audit correlation、local abuse resistance;
5. `#68` / `#67` session / device / trust-anchor lifecycle;
6. `#66` signing-authority isolation。lifecycle boundary を明確にした後で扱う。

単独なら PATCH-compatible な change（例: `#65`、`#69`）でも、途中で `0.2.x` release を切る operational need がなければ `0.3.0` に初めて含めてよいものとします。SemVer classification は release 全体で判断し、含まれるすべての fix に minor bump が必要だとは解釈しません。一方で milestone を単一の巨大 implementation PR にはせず、各 issue は独立した acceptance evidence と review boundary を維持します。

すべての issue が close しただけでは `0.3.0` accepted とはしません。release 前に、結果として得られる public / operator contract が後述の minor-release acceptance gate を満たす必要があります。documented V2 safety invariant を維持できない scope は、milestone 達成のために invariant を弱めるのではなく defer します。

### post-acceptance candidate: first-class Human Handoff coordination

physical CUMG + `mcp-execution-handoff` では、exact macOS Window に対する Agent -> Human -> verifying -> explicit Agent resume、direct/TURN fallback、fresh exact-window verification、restart/context-expiry/generation-rollover recovery、no-auto-replay、quarantine 0 まで受入済みです。acceptance-only Unix bridge は長期 runtime architecture にはしません。

Issue [#152](https://github.com/git-ksk/computer-use-mcp-gateway/issues/152) でこの integration と merged-main physical OS-window acceptance は完了しました。構成原則は **first-class だが optional** です。通常の CUMG capability は Handoff を必須としませんが、Handoff を有効化した deployment では authority decision を best-effort な外付け判定ではなく execution boundary の一部として扱います。操作対象 Agent が canonical Handoff FSM/checkpoint、WebRTC/TURN、capture、Human input、local verification を所有し、Hub は CUMG authorization / ledger / quarantine と conservative な pre-dispatch fence、signed operator-control relay のみを保持します。Hub/Agent に二重の Handoff state machine は作りません。generation rollover は fresh same-surface observation を伴う explicit `rebind_live` とし、Agent は Cua 直前に signed authority binding と実 command surface を再検証します。有効化後に runtime/transport が unavailable になった場合は coordinator を迂回せず fail closed します。

当初の依存順はcomponent migrationまで完了しました。

1. `#152` — first-class CUMG HandoffCoordinator / OS-window regression acceptance — **closed**
2. `mcp-execution-handoff#48` — bounded PTY semantic dogfood — **closed**
3. `mcp-execution-handoff#47` — reusable bounded OS/window primitive — **closed**
4. CUMG `#176` / `#177` — Window / Terminal runtime compositionをupstream `WindowHandoffAdapter` / `TerminalHandoffAdapter` へ移行 — **merged**
5. `#157` — legacy/current launchd coexistenceをfail closed — **closed**
6. `#168` — production cutover前にdependency-complete / import-provenなHandoff runtime packagingを保証 — **closed**

残るcloseoutは意図的にCUMG authority外のupstream課題です。`mcp-execution-handoff#85` はfirst-class Windowのsame-LAN direct physical rerun、#91はTerminal mobile connection/status表示、#46/#45は最終Target Surface terminology/API収束を追跡します。CUMGはupstream naming decisionを先取りせずfirst-class componentをconsumeし続けます。WebRTC qualityは独立したHandoff課題です。

### `0.3.0` 後の candidate: multi-principal northbound identity

Issue [#139](https://github.com/git-ksk/computer-use-mcp-gateway/issues/139) は `0.3.0` Production Hardening closeout には**意図的に含めません**。operational-readiness work を close した後に扱う、次の northbound authentication expansion candidate とします。追跡 milestone は `Post-v0.3 — Multi-principal Northbound Identity` とし、この milestone 自体では release number を事前固定しません。

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
