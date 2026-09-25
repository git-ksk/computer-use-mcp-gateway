# V2 Hosted Secret / Key Rotation Acceptance

Status: **automated core は green。Cloud Run support を NO-GO から変更する前に、#353/#284 で real hosted revision A/B acceptance が必要です。**

このcontractはsecret rotationとexecution authorityを分離します。capacity設定、session affinity、secret version、process lifetimeはいずれもwriter authorityではなく、hosted mutation fenceはdurable Hub writer epoch/revisionです。

## Rotation matrix

| boundary | stable invariant | rotation rule | safety effect |
| --- | --- | --- | --- |
| Hub application key (`CUMG_V2_HUB_SECRET_FILE`) | Agent trustは1本のcontinuity chain | old+new signed continuity proof + monotonic rotation epoch | old Hub writer leaseを復活させない |
| Agent device key | logical `device_id`を維持 | old+new proof、current session invalidate、newer device generationでreconnect | terminal / `Indeterminate` / quarantine / replay stateは不変 |
| Grant-signing key | grant capability contract / max lifetimeを維持 | bounded old/new verifier overlap後old retire | retired signerはnew grantをauthorizeできずoperation replayもしない |
| OAuth introspection client secret | issuer/resource/principal mappingを維持 | reviewed managed-secret provisioningでprocess credentialとしてrotate | authentication credentialのみでexecution authority/durable Hub stateではない |
| OIDC/JWKS signing key | exact issuer/audience/resource/algorithm policy | external IdPの`kid` rotationをbounded JWKS refresh/cacheで処理 | stale/unknown keyはauthentication failure、token lossをexecution successにしない |
| audit fingerprint secret | non-authoritative correlation | 独立rotation | key跨ぎcomparisonはunavailableになりsettlement/replay authorityにならない |
| Handoff viewer/transport material | Agent generation / intervention epochを分離 | viewer/transport generationだけ独立rotate | disconnect/fallbackはHuman `Done`ではなくAgent authorityを復元しない |
| recovery private authority | local/operator recoveryの別boundary | separately reviewed administrative rotation | hosted service identityで代替しない |
| container TLS private key | hosted profileではなし | public TLSはHub process外でterminate | hosted modeではHub TLS key/cert fileを引き続き拒否 |

secret-store/workload identity、OAuth credential、Hub/device identity、grant signing、Handoff authority、durable writer authorityは別trust domainです。1つのrotationで別authorityをwidenしてはいけません。

## Automated acceptance

`tests/v2_hosted_secret_rotation.rs` は既存production primitiveをcompositionし、以下を証明します。

1. 2つのhosted revisionはsuccessive writer epochを取得し、old epochはcommit不可;
2. device-key rotationでold session/keyをinvalidateしても、既にambiguousなoperationはrestart後もexact `Indeterminate` + quarantineを保持しreplay拒否;
3. Hub trust continuityとgrant-signing overlap/retirementは独立rotation;
4. viewer/transport replacementはそのHandoff generationだけrotateし、Agent generation/intervention epochは不変、stale transportは利用不能;
5. OAuth introspection configのDebug出力はclient-secret valueをredact。

既存の`v2_m1_hub_service` session reauthentication、`v2_m1_partition_recovery`、#283 writer-fence/replacement、#277 Handoff routing regressionもevidence setに含めます。

## Hosted rollout contract

real hosted deploymentでは、既存`*_SECRET_FILE` interfaceと互換なreview済みmanaged-secret boundaryからsecret byteを注入します。secret valueをordinary environment variable、revision label、image layer、command line、source control、runtime manifest、durable Hub checkpoint、Handoff routing state、normal audit/telemetryへ置きません。

revision Bはnew credentialを受け取り、authoritative mutation前にnewer durable writer epochを取得し、Aがold Agent streamを保持していても以後commit/dispatchできないことを証明します。old credential/sessionのexpire/revokeをcompletion/replayへ変換せず、`Indeterminate`、quarantine、replay barrierをreplacement越しに保持します。rollbackもfresh writer epochを取得し、old lease、Agent generation、Handoff route、retired grant signerを復活させません。

## Real hosted evidenceの残り

#353は、#284でselected external durable-state provider / managed-secret mechanismを使ったreal revision A/B rollout、concurrent old/new revision、old credential/sessionのexpire/revoke、interrupted effectful operation、Handoff generation分離、rollback/recovery手順、sentinel secret valueがlog/OTLPへ出ないことを記録するまでopenのままです。

そのartifactがgreenになるまでCloud Runはunsupportedです。
