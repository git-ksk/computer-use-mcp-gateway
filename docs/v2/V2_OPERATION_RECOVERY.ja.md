# V2 durable effectful operation recovery

Status: **v0.4 Recovery & Reconciliation の active V2 contract**。

この contract は、northbound MCP response が失われた可能性がある effectful operation に read-only durable recovery を提供します。`execute_process` / `shell` は bounded caller-visible output recovery を維持し、effectful Desktop/Browser call は payload-free terminal marker による status-only recovery を追加します。既存の no-replay、quarantine、exact owner/capability authorization、`retry_safe:false` rule は弱めません。

## Stable operation reference

すべての effectful northbound tool は、`op_` + 32文字の lowercase hexadecimal（128 random bits）という exact form の optional `operation_id` を受け取ります。この field を指定する場合、caller は新しい effectful execution ごとに **call 前に cryptographically secure random な fresh 128-bit value** を生成し、response loss 時の `get_operation` 用に保持してください。覚えやすい値を手書きしたり、counter/pattern を使ったり、過去の operation ID を再利用してはいけません。server は wire shape と replay identity を検証しますが、syntactically valid な128-bit valueが本当にrandom生成されたかを推測する heuristic entropy scoring は意図的に行いません。

代表的な CSPRNG 生成例:

```text
Python:  "op_" + secrets.token_hex(16)
Node.js: "op_" + crypto.randomBytes(16).toString("hex")
```

値は effectful tool 呼び出し前に生成・保持してください。response 全体が失われた場合に server-generated ID を後から知れるとは仮定できません。2026-09-22 dogfood で観測された `op_69696969696969696969696969696969` のような patterned value は、生成してはいけない明示的な例です。

accepted operation ID は既存の authoritative replay identity そのものです。同じ ID で別 execution を試すと `operation_replay` として拒否され、status lookup が replay や resume に変換することはありません。

## Process lifetime / background descendant

`execute_process` / `shell` は bounded operation であり、service launcher ではありません。Unix では Agent が launched operation を専用 supervised process group に置き、Windows では Job Object を使います。cancellation、timeout、ordinary parent completion では、その supervision domain に残っている descendant を cleanup します。したがって plain shell background job（`nohup ... &` を含む）を persistence mechanism として使ってはいけません。supervised process group 内に残っている限り、operation が terminal state に到達すると terminate されます。

これは lifecycle contract であり、すでに Dangerous process/shell capability を authorize された caller に対する OS-wide sandbox ではありません。特に現在の Unix process-group primitive は、descendant が意図的に別 session/process group を作る（例: `setsid()` を call する）、external service manager 経由で reparent する、その他 supervised group から離脱する場合まで cleanup を guarantee できません。このような detachment は unsupported であり、persistent work を作る方法として **依存してはいけません**。より強い Unix containment gap は GitHub issue #96 で追跡し、shell text filtering や heuristic PID killing でごまかしません。

long-running build/release は bounded operation 内に残し、caller-retained `operation_id` + `get_operation` で lost northbound result を recovery します。将来 persistent managed job を追加する場合は、この process boundary を弱めるのではなく、explicit start/status/cancel lifecycle と authorization を持つ別 capability/API とします。

## `get_operation`

`get_operation(operation_id)` は Hub-local の read-only MCP tool です。Agent online を必要とせず、device command を dispatch しません。lookup は original operation を作成した authenticated issuer+subject に scope され、返却前に original exact capability の current authorization も再確認します。wrong-owner と unknown ID は同じ not-found behavior にし、operation reference を cross-principal existence oracle にできないようにします。

public state は次のとおりです。

- `running` — queued / active-not-dispatched / dispatched / cancellation-requested;
- `succeeded` — Agent が original effectful capability の verified terminal result を返した;
- `failed` — verified error result またはその他の proven failed terminal state;
- `cancelled` — process-tree cancellation が証明された;
- `timed_out` — bounded timeout が発火し process-tree termination が証明された;
- `indeterminate` — completion を証明できない。既存 quarantine / no-replay rule が引き続き authoritative。

