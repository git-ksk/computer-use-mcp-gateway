# V2 `0.4.0` release scope

> この日本語版は [`V2_040_RELEASE_SCOPE.md`](V2_040_RELEASE_SCOPE.md) の翻訳です。**英語版をcanonicalとします。**

Status: **active release candidate scope。`v0.4.0` tag / GitHub Release はまだ一度もshipしていません。**

## Purpose

`0.4.0` は、これまで Recovery & Reconciliation と Multi-principal Identity に分けていたworkを統合します。release boundaryは **Recovery, Identity & Semantic Authorization** です。

この統合はacceptanceを弱めません。CUMGでは次を分離します。

1. **artifact inclusion** — candidateにcodeをcompile/shipできること;
2. **support claim** — optional platform/provider capabilityはexplicit acceptance evidenceが存在する場合だけsupportedと表現すること。

したがってartifactに実装が含まれていてもsupport claimを保留できます。

## Included baseline

`0.4.0` candidateには、すでに`main`で完了したcompatible workを含めます。

- durable Recovery & Reconciliation、permanent no-replay、reviewed current-state/Human recovery (#103, #137, #136, #115, #255);
- recovery/operator hardening/readiness (#253, #254, #256);
- least-privilege filesystem observation-root separation (#104);
- reproducible informational performance benchmark (#111);
- existing `AuthenticatedClientPrincipal` / exact authorizerを利用するprovider-neutral OIDC/JWT caller identity (#139 / PR #269);
- bounded Unix containment investigation (#96)。stronger Linux containmentは#267へ分離済み。

integrated candidateに残る**implementation gate**は#221 typed backend-neutral semantic constraintsです。exact capability authorization、grant signing、Handoff、recovery authority、quarantine、no-auto-replayを独立authorityのまま維持する必要があります。

## Support-claim matrix

| Surface | Candidate state | `0.4.0` support-claim rule |
| --- | --- | --- |
| 既存accepted macOS/single-Mac V2 profile | Accepted baseline | normal release closeoutを満たせばsupported維持可 |
| Recovery/reconciliation core | Implemented/accepted | Included |
| Generic OIDC/JWT identity | implementation + CI merged | #139 physical/dogfood acceptance記録前はsigned-token supportをclaimしない |
| Typed semantic authorization | #221 active | release candidate closeout前にimplementation-complete / green必須 |
| Windows Hello recovery | implementation + CI present | #227 physical acceptance前はWindows online-recovery supportをclaimしない |
| Linux FIDO2 UV recovery | implementation + CI present | #228 physical acceptance前はLinux online-recovery supportをclaimしない |
| Cross-platform recovery parity | #217 open | 実際にsupportedとclaimするplatform setだけを対象にcloseする |
| Cloud Run hosted Hub | design only / NO-GO | `0.4.0` support claimではない。#215 implementation/acceptanceはfuture work |
| Second real computer-use backend | #222 future evidence | `0.4.0`必須ではない。backend-neutral claimはexisting evidenceの範囲に限定 |

## Release closeout gate

`v0.4.0`作成前に次を満たします。

1. #221をmergeし、typed constraint、immutable final-command binding、durable bounded audit evidence、stale-decision fencing、full regression、EN/JA normative docsを完了する。
2. exact candidate commitに対してstanding [`../PRODUCT_READINESS.ja.md`](../PRODUCT_READINESS.ja.md) gateを再実行する。
3. durable/wire schema変更をdocumentし、previous supported minorからのupgrade compatibilityを証明する。incompatible downgrade/rolling mixはfail closedを維持する。
4. exact candidate identityからsource-free release-candidate artifactをbuildし、fresh extraction後verify、clean install/upgrade/paired rollback evidenceをgreenにする。
5. reviewed reference deploymentで`v2_doctor` / `v2_status`をhealthyにし、unresolved quarantine / incompatible runtime-tool stateはfail closedを維持する。
6. recovery/no-auto-replay、Dependency Review、CodeQL、docs/link validation、conformance、release packagingをgreenにする。
7. release noteで上記support-claim matrixを明示する。acceptance pendingのoptional platform/provider implementationをsupportedと書かない。
8. released safety/reliability invariant failureを示す新evidenceがないことを確認する。

## Base artifactをblockしないもの

関連support claimを明確に保留する場合、次はOPENのままでもbase artifactをblockしません。

- #217/#227/#228 physical Windows/Linux recovery acceptance;
- #139 signed-token dogfood acceptance。ただしcandidateでgeneric signed-token identityをnot-yet-supportedと明示する場合のみ;
- #215 Cloud Run implementation/acceptance;
- #222 second-backend proof。

これはsupport claimだけの例外です。core CUMG safety invariantを壊すことが分かっているimplementationをshipしてよいという意味ではありません。

## `0.4.0`の後

working next minorは次です。

- **`0.5.0` — Least-privilege Workspace:** #83, #105, #107。
- **`0.6.0` — Managed Developer Execution:** #106, #114, #267。

Cloud Run #215とsecond-backend proof #222は、numbered releaseへ明示admitするまでevidence-driven future trackとします。
