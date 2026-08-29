# Versioning と release policy

> この日本語版は [`VERSIONING.md`](VERSIONING.md) の翻訳です。**英語版を canonical（正典）とし、解釈に差がある場合は英語版を優先します。**

CUMG は Semantic Versioning を採用し、pre-1.0 policy を明示します。

現在の released line は **0.3.x** です。`v0.3.0` は V2 Production Hardening / Operational Readiness milestone を表し、`v0.2.0` は immutable な V2-complete historical tag として維持します。

## Version format

version は `MAJOR.MINOR.PATCH`、Git tag は `vMAJOR.MINOR.PATCH` を使います。

例: `0.2.1`、`0.3.0`、`0.10.0`、`1.0.0`。

`0.9.x` の次を必ず `1.0.0` にするルールはありません。project が 1.x compatibility commitment を引き受ける準備ができるまでは、`0.10.0`、`0.11.0` 以降の pre-1.0 minor を使えます。

## PATCH: `0.y.Z`

current minor line の shipped public contract と互換性を維持する場合は patch release を使います。

典型例:

- bug fix;
- compatible security hardening;
- reliability、observability、resource fix;
- compatible dependency/backend pin update;
- documented contract を維持する packaged-default correction。

Docs-only change は通常 crate version bump / tag を必要としません。docs-only patch release は、重大な published security/deployment instruction の訂正など、immutable な corrected release snapshot を残す operational value が高い場合に限ります。

## MINOR: `0.Y.0`

meaningful public-contract expansion、または deliberate incompatible pre-1.0 change がある場合は新しい minor release を使います。

典型例:

- 新しい northbound capability / capability family;
- advertised compatibility contract を持つ新しい supported backend;
- 新しい compatibility boundary を導入する protocol/schema behavior;
- deliberate incompatible command/configuration/behavior change;
- operator が依存できる内容を実質的に変える substantial runtime feature。

roadmap phase、PR count、elapsed time、documentation milestone だけでは minor version を上げません。

## 1.0 より前の breaking change

pre-1.0 breaking change は patch ではなく minor release で行います。`CHANGELOG.md` に明記し、user が configuration / integration behavior を変更する必要がある場合は migration guidance を提供し、relevant compatibility/schema/security docs を更新します。

compatibility を維持すること自体が vulnerability を残す security emergency では breaking change を許可します。release note には break と security reason を記載しますが、適切な時期より前に exploit-sensitive detail を公開しません。

## Schema version は独立管理

project/crate version、wire protocol schema、capability-advertisement schema、durable-state schema はそれぞれ目的が異なります。

- `CONTROL_SCHEMA_VERSION` は live control-schema compatibility boundary が変わるときに変更;
- capability-advertisement schema version は live advertisement boundary が変わるときに変更;
- `DEVICE_REGISTRY_SNAPSHOT_SCHEMA_VERSION` と `GRANT_LEDGER_SNAPSHOT_SCHEMA_VERSION` は persisted structure を独立に versioning し、future `CONTROL_SCHEMA_VERSION` bump だけを理由に変更しない。persisted structure 自体が変わる場合だけ bump する;
- historical v0.2.x checkpoint は当時の control schema number を persisted registry / grant-ledger tag に流用していた。runtime restore が support するのは review 済みの v0.2.0 以降の lineage のみで、registry は `2/capability 2`、`3/capability 3`、`4..=7/capability 4`、grant-ledger は `2..=7`。prototype tag `1`、unknown/future tag、不可能な control/capability pairing は fail closed;
- migration 時に historical capability advertisement を live authority へ昇格させない。Hub は historical pairing を検証し、device identity / generation を restore した上で device を offline にし、dispatch 前に current live schema の fresh Agent advertisement を要求する;
- crate release だけを理由に schema version を自動 increment しない;
- schema change から算術的に crate version を決めず、public compatibility impact に応じて PATCH/MINOR rule を使う。

既に supported な release line の checkpoint を、wire/configuration behavior を変えず backward-compatible に restore する persisted-state migration は PATCH で扱えます。documented persisted-state version の support removal、新しい incompatible persisted-state shape、operator に state transformation を要求する変更は新しい compatibility boundary なので、pre-1.0 では通常 MINOR が必要です。したがって、この backward-compatible checkpoint migration 自体は **MINOR version bump を要求しません**。

## Durable-state writer compatibility と maintenance pairing

execution-safety durable-state schema は crate version とは独立した operational compatibility boundary です。operator がたまたま新しい maintenance binary を実行したという理由だけで、offline recovery が古い authoritative checkpoint を新しい representation に暗黙変換してはいけません。

そのため `v2_maint resolve` は input checkpoint が support する writer contract を維持し、post-resolution state をその contract で表現できるか検証し、表現できない場合は **publication 前に** fail します。packaged deployment では `v2_hub` と `v2_maint` を同じ reviewed build/release artifact から install し、pair として一緒に upgrade します。rollback checkpoint を保持する場合は対応する version-paired binary も rollback asset として保持してください。新しい source checkout の maintenance binary を、古い deployed Hub と pair された binary の代替として任意に使う運用は support しません。

