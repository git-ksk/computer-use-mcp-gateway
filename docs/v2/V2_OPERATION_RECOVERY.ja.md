# V2 durable effectful operation recovery

Status: **v0.4 Recovery & Reconciliation の active V2 contract**。

この contract は、northbound MCP response が失われた可能性がある effectful operation に read-only durable recovery を提供します。`execute_process` / `shell` は bounded caller-visible output recovery を維持し、effectful Desktop/Browser call は payload-free terminal marker による status-only recovery を追加します。既存の no-replay、quarantine、exact owner/capability authorization、`retry_safe:false` rule は弱めません。

## Stable operation reference

すべての effectful northbound tool は、`op_` + 32文字の lowercase hexadecimal（128 random bits）という exact form の optional `operation_id` を受け取ります。process/shell、effectful Desktop、effectful Browser が対象で、observation-only tool はこの field を受け取りません。response loss を recovery する必要がある work では、caller が call **前**に cryptographically random な fresh ID を生成して保持してください。response 全体が失われた場合に server-generated ID を後から知れるとは仮定できません。

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

`original_retry_safe` は常に `false` です。mutating command の blind retry ではなく recovery を使います。

## Durable result boundary

`execute_process` / `shell` では、Hub が保存するのは recovery に必要な bounded caller-visible terminal result のみです。既存 `ProcessOutput` field または stable `DeviceErrorCode` を保存し、stdout / stderr は既存の streamごと 16 KiB bound と truncation flag を維持します。それ以外の effectful Desktop/Browser capability では、already-authoritative な terminal state / execution receipt と payload-free `effectful_status` marker だけを durable recovery record に保存します。status lookup のために screenshot、typed text、URL、clipboard content、browser/backend result payload、GUI state をコピーしません。

recovery record は original command text、argv、cwd、environment entry を受け取らず、persist もしません。telemetry にも追加しません。recovered stdout/stderr は意図的に caller-visible な result data なので、既存 state-directory protection の対象となる sensitive local Hub checkpoint data として扱います。

recovery material はまず authoritative execution-safety operation record に埋め込み、terminal state、owner、generation、receipt と bounded process/shell output または payload-free effectful marker を northbound delivery より先にまとめて persist します。Agent generation rollover で通常の terminal admission record を compact する際は、recoverable record を同じ bounded recovery archive へ移します。archive は **最大8件かつ encoded total 256 KiB** のままで、古い detailed record から eviction します。execution-safety schema v9 は effectful status-only marker を導入し、その marker を含む checkpoint の v8 reader への downgrade は拒否します。旧 schema は representational limit 内で引き続き readable です。

archive から eviction された `operation_id` が `operation_not_found` になっても、元operationが retry-safe になったことを意味しません。caller は fresh random operation ID を再利用せず、外部状態をreconcileしてから新しいworkとして判断する必要があります。

## Failure / ambiguity rules

proven terminal result の後で northbound response が失われても durable terminal state は変わりません。後続 `get_operation` は Agent に contact せず durable state を返し、process/shell では bounded output も返せますが、Desktop/Browser recovery は意図的に status-only です。Hub が terminal completion を証明できない場合、operation は `indeterminate` のままで、lookup はその事実を返すだけで retry を authorize しません。indeterminate operation の operator resolution は引き続き別の trusted recovery action であり、欠落した process/shell result を synthesize しません。

process spawn 後の local supervision failure は、low-level error type だけではなく **terminal proof の有無**で分類します。pipe/reader setup、poll、cancellation/timeout termination、wait の failure は、Agent が supervised process domain の terminality を別途証明できた場合だけ ordinary terminal failure にできます。local process worker 自体が panic/disappear した場合は spawn/terminal boundary を証明できないため、Agent は conservative に unproven と扱います。terminality を証明できない場合、Agent は normal result を送らず reconnect し、Hub の既存 connection-loss path が durable `indeterminate` + quarantine を記録します。一方、process-domain termination を証明した後で検出された stdout/stderr reader I/O failure は ambiguity ではなく terminal failure のままです。reconnect path では conservative な public indeterminate reason として `ConnectionLost` が persist される場合がありますが、この diagnostic coarseness が replay を許可することはありません。
