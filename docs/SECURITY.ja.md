# セキュリティ

> この日本語版は [`SECURITY.md`](SECURITY.md) の翻訳です。**英語版を canonical（正典）とし、解釈に差がある場合は英語版を優先します。**

computer-use は client に sensitive desktop capability への access を与えます。この gateway は単なる transport adapter ではなく、security boundary として扱ってください。

## V1 のデフォルト

- deliberately reviewed な別 bind を使う場合を除き、`127.0.0.1` のみで listen する。
- remote access の前に authenticated TLS termination を要求する。
- backend を直接公開せず stdio 上に置く。
- inbound MCP Host authority と browser Origin value を検証する。
- gateway allowlist が empty の場合は全 tool を deny する。
- discovered backend tool をすべて公開するには explicit `CUMG_ALLOW_TOOLS=*` を要求する。
- call forwarding より前に deny rule を適用する。
- 1つの physical desktop に対する operation を serialize する。
- bounded connection/tool timeout と reconnect backoff を使う。
- upstream cancellation を実際の downstream MCP request ID に propagate する。
- failed、timed-out、cancelled tool call を automatic replay しない。
- raw tool argument、result、screenshot、clipboard value、credential をデフォルトで log しない。

## Policy layer

authorization は exact-name based のままです。`CUMG_DENY_TOOLS` は `CUMG_ALLOW_TOOLS` より優先されます。

V1 は audit/review 用に tool を `observe`、`interact`、`system`、`dangerous` にも classify します。unknown / newly discovered name は review されるまで `dangerous` と分類します。semantic classification は access を grant せず、exact-name allowlist を widen しません。

argument-level constraint が必要な場合、Cua 自身の policy engine を optional second layer として利用できます。[`../examples/cua-policy.yaml`](../examples/cua-policy.yaml) を出発点として target machine 用に review してください。

read-only operation でも private desktop data を公開する可能性があります。screenshot、accessibility information、window/app metadata、同様の observation capability は sensitive data access として扱ってください。

## Failure と cancellation semantics

read-only discovery は transport failure 後に reconnect / retry できる場合があります。computer-use action は、desktop が既に action を部分的に適用している可能性があるため扱いが異なります。

in-flight tool call では gateway が downstream MCP request ID を保持します。northbound request が cancel された場合、gateway は同じ request ID に対する downstream `notifications/cancelled` を送り、replay せず error を返します。tool timeout も同じ no-replay rule に従い、later request 用の recovery より前に downstream cancellation を試みます。

deterministic CI fixture は、downstream cancellation ID が in-flight backend request ID と一致することを検証します。

## Host / Origin validation

MCP boundary は Host / Origin guard を使います。default accepted authority/origin は loopback-oriented です。remote deployment では exact expected public authority/origin を設定するか、trusted proxy で Host を deliberate に rewrite してください。proxy configuration を通すためだけに guard を disable しないでください。

[`DEPLOYMENT.md`](DEPLOYMENT.md) を参照してください。

## Health metadata

`/healthz` は readiness を返し、gateway-owned backend child process の operational metric を含む場合があります。

- PID
- cumulative CPU seconds
- RSS bytes

raw desktop content は含みませんが、remote reachable な health route も同じ authenticated deployment boundary の背後に置くべきです。

macOS では Cua が supported application/daemon lifecycle を利用する場合があるため、これら metric は gateway が直接所有する child を示し、すべての Cua process の aggregate usage を表すものではありません。

## Cloudflare deployment

推奨 topology:

```text
remote MCP client
    |
authenticated TLS / Cloudflare Access
    |
Cloudflare Tunnel
    |
127.0.0.1:<gateway>
    |
Cua stdio
```

gateway は loopback のままにします。実 tunnel credential、Access token、private hostname、`.env` file、generated private key、PKCS#12 bundle、local `secrets/` directory を commit しないでください。repository ignore rule は defense in depth であり、secret manager や repository secret scanning の代替ではありません。

## Local physical desktop acceptance

