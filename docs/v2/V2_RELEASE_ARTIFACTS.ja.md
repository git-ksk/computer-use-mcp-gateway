# V2 release artifact と single-Mac installation

> English canonical: [V2_RELEASE_ARTIFACTS.md](V2_RELEASE_ARTIFACTS.md)

この文書は #237 後の distribution boundary を定義します。release artifact は install provenance を改善しますが、CUMG の execution / recovery / Human Handoff / mutation authority にはなりません。

## Distribution scope

| Platform / profile | Artifact status | Normal installation boundary |
| --- | --- | --- |
| macOS single-Mac reference profile | install-capable reviewed release candidate | source checkout 不要の artifact install / upgrade。activation 前の local stable Apple code signing は引き続き必須 |
| Linux Hub / Agent | reviewed native candidate evidence | 既存 source / service packaging。official binary installer claim はまだしない |
| Windows desktop Agent | reviewed native candidate evidence | 既存 source / Task Scheduler profile。official binary installer claim はまだしない |

CI artifact は自動的に official GitHub Release asset にはなりません。`v0.4.0` はまだshipしておらず、verified archiveはrelease-candidate evidenceのままです。将来の release で artifact を昇格する場合も documented release procedure を通し、CI success だけで tag / published asset を作りません。

## macOS artifact identity

macOS bundle は CUMG / Handoff source code について self-contained です。`release-artifact-manifest.json` schema v2 は package version、exact CUMG commit、Hub/Agent application schema、platform/architecture、exact `mcp-execution-handoff` commit、`single-mac-artifact-v1` profile、全 allowlisted file の size / SHA-256 を記録します。

bundle には paired Hub/Agent/maintenance/operator binaries、`v2_recover`、macOS Secure Enclave helper、single-Mac LaunchAgent template、bounded install/upgrade tooling、self-contained Handoff runtime payload を含めます。Handoff payload は同じ CUMG commit と reviewed Handoff commit を結ぶ inner manifest を持ち、runtime file をすべて hash します。unexpected file、unsafe path、symlink、production dependency 欠落、digest drift、commit mismatch は fail closed です。

reviewed Handoff pin は `packaging/release/single-mac-handoff.json` に置き、release workflow の pin と一致しない場合は build を拒否します。normal operator が Handoff Git commit を調査・入力する必要はありません。

## Activation 前の integrity

workflow は archive-level `.sha256` を生成します。release/candidate publication channel を trust source として extraction 前に checksum を確認し、fresh extraction 後は bundled verifier で closed manifest を再検証します。

```bash
python3 install/v2_artifact_install.py inspect --bundle-dir "$PWD"
```

manifest/checksum は operation authorization、quarantine clear、recovery decision、mutation-authority transfer、desktop side effect の証明には使いません。distribution evidence のみです。

CI artifact は Apple-notarized public installer ではありません。reviewed single-Mac profile は既存 TCC continuity boundary を維持し、activation 前に verified bytes を private staging へ copy して operator の exact code-signing fingerprint / Team ID で TCC-sensitive executable/helper を stable-sign します。ad-hoc fallback はありません。installed `runtime-manifest.json` は post-signing digest を記録し、`v2_doctor` が検証します。

## First install

clean supported Mac とは CUMG/Handoff source repository が不要という意味で、deployment identity を artifact が自動生成する意味ではありません。operator は supported interactive macOS user session、Python 3、Node.js、reviewed Cua Driver、valid local Apple signing identity、separately provisioned owner-private secret/trust/policy、reviewed stable device/resource/proxy identity を用意します。

artifact には `install/single-mac-profile.example.json` と `install/README.md` を含めます。secret/trust bytes は artifact/profile に入れません。

まず non-activating preflight を実行します。

```bash
python3 install/v2_artifact_install.py install \
  --bundle-dir "$PWD" \
  --profile /secure/cumg/single-mac-profile.json \
  --provisioning-dir /secure/cumg/provisioning \
  --preflight-only
```

その後 `--preflight-only` を外して実行します。installer は outer artifact / architecture / profile / private provisioning / inner Handoff payload を installed state 作成前に検証し、TCC-sensitive executable を private staging で stable-sign、exact paired runtime を install、fresh `owner=v2` mutation-authority domain のみ initialize、signer -> Hub -> Agent の順で起動し、**installed** `v2_doctor` と `v2_status` が healthy になるまで success を返しません。

existing installation は拒否し、upgrade path を使います。startup/post-check failure ではその invocation が開始した service を停止します。operation replay や recovery success の捏造はしません。

## Artifact-backed upgrade / rollback

既存 reviewed single-Mac deployment は bundled helper を既存 one-shot launchd wrapper 経由で実行します。

```bash
python3 install/v2_launchd_maintenance_job.py \
  run-upgrade --artifact-bundle "$PWD"
```

`RunAtLoad=true`、`KeepAlive=false`、observed runs=1、automatic retry なし、temporary plist cleanup の契約は変わりません。artifact mode は durable maintenance transaction / service drain より前に outer bundle と inner Handoff pair を検証し、artifact binary の private copy を local stable signing に使います。source mode は maintainer-only として残します。

既存 upgrade contract（quarantine=0、Handoff idle、mutation-authority fence、Hub drain、paired rollback archive、installed runtime re-hash、signer -> Hub -> Agent restart、healthy `v2_doctor`）をそのまま使います。artifact mode は不完全な old rollback runtime を **new** Handoff payload から再構成せず拒否します。

rollback は version-paired recovery action のままです。newer incompatible state に old binary だけを戻したり、rollback で quarantine を clear / ambiguous operation を retry してはいけません。

## Supply-chain / publication strategy

現在の reproducible evidence は exact CUMG/Handoff commits、Cargo/npm lockfiles、pinned GitHub Actions、Dependency Review / CodeQL、archive checksum、closed outer/inner manifests、fresh extraction 後の Linux/macOS/Windows native smoke、source-free macOS installer inspection と install/startup/doctor/status orchestration coverage です。

**official** binary GitHub Release では、exact Cargo/npm lockfile graph から reviewed SBOM/license inventory を生成して添付し、published archive checksum を protected workflow/source commits に結ぶ provenance/attestation も付与します。macOS notarization は別の distribution decision であり、実装・acceptance までは CI candidate を notarized general-purpose installer と表現しません。

signing / SBOM / provenance / release metadata は evidence であり、principal/device/capability authority にはしません。
