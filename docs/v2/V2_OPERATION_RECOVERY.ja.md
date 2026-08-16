# V2 process / shell operation recovery

Status: **v0.3 production hardening の active V2 contract**。

この contract は、northbound MCP response が失われた可能性がある bounded `execute_process` / `shell` operation に read-only durable recovery を提供します。既存の no-replay、quarantine、`retry_safe:false` rule は弱めません。この slice では Browser/Desktop semantic operation を result recovery 対象にはしません。

## Stable operation reference

`execute_process` と `shell` は、`op_` + 32文字の lowercase hexadecimal（128 random bits）という exact form の optional `operation_id` を受け取ります。long-running または mutating work では、caller が call **前**に cryptographically random な fresh ID を生成して保持してください。CUMG-generated ID も通常の process/shell response には返しますが、response 全体が失われた場合に server-generated ID を後から知れるとは仮定できません。

accepted operation ID は既存の authoritative replay identity そのものです。同じ ID で別 execution を試すと `operation_replay` として拒否され、status lookup が replay や resume に変換することはありません。

## Process lifetime / background descendant

`execute_process` / `shell` は bounded operation であり、service launcher ではありません。Unix では Agent が launched operation を専用 supervised process group に置き、Windows では Job Object を使います。cancellation、timeout、ordinary parent completion では、その supervision domain に残っている descendant を cleanup します。したがって plain shell background job（`nohup ... &` を含む）を persistence mechanism として使ってはいけません。supervised process group 内に残っている限り、operation が terminal state に到達すると terminate されます。

これは lifecycle contract であり、すでに Dangerous process/shell capability を authorize された caller に対する OS-wide sandbox ではありません。特に現在の Unix process-group primitive は、descendant が意図的に別 session/process group を作る（例: `setsid()` を call する）、external service manager 経由で reparent する、その他 supervised group から離脱する場合まで cleanup を guarantee できません。このような detachment は unsupported であり、persistent work を作る方法として **依存してはいけません**。より強い Unix containment gap は GitHub issue #96 で追跡し、shell text filtering や heuristic PID killing でごまかしません。

long-running build/release は bounded operation 内に残し、caller-retained `operation_id` + `get_operation` で lost northbound result を recovery します。将来 persistent managed job を追加する場合は、この process boundary を弱めるのではなく、explicit start/status/cancel lifecycle と authorization を持つ別 capability/API とします。

## `get_operation`

`get_operation(operation_id)` は Hub-local の read-only MCP tool です。Agent online を必要とせず、device command を dispatch しません。lookup は original operation を作成した authenticated issuer+subject に scope され、返却前に original `ExecuteProcess` / `Shell` capability の current authorization も再確認します。wrong-owner と unknown ID は同じ not-found behavior にし、operation reference を cross-principal existence oracle にできないようにします。

public state は次のとおりです。

- `running` — queued / active-not-dispatched / dispatched / cancellation-requested;
- `succeeded` — Agent が verified ordinary process/shell result を返した;
- `failed` — verified error result またはその他の proven failed terminal state;
- `cancelled` — process-tree cancellation が証明された;
- `timed_out` — bounded timeout が発火し process-tree termination が証明された;
- `indeterminate` — completion を証明できない。既存 quarantine / no-replay rule が引き続き authoritative。

`original_retry_safe` は常に `false` です。mutating command の blind retry ではなく recovery を使います。

## Durable result boundary

Hub が保存するのは recovery に必要な bounded caller-visible process/shell terminal result のみです。既存 `ProcessOutput` field または stable `DeviceErrorCode` を保存します。stdout / stderr は既存の streamごと 16 KiB bound と truncation flag を維持します。この v0.3 slice では inline cap を超えた bytes の retrieval は追加せず、別の output-reference capability に分離します。

recovery record は original command text、argv、cwd、environment entry を受け取らず、persist もしません。telemetry にも追加しません。recovered stdout/stderr は意図的に caller-visible な result data なので、既存 state-directory protection の対象となる sensitive local Hub checkpoint data として扱います。

recoverable result はまず authoritative execution-safety operation record に埋め込み、terminal state、owner、generation、receipt、result を northbound delivery より先にまとめて persist します。Agent generation rollover で通常の terminal admission record を compact する際は、recoverable process/shell result だけを同じ execution-safety snapshot 内の bounded recovery archive へ移します。archive は **最大8件かつ encoded total 256 KiB** に制限し、古い result から eviction します。これにより generation/reconnect を跨ぐ recovery と persistence boundedness を両立します。execution-safety snapshot schema v2 は従来の result-less v1 form を restore できますが、v1 snapshot が recoverable result/archive を含むことは許可しません。

archive から eviction された `operation_id` が `operation_not_found` になっても、元operationが retry-safe になったことを意味しません。caller は fresh random operation ID を再利用せず、外部状態をreconcileしてから新しいworkとして判断する必要があります。

## Failure / ambiguity rules

proven terminal result の後で northbound response が失われても durable terminal state は変わりません。後続 `get_operation` は Agent に contact せず result を返します。Hub が terminal completion を証明できない場合、operation は `indeterminate` のままで、lookup はその事実を返すだけで retry を authorize しません。indeterminate operation の operator resolution は引き続き別の trusted recovery action であり、欠落した process/shell result を synthesize しません。
