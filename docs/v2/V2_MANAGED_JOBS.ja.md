# V2 Managed Developer Jobs

Issue #106 は、長時間の開発用 process を明示管理する lifecycle を追加します。`execute_process` / `shell` を service launcher に変えるものではなく、background shell の互換逃げ道も追加しません。

## Authority boundary

managed job は2つの exact capability を使います。

- `ManagedJobControl` — Dangerous。`managed_job_start` / `managed_job_renew` / `managed_job_stop` に必須。
- `ManagedJobObserve` — Observe。`managed_job_status` / `managed_job_output` に必須。

`ExecuteProcess`、`Shell`、capability class、cwd root、filesystem root、browser authority からこれらを推論しません。northbound authorization は exact principal + stable device + exact capability と live Agent advertisement の intersection を維持します。

`managed_job_start` が受けるのは structured program + argv + cwd + allowlist 済み environment entry だけです。free-form shell input はありません。`nohup`、`setsid`、daemonize、service-manager escape、shell backgrounding を persistence mechanism としてサ�]ートしません。

## Identity / fencing

Agent は private random job locator を所有し、Hub はこれを northbound へ公開しません。Hub は dispatch 前に opaque random `job_ref` の枠を予約し、Agent start 成功後に exact authenticated owner、stable device ID、Agent generation、capability revision、start operation ID、private Agent locator へ bind します。

unknown ref、wrong owner、stale generation/revision、expired ref は cross-owner existence を漏らさず fail closed します。Hub restart で in-memory public-ref registry は失効し、Agent session loss では managed-job cleanup を開始します。

## Lifetime / revocation

job は最大6時間の hard lifetime と renewable control lease を持ちます。default lease は2分、1回の renew は最大5分です。`managed_job_renew` は毎回 fresh `ManagedJobControl` authorization を通ります。権限 revoke 後は renew できず、最後に発行済みの lease deadline で Agent が cleanup を開始します。

state は `starting`、`running`、`stop_requested`、`completed`、`stopped`、`expired`、`indeterminate_termination` です。`stop_requested` を `stopped` として返しません。completed / stopped / expired は supervised process domain の terminal proof がある場合だけです。

## Process-control guarantee

managed job は bounded process execution と同じ structured-process policy / supervision primitive を再利用します。

- macOS を含む Unix: dedicated process group
- Windows: Job Object

これは lifecycle guarantee であり、filesystem/network sandbox ではありません。Unix process group から deliberate に離脱する挙動は unsupported です。より強い Linux cgroup-v2 containment は #267 の範囲であり、本機能から推論しません。

## Ambiguous termination / recovery

明示 `stop` で terminality を証明できない場合、effectful stop operation は既存の Indeterminate / no-replay / quarantine path に入ります。

lease expiry や session/shutdown cleanup のような非同期停止で terminality を証明できない場合、対応する active northbound operation はありません。そのため Agent は `managed_job_fail_closed = true` を durable checkpoint に保存し、新規 work を拒否し、doctor error を出します。heartbeat 設定とは独立した100ms safety pollで検知します。

この状態は自動解除しません。offline operator recovery は次です。

1. Agent を停止する。
2. orphan managed process が残っていないことを独立に確認する。
3. `v2_maint inspect-managed-job-safety --agent-state-dir ...` で確認する。
4. `v2_maint clear-managed-job-fail-closed --agent-state-dir ... --evidence "..."` で明示解除する。

clear は Agent state-directory lock を取得するため live Agent に対して実行できません。evidence は bounded operator gate であり、argv/cwd/env/output/secrets を含めてはいけません。

## Output / privacy

stream ごとの rolling buffer は最大4 MiB、1回の read は最大64 KiBです。range result は absolute offset と `earliest_available_offset`、`next_offset`、`total_bytes`、`eof`、`gap_before_range`、`history_truncated` を返します。

northbound output は base64 です。default telemetry に raw output、job ref、Agent locator、argv、cwd、environment value、PID、host output path を記録しません。

## Replay / response loss

managed control call は既存 operation ledger を使います。同じ effectful operation ID の再利用は拒否され、自動 replay はありません。

Agent start 後に response が失われた場合、caller は `job_ref` を受け取れず、同じ operation ID を retry してはいけません。job は既発行 lease / hard lifetime により bounded のままです。`get_operation` で replay せず durable status を確認できます。

## Schema compatibility

Issue #106 は control schema 11 / capability schema 7 を使います。outer signed envelope shape は変わらないため Hub-Agent envelope schema は6のままです。control/capability mismatch は fail closed します。Agent checkpoint schema は5のままで、managed-job fail-closed field は historical checkpoint に対して safe default を持つ additive field です。
