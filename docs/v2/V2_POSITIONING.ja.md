# V2 product positioning

> この日本語版は [`V2_POSITIONING.md`](V2_POSITIONING.md) の翻訳です。**英語版を canonical（正典）とし、解釈に差がある場合は英語版を優先します。**

Status: **V2 execution-safety baseline accepted / complete (2026-08-13)。Desktop、Browser core、Browser transfer は complete です。**

accepted P0 implementation/gap-analysis record は [`V2_P0_EXECUTION_SAFETY.md`](V2_P0_EXECUTION_SAFETY.md) です。最終 V2 core は、明示的で authoritative な operation ledger、owner/generation fencing、durable desktop quarantine、evidence-bearing receipt、explicit resolution、fixed-set multi-device invariant proof、backend portability、trusted real-Cua physical acceptance を備えています。

この文書は、V2-M1 acceptance と最終 competitor review 後の V2 product boundary を定義します。この boundary は、「secure remote computer use」「vendor-neutral device control plane」「multi-machine MCP」より意図的に狭く設定されています。

## Positioning

`computer-use-mcp-gateway` V2 は、**stateful interactive desktop の delegated control のための uncertainty-aware execution-safety layer** です。

短い product statement は次のとおりです。

> Authorization は agent が action を実行してよいかを決める。CUMG はさらに、desktop operation の owner が誰かを決め、side effect が不確実になったときに結果を推測しない。

CUMG は、単に次の性質を持つこと自体を差別化要因とは主張しません。

- vendor-neutral であること。
- delegated-authorization / capability-token system であること。
- physical-device / fleet control plane であること。
- computer-use engine や screenshot/input implementation であること。
- AI-native remote desktop product であること。
- generic MCP gateway であること。
- generic capability broker であること。
- device registry、reservation service、multi-machine router であること。

これらの領域には既に相当量の OSS と standards coverage があります。SINT Protocol や Arm Device Connect は、より広い「vendor-neutral physical-device execution/governance」カテゴリと実質的に重なり、OpenClaw、OAHL、QuickDesk、Obot、delegated-authorization project は隣接する別 layer をカバーします。

最終 review では、execution-safety の観点で特に関連性の高い2つの reference も確認しました。

- **ROSClaw** は embodied agent 向けに、exact permit、Agent Session、有限 deadline、exclusive resource/body lease、durable action transition、restart recovery、interrupted-real-action recovery、operator acknowledgement、generation disarming、旧 physical action の no replay/no resume という、非常に近い physical-execution contract を既に実装しています。
- **Agent libOS** は、provider execution より前に intent を persist し、side-effect boundary より前に authority を consume/reserve し、ambiguous failure 後も `unknown` effect を保持し、restart をまたいだ duplicate/replayed settlement を防ぐという、近い durable external-effect semantics を提供します。

したがって CUMG は、lease/recovery/no-replay や durable ambiguous-effect theory 自体を固有のものと主張してはいけません。擁護可能な boundary は、それらの safety semantics class を **interactive desktop 向けに specialization / integration** することです。つまり、shared desktop session 上の state-changing operation の execution outcome が曖昧になったときに、exclusive ownership と fail-closed recovery を提供することです。

## Core scenario

中核となる問題は次の sequence です。

```text
external principal
      |
      v
specific interactive desktop
      |
      v
exact capability
      |
      v
exclusive operation ownership + fencing
      |
      v
state-changing action
(click / type / drag / process / other effect)
      |
      v
cancel / timeout / disconnect / lost response
      |
      v
can non-execution or termination be proven?
      |
  +---+---+
  |       |
 yes      no
  |       |
terminal  indeterminate
          |
          v
      device quarantine
          |
          v
    explicit resolution
```

ambiguous operation は、client、Hub、Agent、transport、backend が再接続したという理由だけで automatic replay されることはありません。

## Thin-waist architecture

```text
IdP / MCP OAuth / OIDC / IAM / delegated-auth protocol
                    |
            authorization adapter
                    |
          +-------------------+
          |     CUMG CORE     |
          | operation ID      |
          | ownership / lease |
          | fencing / gen     |
          | replay barrier    |
          | indeterminate     |
          | quarantine        |
          | explicit resolve  |
          | no auto-replay    |
          +---------+---------+
                    |
              backend adapter
             /       |        \
           Cua     native     other
```

