# V2 Cloud Run Hub support gate

Status: Issue #215 の **design complete / implementation・acceptance pending**。

Cloud Run はまだ CUMG Hub の supported deployment ではありません。既存 single-host / VM Hub profile が引き続き supported model です。この文書は、Cloud Run support claim を出す前に必要な architecture と evidence を固定します。

## Cloud Run 現行仕様の再確認

以下は 2026-09-03 時点で Google Cloud 公式 documentation を再確認した platform fact です。CUMG invariant ではないため、physical acceptance 時にも再確認します。

- Cloud Run service request timeout は default 5分、最大60分です。timeout 到達時は network request が切断されますが、serving container instance 自体が必ず terminate されるわけではありません。reconnect は新しい request であり、同じ instance に戻る保証はありません。
- service instance shutdown 前に `SIGTERM` が送られ、公式 contract は `SIGKILL` まで10秒の graceful-shutdown window を示します。minimum instance も restart され得ます。
- writable container filesystem は disposable / in-memory で、instance replacement を跨いで durability を持ちません。authoritative checkpoint / quarantine / replay barrier / recovery state を置けません。
- service は configured `PORT` をlistenする single ingress container を持ちます。native gRPC は HTTP/2 が必要で、end-to-end HTTP/2 では Google frontend が public TLS を terminate した後、container は `h2c` を受け取ります。
- session affinity は best effort であり、instance termination / unavailable により切れます。execution-safety authority には使えません。
- min/max instance は capacity control であり fencing ではありません。minimum instance はrestartされ得て、revision rollout では old/new revision instance が同時に生存し得るため、「authoritative writer が1つ」の証明にはなりません。

Authoritative reference:

- <https://docs.cloud.google.com/run/docs/configuring/request-timeout>
- <https://docs.cloud.google.com/run/docs/container-contract>
- <https://docs.cloud.google.com/run/docs/configuring/http2>
- <https://docs.cloud.google.com/run/docs/configuring/session-affinity>
- <https://docs.cloud.google.com/run/docs/configuring/min-instances>
- <https://docs.cloud.google.com/run/docs/configuring/max-instances-limits>

## 現行 CUMG との非互換点

現在の Hub は deployment flag の変更だけでは Cloud Run ready になりません。

1. `CheckpointStore` は local filesystem 前提で、private pending file、flush/fsync、no-clobber publication、directory fsync に commit proof を依存します。
2. `v2_hub` は Agent gRPC/TLS と northbound MCP HTTP を別 listener で公開します。Cloud Run ingress は1 portです。
3. 現行 Hub authority は1つの `HubHandle` と process-local coordination に依存し、pending operation も memory 上で管理します。session affinity は replacement / concurrent instance 間の authority になりません。
4. planned shutdown drain default 30秒は、Cloud Run の documented 10秒 shutdown grace より長いです。
5. Agent session lifetime default 3600秒は Cloud Run maximum request timeout と同値で、controlled rotation の platform headroom がありません。

したがって `min-instances=1` / `max-instances=1` は non-authoritative PoC には使えても support 根拠にはできません。

## Hosted architecture 必須要件

### 1. Provider-neutral durable authoritative state

hosted profile support 前に durable Hub-state seam を導入します。すべての provider を runtime swap 可能にする必要はありませんが、CUMG execution model が POSIX filesystem publication semantics を前提にしてはいけません。

backend は device generation、operation ownership、dispatch state、terminal receipt、`Indeterminate` quarantine、permanent replay tombstone、recovery state、compatibility metadata を含む authoritative Hub snapshot 全体について、現行以上の durability を提供する必要があります。

最低要件:

- monotonic revision/version 付き exact current-state read;
- expected revision に対する transactional compare-and-commit;
- monotonically increasing writer/fencing epoch allocation;
- every authoritative mutation を expected state revision + current writer epoch の両方にcondition;
- complete new authoritative state の atomic publication または no publication;
- authority changed と見なす前の durable read-after-commit;
- latest read failure 時に older committed state へ自動fallbackしない;
- bounded / explicit backup・restore・schema migration;
- persistence unavailable / ambiguous / partial failure を fail closed。

Cloud Storage / mounted filesystem を自動的に equivalent と見なしません。採用する場合は別途 proof が必要です。

### 2. Effect dispatch 直前の fencing

古い Cloud Run instance が old Agent stream を保持しているだけで effect dispatch できてはいけません。

すべての effectful operation で、authoritative admission/dispatch transition を **southbound dispatch 直前に current writer epoch 条件付きで durable commit** します。別 instance が writer epoch または state revision を進めて compare-and-commit が失敗した場合、stale instance は dispatch できません。

terminal settlement、quarantine/recovery mutation、generation change、replay-barrier mutation も同じ epoch/revision condition を使用します。process-local `pending` は cache/coordination aid として残せますが source of truth にはできません。

これが session affinity / `max-instances=1` だけでは不十分な主理由です。

### 3. Hosted Agent stream profile

Cloud Run request timeout と CUMG session lifetime は別 clock とします。

initial hosted acceptance profile:

- Cloud Run request timeout: `3600s`;
- CUMG maximum Agent session lifetime: **`3300s`（55分）**;
- existing pre-expiry reauthentication drain: **`30s`**。

