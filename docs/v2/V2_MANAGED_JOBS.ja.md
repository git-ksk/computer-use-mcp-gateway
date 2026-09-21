# V2 Managed Developer Jobs

Issue #106 は、長時間動く開発用 process を明示的に管理する lifecycle を追加します。
`execute_process` / `shell` を service launcher に変えるものではなく、background shell の亖換逃げ道も追加しません。

## Authority boundary

managed job は次の exact capability を使います。

- `ManagedJobControl` — **Dangerous**。`managed_job_start` / `managed_job_renew` / `managed_job_stop` に必須。
- `ManagedJobObserve` — **Observe**。`managed_job_status` / `managed_job_output` に必須。

これらは `ExecuteProcess`、`Shell`、capability class、cwd root、filesystem root、browser authority から推論しません。
northbound authorization は exact principal + stable device + exact capability と live Agent advertisement の intersection を維持します。

`managed_job_start` が受けるのは structured `program` + argv + cwd + allowlist 済み environment entry だけです。
free-form shell input はありません。`nohup`、`setsid`、daemonize、service-manager escape、shell backgrounding を persistence mechanism としてサポートしません。

## Identity / fencing

Agent は private random job locator を所有し、Hub はこれを northbound へ公開しません。
Hub は dispatch 前に random opaque `job_ref` の枠を予約し、Agent start 成功後に次へ bind します。

- exact authenticated `OperationOwner`
- stable device ID
- Agent generation
- capability revision
- start operation ID
- private Agent locator

unknown ref、wrong owner、stale generation/revision、expired ref は cross-owner existence を漏らさず同様に fail closed します。
Hub restart で in-memory public-ref registry は失劻し、Agent session loss では managed-job cleanup を開始します。

## Lifetime / revocation

job は2つの期限を持ちます。

1. hard lifetime: 最大6時間
2. renewable control lease: 1回最大5分

default lease は2分です。`managed_job_renew` は毎回 fresh `ManagedJobControl` authorization を通ります。
権限 revoke 後は renew できず、最後に発行済みの lease deadline で Agent が cleanup を開始します。

state model:

- `starting`
- `running`
- `stop_requested`
- `completed`
- `stopped`
- `expired`
- `indeterminate_termination`

`stop_requested` を `stopped` として返しません。
`completed` / `stopped` / `expired` は supervised process domain の terminal proof がある場合だけ成立します。

## Process-control guarantee

managed job は bounded process execution と同じ structured-process policy / supervision primitive を再利用します。

- macOS / Unix baseline: dedicated process group
- Windows baseline: Job Object

これは lifecycle guarantee であり、filesystem / network sandbox ではありません。
Unix process group から deliberate に離脱する挙動は managed job では引き続き unsupported です。#267 の optional Linux cgroup-v2 backend は bounded `execute_process` / `shell` のみが対象で、managed-job cgroup integration は current contract に含めません。

## Ambiguous termination

明示 `stop` で terminality を証明できない場合、effectful stop operation は既存の `Indeterminate` / no-replay / quarantine path に入ります。

lease expiry や session/shutdown cleanup のような非同期停止で terminality を証明できない場合、対応する active northbound operation はありません。
そのため Agent は `managed_job_fail_closed = true` を durable checkpoint に保存し、新規 work を拒否し、doctor error を出します。
heartbeat 設定とは独立した100ms safety pollで検知します。

この状態は自動解除しません。offline operator recovery は次です。

1. Agent を停止する。
2. orphan managed process が残っていないことを独立に確認する。
3. `v2_maint inspect-managed-job-safety --agent-state-dir ...` で確認する。
4. `v2_maint clear-managed-job-fail-closed --agent-state-dir ... --evidence "..."` で明示解除する。

clear は Agent state-directory lock を取得するため、live Agent に対して実行できません。
evidence は bounded operator gate であり、argv / cwd / env / output / secrets を含めてはいけません。

## Output / privacy

stream ごとの rolling buffer は最大4 MiB、1回の read は最大64 KiBです。
range result は `earliest_available_offset`、`next_offset`、`total_bytes`、`eof`、`gap_before_range`、`history_truncated` を含みます。

northbound output は base64 で返します。
default telemetry に raw output、job ref、Agent locator、argv、cwd、environment value、PID、host output path を記録しません。

## Replay / response loss

managed control call は既存 operation ledger を使います。
同じ effectful operation ID の再利用は拒否され、自動 replay はありません。

Agent start 後に response が失われた場合、caller は `job_ref` を受け取れず、同じ operation ID を retry してはいけません。
job は既発行 lease / hard lifetime により bounded のままです。
`get_operation` で replay せず durable status を確認できます。

## Schema compatibility

Issue #106 は historical v0.6 development pairing として control schema 11 / capability schema 7 を導入しました。Issue #114 により current live v0.6 pairing は control schema 12 / capability schema 8 へ進みますが、#106 managed-job semantics 自体は維持します。outer signed envelope shape は変わらないため Hub-Agent envelope schema は6のままです。control/capability schema mismatch は fail closed します。

persisted registry shape は引き続き schema 8 です。released v0.5 の 8/6 と historical #106 v0.6 の 8/7 は historical pairing としてのみ restore でき、stale capability advertisement は捨てられます。current dispatch 前には registry 8 / capability 8 の fresh advertisement が必須です。

Agent checkpoint schema は5のままで、managed-job fail-closed field は historical checkpoint に対して safe default を持つ additive field です。

Playwright sandbox job は内部で lifecycle primitive を再利用しますが、northbound では generic managed job ではありません。別 exact capability と別 pwtest_ ref registry を使い、ManagedJobControl / ManagedJobObserve では Playwright test を操作できません。