core より上と下の layer は交換可能です。generic authorization、device fabric、transport、fleet registry、execution backend は product-specific value ではありません。

## CUMG が所有するもの

### 1. Physical operation identity と ownership

state-changing desktop action には、必ず explicit operation identity と authoritative owner が必要です。

CUMG が所有するもの:

- explicit operation ID。
- per-device operation admission と exclusive ownership。
- generation/fencing check。
- conflict する desktop action の serialization。
- ownership を黙って transfer できない reconnect/restart rule。
- operation generation をもう所有していない stale Agent/session result の拒否。

### 2. Ambiguous side-effect state

cancellation request、disconnect、timeout、lost response が起きても、click、drag、keystroke、process、その他 state-changing effect が実行されなかったことの証明にはなりません。

non-execution または termination を証明できない場合、CUMG は success/failure を推測せず `indeterminate` outcome を persist しなければなりません。

`indeterminate` は通常の transport error ではなく、durable execution state です。

### 3. Fail-closed quarantine と explicit recovery

ambiguous な state-changing work について、CUMG は次を所有します。

- replay rejection。
- restart-safe な ambiguous in-flight state。
- device quarantine。
- affected desktop を reuse する前の explicit resolution。
- restart/reconnect をまたいだ ambiguous operation identifier の保持。
- reconnect、failover、client retry 後の no automatic replay。

resolution path は、liveness や新しい connection から安全性を推測するのではなく、安全判断を明示的にしなければなりません。

### 4. Exact execution boundary

external authorization は、ローカル実行に関する次の問いまで縮約されます。

```text
principal -> stable desktop -> exact DeviceCapability
```

CUMG は MCP Authorization/OAuth、OIDC、IAM-like system、SINT-style capability system、Grantex/Open Agent Auth-class protocol、その他 maintained authorization source を利用できます。authentication 自体は CUMG product logic ではありません。external identity system または reviewed authentication edge が caller identity を証明し、northbound adapter がそれを `AuthenticatedClientPrincipal { issuer, subject }` に縮約します。

CUMG authorization はその縮約後に始まります。`DeviceCapabilityAuthorizer` が答えるのは、認証済み principal が1つの stable desktop 上で1つの exact `DeviceCapability` を使用できるかどうかだけです。identity storage、password/session handling、token issuance、general-purpose account management は CUMG の外に置きます。そのため self-contained signed JWT で identity を確立するだけなら CUMG user database は不要で、authorization storage は別 concern です。

runtime は3つの明示的authentication adapterをサポートします。RFC 7662 OAuth introspection、multi-principal deployment向けprovider-neutral signed OIDC/JWT、意図的にsingle-principalとするdeployment向けtrusted authenticated-proxy fixed-principal adapterです。OIDC/JWTはoperator-configured issuer、exact audience、pinned HTTPS JWKS、asymmetric algorithm allowlist、time claim、stable subjectを検証して同じprincipal typeを生成し、provider-specific behaviorはadapterで終端します。trusted-proxy adapterはloopback originを要求し、principal identityはoperator configurationのみから取得し、client identity headerを信頼しません。

northbound credential は Agent credential ではなく、device-scoped execution grant の代わりに southbound へ転送してはいけません。custom value は新たな generic authorization protocol を発明することではなく、authorized intent を上記 operation-ownership state machine に bind することです。

### 5. Backend-neutral execution evidence

Cua は最初の GUI/computer-use backend であり、product boundary ではありません。

native platform adapter、OpenClaw-backed execution、その他 implementation は、core state machine を弱めずに operation を supported terminal/ambiguous outcome のいずれかへ map できるだけの evidence を提供できる場合に統合できます。

backend が clean termination を証明できるなら quarantine を回避できます。証明できない場合、CUMG は conservative なままです。

## Design reuse policy

CUMG は、**無関係な product scope を引き継がずに、実証済み execution-safety idea を reuse する**べきです。

