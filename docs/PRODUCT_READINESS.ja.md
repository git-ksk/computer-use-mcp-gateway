# Product Readiness gate

> English canonical: [PRODUCT_READINESS.md](PRODUCT_READINESS.md)

この文書は Issue #213 で確立した恒久的な release-readiness checklist です。これは **release gate** であり feature backlog ではありません。supported distribution/deployment scope 上で明示的に対象外と判断できる項目だけを N/A にできます。製品化のために CUMG の execution-safety、quarantine、no-auto-replay、authorization、privacy boundary を弱めてはいけません。

各 release PR では以下の checklist を PR または linked acceptance record にコピーし、具体的 evidence を添付します。近い version や source-tree dogfood だけから readiness を推測しません。

## Release checklist

### Distribution / release integrity

- [ ] platform/profile ごとの distribution scope を official installable artifact / reviewed candidate evidence / source-supported only として明示する。
- [ ] published archive は deterministic checksum と closed manifest を持ち、unexpected/missing file、unsafe path、symlink、digest drift を fail closed にする。
- [ ] release artifact に credential、private endpoint、temporary acceptance data、repository tree、unrelated build product を含めない。
- [ ] signing/notarization claim を実態と一致させる。実装・acceptance 前の CI candidate を notarized/official と表現しない。
- [ ] official binary release では [`VERSIONING.ja.md`](VERSIONING.ja.md) が要求する reviewed SBOM/license inventory と provenance/attestation strategy を満たす。

### Install / upgrade / rollback / durable state

- [ ] 少なくとも1つの supported reference topology に、activation 前に artifact/config/trust を検証し healthy diagnostics で終わる documented first-install path がある。
- [ ] normal upgrade は version-paired Hub/Agent/`v2_maint`/recovery/Handoff component を使い、適用対象では durable one-shot maintenance transaction を維持する。
- [ ] wire/durable-state compatibility を変更する release は admitted previous version からの upgrade path を証明し、safe rollback boundary を文書化する。
- [ ] rollback が newer incompatible state 上へ old binary だけを戻したり、quarantine clear や ambiguity の retry authority 化を行わない。
- [ ] mixed/incompatible version と unsupported state representation は fail closed にする。

### First run / operator diagnostics

- [ ] supported reference path が documentation map から容易に見つかり、install -> healthy `v2_status`/`v2_doctor` -> read-only semantic call -> explicitly authorized effectful call -> documented recovery まで辿れる。
- [ ] secret/trust anchor/permission/capacity/backend/topology/maintenance-state の missing/unsafe failure を sensitive value を出さず bounded/actionable に説明する。
- [ ] `v2_status`/`v2_doctor` は diagnostic/composition surface のままで、authorization/recovery/replay/mutation authority にはならない。

### Operational readiness

- [ ] supported reference deployment の service start/stop/drain、restart/reconnect、TLS/key expiry、storage pressure、quarantine/recovery、incident response、backup/restore boundary を文書化する。
- [ ] backup/restore は exact paired runtime identity、durable state、mutation authority、unresolved quarantine を保持し、checkpoint hand-edit や settlement の捏造を行わない。
- [ ] common `operator_action_required` state に、normal operator が raw checkpoint を読む必要のない privacy-bounded next step がある。

### Reliability / compatibility

- [ ] admitted change class に必要な deterministic test、restart/reconnect/fault-injection evidence、relevant resource/concurrency check が green。
- [ ] supported CUMG minor line、OS/Cua/backend/deployment compatibility、schema mismatch behavior、migration/deprecation guidance が current かつ evidence-backed。
- [ ] operator-critical な EN/JA normative documentation が同期している。

### Security / privacy invariants

- [ ] exact principal/device/capability authorization を維持または deliberate に強化する。
- [ ] ambiguous effectful work は既存 authoritative settlement path が成功するまで `Indeterminate`/quarantine のままとし、automatic replay を追加しない。
- [ ] artifact checksum/signing/SBOM/provenance/diagnostics/LLM・observational evidence を execution/recovery authority にしない。
- [ ] default log/telemetry/release metadata に credential、raw desktop content、command/result、private identity material、documented bounded audit contract 外の payload-bearing data を出さない。

## Evidence map

- Distribution scope / first install / artifact upgrade: [`v2/V2_RELEASE_ARTIFACTS.ja.md`](v2/V2_RELEASE_ARTIFACTS.ja.md)。
- Single-Mac lifecycle / diagnostics / effectful-path acceptance: [`v2/V2_SINGLE_MAC_PRODUCTION.ja.md`](v2/V2_SINGLE_MAC_PRODUCTION.ja.md)。
- Backup/restore: [`v2/V2_BACKUP_RESTORE.ja.md`](v2/V2_BACKUP_RESTORE.ja.md)。
- OS/Cua compatibility と automated evidence: [`TESTING.md`](TESTING.md) の Real-Cua compatibility matrix。
- Durable/wire compatibility と release support rule: [`VERSIONING.ja.md`](VERSIONING.ja.md)。
- Operator recovery / common action-required state: [`TROUBLESHOOTING.md`](TROUBLESHOOTING.md) / [`v2/STATUS.ja.md`](v2/STATUS.ja.md)。

## 初回 #213 closeout baseline — 2026-08-31

post-v0.3 の初回 Product Readiness gate は以下の evidence で完了します。

- #224: closed release-candidate manifest/checksum と fresh-extraction smoke boundary。
- #226/#233/#234/#235/#109/#236: lane-scoped readiness、privacy-bounded incident review、durable one-shot maintenance status、unified operator status、exact durable online-recovery confirmation、no-replay Human-guided recovery。
- #237 / PR #247: source-free single-Mac artifact install/upgrade、exact CUMG/Handoff pairing、recovery helper packaging、fail-closed artifact verification、clean-install orchestration test、paired rollback、installed doctor/status success gate。Linux/Windows は official binary installer claim ではなく candidate evidence のまま。
- [`v2/V2_RELEASE_ARTIFACTS.ja.md`](v2/V2_RELEASE_ARTIFACTS.ja.md): distribution scope と SBOM/provenance/notarization boundary。
- [`v2/V2_SINGLE_MAC_PRODUCTION.ja.md`](v2/V2_SINGLE_MAC_PRODUCTION.ja.md): reviewed single-Mac profile の lifecycle、diagnostics、recovery、rollback。 [`v2/V2_BACKUP_RESTORE.ja.md`](v2/V2_BACKUP_RESTORE.ja.md): coherent backup/restore boundary。
- [`TESTING.md`](TESTING.md)、[`VERSIONING.ja.md`](VERSIONING.ja.md)、[`DEPLOYMENT.md`](DEPLOYMENT.md)、[`v2/STATUS.ja.md`](v2/STATUS.ja.md): compatibility/release/operations/acceptance evidence。

#213 close は compatible stabilization や future-platform issue の全完了を意味しません。#215/#115/#104/#111/bounded #96 は released invariant failure の新 evidence がない限り non-blocking stabilization です。#217/#227/#228 を含む Recovery & Reconciliation 拡張は後続 milestone work のままです。