Accessibility / Screen Recording grant が付いた Mac は high-trust machine です。そのため physical desktop acceptance は operator-controlled / local-only であり、normal GitHub Actions は GitHub-hosted runner を使い、それら desktop grant を受け取りません。

`scripts/v2_desktop_acceptance.sh` は、trusted logged-in Mac 上の reviewed checkout から、必要な physical-action ACK variable をすべて明示的に `1` に設定した場合にのみ実行してください。daily-use workstation より dedicated test Mac を推奨します。[`V2_LOCAL_DESKTOP_ACCEPTANCE.md`](v2/acceptance/V2_LOCAL_DESKTOP_ACCEPTANCE.md) と [`V1_ACCEPTANCE.md`](V1_ACCEPTANCE.md) を参照してください。

first-class Handoff coordination を有効化する場合、`CUMG_V2_HANDOFF_CONTROL_SOCKET` は northbound MCP tool ではなく、独立した local operator plane です。親 directory は private / non-symlink とし、Hub が mode `0600` で socket を作成しますが、役割は signed / session-fenced control を Agent-owned canonical Handoff runtime へ relay することだけです。local caller は principal / device / Window authority を指定できません。CUMG が still-valid interaction context と current Agent session fence から exact target を供給し、Agent は Cua 直前に device / generation / revision と exact command surface を再検証します。`begin` は CUMG execution state でも fence され、既知の unsettled device work がある場合は `handoff_agent_not_idle`、未解決の CUMG desktop quarantine がある場合は `handoff_device_quarantined` で拒否し、race に備えて Agent 側の独立した final idle check も維持します。explicit operator control が返す takeover locator は capability material であり、generic log や public report にコピーしてはいけません。Agent-local Handoff runtime failure は fail-closed のままで、authority を復元する目的で auto-restart しません。expired Handoff recovery abandonment は explicit local-operator action のみに限定し、exact recovery epoch を要求し、durable deletion 成功後に signed Handoff recovery checkpoint だけを clear します。CUMG quarantine を resolve せず、execution evidence を捏造せず、prior semantic action を success 扱いせず、replay も authorize しません。

P1 final physical acceptance は 2026-08-13、trusted `main` commit `bb39390f3587902a7df918fe1ff4a8b28c328d50`、Desktop E2E run `31675515516` で実行されました。runner は dedicated label で ephemeral registration され、trusted `main` checkout のみを実行し、job 後に自動 unregister されました。V2 P1 step では、explicit resolution / reuse より前に replay することなく、Hub/Agent restart と generation advance をまたいで exact quarantine が生存することを要求しました。

## CI supply chain

normal CI は read-only repository permission と locked Rust dependency resolution を使用します。real-Cua smoke より前に、CI は pinned Cua installer、platform release payload、installed executable identity を検証し、installed binary が independently verified release payload と一致することを要求します。

deterministic V1 quality fixture は desktop に触れません。cancellation、100-call soak behavior、short-window idle resource regression check、backend process telemetry、選択された applicable official MCP conformance scenario をカバーします。

## Logs と reporting

Gateway audit log は tool name、semantic class、policy decision、outcome、duration など coarse metadata を記録します。raw argument/result と credential は normal log に含めないでください。

security-sensitive report では credential や無関係な private desktop data を public issue に含めないでください。利用可能なら GitHub private vulnerability reporting を優先してください。

## V2 trust model

V2 は northbound authenticated client principal、Hub transport identity、grant-signing authority、Agent device identity を分離します。key rotation は continuity proof を要求し、bounded admission と signed cancellation/reconnect semantics は ambiguous operation に対して fail closed します。compromised-component analysis と non-claim の全体は [`V2_THREAT_MODEL.ja.md`](v2/V2_THREAT_MODEL.ja.md) にあります。