### ROSClaw

ROSClaw は、デフォルトで fork する codebase ではなく、physical execution ownership と recovery semantics の主要 reference として扱います。

conceptual mapping は意図的に review されています。

| CUMG | ROSClaw analogue |
| --- | --- |
| desktop/device | Body/resource |
| `DeviceCapability` | Capability / exact Permit scope |
| operation ID | Action ID |
| operation owner / principal session | Agent Session / actor |
| desktop lease | Action/resource/body lease |
| generation fencing | daemon generation / DISARMED recovery boundary |
| replay barrier | idempotency + no old Action ID replay |
| `indeterminate` | interrupted REAL action with unknown outcome |
| quarantine | recovery-required / DISARMED physical boundary |
| explicit resolution | operator `acknowledge-recovery` |
| result evidence | `ExecutionReceipt` |

CUMG state machine を変更する前に proposed semantics を ROSClaw と比較し、interactive desktop に適合するなら、より強い behavior を採用します。

desktop body を追加するだけのために ROSClaw を長期 fork してはいけません。ROSClaw は robot/runtime/sandbox/memory/team など、CUMG の意図的に狭い scope 外の concern を持つ、より広い embodied Agent OS へ進化しています。将来 `DesktopBody` / adapter の compatibility experiment を行う場合は、その product surface を持ち込まず isolated にできることが条件です。

### Agent libOS

Agent libOS は durable ambiguous external-effect accounting の reference として扱います。effect boundary より前に intent を persist し、dispatch より前に authority を reserve/consume し、crash/restart をまたいで unknown effect を保持し、duplicate settlement から finalization を guard する考え方です。

CUMG は desktop operation state machine を強化する範囲でこれらの invariant を借用しつつ、未解決の ambiguous GUI operation が competing principal から shared interactive session 全体を quarantine できる、という desktop-level の追加要件を維持します。

## Keep / adapt / retire / reuse

### Keep: project-owned core

custom semantics は uncertainty-aware desktop execution safety を直接 encode するものだけ保持します。

- explicit operation identity。
- exclusive per-desktop operation ownership。
- ownership 維持に必要な lease/fencing/generation semantics。
- stale-result rejection。
- state-changing operation の replay barrier。
- durable `indeterminate` state。
- quarantine と explicit resolution。
- ambiguous state-changing work の no automatic replay。
- requested cancellation と proven non-execution / proven termination を区別する cancellation semantics。
- privacy-preserving operation/policy/outcome evidence。

### Adapt: replaceable に保つ

将来置換可能な既存 implementation の周囲は interface を維持します。

- principal/authentication adapter。
- grant issuer/verifier。
- Agent/workload identity provider/verifier。
- Hub-Agent transport binding。
- policy-engine integration。
- persistence/checkpoint store。
- device registry/fleet provider。
- backend adapter。

### Retire or replace

既に存在するという理由だけで custom infrastructure を維持しません。core invariant を弱めず同等 behavior を証明できる場合は、maintained standard / OSS を優先します。

候補:

- MCP Authorization / OAuth / OIDC。
- generic delegated-authorization protocol / capability-token system。
- generic physical-device discovery/registry/fabric layer。
- TLS / certificate lifecycle。
- 規模が正当化する場合の SPIFFE 等 workload identity。
- OpenTelemetry/OTLP。
- OS service supervision。
- generic policy engine。
- generic fleet-routing component。

replacement は、regression evidence により CUMG execution-safety invariant が維持または改善されることを示した後にのみ受け入れます。

2026-08-13 の P2 review と具体的 adoption decision は [`V2_P2_REPLACEMENT_SEAMS.md`](V2_P2_REPLACEMENT_SEAMS.md) に記録しています。ここで追加するのは narrow authorization seam と Computer Use backend seam であり、external dependency や新 control plane ではありません。

### Reuse externally

CUMG は overlapping surface を再実装するのではなく、maintained OSS を利用・統合する方針を取ります。

監視すべき主な project/category:

- **ROSClaw** — physical action ownership、exclusive lease、interrupted-action recovery、no-resume/no-replay semantics に最も近い reference。
- **Agent libOS** — durable unknown external effect と guarded post-crash settlement に最も近い reference。
- **SINT Protocol** — capability token、physical-AI governance、action identity/replay/evidence、edge authority。
- **Arm Device Connect** — vendor-neutral physical-device discovery、identity、registry、ACL、multi-tenant state、agent/device connectivity。
- **OpenClaw** — agent runtime、paired node、Computer Use execution。
- **OAHL** — hardware capability abstraction と device reservation。
- **QuickDesk** — remote Computer Use と multi-device/fleet UX。
- **Obot** — identity、MCP governance、workstation enrollment、audit。
- **Grantex / Open Agent Auth-class systems** — delegated authorization。

external component を CUMG uncertainty-aware execution core の外に置ける場合は integration を優先します。

## 2026-08-12 時点の competitive boundary

| Project/category | Strong overlap | Boundary CUMG should retain |
| --- | --- | --- |
| ROSClaw | exact permits、Agent Sessions、exclusive physical-resource lease、durable action ledger、restart recovery、unknown interrupted REAL action、operator recovery acknowledgement、no old-action replay | lightweight interactive-desktop specialization、shared GUI-session fencing、external desktop principal/auth integration、desktop/backend evidence semantics |
| Agent libOS | durable pending external-effect intent、effect 前の authority reservation、`unknown` ambiguous effect state、guarded finalization、crash recovery | shared interactive desktop/session の quarantine と competing-principal ownership semantics |
| SINT Protocol | capability token、physical execution governance、action claim、replay defense、revocation、edge enforcement、terminal evidence | interactive-desktop ambiguous side-effect state、persistent quarantine、explicit safe reuse resolution |
| Arm Device Connect | vendor-neutral device fabric、identity、registry、ACL、distributed state、multi-tenant agent/device invocation | generic fleet/device connectivity ではなく desktop operation ownership と uncertainty state machine |
| OpenClaw | paired node、multi-node control、Computer Use、command/capability policy、cancellation | external-principal binding と conservative ambiguous desktop-operation recovery |
| OAHL | hardware capability、device policy、exclusive reservation | restart/reconnect-safe ownership、stale-result fencing、ambiguous-execution quarantine semantics |
| QuickDesk | remote Computer Use、MCP、multi-device/fleet | remote-desktop transport/UX ではなく execution safety |
| Obot | identity、MCP governance、device enrollment、audit | physical desktop operation ownership と side-effect ambiguity handling |
| delegated-auth protocols | scope、expiry、revocation、agent identity | authorized intent を desktop operation state machine に bind すること |

これらの neighbor は今後改善される前提で考えます。したがって differentiation は category wording ではなく executable invariant と test で守るべきです。

## Core-first implementation priority

今後の implementation order は意図的に **core-first** です。

### Priority 0 — reference-model gap analysis

**P0 hardening pass では完了済みです。[`V2_P0_EXECUTION_SAFETY.md`](V2_P0_EXECUTION_SAFETY.md) を参照してください。** 今後も、state-machine change の前には ROSClaw の action/session/permit/lease/recovery/receipt model と Agent libOS の external-effect persistence/finalization model を one-to-one で比較する、という reviewed rule を維持します。

- CUMG が既に match している semantics を特定する。
- desktop に適合する、より強い proven semantics を採用する。
- accidental divergence ではなく desktop-specific deviation を明示的に document する。
- unrelated embodied-Agent/runtime scope を fork/import しない。
- 将来の compatibility/adapter experiment を可能にしておく。

### Priority 1 — operation state machine を harden

- operation ownership transition を explicit にし、網羅的に test する。
- terminal `completed`、`failed`、proven-cancelled outcome に十分な evidence を定義する。
- uncertain transition は transport-shaped error ではなく durable `indeterminate` に収束させる。
- ownership generation change 後の late/stale result を拒否する。

### Priority 2 — quarantine と resolution を first-class にする

- connection/session lifetime とは独立して quarantine を persist する。
- explicit かつ auditable な resolution path を公開する。
- resolver が解決対象の ambiguous operation を明示することを要求する。
- resolution が旧 operation の replay を誤って authorize しないことを保証する。
- quarantine 中と resolution 中の restart/crash を test する。

