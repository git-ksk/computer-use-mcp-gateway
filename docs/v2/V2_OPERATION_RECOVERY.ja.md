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

## Durable result boundary

`execute_process` / `shell` では、Hub が保存するのは recovery に必要な bounded caller-visible terminal result のみです。既存 `ProcessOutput` field または stable `DeviceErrorCode` を保存し、stdout / stderr は既存の streamごと 16 KiB bound と truncation flag を維持します。それ以外の effectful Desktop/Browser capability では、already-authoritative な terminal state / execution receipt と payload-free `effectful_status` marker だけを durable recovery record に保存します。status lookup のために screenshot、typed text、URL、clipboard content、browser/backend result payload、GUI state をコピーしません。

recovery record は original command text、argv、cwd、environment entry を受け取らず、persist もしません。telemetry にも追加しません。recovered stdout/stderr は意図的に caller-visible な result data なので、既存 state-directory protection の対象となる sensitive local Hub checkpoint data として扱います。

recovery material はまず authoritative execution-safety operation record に埋め込み、terminal state、owner、generation、receipt と bounded process/shell output または payload-free effectful marker を northbound delivery より先にまとめて persist します。Agent generation rollover で通常の terminal admission record を compact する際は、recoverable record を同じ bounded recovery archive へ移します。archive は **最大8件かつ encoded total 256 KiB** のままで、古い detailed record から eviction します。execution-safety schema v9 は effectful status-only marker を導入し、schema **v14** は application-targeted operation 向けの bounded private recovery-target metadata を追加します。review済みの旧schemaはrepresentational limit内で引き続きreadableで、v14 recovery-target stateを捨てるdowngradeはfail closedします。

### Local-only application recovery target identity (v0.7 / #289)

`launch_application` は既にboundedな application `identifier` / `name` selectorだけをprivate recovery metadataとして記録します。`terminate_application` がAgentへdispatchするcommandは従来どおりexact PIDだけですが、v0.7 northbound tool schemaでは prior observation（例: `list_windows`）でcallerが選択したbounded `application` identityも必須にします。Hubは `{process_id, application}` をauthoritative checkpoint内にだけpersistし、terminationが `Indeterminate` になった後でもlocal operatorが独立検証すべきapplicationを特定できるようにします。新規v14 operationでtargetがmissing / malformed / capability mismatch / corruptedならfail closedします。target metadataを持たないhistorical v13 checkpointはlegacy stateとしてreadableですが、captureしていないidentityを後から捏造しません。

recovery-target metadataは **execution evidenceではありません**。`confirmed_completed` / `confirmed_not_executed` の成立、quarantine clear、replay authorization、mutation-authority switch、Human presence bypassには使えません。`get_operation`、通常MCP result/tooling、generic `inspect-quarantine` JSON、unified `cumg_status`、log、metric、ordinary telemetryはraw targetを出しません。reviewed local `v2_maint incident-brief` だけがstate-directory/operator filesystem authorization下でbounded targetを表示でき、Humanが独立検証するために使います。そのJSONは `recovery_metadata_only`、`settlement_authority=false`、`replay_authority=false` を明示します。

**v0.7 client migration:** `terminate_application` は `process_id` と `application` の両方が必須です。v0.6の `{process_id}` shapeを使うclientはMCP discoveryをrefreshし、選択したPIDに対応するbounded application identityを渡してください。これはnorthboundのpre-1.0 minor breaking changeだけで、Agent `DeviceCommand::TerminateApplication` はPID-onlyのままなのでlive control-schema bumpは不要です。

archive から eviction された `operation_id` が `operation_not_found` になっても、元operationが retry-safe になったことを意味しません。caller は fresh random operation ID を再利用せず、外部状態をreconcileしてから新しいworkとして判断する必要があります。

## Failure / ambiguity rules

proven terminal result の後で northbound response が失われても durable terminal state は変わりません。後続 `get_operation` は Agent に contact せず durable state を返し、process/shell では bounded output も返せますが、Desktop/Browser recovery は意図的に status-only です。Hub が terminal completion を証明できない場合、operation は `indeterminate` のままで、lookup はその事実を返すだけで retry を authorize しません。indeterminate operation の operator resolution は引き続き別の trusted recovery action であり、欠落した process/shell result を synthesize しません。

process spawn 後の local supervision failure は、low-level error type だけではなく **terminal proof の有無**で分類します。pipe/reader setup、poll、cancellation/timeout termination、wait の failure は、Agent が supervised process domain の terminality を別途証明できた場合だけ ordinary terminal failure にできます。local process worker 自体が panic/disappear した場合は spawn/terminal boundary を証明できないため、Agent は conservative に unproven と扱います。terminality を証明できない場合、Agent は normal result を送らず reconnect し、Hub の既存 connection-loss path が durable `indeterminate` + quarantine を記録します。一方、process-domain termination を証明した後で検出された stdout/stderr reader I/O failure は ambiguity ではなく terminal failure のままです。reconnect path では conservative な public indeterminate reason として `ConnectionLost` が persist される場合がありますが、この diagnostic coarseness が replay を許可することはありません。

## Managed jobs (#106)

長時間の開発workは `nohup`、`setsid`、shell backgrounding、service-manager escape ではなく、[V2_MANAGED_JOBS.ja.md](V2_MANAGED_JOBS.ja.md) の独立 managed-job lifecycle を使います。明示stopの曖昧性は既存 Indeterminate/no-replay/quarantine path に入り、非同期 lease-expiry / shutdown の曖昧性は Agent-local fail-closed を durable に保存して explicit offline operator recovery を要求します。