V2-M1 は 2026-08-12 に single-secure-Agent acceptance gate を pass しました。production candidate は TLS-protected gRPC と independently signed application identity を分離し、principal -> stable device -> exact capability grant を維持し、northbound OAuth bearer token を Agent に転送しません。ambiguous desktop cancellation は replay を authorize せず `indeterminate` のまま device を quarantine します。packaged Linux production では grant-signing private key を別 UID の `v2_grant_signer` service に分離し、`v2_hub` は signer public verifier と Unix socket だけを持ちます。external signer unavailable/deny 時に in-process fallback はなく、signer 自身が exact capability / TTL / clock-skew ceiling を独立に検証してから canonical grant を sign します。ordinary server-certificate renewal は ACME が担い、Hub/device/grant key rotation は独立かつ continuity-proven のままです。OpenTelemetry/OTLP default telemetry は sensitive operation payload を除外します。[`V2_M1_ACCEPTANCE.md`](v2/acceptance/V2_M1_ACCEPTANCE.md)、[`V2_THREAT_MODEL.ja.md`](v2/V2_THREAT_MODEL.ja.md)、[`V2_GRANT_SIGNING.md`](v2/V2_GRANT_SIGNING.md) を参照してください。

post-M1 P0 hardening は、この ambiguity boundary を authoritative operation ledger で明示します。authenticated issuer/subject ownership と Agent generation の両方が settlement を fence し、dispatch 済み uncertainty は reconnect/restart をまたぐ exact-operation desktop quarantine として persist され、pre-ambiguity queue work は resume せず cancel されます。reuse には explicit、auditable、persistence-gated resolution が必要です。recovery evidence string は bounded metadata であり、raw desktop content、command、result、secret を含めてはいけません。[`V2_P0_EXECUTION_SAFETY.md`](v2/V2_P0_EXECUTION_SAFETY.md) を参照してください。

execution-safety schema v3 は、この authority boundary を変えずに privacy-preserving な reconciliation correlation を追加します。effectful northbound call は bounded な `workflow_id`、`workflow_step_id`、`client_correlation_id` label を付与できますが、これらは untrusted audit label にすぎず、principal ownership、capability authorization、generation fence、retry safety、quarantine resolution を変更できません。canonicalization が明示的に定義された shell/process request では、deployment-private key から dispatch 前に生成した keyed HMAC-SHA256 fingerprint を保持できます。fingerprint 一致が意味するのは「同じ key generation 下の同一 canonical request」のみであり、execution/completion の proof、replay authorization、quarantine clear にはなりません。通常 inspection が公開するのは fingerprint の有無、または local な same/different/unavailable comparison だけで、fingerprint/key 自体は公開しません。schema v1/v2 state は v3-only field を主張しない限り引き続き読めますが、unsafe downgrade / forged combination は fail closed します。raw command/argv/cwd/env/result/credential content は durable audit / inspection output から引き続き除外します。

execution-safety schema v4 は transient な response-loss / restart ambiguity に対して、意図的に狭く制限した automatic settlement path を追加します。effectful command を送信する前に、Hub は operation を exact stable device、original Agent generation、exact capability、capability revision、dispatch fence として使う one-shot grant identifier に durable bind します。Agent が durable に保持できるのは、通常の execution path が Hub が受理可能な terminal result にすでに到達した後に生成した payload-free terminal proof 最大64件だけです。後続の authenticated generation では、その bounded journal を fresh session nonce に対して再署名して報告します。Hub が既存 `Indeterminate` を `auto_resolved` にできるのは、signed proof が保存済み binding の全項目に exact match し、evidence class も既存の authoritative class（`VerifiedAgentResult`、`VerifiedRemoteError`、`ProvenProcessTermination`）である場合だけです。candidate terminal checkpoint の commit が成功してから live quarantine を除去します。proof が無ければ `unrecoverable_evidence_gap`、stale / forged / mismatch / unprovable な claim は `operator_required` のまま、または fail closed します。この path は command の再送、operation retry、request fingerprint の execution evidence 化を一切行わず、raw command / environment / result / credential / grant token / fingerprint value も保存・公開しません。persist する grant ID は one-shot の opaque correlation fence であり bearer grant/token ではなく、通常の maintenance output には公開しません。schema v1/v2/v3 checkpoint は v4-only state を主張しない限り引き続き読め、v4 reconciliation state を失う downgrade は拒否します。capability schema v5 により reconciliation-report frame は coordinated Hub/Agent protocol boundary となり、old/new mixed peer は stream を部分解釈せず handshake で fail closed します。