`original_retry_safe` は常に `false` です。mutating command の blind retry ではなく recovery を使います。northbound error が `device_indeterminate` の場合は、bounded な actionable guidance として `execution_may_have_occurred=true`、`blind_replay_safe=false`、`next_action=get_operation_then_reconcile`、`follow_up_effectful_operation=new_operation_id_required` を返します。exact `get_operation` がまだ `indeterminate` の場合は `next_action=reconcile_indeterminate` へ進みます。これらは authoritative operation state から導出し、command text の heuristic 解析は行いません。reconciliation 後に effectful work を続ける場合も fresh operation ID の新規operationとして実行し、quarantine中のold operationをreplayしません。

### Structured refusal remediation は recovery authority ではない (#379)

caller-facing semantic refusalは既存stable error `code`をそのまま再利用し、[`../TROUBLESHOOTING.md`](../TROUBLESHOOTING.md)で定義するboundedな `required_actor` / `next_action` metadataだけをadditiveに返せます。このhintはexecution history、quarantine、capability authorization、browser trust/consent、interaction scope、replay safetyを変更しません。`device_indeterminate`、`confirmed_not_executed`、`mutation_resume_required` は専用recovery semanticsを維持し、generic refusal remediationがそれを上書きしたりsettlementを捏造したりしてはいけません。

### Hub-authoritative pre-enqueue non-delivery (v0.8 / #377)

execution-safety schema **v15** は、Hubがeffectful operationをすでにdurable `Dispatched`としてcommitした後でも、exact first outbound attemptが **per-session Hub→Agent queueにencoded commandをacceptされる前** に失敗したことをHub自身が証明できる狭いケースだけをautomatic non-execution proofとして追加します。対象は `encode_hub_frame()` がenqueue前に失敗した場合、またはTokio `mpsc::Sender::send()` がreceiver closedによりunsent frameを返した場合だけです。provider log、Agent evidenceの欠落、timeout、successful enqueue後のtransport/writer failure、silenceは同じ証拠として扱いません。

Hubはproofをexact operation / owner / device generation / capability revision / one-shot dispatch grantへbindし、`hub_encode_failed_before_enqueue` または `hub_outbound_closed_before_enqueue` のpayload-free terminal `Cancelled` receiptとbounded auto-resolution audit recordを作り、candidate checkpointを **live state置換より先に** commitします。checkpoint saveに失敗した場合はterminal proofをmemory上だけでpublishせず、既にdurableな`Dispatched` recordをfail-closedのまま残し、通常のsession cleanup/restart ruleでambiguityを維持します。

northboundはimmediate failureを `confirmed_not_executed`、`execution_may_have_occurred=false` として返し、後から `get_operation` した場合も `state=cancelled` に加えて `resolution=confirmed_not_executed` と同じbounded guidanceを返します。どちらも `retry_safe=false` / `blind_replay_safe=false` を維持します。old operation IDはterminal replay tombstoneのままです。effectがまだ必要なら `next_action=retry_with_new_operation_id_if_still_needed`、`follow_up_effectful_operation=new_operation_id_required` に従いfresh operation identityを使います。outbound queueがframeをacceptした後のfailureはこのpathを使えず、既存 `Indeterminate` / quarantine / reconciliation contractに従います。

historical schema-v14 checkpointは引き続きreadableです。v15 pre-enqueue non-delivery evidenceを含むcheckpointは、そのevidence classを失うv14へのlossy downgradeを拒否します。

operator surfaceもboundedのままです。`inspect-quarantine` はactive quarantineが消えた後もrecent automatic resolutionを併記し、`audit-reconciliation <operation_id>` はAgent stateを参照せずこのsettled proofを `authoritative_hub_protocol` / `no_recovery_required` として表示し、`get_operation` は `resolution=confirmed_not_executed` を返します。incident briefは意図的に未解決quarantine incident専用のままです。

## Durable result boundary

`execute_process` / `shell` では、Hub が保存するのは recovery に必要な bounded caller-visible terminal result のみです。既存 `ProcessOutput` field または stable `DeviceErrorCode` を保存し、stdout / stderr は既存の streamごと 16 KiB bound と truncation flag を維持します。それ以外の effectful Desktop/Browser capability では、already-authoritative な terminal state / execution receipt と payload-free `effectful_status` marker だけを durable recovery record に保存します。status lookup のために screenshot、typed text、URL、clipboard content、browser/backend result payload、GUI state をコピーしません。