### Priority 3 — reconnect/restart/concurrency 下の ownership を証明

- competing principal が in-flight/ambiguous operation を steal/inherit できない。
- reconnect が old work の new owner を作らない。
- Hub/Agent restart が必要な fencing と ambiguity state を保持する。
- stale Agent generation が old operation を finalize できない。

### Priority 4 — fleet product ではなく multi-device proof

上記 state machine を十分強くした後にのみ、次を証明します。

1. ambiguous action 後も Device A を quarantined に保てる。
2. Device B は別の authorized principal から独立して利用できる。
3. explicit resolution まで second principal は Device A を取得できない。
4. Hub restart が両 device の独立 state を保持する。
5. reconnect/failover が Device A の ambiguous action を replay しない。

これらの invariant が通る前に fleet UX、broad discovery、dashboard、orchestration を優先しません。

### Priority 5 — backend portability proof

core が偶然 Cua-specific になっていないことを、cancellation/result behavior が実質的に異なる second execution backend または deterministic reference backend を少なくとも1つ統合して証明します。

backend が CUMG state machine に adapt するのであり、backend に合わせて CUMG state machine を弱めてはいけません。

## 将来 subsystem の decision rule

新 subsystem を実装する前に、次の順で確認します。

1. desktop operation ownership、ambiguity handling、quarantine、explicit resolution、no-replay safety を直接強化するか。
2. yes なら core-priority work。
3. no なら、その concern は maintained standard/platform/OSS が既に所有しているか。
4. yes なら parallel implementation を作らず integrate / replace する。
5. external solution が合わないなら、custom semantics を必要とする exact execution-safety property を document し、その custom surface を narrow、backend-neutral、transport-neutral に保つ。

## GO / NO-GO rule

**GO:** interactive desktop の delegated control に対する uncertainty-aware execution safety を改善し、証明する。

**NO-GO by default:** 技術的に可能というだけで、general agent authorization protocol、general physical-device fabric、generic fleet manager、remote-desktop product、multi-machine router を構築する。

将来 maintained OSS が、同等の per-desktop operation ownership、fencing、durable `indeterminate` quarantine、explicit resolution、no-auto-replay semantics を提供するようになった場合は、sunk cost を守るのではなく integration / retirement を再評価します。

### P1 proof status — fixed-set composition only

P1 proof は P0 operation state machine を変更せず、Priority 4 と 5 に必要な最小 composition を実装します。`FixedMultiDeviceHub` は、explicitly provisioned stable device ID から既存 `SingleDeviceHub` service/handle への immutable map であり、device ごとに別 checkpoint directory を持ちます。device discovery、fleet registry、shared scheduler、product routing plane ではありません。

proof では、1つの principal のもとで Device A が durable quarantine のままでも、別 principal の Device B は native shell work を継続できること、A の reconnect が A の generation だけを進めること、A の partition が B を block しないこと、Hub reconstruction が2つの checkpoint を独立して復元すること、old ambiguous A operation が automatic replay されないことを示します。stale/wrong-owner settlement は unchanged P0 fence が拒否します。

backend portability は、cancellation/result contract が Cua と実質的に異なる deterministic process-like reference executor で証明します。proven not-started / clean termination は既存 terminal evidence class に map でき、unprovable post-commit outcome は `indeterminate` に map します。両 backend で同じ operation identity、owner、generation、quarantine、explicit resolution、receipt、no-replay core を使用します。

P1 physical acceptance は 2026-08-13 に trusted `main` commit `bb39390f3587902a7df918fe1ff4a8b28c328d50`、Desktop E2E run `31675515516` で完了しました。real-Cua lane は ambiguity -> durable quarantine -> replay なしの Hub/Agent restart と generation advance -> exact explicit resolution -> safe reuse を証明しました。workflow は manual、`main`-only のままで、ephemeral な TCC-granted macOS runner 上で実行されました。これにより P1 を fleet/backend product work に広げることなく P1 residual を close しています。