`inspect-quarantine` のような read-only inspection command は recovery authority にはならず、supported checkpoint を mutate せず読むことができます。authority-bearing maintenance で intended Hub が必要な durable representation を読めない場合は、先に documented compatible Hub + maintenance path で upgrade してください。state の手編集、forced schema downgrade、release tag の移動で回避しません。

## `1.0.0` の条件

`1.0.0` は **CUMG が stable public compatibility contract を維持する意思を持つ**ことを意味します。あらゆる機能が実装済みであることを意味しません。

`1.0.0` より前に、少なくとも次を満たします。

1. supported northbound semantic surface と core execution-safety invariant を明示的に stable と位置付ける;
2. supported upgrade に必要な control/capability schema compatibility と failure behavior を文書化する;
3. supported backend/deployment compatibility matrix を文書化し、repeatable acceptance evidence で裏付ける;
4. versioning、release、security、support、deprecation rule を文書化し、実際の運営で守る;
5. maintainer が 1.x 内では backward-compatible change を維持し、emergency security case を除く deliberate public-contract break を future major release に限定する準備ができている。

feature count は 1.0 gate ではありません。product boundary を先に変更しない限り、fleet management、remote desktop、generic device fabric、broad orchestration、すべての backend capability は 1.0 の必須条件ではありません。

## Deprecation と support

1.0 より前は practical なら deprecation を推奨しますが、incompatible change は migration note 付きで次の minor release に入れられます。

1.0 以降:

- compatible addition/deprecation は minor release;
- fix は patch release;
- intentional public-contract removal/break は新しい major release;
- 通常、deprecated public surface は removal 前に少なくとも1つ後続 minor release の間は利用可能にします。security 上必要なら早期 removal を許可します。

1.0 より前は **latest released minor line** のみを actively supported line とします。古い 0.x line は best-effort で routine backport は行いません。severe security backport は discretionary exception であり、LTS promise ではありません。

## Release-candidate artifact

現在公開済みの `v0.3.0` release は、将来の GitHub Release が reviewed binary asset を明示的に含めるまでは **source-only** のままです。CI artifact を supported distribution へ暗黙昇格させません。

`Release Candidate Artifacts` workflow は、1つの exact checkout から Linux / macOS / Windows の bounded native candidate を build します。`scripts/v2_release_candidate.py` は platform allowlist に含まれる V2 binary だけを copy し（Unix-only operator binary は Windows から除外）、package version、exact source commit、platform/architecture、各 file の size/SHA-256 を `release-artifact-manifest.json` に記録し、archive と archive-level `.sha256` record を生成します。artifact manifest は distribution evidence に限定し、installed single-Mac `runtime-manifest.json` を置き換えず、execution/recovery authority にもしません。

verification は意図的に fresh extraction で行います。

```bash
python3 scripts/v2_release_candidate.py verify \
  --archive dist/cumg-v0.3.0-macos-arm64.tar.gz \
  --checksum dist/cumg-v0.3.0-macos-arm64.tar.gz.sha256 \
  --extract-dir /tmp/cumg-candidate

python3 scripts/v2_release_candidate.py smoke \
  --bundle-dir /tmp/cumg-candidate/cumg-v0.3.0-macos-arm64
```

`verify` は candidate acceptance 前に checksum mismatch、unexpected/missing file、unsafe path、symlink、malformed identity metadata、per-file size/digest drift を拒否します。`smoke` は source checkout や `cargo run` ではなく extracted bundle 内の packaged binary を実行します。

これらの candidate は **official production installer ではありません**。将来の release で installable supported distribution を claim する前に、対象 platform の signing/notarization 方針、SBOM/provenance strategy、clean-machine install acceptance、explicit supported-platform matrix を完了させます。CI workflow は review 用 candidate を upload するだけで、Git tag や GitHub Release を create/mutate しません。

## Release procedure

通常 release は `main` から dedicated release PR で準備します。

1. 対象 implementation/docs change が `main` に merge 済みであることを確認。
2. current `main` から `release/vMAJOR.MINOR.PATCH` を作成。
3. `Cargo.toml` と対応する `Cargo.lock` package version を更新。
4. `CHANGELOG.md` に release section を追加し、compatibility/breaking note と meaningful acceptance evidence を記載。
5. 新しい released state を表す必要がある status/version reference のみ更新。
6. required CI と change class が要求する release-specific acceptance を実行。
7. protected `main` process で release PR を merge。
8. merge 後の `main` commit に annotated `vMAJOR.MINOR.PATCH` tag を作成。
9. matching GitHub Release を作成。0.x は pre-release、`1.0.0` 以降は alpha/beta/RC を明示する場合を除き stable とする。

published tag は immutable です。release tag を移動・再利用しません。問題があれば新しい patch/minor release で fix forward します。

## Pre-release identifier

final release 前に candidate build の validation が必要な場合だけ SemVer pre-release identifier を使います。例: `0.3.0-rc.1`、`1.0.0-beta.1`、`1.0.0-rc.1`。milestone count を増やすためだけには作りません。

## Changelog rule

`CHANGELOG.md` は release-oriented であり、commit log の複製ではありません。user/operator に重要な capability、compatibility、migration、security、supported-backend、reliability/operations、acceptance change を記載します。routine internal refactor と purely editorial change は、release claim に実質的な影響がない限り個別 bullet を必要としません。