recovery record は original command text、argv、cwd、environment entry を受け取らず、persist もしません。telemetry にも追加しません。recovered stdout/stderr は意図的に caller-visible な result data なので、既存 state-directory protection の対象となる sensitive local Hub checkpoint data として扱います。

recovery material はまず authoritative execution-safety operation record に埋め込み、terminal state、owner、generation、receipt と bounded process/shell output または payload-free effectful marker を northbound delivery より先にまとめて persist します。Agent generation rollover で通常の terminal admission record を compact する際は、recoverable record を同じ bounded recovery archive へ移します。archive は **最大8件かつ encoded total 256 KiB** のままで、古い detailed record から eviction します。execution-safety schema v9 は effectful status-only marker を導入し、schema **v14** は application-targeted operation 向けの bounded private recovery-target metadata を追加します。review済みの旧schemaはrepresentational limit内で引き続きreadableで、v14 recovery-target stateを捨てるdowngradeはfail closedします。

### Local-only application recovery target identity (v0.7 / #289)

`launch_application` は既にboundedな application `identifier` / `name` selectorだけをprivate recovery metadataとして記録します。`terminate_application` がAgentへdispatchするcommandは従来どおりexact PIDだけですが、v0.7 northbound tool schemaでは prior observation（例: `list_windows`）でcallerが選択したbounded `application` identityも必須にします。Hubは `{process_id, application}` をauthoritative checkpoint内にだけpersistし、terminationが `Indeterminate` になった後でもlocal operatorが独立検証すべきapplicationを特定できるようにします。新規v14 operationでtargetがmissing / malformed / capability mismatch / corruptedならfail closedします。target metadataを持たないhistorical v13 checkpointはlegacy stateとしてreadableですが、captureしていないidentityを後から捏造しません。

recovery-target metadataは **execution evidenceではありません**。`confirmed_completed` / `confirmed_not_executed` の成立、quarantine clear、replay authorization、mutation-authority switch、Human presence bypassには使えません。`get_operation`、通常MCP result/tooling、generic `inspect-quarantine` JSON、unified `cumg_status`、log、metric、ordinary telemetryはraw targetを出しません。reviewed local `v2_maint incident-brief` だけがstate-directory/operator filesystem authorization下でbounded targetを表示でき、Humanが独立検証するために使います。そのJSONは `recovery_metadata_only`、`settlement_authority=false`、`replay_authority=false` を明示します。

**v0.7 client migration:** `terminate_application` は `process_id` と `application` の両方が必須です。v0.6の `{process_id}` shapeを使うclientはMCP discoveryをrefreshし、選択したPIDに対応するbounded application identityを渡してください。これはnorthboundのpre-1.0 minor breaking changeだけで、Agent `DeviceCommand::TerminateApplication` はPID-onlyのままなのでlive control-schema bumpは不要です。

archive から eviction された `operation_id` が `operation_not_found` になっても、元operationが retry-safe になったことを意味しません。caller は fresh random operation ID を再利用せず、外部状態をreconcileしてから新しいworkとして判断する必要があります。

### Authoritative backend execution receipt（v0.7 / #290）

backend は、effectful operation に対して review 済みの **durable execution-receipt** contract を opt-in できます。これは provider text や log の generic な解釈ではありません。Agent が adapter に問い合わせるのは、通常の backend call が ambiguous になった後に既に記録済みの receipt 1件だけで、lookup は mutation の replay、current GUI state の probe、heuristic による success 推測を行ってはいけません。receipt は bounded schema が valid で、stable device ID、original device generation、operation ID、`DeviceCapability`、capability revision、one-shot dispatch grant、backend/provider identity と version、monotonic な provider sequence、必要な command target binding のすべてが exact match する場合だけ受理します。application launch/termination では target binding を private な #289 recovery target とも照合しますが、人間向け private `application` label は recovery metadata のままで backend evidence にはなりません。

受理した receipt は reconnect 処理より先に Agent checkpoint へ persist し、既存の payload-free `AgentTerminalEvidence` へ変換します。fresh authenticated Agent session は **既存 signed reconciliation report** を送信し、Hub は既存 #124 `reconcile_authoritative_terminal()` state machine だけで処理します。Hub は candidate terminal checkpoint を persist してから live quarantine を解除します。別の receipt settlement state machine や新しい replay authority は作りません。receipt が missing / malformed / stale / duplicate-conflicting / cross-operation / wrong-generation / wrong-capability・revision・grant・target / unsupported-schema / provider mismatch の場合、original outcome は `Indeterminate` のままです。quarantine と permanent no-auto-replay も維持します。

