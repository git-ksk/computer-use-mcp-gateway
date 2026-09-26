# V2 hosted Cloud Run acceptance

Status: Issue #284 の **real hosted execution pending**。v0.9.0 Cloud Run support gate の bounded な公開evidence recordであり、作成しただけでは Cloud Run を supported にしません。

Canonical design: [../V2_CLOUD_RUN_HUB.ja.md](../V2_CLOUD_RUN_HUB.ja.md)

## Evidence boundary

公開evidenceはexact tested software / safety outcomeをbindしますが、credential、raw endpoint/project identifier、secret-store locator、Handoff locator/session material、network address、frame、Human input、application contentは公開しません。provider固有deployment commandとsecret-bearing artifactはprivate operator workspaceに保持し、公開recordにはbounded metadataとcryptographic digestだけを残します。

## Exact run identity

| Field | Required evidence |
| --- | --- |
| CUMG | exact 40-hex `main` commit + release-manifest digest |
| Handoff | exact reviewed commit + package version |
| Cloud Run image | tagだけでなくimmutable image digest |
| schemas | control / capability / registry / Hub-Agent / execution-safety / Agent-M1 version |
| PostgreSQL | provider class / major version / region class / connection mode。endpoint/project名はprivate |
| deployment config | reviewed non-secret config digest |
| secrets | opaque version/fingerprintのみ。value / secret-store pathは禁止 |

## Required real-hosted scenario

各rowはrun identifier / timestamp window / exact tested revision / bounded outcomeで裏付けます。

- one-port h2c ingressで MCP / Agent gRPC / hosted Handoff のauthentication boundaryを分離;
- revision Bがnewer durable writer epochを取得し、still-live Aはcommit/dispatch不可;
- pre-dispatch terminationしたoperationが後からdispatchされない;
- terminal proofなしのpost-dispatch interruptionがreplacement後もexact `Indeterminate` + quarantine;
- interrupted old operationは永久にnon-replay;
- 3300s proactive Agent rotationでfresh authentication + generation advance;
- forced request/transport lossをsuccessful completionとして扱わない;
- hosted drainはnew admissionを閉じ <=8s、forced terminationからsuccess/replayを生成しない;
- PostgreSQL outage中はauthoritative mutation/dispatch不可、valid durable read + fresh epoch後のみrecovery;
- secret/key rolloutでauthorityを二重化せず、expired/revoked credentialがsettlement/replayにならない;
- reviewed log/OTLPにsentinel secret valueやprohibited payload/locator materialがない;
- backup/restoreでrevision lineage / quarantine / replay barrierを保持し、fresh writer epoch後にserve;
- Hub-Agent partitionがAgent authority復元やquarantine clearにならない;
- physical hosted Handoffで Human active -> Agent deny -> Done -> fresh verification -> explicit resume;
- viewer/WebRTC/WSS/TURN generation replacementがHuman inputをreplayせず、Agent/Handoff authority epochも変えない;
- rollbackはfresh deployment/writer acquisitionであり、old writer lease / generation / route lease / signerを復活させない。

## Public run record

```text
date:
cumg_commit:
image_digest:
handoff_commit:
handoff_package:
postgres_provider_class:
postgres_major:
region_class:
connection_mode:
cloud_run_request_timeout_seconds:
cloud_run_concurrency:
cloud_run_min_instances:
cloud_run_max_instances:
private_config_digest:
private_evidence_bundle_digest:
revision_a_opaque_id:
revision_b_opaque_id:
result: PASS | FAIL
failed_rows:
notes:
```

`revision_*_opaque_id` は公開可能なbounded label/digestとし、Cloud project/revision URLやsecret-store locatorを入れません。

## Acceptance authority

deterministic core testは必要条件ですがreal-hosted rowの代替ではありません。required rowが全greenとなり #215/#277/#353/#284 がevidence付きでcloseするまで Cloud Run は **NO-GO**。hosted Handoffをgreenにする前に upstream `mcp-execution-handoff#19` / `#12` もacceptedである必要があります。