CUMG側で platform request deadline の約5分前にstreamを閉じ、normal rotation は existing semantics の fresh authenticated handshake + generation advance を必須とします。

5分 margin は operational headroom であり safety proof ではありません。Cloud Run はそれ以前にもdisconnectできます。platform timeout、instance replacement、network loss、unexpected stream close はすべて ordinary transport loss であり、success、replay authority、automatic quarantine clear にはなりません。

### 4. Hosted shutdown contract

Cloud Run profile は local default 30秒 drain が `SIGTERM` 後に完了することへ依存できません。

initial hosted profile では application drain を **最大8秒** とし、直ちにnew admissionを閉じ、残るplatform graceはstream/server teardownへ使います。ただし安全性は8秒すべて利用できることにも依存しません。

- durable dispatched marker 前のworkはrestart後もdispatchしない;
- durable dispatched済みだがauthoritative terminal proofがないworkは conservative に `Indeterminate` / quarantine としてrestore可能;
- forced termination は completion を生成せず replay をauthorizeしない;
- restart は external durable backend から exact replay barrier / quarantine を復元する。

extra quarantine はacceptable conservative failureですが、ambiguity loss は不可です。

### 5. One-port protocol multiplexing

supported Cloud Run Hub は Cloud Run `PORT` 上の reviewed ingress service 1つを使います。recommended direction は application-level HTTP/2/h2c listener で closed protocol surface だけをrouteすることです。

- Agent gRPC service method -> existing Agent application authentication/device identity + signed protocol semantics;
- northbound MCP path -> existing OAuth/trusted-proxy principal authentication + exact CUMG authorization;
- documented health/metadata path -> existing coarse/read-only policy。

 generic pass-through route は作りません。

Cloud Run が public TLS を terminate するため、hosted ingress は現在の private `v2_hub` TLS listener shape に依存できません。ただし Agent identity は弱まりません。Agent application-level Ed25519 identity/enrollment は transport TLS と独立したままです。hosted profile は Google frontend trust、service内 h2c、northbound HTTPS resource identity を明示します。

### 6. Instance count / concurrency は authority ではない

initial operational profile は cost/predictability のため `min-instances=1` / `max-instances=1` を使用しても構いませんが、acceptance は rollout/replacement が作り得る **2つの同時live Hub revision/instance** で安全性を意図的に証明します。

exact Cloud Run concurrency value は acceptance artifact に記録します。これは capacity/latency setting であり security boundary ではありません。値を変更しても single-writer fencing / no-replay behavior が変わってはいけません。

### 7. Secret / observability / recovery

supported profile は以下も document / accept します。

- secret value をlogしない managed secret/key provisioning;
- persistence failure、writer-fence loss、Agent disconnect、quarantine、repeated stream-rotation failure の coarse health/alert;
- instance replacement 時の OTLP behavior;
- durable-state backup/restore + schema migration;
- old/new binary coexist を含む revision rollout / rollback;
- security/recovery invariant と分離した cost guidance。

## Support gate

以下のevidenceが揃うまで Cloud Run support は **NO-GO** です。

| Gate | Current status |
| --- | --- |
| 現行 Cloud Run limit 再確認 | Design evidence complete (2026-09-03) |
| ephemeral filesystem を authoritative state から排除 | Design decision complete / implementation pending |
| provider-neutral durable Hub-state backend | Pending |
| monotonic writer fencing + stale-writer dispatch denial | Pending |
| one-port h2c gRPC + MCP ingress / separate auth boundary | Pending |
| 3300s proactive Agent stream rotation acceptance | Pending |
| <=8s hosted drain + forced-kill fail-closed acceptance | Pending |
| concurrent old/new revision fencing test | Pending |
| replacement後 durable quarantine/replay-barrier restore | Pending |
| hosted deploy/upgrade/rollback/backup/alerting runbook | Pending |
| physical Agent + real Cua interrupted-effect acceptance | Pending |

これらがopenの間、既存 VM/single-host deployment は unchanged / supported のままです。

## Acceptance scenario

#215 close 前に最低限以下を実施します。

1. 同じdurable stateに2 Hub instanceを起動し、current writer epochだけがcommit/dispatchできることを証明;
2. dispatch commit前にcurrent writerをterminateし、後からdispatchされないことを証明;
3. durable dispatch直後・terminal proof前にterminateし、restartでexact `Indeterminate` quarantine、no replayとなることを証明;
4. existing reviewed recovery pathでquarantineを解消し、old operationがpermanent non-replayableであることを証明;
5. 3600秒より前にAgent streamをrotateし、fresh handshake/generation semanticsを証明;
6. forced request/transport lossをsuccessful session completionとして扱わないことを証明;
7. revision A/B同時live rolloutでstale Aがmutation/dispatchできないことを証明;
8. single hosted ingressでAgent gRPC / northbound MCPをrouteし、双方が相手側credential/routeを拒否することを証明;
9. durable backend backup/restoreでexact quarantine/replay barrierが維持されることを証明;
10. physical Agent + real Cuaでdeliberately interrupted effectを再実施。

hosted availability 改善を理由に commit-before-authority-change、`Indeterminate`、quarantine、no-auto-replay contract を弱めてはいけません。