execution-safety schema v5 は、terminal outcome を後から正直に再構成できない古い `Indeterminate` operation のために、settlement とは別の **unknown-outcome retirement** path を追加します。retirement は settlement ではありません。historical state は `Indeterminate` のままで terminal receipt を生成せず、completed / cancelled / not-executed へ書き換えません。review 済み v1 retirement policy は意図的に `Scroll` と `MovePointer` のみに限定し、durable device registry が original dispatch より strictly newer generation を示すこと、reconciliation state が `operator_required` または `unrecoverable_evidence_gap` であること、exclusive offline local-maintenance authority から実行されることを要求します。old operation ID は permanent replay tombstone として残り、quarantine clear は dispatch / resume / re-sign / reconstruct を一切行いません。retirement audit は `outcome=unknown`、capability/policy、original/authorizing generation、prior reconciliation state、bounded reason metadata、local maintenance authority、timestamp、`replayed=false` を durable に保持します。high-impact capability は fail closed します。schema v1/v2/v3/v4 checkpoint は representational limit 内で引き続き読めますが、v5 retirement state を含む checkpoint を、その区別を失う schema へ downgrade することは拒否します。versioned retirement policy は offline maintenance から明示指定し、将来の policy 追加で既存 runbook が暗黙に authority を広げないようにします。durable retirement history は64件に上限し、上限到達時は checkpoint state を無制限に増やさず quarantine を維持したまま fail closed します。

execution-safety schema v6 は v5 の retirement contract を維持したまま、text input の ambiguity に対する bounded な manual resolution `confirmed_effect_applied_uncommitted` を追加します。これは `type_text` / browser text input に限り、input side effect は発生したが独立した submit/commit action は発生していないことを independent evidence で確認できた場合だけ使用できます。元 operation は terminal replay tombstone となり retry/replay は一切許可せず、`confirmed_not_executed` と `confirmed_completed` のどちらとも明確に区別されます。この v6-only resolution を含む checkpoint は、その区別を失う v5 への downgrade を拒否します。

### V2 payload-safe observability

V2 diagnostic output 自体が security boundary です。default tracing event / OTel metric には raw `DeviceCommand`/`DeviceResult` value、process stdout/stderr、shell text/argv/environment value、operation payload 内の file path/content、OAuth bearer token/introspection secret、exact grant、protocol signature、private key material を含めてはいけません。V2 Hub/Agent/backend/persistence boundary で使う Error / Debug formatting は stable error code に縮約し、unexpected signed protocol message は object 全体を `Debug` formatting せず message kind で表現します。OAuth debug representation は introspection client secret と authenticated principal を redact します。northbound の process/shell policy failure が公開できるのは closed な privacy-safe category と固定 message だけです。requested/allowed path、command text/argument、environment key/value、raw OS spawn/I/O error は contract に含めません。

`operation_id`、stable `device_id`、Agent `generation` は safety state correlation に必要なので structured log に現れる場合がありますが、metric label にはしません。principal issuer/subject は default では log しません。OTel metric attribute は capability、outcome、reason、persistence component など closed domain に限定します。request path、tool/command name、principal、identifier を metric attribute に追加してはいけません。

