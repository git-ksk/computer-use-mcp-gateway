# V2 single-Mac backup / restore

> English canonical: [V2_BACKUP_RESTORE.md](V2_BACKUP_RESTORE.md)

この runbook は reviewed macOS single-Mac profile の backup / restore を扱います。意図的に conservative です。backup/restore は durable truth と paired runtime identity を保持するためのもので、quarantine settlement、operation retry/replay、mutation authority transfer、desktop side effect の成否証明には使いません。

## Authoritative な対象

通常 install root は次です。

```text
~/Library/Application Support/computer-use-mcp-gateway/
```

recoverable backup では次を1つの coherent / versioned set として保持します。

- `v2/state/hub` / `v2/state/agent` durable checkpoint;
- `mutation-authority` state;
- installed paired binaries と `runtime-manifest.json`;
- active immutable Handoff `runtime-*` generation と managed runtime env file;
- current reviewed LaunchAgent plist と profile 再構築に必要な non-secret configuration;
- deployment が参照する owner-private secret/trust/key file（構成時の Hub/grant/TLS/trusted-proxy material を含む）;
- deployment の supported rollback boundary に残っている rollback asset。

secret / trust anchor / Handoff runtime / configuration を install root 外に配置している deployment では、その exact external path も backup inventory に含めます。default profile が install root を使うことだけを根拠に「root だけで完全」と推測しません。

`~/Library/Caches/cumg-v2/` は socket 等の runtime state で、authoritative backup state ではありません。restore 後の起動時に owner-private directory として再作成します。

## Backup procedure

1. `v2_status` / `v2_doctor` が読めることを確認し、bounded status/evidence だけを記録する。non-zero は cleanup/replay authority ではない。
2. quarantine が存在する場合は exact に保持する。backup を「clean」に見せるため resolve/retire/clear しない。
3. documented lifecycle で reviewed service を drain/stop し、Hub/Agent checkpoint writer が停止してから copy する。live state directory の単純copyを atomic application snapshot とみなさない。
4. Hub/Agent/signer 停止中に、上記 authoritative inventory を owner-private backup destination へ bytes/name/permission/directory structure を保持して copy する。operator-approved backup mechanism を使い、checkpoint JSON を変換・手編集しない。
5. installed `runtime-manifest.json`、利用可能なら exact artifact/archive checksum、CUMG/Handoff source identity、package version、architecture、Cua version、backup time を non-authoritative inventory metadata として別記録する。
6. backup 内で最も sensitive な material に合わせて保護する。secret や sealed recovery-key material を含む backup は、CUMG log が payload-free でも sensitive である。
7. signer -> Hub -> Agent で再起動し、通常の `v2_status` / `v2_doctor` post-check を要求する。backup 作成によって authority/quarantine state を変えない。

upgrade の immediate rollback には upgrade helper が作る paired rollback bundle を優先します。general backup は release-paired rollback contract の代替ではありません。

## Restore procedure

restore は intended trusted machine/profile に対して、exact backup set を特定した後だけ行います。

1. Hub/Agent/signer と conflicting legacy writer を停止したままにし、restore 中は effectful backend writer を動かさない。
2. supported macOS user session、reviewed Cua version、Node/Python runtime、Apple code-signing identity/TCC permission、proxy/tunnel policy、backup 外の external trust anchor を別途再確立する。filesystem backup は OS authorization を復元しない。
3. durable Hub/Agent state、mutation-authority state、installed runtime identity、active Handoff generation、configuration、required secret/trust file を含む complete paired set をrestoreする。newer state に old binary だけ、または incompatible newer binary に old state だけを戻さない。
4. owner-private permission を保持する。symlink、group/world writable、missing/unexpected trust/secret/state path は implicit repair せず拒否する。
5. restored `runtime-manifest.json` を検証し、同じ backup または reviewed compatible artifact に pair された `v2_maint`/runtime を使う。arbitrary newer checkout で restored old state を mutate しない。
6. signer -> Hub -> Agent の順で起動し、fresh authenticated Agent generation / current capability advertisement を要求する。checkpoint から liveness を継承しない。
7. `v2_status` / `v2_doctor` を実行する。restore 前の unresolved quarantine は unresolved のまま保持し、mixed/unsupported schema/runtime identity は fail closed にする。
8. deliberately authorized effectful action より先に harmless read-only semantic smoke を行う。backup 時点で ambiguous だった effectful operation は通常 incident/recovery flow を使い、restore を理由にretryしない。

## Secure Enclave recovery key の制限

`v2_recovery_enclave_helper` の sealed file は non-exportable Secure Enclave key を再オープンするための bounded representation です。backup で local recovery metadata を保持できる場合はありますが、private key を別Mac/Secure Enclaveへportableにするものではありません。machine replacement では new endpoint recovery key を provision し、reviewed provisioning flow で Hub trust を更新します。sealed file copy を recovery authority migration と表現してはいけません。

## Restore acceptance

restore 成功とするには次を満たします。

- exact runtime/manifest verification が成功;
- Hub/Agent durable state が supported compatibility contract でreadable;
- mutation authority の expected owner/epoch が保持され、process liveness から推測されていない;
- restore 後に fresh Agent session が authenticated;
- unresolved quarantine count/identity が silent clear されず保持;
- Handoff が safely idle または explicit recovery state を維持;
- `v2_status` / `v2_doctor` が bounded actionable state を返す;
- pre-restore ambiguous operation が replay されていない。

backup archive / inventory metadata は evidence / recovery material であり、principal/device/capability authority や operation settlement authority にはなりません。