receipt provenance は意図的に provider-specific です。default の `ComputerUseBackendAdapter::recover_execution_receipt()` は evidence を返さないため、**command を実行できるだけでは receipt-authoritative backend になりません**。現在の Cua MCP adapter には review 済み durable provider-receipt lookup contract がないため、Agent が definite result を得る前に Cua response を失った場合は引き続き `Indeterminate` / operator-required です。Agent が definite normal result を既に受け取った後で Hub transport だけを失った場合は、backend receipt を使わず従来の Agent terminal-evidence journal が処理します。process/shell も既存 terminal-result path を維持し、#290 は process output、backend log、current application/browser state、OS journal を settlement evidence と解釈しません。

receipt は payload-free で、bounded な operation/dispatch/provider provenance、sequence、target binding、terminal outcome/evidence class だけを持ちます。raw argv、typed text、URL、clipboard、credential、screenshot、GUI payload、provider response text、stdout、stderr は保存しません。generic MCP result、`get_operation`、`cumg_status`、log、metric、ordinary telemetry は receipt payload や private recovery target を公開しません。local operator 向け `v2_maint audit-reconciliation` / `incident-brief` は bounded receipt provenance（provider/version/contract schema/sequence/target-match status）を explicit な CUMG-authority label 付きで表示できますが、external diagnostic は `observational_only` のままです。

#290 が変更するのは Agent-local durable state だけです。M1 checkpoint schema **v6** は bounded receipt journal を追加し、historical schema v5 は receipt state を含まない場合に引き続き readable です。receipt を持つ v6 checkpoint は v5 で表現できないため lossy downgrade は fail closed します。authoritative Hub execution-safety schema は、#377のHub-local pre-enqueue non-delivery evidence追加によりreleased v0.8では **v15** です。#290自体が変更するのは引き続きAgent-local durable stateだけで、live signed reconciliation message も既存 `AgentTerminalEvidence` shape を使うため、`CONTROL_SCHEMA_VERSION`、capability-advertisement schema、Hub-Agent wire schema は変更しません。

## Failure / ambiguity rules

proven terminal result の後で northbound response が失われても durable terminal state は変わりません。後続 `get_operation` は Agent に contact せず durable state を返し、process/shell では bounded output も返せますが、Desktop/Browser recovery は意図的に status-only です。Hub が terminal completion を証明できない場合、operation は `indeterminate` のままで、lookup はその事実を返すだけで retry を authorize しません。indeterminate operation の operator resolution は引き続き別の trusted recovery action であり、欠落した process/shell result を synthesize しません。

process spawn 後の local supervision failure は、low-level error type だけではなく **terminal proof の有無**で分類します。pipe/reader setup、poll、cancellation/timeout termination、wait の failure は、Agent が supervised process domain の terminality を別途証明できた場合だけ ordinary terminal failure にできます。local process worker 自体が panic/disappear した場合は spawn/terminal boundary を証明できないため、Agent は conservative に unproven と扱います。terminality を証明できない場合、Agent は normal result を送らず reconnect し、Hub の既存 connection-loss path が durable `indeterminate` + quarantine を記録します。一方、process-domain termination を証明した後で検出された stdout/stderr reader I/O failure は ambiguity ではなく terminal failure のままです。reconnect path では conservative な public indeterminate reason として `ConnectionLost` が persist される場合がありますが、この diagnostic coarseness が replay を許可することはありません。

## Managed jobs (#106)

長時間の開発workは `nohup`、`setsid`、shell backgrounding、service-manager escape ではなく、[V2_MANAGED_JOBS.ja.md](V2_MANAGED_JOBS.ja.md) の独立 managed-job lifecycle を使います。明示stopの曖昧性は既存 Indeterminate/no-replay/quarantine path に入り、非同期 lease-expiry / shutdown の曖昧性は Agent-local fail-closed を durable に保存して explicit offline operator recovery を要求します。
