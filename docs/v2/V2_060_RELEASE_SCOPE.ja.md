# V2 v0.6.0 release scope / acceptance

> English版がcanonicalです: [V2_060_RELEASE_SCOPE.md](V2_060_RELEASE_SCOPE.md)

Release: **v0.6.0 — Managed Developer Execution**

## release scope

- #106: separate authorityを持つmanaged long-running developer job。lease / hard lifetime / concurrency / output / opaque ref / explicit stopをbounded化し、async termination ambiguityはfail closed。
- #114: separate authorityを持つsandboxed Playwright/E2E。operator-configured Docker/Podman-compatible provider、digest-pinned image、network none、read-only workspace、private writable artifact、sanitized env、resource ceiling、provider absence proof、ambiguity時no-replay。
- #267: Linux限定optional cgroup-v2 process/shell hardening。explicit reviewed delegation、pre-exec placement、kernel `nsdelegate` cgroup-namespace containment、`cgroup.kill`、`populated 0` proofが成立する場合だけ有効。portable Unix/macOS process-groupとWindows Job Object claimは不変。
- #335: final integration / compatibility / artifact / upgrade / dogfood / docs / Product Readiness gate。

managed jobはgeneric shell/processからのpersistence escapeではなく独立capabilityです。Playwright authorityもmanaged jobと分離します。Linux cgroup containmentはprocess lifecycle hardeningであり、Playwright/filesystem/network sandboxではありません。

## release identity / compatibility

v0.6.0 snapshot:

- crate/package **0.6.0**
- control schema **12**
- capability schema **8**
- persisted device registry **8**
- Hub-Agent transport **6**

v0.5.0はcontrol 10 / capability 6 / registry 8 / Hub-Agent 6、#106 development historyは11/7です。registry restoreはreview済みhistorical pairingだけを受理し、stale capability advertisementを破棄してdeviceをoffline化し、current Agentのfresh advertisementを要求します。live old/new schemaのrolling mixはfail closedです。

したがってv0.5.0→v0.6.0は**paired Hub/Agent/maintenance/runtime upgrade**であり、mixed-version rolling deploymentではありません。Playwright / Linux-cgroup設定はoptionalでdefault absentのため、valid v0.5.0 configがupgradeだけでauthorityを広げることはありません。

v0.6.0がcurrent capability-8 stateを書いた後にv0.5.0へrollbackする場合は、pre-upgrade v0.5.0 checkpointとversion-paired v0.5.0 binaries/configを復元します。v0.5.0 binaryへv0.6.0 checkpointを再解釈させません。state hand-editやtag移動は禁止です。

## acceptance evidence

- #106 / #114 / #267 は本gate前にclosed。
- local deterministic gateはall-target fmt/check/clippy/test、docs/link、schema migration/mixed-version、release candidate manifest、artifact install/upgrade/rollback、V1 regression/conformanceを含む。
- #267 Linux CIはreal delegated cgroup-v2 subtreeで`setsid()` cleanup、fork race cleanup、`nsdelegate` migration containment、bad delegation unavailable、portable process-group regression不変を検証。
- managed-job lifecycleはtrusted physical macOSのreal process supervision domainでdogfoodし、start/renew/stop/lease/output lifecycleにbackground-shell escapeを使わない。
- Playwright real-provider acceptance は trusted Apple-silicon Mac 上で Docker 29.5.3、`@playwright/test` 1.63.0、digest-pinned arm64 image `sha256:a0f4498920a5dbac63196d9140ed738ef00470f27e2e74029abd8850b7bd5717` を使ってPASSしました。real offline Chromium test が fixed `network none` profile で完走し、provider cleanup proof 後にのみterminal successとなることを確認しています。compiled surfaceだけで unavailable provider を supported とみなしません。
- Release Candidate ArtifactsはLinux/macOS/Windowsでexact release identityをbuild/verify/smoke。CI archiveはevidenceでありofficial binary installerではない。
- macOS/Windows process/shell、Handoff、recovery、quarantine、no-auto-replay regressionは引き続きrequired。

## Product Readiness snapshot

standing [Product Readiness checklist](../PRODUCT_READINESS.ja.md)をexact release scopeへ適用します。

- distribution identityはrelease-manifest v3 / installed runtime-manifest v4でexact commit/package/schemaへbind。
- clean install / upgradeはfail closedで、trust/secretsとplatform service-manager inputsはdocumented pre-provisioned input。
- authoritative state / rollback / quarantine / no-auto-replayはversion-pairedのまま。publication metadataをauthorityにしない。
- diagnosticsはprivacy-boundedで、command text / host path / secret / Playwright grep/test body / provider ID / cgroup pathをnorthboundへ出さない。
- optional provider/platform hardeningはlocal startup probe成功後だけadvertise。
- exact release commitでdependency review / CodeQL / docs links / native candidate packaging / protected CI greenをtag前に要求。

## support-claim boundary

compiled surfaceとsupport claimを分離します。

- #139 signed OIDC/JWT verification implementationはcompiledだが、provider-specific physical signed-token supportはdogfood acceptance完了まで**withheld**。
- #217 cross-platform recovery parityは**open**のままで、v0.6.0からblanket parity claimを作らない。
- #228 Linux FIDO2 UV recoveryはtrusted physical Linux + real UV-capable FIDO2 acceptance完了まで**withheld**。
- Hosted Cloud Run Hub/Handoff (#215/#275-#284) はunsupported / out of scope。
- #289/#290 recovery research、#222 second-real-backend proof、unrelated operator UXはrelease gate外。

## publication boundary

`v0.6.0`はseparately reviewed binary asset / SBOM-license inventory / provenance-attestationを添付しない限り**source-only GitHub pre-release**です。verified CI release-candidate archiveはrelease evidenceのみ。

immutable `v0.6.0` tagとGitHub pre-releaseはrelease PR merge後、exact resulting `main`のrequired push checks / Release Candidate Artifactsがgreenになってから作成します。
