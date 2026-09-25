# V2 Cloud Run Hub support gate

Status: Issue #215 の **design complete / #282 ingress merge済み / #283 durable-state・writer-fence coreはv0.9.0 candidate branchで実装済み / hosted provider・deployment/physical acceptance pending**。

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

## 残る hosted gap

deployment flag の変更だけではまだ Cloud Run ready になりませんが、#282 と #283 automated core により以前のarchitecture blockerの一部は解消しています。

1. #283でauthoritative persistenceをprovider-neutral state-store/CAS contractへ分離し、`CheckpointStore`はlocal backendとして維持します。hosted profile用production external durable providerは未選定で、同じconformance contractを満たす必要があります。
2. #282はmerge済みで、hosted modeはAgent gRPC / MCP / hosted Handoffをexact h2c `PORT` 1つで処理できます。VM/single-host listener layoutは変更しません。real Cloud Run ingress acceptanceは未完です。
3. #283 coreではsession affinity/process ownershipではなくdurable writer epoch/revisionをauthorityにします。selected external providerでのreal concurrent hosted revision acceptanceはまだ必要です。
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

#283 core では provider-neutral な `HubAuthoritativeStateStore` contract、local `CheckpointStore` adapter、deterministic in-memory conformance provider を実装しています。contract は monotonic state revision + writer epoch を持ち、両方に対する compare-and-commit と、live authority を進める前の complete committed snapshot read-back verification を必須にします。local publication は append-only checkpoint sequence を physical CAS boundary として使用します。historical Hub M1 schema 5/6 は unfenced migration input としてreadableのまま、最初のHub writer acquisitionでexplicit durable fence付きHub M1 schema 7をpublishします。schema 7なのにfenceが欠落したcheckpointはlegacy扱いせず拒否します。

automated coreでは two-writer replacement、local cross-instance CAS race、provider unavailable時のpre-dispatch fail-closed、old live Agent streamを保持するstale writerのdispatch拒否、durable dispatch後のrestartでexact `Indeterminate` quarantine復元、stale recoveryからのquarantine clear拒否も証明します。これはcore semanticsの証拠であり、#283でCloud Run/database固有durable providerを選定しません。real hosted revision overlap / backup / physical acceptanceは#284/#215に残ります。

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

merge済みPR #285ではhosted profile candidateをCloud Run `PORT`上の1つのapplication-level HTTP/2/h2c listenerとして実装します。explicit `CUMG_V2_HOSTED_PROFILE=true` の場合だけ有効で、通常のVM/single-host listener layoutは変更しません。

shared listenerは `HostedIngressClassifier` が許可するclosed surfaceだけへrouteします。

- exact Agent gRPC `OpenSession` -> 既存Tonic `AgentControlServer` と不変のapplication-level Ed25519/device protocol;
- exact protected MCP resource + RFC 9728 metadata path -> 既存northbound OAuth/OIDC/introspection router と exact CUMG authorization;
- exact hosted Handoff `/context` / `/control` / RFC 9728 metadata path -> #277 operator OAuth resource と exact principal/device/action authorization。

hosted startupではGoogle frontendがpublic TLSを終端しcontainer内はh2cとするため、Hub TLS certificate/key fileと `CUMG_V2_MCP_BIND` の併用をfail closedで拒否します。public hosted profileではtrusted-proxy authも拒否します。MCPとhosted Handoffはdistinct protected resource URIを必須とし、OIDC modeではaudienceもdistinctにします。OAuth introspection modeでは各tokenをexact resource URIに対して検証します。Agent Ed25519 identityはtransport TLSから独立したままです。

