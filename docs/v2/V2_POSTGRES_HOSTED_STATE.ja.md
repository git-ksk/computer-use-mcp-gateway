# V2 PostgreSQL hosted Hub state

Status: Issue #391 のimplementation-enabling contractです。このbackendは#284 Cloud Run acceptanceを可能にしますが、それだけでCloud Run NO-GO support decisionは変更しません。

## Purpose

Cloud Run instanceはreplaceableで、container writable filesystemはauthoritative stateにできません。hosted Hubはcomplete authoritative Hub snapshotをPostgreSQLへ保存し、VM/single-host deploymentは既存local CheckpointStoreを維持します。

PostgreSQL backendは#283と同じcontractを実装します。

- stable state keyごとにcomplete HubPersistentStateを1 row保存;
- monotonic logical revision;
- monotonic writer epoch;
- expected revision / epoch双方一致時のみcompare-and-commit;
- live authorityを進める前にdurable commit;
- exact read-after-commit verification;
- stale writerはmutation/dispatch不可;
- ambiguous COMMIT / post-COMMIT verificationはReadAfterCommitMismatchとしてwriterを永久fence;
- COMMIT前provider unavailableではcandidateをpublishしない。

## Runtime topology

CUMG_V2_HOSTED_PROFILE=trueではv2_hubにPostgreSQL設定が必須です。

- CUMG_V2_POSTGRES_HOST: TCP hostまたはUnix socket directory。Cloud SQLではmounted `/cloudsql/PROJECT:REGION:INSTANCE` directoryを利用でき、remote PostgreSQL serviceではreview済みDNS hostnameと `CUMG_V2_POSTGRES_TLS_MODE=verify-full` を使用します。
- CUMG_V2_POSTGRES_PORT: default 5432。
- CUMG_V2_POSTGRES_DATABASE: database名。
- CUMG_V2_POSTGRES_USER: runtime DB role。
- CUMG_V2_POSTGRES_PASSWORD_FILE: optional private password file。password byteをinline CUMG environment variableとして受け取りません。
- CUMG_V2_POSTGRES_STATE_KEY: deployment-owned stable row key。Hub revision rolloutやAgent key rotationを跨いで変更しません。
- CUMG_V2_POSTGRES_CONNECT_TIMEOUT_SECS: bounded connect timeout。
- CUMG_V2_POSTGRES_QUERY_TIMEOUT_SECS: bounded statement / lock timeout。

serving processはdatabase schemaをcreate/migrateしません。

## Migration / least privilege

hosted Hub起動前に、separate migration identityでpackaging/postgres/001_hosted_hub_state.sqlを適用します。

serving roleに必要なのは以下だけです。

- database CONNECT;
- target schema USAGE;
- cumg_hub_stateへのSELECT / INSERT / UPDATE。

CREATE、ALTER、DROP、DELETE、TRUNCATE、ownership、superuser、schema migration authorityは不要です。

operator手順:

1. admin/migration identityでdatabaseを作成/選択;
2. packaging/postgres/001_hosted_hub_state.sqlを適用;
3. runtime login/roleをHub process外で作成;
4.不要privilegeをrevokeし、CONNECT / USAGE / SELECT / INSERT / UPDATEだけgrant;
5. password auth使用時はreview済みmanaged-secret file boundaryからDB passwordを供給;
6. stable state keyで1 revisionをdeployしinitial writer epoch/revision作成を確認;
7. その後#284でoverlapping revision acceptanceを実施。

## Bootstrap

empty tableはvalidです。最初のhosted Hubはconfigured state keyにrevision 1 / writer epoch 1のrowをexactly one作成し、直後にread verifyします。

後続Hub processはcomplete stateを維持したままrevisionとwriter epochをatomic incrementしてauthorityを取得します。process lifetime、Cloud Run revision name、instance count、session affinity、DB credential versionをwriter authorityにはしません。

## Rollout / rollback

revision BはAと同じdatabase / stable state keyを使います。Bはauthoritative mutation / effect dispatch前にnewer writer epochを取得します。Aがaliveでold Agent streamを保持していても、それ以降のauthoritative commitはstaleとして失敗します。

rollbackはold process authorityの復活ではなくnew deploymentです。rollback revisionもsame state keyへconnectし、さらにfresh writer epochを取得します。live newer rowへolder DB rowを上書きしません。

schema rollbackはtarget binaryがcurrent Hub state schemaと明示compatibleな場合だけ許可します。それ以外はnewer binaryを維持するかoffline reviewed migrationを行います。

## Failure classification

COMMIT前のconnection/query/lock failureはprovider unavailableで、candidateはauthoritativeではありません。

COMMIT開始後のtimeout/errorはambiguousとしてReadAfterCommitMismatchに分類し、writerをfenceします。replacement processがexact durable stateを読みnewer epochを取得する必要があります。

successful COMMIT後も、read-backがmissing/malformed/schema incompatible/unequalならReadAfterCommitMismatchです。

CUMG 1 MiB checkpoint payload ceilingはDB mutation前に強制し、migration側にも同じpayload checkを置きます。

## Backup / restore boundary

#391はdurable providerを提供しますが#284 backup/restore acceptanceをclaimしません。hosted supportには、writerがconcurrent overwriteできない状態でreal provider backup/restoreまたはequivalent PostgreSQL backup procedureを#284で証明します。

restoreではcomplete row payload、revision、writer epoch、quarantine、replay barrierを保持します。restore後最初のHubはserve前にnewer writer epochを取得します。

## Automated evidence

- #283 deterministic writer-fence / restart-quarantine regressionを維持;
- async hosted-store Hub testでstale live-stream dispatch denialを証明;
- async ambiguous-commit testでwriter fence + replacement後Indeterminate quarantine復元を証明;
- tests/v2_postgres_hub_state.rsをreal PostgreSQL serviceで実行し、successive epoch、stale writer拒否、exact one-winner CAS race、schema-7 durable fence round-trip、mutation前oversize拒否を証明;
- CI Rust jobでPostgreSQL 17 serviceをprovision;
- local CheckpointStore / VM single-host startupは変更しない。

## External PostgreSQL acceptance boundary

#284が引き続きreal Cloud Run / external PostgreSQL evidenceを所有します。

- exact PostgreSQL provider/deployment identity / version / region / connection mode;
- runtime service-account/IAM boundary;
- managed-secret passwordまたはreview済みalternative auth;
- concurrent Cloud Run revision A/B overlap;
- dispatch前後forced termination;
- backup/restore;
- secret rotation/log inspection;
- partition / Handoff lifecycle;
- rollback/recovery / alerting。

#284/#215がgreenになるまでCloud Runはunsupportedです。
