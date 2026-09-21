# V2 effectful execution budget

> 英語版が canonical です。[V2_EXECUTION_BUDGET.md](V2_EXECUTION_BUDGET.md) を参照してください。

Issue #319 は semantic command 自身の実行時間と bounded Cua MCP tool timeout の不整合を閉じます。CUMG は、review済み timing parameter だけで backend deadline 内に完了できないことが分かる effectful request を dispatch してはいけません。

## Invariant

1. budget validation は Cua tool call、interaction-session refresh、その他 backend dispatch より前に実行します。
2. budget超過は typed execution_budget_exceeded terminal error です。backend effect を dispatchしていないため retry-safe で、quarantineを作りません。
3. configured Cua tool timeout は hard outer deadline のままです。long command のために global timeout を引き上げません。
4. duration-bearing work の admission 前に backend / IPC / cancellation headroom を予約します。
   - reserve = max(2秒, tool timeoutの20%)。ただし全timeoutを上限とします。
   - effective semantic budget = tool timeout - reserve。
   - required == effective はaccept、required > effective はrejectです。
5. 実際のbackend dispatch後にtimeoutした場合は従来どおり Indeterminate、effectful workをquarantineし、自動retry/replayしません。
6. generic timeout diagnostic にpayload、text、host path、target identityを追加しません。

default 30秒のCua tool timeoutではeffective semantic budgetは24秒です。

## TypeText timing

pinしている Cua Driver 0.19.3 のpaced text pathはUnicode scalar value単位で反復します。そのためCUMGはUTF-8 byte lengthではなく text.chars().count() を使います。

cross-platformで安全側の見積りは次です。

    per_scalar =
        max(
            macOS physical synthesis: 24ms + max(delay_ms, 8ms),
            Windows paced/post-message worst scalar: 28ms + delay_ms,
            Linux XTest conservative scalar: 10ms + delay_ms
        )

    required = scalar_count * per_scalar + 2000ms fixed drain/settle

AX/UIA/foregroundなど高速routeでも、admission後にfallbackし得るため意図的にover-budgetします。legacy TypeText は既存default pacing 30msをbudgetに使います。

2026-09-20 production incidentの約1403 scalar / 30ms / outer timeout 30秒というshapeは、このconservative contractでは約83.374秒必要となるためbackend dispatch前にrejectされます。

MAX_TYPE_TEXT_BYTES は独立したcarrier/input boundとして残ります。byte数をtiming event数としては使いません。

## Duration-bearing capability audit

| Surface | Duration source | Budget |
| --- | --- | --- |
| TypeText | scalar count + default pacing | conservative type-text estimate |
| TypeTextAdvanced | scalar count + delay_ms | conservative type-text estimate |
| PointerDrag | duration_ms | requested duration |
| PointerDragAdvanced | duration_ms | requested duration |
| VerifyUiState | timeout_ms | requested duration |
| VerifyUiStateContextual | timeout_ms | requested duration |
| KeyboardInput | caller duration/repeatなし | semantic duration budgetなし |
| Browser semantic operation | current CUMG surfaceにcaller-controlled wait durationなし | additional budgetなし |
| Process / Shell | supervised executor自身のtimeout_ms | Cua tool timeoutとは別管理 |

将来caller-controlled durationを持つCua semantic commandを追加する場合、このauditへ追加するか独立bounded deadlineを証明する必要があります。

## Timeout ambiguity / diagnostics

dispatch後のCua timeoutだけではOS側effectが起きたか証明できません。したがって以下を維持します。

- Agentはterminal completion evidenceを作りません。
- operationはIndeterminateです。
- exact operationはreplay不可のままです。
- quarantineが新しいeffectful workをfenceします。

#319ではさらに、AgentからHubへsigned / payload-freeなIndeterminateAckを送り、backend_timed_out causeを保持します。authenticated connection、device generation、exact operation IDにbindされますが、これはdiagnostic/quarantine metadataのみで、completed/not-executed claimやreplay authorityにはなりません。

ackがHubへ届いた場合、durable quarantine reasonは後続disconnectでgeneric connection_lostへ劣化せずbackend_timed_outを保持します。ack自体が失われた場合は、従来のconnection-loss処理がconservative fallbackです。

この変更はHub-Agent outer wire variantを追加するため、#314 で HUB_AGENT_SCHEMA_VERSION = 6 への統合と mixed-version fail-closed release acceptance を完了しました。

## Release acceptance

automated coverageは少なくとも以下を証明します。

- effective budget直下 / equal / 超過のboundary。
- UTF-8 byte数ではなくUnicode scalar timing。
- 2026-09-20 incident shapeがbackend dispatch前にrejectされること。
- ordinary short typingに影響しないこと。
- duration-bearing capability auditが明示的であること。
- post-dispatch backend timeoutがIndeterminate/no-replayのままであること。
- signed timeout-cause ackがpayload-free / signature-boundで、durable quarantine reasonにBackendTimedOutを保持すること。
- v0.5 release closeout前にcontrolled delay-bearing text inputのreal-Cua acceptanceを1件実施すること。