`RUST_LOG` で verbosity を上げても payload-free policy は緩みません。diagnostic gap を埋めるため command/result object や underlying provider exception を log するのではなく、bounded `error_code` / event field を追加してください。external collector、reverse proxy、service manager でも application boundary を無効にする body/header capture を避ける必要があります。event/metric taxonomy と incident correlation key は [`DEPLOYMENT.md`](DEPLOYMENT.md#overload-and-observability) を参照してください。


## V2 P1 fixed-set multi-device security review

P1 が追加するのは P0 core 周辺の fixed composition だけです。security review は要求された cross-device failure class をカバーします。

- **cross-device ownership bleed:** 各 device は独立した `SingleDeviceHub`、authoritative controller、checkpoint directory、queue、live session、generation を持つ。unresolved operation / quarantine を entry 間で transfer する API はない。
- **device ID / generation confusion:** routing は exact pre-provisioned stable device ID を要求する。選択された P0 service は provisioned device identity、signed session material、operation identity、capability revision、generation を引き続き verify する。reconnect が進めるのはその device の generation のみ。
- **stale routing:** fixed map は construction 後 immutable。old A result を B に route できる discovery、reassignment、failover-to-another-device operation はない。
- **shared/global queue bypass:** P1 は shared queue を導入しない。admission/load shedding は各 existing per-device Hub 内のままで、A の quarantine を B の capacity/queue 経由で bypass できない。
- **checkpoint restore consistency:** construction は duplicate state directory を拒否する。Hub restart は各 P0 checkpoint を独立して reconstruct し、ある device の restore failure を別 device state の inherit permission と解釈しない。
- **duplicate/late result or cancellation acknowledgement:** unchanged P0 operation/owner/generation fence が stale settlement と duplicate finalization を拒否する。separate service instance により signed A stream が B execution stream になることも防ぐ。
- **resolution target confusion:** recovery は exact device の `HubHandle` と exact ambiguous operation ID から呼ぶ。同じように見える別 device operation を resolve できる fleet-wide lookup はない。
- **compromised backend evidence:** trust boundary は不変。malicious authenticated Agent/backend は terminal evidence を虚偽主張したり CUMG 外で side effect を実行できる。reference executor が証明するのは conforming backend の adapter classification rule であり、remote attestation / Byzantine proof ではない。

この proof は generic authorization、mutable device enrollment/discovery、fleet scheduler、新 policy language、native GUI backend、ROSClaw fork を意図的に追加しません。

## V2 P2 replacement-seam security boundary

P2 は execution-safety authority を external authorization system、policy engine、device fabric、Computer Use runtime に delegate しません。詳細 review は [`V2_P2_REPLACEMENT_SEAMS.md`](v2/V2_P2_REPLACEMENT_SEAMS.md) にあります。

2つの新 seam は意図的に one-way / narrow です。

- `DeviceCapabilityAuthorizer` が答えられるのは、1つの authenticated principal が1つの stable device ID 上で1つの exact `DeviceCapability` を使えるかどうかだけです。operation の create/settle、ownership/generation change、quarantine clear、northbound bearer token の Agent forwarding はできません。
- `ComputerUseBackendAdapter` は typed capability を advertise し、backend-specific GUI observation を bounded CUMG model に normalize し、existing `BackendExecutionOutcome` を返せます。Hub ledger / resolution path は所有できません。backend tool/session name 自体は authorization capability ではありません。mutating command の provider dispatch 後に cancellation、timeout、disconnect、generic backend error、response loss、malformed/unprovable completion が発生し、non-execution の十分な evidence がない場合、adapter/Agent boundary では `BackendOutcomeIndeterminate` と分類します。Hub は reason `BackendOutcomeUnproven` を持つ durable `Indeterminate` を persist し、unchanged quarantine / explicit-resolution / no-auto-replay path に従います。read-only backend failure は definite のまま扱える場合があります。GUI snapshot には sensitive window title、label、value、screenshot が含まれる可能性がありますが、request result のまま扱い default telemetry に copy してはいけません。

future SINT/Grantex/Open Agent Auth/OPA/Cedar adapter は authorization state が unavailable / ambiguous の場合 fail closed しなければなりません。future Arm Device Connect / other fabric integration は discovery / liveness を routing input としてのみ扱い、ownership、safe settlement、safe reuse の proof にしてはいけません。future OpenClaw / other Computer Use adapter は2つ目の authoritative action lifecycle を導入せず、CUMG operation ID / fence 配下の executor のままでなければなりません。

existing compromised-backend boundary は引き続き適用されます。malicious authenticated backend は claimed result について嘘をついたり CUMG 外で action を実行できます。adapter seam は remote attestation を作りません。P2 はこの trust boundary を広げないよう設計されています。