candidate startup contractは `CUMG_V2_HOSTED_PROFILE=true`、Cloud Run `PORT`、既存のcomplete MCP resource/policy/OAuth configurationに加え、`CUMG_V2_HOSTED_HANDOFF_RESOURCE`、`CUMG_V2_HOSTED_HANDOFF_REQUIRED_SCOPES`、`CUMG_V2_HOSTED_HANDOFF_POLICY_FILE` を必須とします。OIDC/JWT modeではさらに `CUMG_V2_HOSTED_HANDOFF_OIDC_AUDIENCE` を必須とし、`CUMG_V2_OIDC_AUDIENCE` とdistinctにします。introspection modeではHandoff OIDC audience設定を拒否します。

generic pass-through/fallback proxyはありません。unknown/near-match path、unsupported method、cross-surfaceのwrong content typeはselected serviceへ入る前に拒否します。このcandidateはCUMG `/healthz` routeを公開しないため、healthはunauthenticated application endpointではありません。protected-resource metadataはexact GET-onlyです。`tests/v2_hosted_one_port.rs` でreal h2c listener上のTonic gRPC + MCP/Handoff HTTP/2同居とcross-surface delivery拒否を証明します。

### 6. Hosted Handoff composition

Handoff-enabled hosted profile は [`V2_HOSTED_HANDOFF_TOPOLOGY.ja.md`](V2_HOSTED_HANDOFF_TOPOLOGY.ja.md) の Agent-owned composition も満たす必要があります。Hub-local Unix operator socket は single-host/VM deployment では引き続き有効ですが、hosted operator interface にはしません。hosted lifecycle control は normal MCP tool discovery と別に authentication/authorization し、caller に PID/window authority を生成させず、bounded かつ fenced な control だけを Agent-owned canonical Handoff runtime へrelayします。

Human media/input と STUN/TURN/provider credential は CUMG authoritative state の外側に維持します。viewer/transport generation は Agent generation / Handoff epoch と分離し、Hub replacement が hosted routing metadata から Human/Agent authority を復元することはありません。old Agent stream や stale permissive Handoff cache を保持する stale hosted instance も、上記と同じ writer-epoch/revision fence により effect dispatch 前に拒否します。

### 7. Instance count / concurrency は authority ではない

initial operational profile は cost/predictability のため `min-instances=1` / `max-instances=1` を使用しても構いませんが、acceptance は rollout/replacement が作り得る **2つの同時live Hub revision/instance** で安全性を意図的に証明します。

exact Cloud Run concurrency value は acceptance artifact に記録します。これは capacity/latency setting であり security boundary ではありません。値を変更しても single-writer fencing / no-replay behavior が変わってはいけません。

### 8. Secret / observability / recovery

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
| ephemeral filesystem を authoritative state から排除 | core seam complete。hosted external durable providerはpending |
| provider-neutral durable Hub-state backend | #283 core実装済み: trait + local adapter + deterministic conformance provider。hosted external providerはpending |
| monotonic writer fencing + stale-writer dispatch denial | #283 automated core green。hosted revision-overlap acceptanceはpending |
| one-port h2c gRPC + MCP + hosted Handoff ingress / separate auth boundary | PR #285 merge済み。local h2c integration green、real hosted acceptanceはpending |
| 3300s proactive Agent stream rotation acceptance | Pending |
| <=8s hosted drain + forced-kill fail-closed acceptance | Pending |
| concurrent old/new revision fencing test | deterministic two-writer core green。real hosted revision A/B acceptanceはpending |
| replacement後 durable quarantine/replay-barrier restore | core replacement/restart regression green。hosted backup/restore acceptanceはpending |
| hosted deploy/upgrade/rollback/backup/alerting runbook | Pending |
| Hosted Handoff operator/routing + Agent-owned authority composition | #275 design / #276 pin / #277 operator-routing; implementation・acceptance pending |
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
11. hosted operator/routing path で Handoff を有効化し、Hub replacement / viewer reconnect / transport fallback が Agent/Human authority を復元できないことを証明したうえで、physical Agent 上で Human active -> Agent deny -> Done -> fresh verification -> explicit resume を完了する。

hosted availability 改善を理由に commit-before-authority-change、`Indeterminate`、quarantine、no-auto-replay contract を弱めてはいけません。
