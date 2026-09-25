# V2 single-Mac verified backup / restore

> English が canonical です。[English](V2_BACKUP_RESTORE.md)

この runbook は reviewed macOS single-Mac profile の verified backup / restore contract を定義します。workflow は意図的に fail-closed です。backup が保持するのは 1 つの snapshot boundary に存在した durable runtime truth であり、quarantine settlement、operation retry/replay、mutation authority transfer、snapshot 後に work が存在しなかったことの証明、copied file からの新しい authorization 生成には使いません。

automated workflow は `v2_backup_restore.py` が実装します。

## Security model と non-claim

verified backup には独立した 3 つの性質があります。

1. **coherent snapshot** — durable set を copy する間、Hub/Agent writer と effectful mutation を停止する。
2. **integrity / identity** — included file を bounded / hash / permission check し、1 つの exact CUMG/Handoff runtime identity に bind する。
3. **external anchoring** — canonical backup manifest の SHA-256 を backup set 外へ保存し、restore / activation 時に必須にする。

manifest digest を bundle 内だけへ置いても、backup と manifest の両方を書き換える attacker は検出できないため、external record を必須にします。

backup 自身から **freshness は証明できません**。snapshot 後に effectful work が行われ、その replay history が backup に含まれない可能性があります。そのため restore は staged restore と explicit activation の 2 phase に分離します。operator は intended recovery に対して lineage を受け入れられる snapshot を選択し、snapshot 後の effectful work があり得る場合は、新しい effectful work を許可する前に運用履歴を reconcile します。backup tool は存在しない tombstone を製造せず、「snapshot 後に何も起きなかった」と推測しません。

## Supported restore topology

automated workflow が support するのは reviewed single-user / single-Mac profile のみです。

- backup に記録された同一 absolute install root;
- backup に記録された同一 LaunchAgent directory;
- owner-private CUMG state/configuration;
- install root 内にある authority-bearing referenced file;
- exact active immutable Handoff runtime generation;
- `runtime-manifest.json` が列挙する exact installed CUMG binary set.

username/home path 変更、別 layout への移動、install root 外の arbitrary authority-bearing file からの restore は automated restore の対象外です。backup 内容を書き換えず、reviewed install/enrollment flow で re-provision します。

Secure Enclave sealed recovery file は portable recovery authority ではありません。他の Mac へ copy しても non-exportable private key は移行しません。

## Authoritative backup inventory

verified backup は reviewed durable class のみを含みます。

### CUMG runtime identity

- `runtime-manifest.json`;
- manifest が列挙する exact binary;
- original bytes / mode / SHA-256 / valid macOS code signature;
- `v2/handoff/managed-runtime.env` が選択する active Handoff `runtime-*` generation;
- active generation の `runtime-generation-manifest.json` と exact file tree;
- sensitive opaque file としての `v2/handoff/managed-runtime.env`.

backup manifest には CUMG source commit、Handoff source commit、package version、schema version、active runtime generation 等の bounded identity metadata だけを記録します。secret environment value を manifest/log field へ複製しません。

### Hub / Agent durable state

Hub state:

- committed `hub-%020d.json` checkpoint;
- configured 時の active reviewed online-recovery verifier file.

Agent state:

- committed `agent-%020d.json` checkpoint;
- 存在する場合の reviewed online-recovery handoff file: challenge / authorization / resolved evidence.

workflow は committed checkpoint history を file のまま copy し、checkpoint JSON を deserialize / rewrite しません。backup 前と activation 前に latest checkpoint が supported compatibility contract で readable であることを要求します。

Hub M1 schema 7以降では durable writer fence もauthoritative checkpointの一部です。fence schema、monotonic revision、writer epochをbackup/restoreでbyte-for-byte保持し、restoreがそれらを新造・減算・除去したり、新しいauthority生成に再利用したりしてはいけません。current schemaなのにfenceが無いHub checkpointはinvalidです。offline local maintenanceが同じwriter epochのままstate revisionを進められるのは、exclusive state-directory maintenance lockを保持し、local checkpoint CASでcommitし、exact read-backを検証する場合だけです。その後に起動するHub processはservice開始前に必ずstrictly newer writer epochを取得します。

state directory 下に存在しても次は明示的に non-authoritative で、backup から除外します。

- `.cumg-v2-state.lock` と pending checkpoint;
- `browser-upload-staging` / `browser-download-staging`;
- Playwright/runtime artifact;
- transfer payload / ephemeral-data store;
- stale `*.pre-rotate-*` recovery-key copy;
- log / audit stream / socket / PID / cache / temporary file.

unknown file が approved durable class にも reviewed non-authoritative class にも一致しない場合、automated backup は fail closed します。将来 durable state が増えた際に silent omission しないためです。

### Mutation authority

`mutation-authority.json` は durable で byte-for-byte 保存し、その `owner` と positive `epoch` を manifest に記録します。

`mutation-authority.lock` は coordination state であり backup truth ではありません。backup 中は existing lock を exclusive に保持します。restore は fresh owner-private lock file を作成し、古い lock inode を戻したり authority epoch を increment/change したりしません。

### Provisioned trust / secret

reviewed LaunchAgent environment を inventory し、install root 内で参照される regular owner-private authority-bearing file のみを copy します。対象には configured Hub/grant/TLS/trusted-proxy/device/recovery material を含みます。

socket、executable prerequisite、run/cache root、OS service への参照は configuration であって backup payload ではありません。install root 外の authority-bearing file reference は automated workflow では拒否します。

backup は含まれる最も sensitive な secret に合わせて保護します。

## Coherent backup procedure

1. bounded read-only `v2_status` / `v2_doctor` inspection を行う。quarantine は許可され、backup のために clear しない。
2. reviewed lifecycle で Agent、Hub、grant signer、conflicting legacy effectful writer を停止する。Handoff は idle または durable recovery/checkpoint state で表現されていること。
3. reviewed LaunchAgent family が unloaded であることを要求する。
4. owner/epoch を変更せず shared mutation-authority lock を exclusive acquisition する。busy なら fail。
5. runtime manifest、installed binary hash/signature、active Handoff generation、state permission/schema、LaunchAgent configuration、referenced secret/trust file を検証する。
6. approved durable inventory を新しい owner-private staging directory に copy する。symlink / special file / weak permission / oversize / unexpected durable path は fail closed。
7. exact relative path / size / mode / hash / runtime identity / checkpoint sequence・schema summary / mutation owner・epoch / excluded-class policy を持つ canonical manifest を生成する。secret payload は含めない。
8. completed backup directory を atomic publish する。
9. canonical `manifest_sha256` を表示する。この値は backup set 外へ保存する。
10. original deployment は別途 restart し、通常の post-start health check を要求する。backup creation 自身は durable authority state を変更しない。

live directory copy は supported snapshot ではありません。

reviewed service 停止後の packaged invocation 例:

```bash
ROOT="$HOME/Library/Application Support/computer-use-mcp-gateway"
LAUNCH_AGENTS="$HOME/Library/LaunchAgents"
BACKUP="/secure/cumg-backups/cumg-v2-$(date +%Y%m%d-%H%M%S)"

python3 install/v2_backup_restore.py backup \
  --install-root "$ROOT" \
  --launch-agent-dir "$LAUNCH_AGENTS" \
  --output "$BACKUP"
```

出力された `manifest_sha256` は `$BACKUP` の外へ保存します。digest の唯一の copy を backup directory 内だけへ置いてはいけません。

## Verification

`verify` は restore も state mutation も行いません。external に保存した `manifest_sha256` を要求し、次を拒否します。

- digest mismatch;
- missing / additional / symlink / special / weak-permission / modified file;
- unsupported/newer backup manifest または checkpoint schema;
- mixed CUMG/Handoff runtime identity;
- invalid runtime-generation file set;
- invalid installed binary code signature;
- mutation owner/epoch mismatch;
- unsafe または external authority-bearing reference.

unanchored inspection は bounded metadata の表示にのみ使え、restore / activation の根拠にはできません。

```bash
python3 install/v2_backup_restore.py verify \
  --backup "$BACKUP" \
  --expected-manifest-sha256 "$MANIFEST_SHA256"
```

`inspect --backup "$BACKUP"` は意図的に unanchored / read-only で、bounded inventory 表示だけに使います。

## Staged restore

restore target は clean である必要があります。reviewed/legacy service が active でなく、existing CUMG install root、destination LaunchAgent、competing mutation-authority state が存在しないことを要求します。

`restore` phase:

1. external manifest digest で complete backup を verify;
2. exact runtime/state/trust pairing を再検証;
3. intended install root の sibling に private staging root を作成;
4. approved file を exact bytes/mode で復元;
5. lock inode は copy せず fresh coordination lock file を作成;
6. staged tree を再 verify;
7. reviewed LaunchAgent は未 install/unloaded のままにし、effectful activation は行わない。

したがって file copy だけで staged restore が running mutation authority になることはありません。

```bash
python3 install/v2_backup_restore.py restore \
  --backup "$BACKUP" \
  --expected-manifest-sha256 "$MANIFEST_SHA256"
```

command が表示する exact `stage_dir` を activation 用に保持します。activation 前に restore が中断した場合、marker/digest を確認した tool-created staging root だけを削除して再実行します。individual file の partial promotion は行いません。

## Explicit activation

activation には同じ external manifest digest と tool が生成した exact staged restore が必要です。

promotion 前に次を再確認します。

- destination path が引き続き clean;
- conflicting service/writer が loaded でない;
- backup/staged hash と runtime identity が unchanged;
- mutation owner/epoch が snapshot と一致;
- staged unresolved quarantine/replay state が backup と exact に一致;
- active Handoff runtime identity が exact;
- intended local prerequisite が存在する.

activation は staged install root を atomic promote し、reviewed LaunchAgent を install して signer -> Hub -> Agent の順に start します。Agent は fresh authenticated generation と current capability advertisement を確立する必要があり、checkpoint 上の liveness を継承しません。

```bash
python3 install/v2_backup_restore.py activate \
  --backup "$BACKUP" \
  --expected-manifest-sha256 "$MANIFEST_SHA256" \
  --stage-dir "$STAGE_DIR"
```

post-activation acceptance:

- expected mutation owner/epoch;
- staged snapshot に存在した quarantine は、そのまま quarantined であるか、既存 authoritative recovery/reconciliation contract（例: exact persisted #290 backend receipt）だけで除去され、backup/restore 自身では除去されない;
- persistent quarantine が残らない場合は `v2_status` / `v2_doctor` が healthy;
- persistent quarantine が残る場合、`v2_status` は expected `previous_operation_outcome_unknown / review_incident` のみを action-required として示し、`v2_doctor` の `unsafe` は preserved live-quarantine/recovery boundary 由来だけを許可する。無関係な error は activation acceptance を fail させる;
- pre-restore ambiguous operation を自動 retry/replay していない;
- deliberate effectful action より先に harmless read-only semantic smoke を実施.

startup/health acceptance が失敗した場合、新しく activate した service を停止し、restored state は bounded diagnosis のため保持します。health を green にする目的で quarantine clear や state roll-forward を自動実行しません。

## Snapshot freshness / rollback boundary

external manifest digest が証明するのは「どの backup set を選んだか」であり、「その deployment が ever produced した newest state か」ではありません。これは copied file から CUMG が推測できない snapshot の基本的性質です。

したがって:

- general backup は disaster-recovery material であり snapshot 後の idempotency oracle ではない;
- failed upgrade の immediate rollback には release-paired upgrade rollback bundle を優先;
- backup restore は upgrade transaction record を consume/rewrite しない;
- install 周辺に rollback asset が存在しても current runtime authority に promote しない;
- post-backup effectful work の可能性があり acceptable lineage を確立できない場合、restored profile を non-effectful のまま保持し、通常の recovery/re-provisioning process を使う.

## Acceptance gate

Issue #347 は automated regression で次を示した時だけ complete です。

- coherent backup が stopped writer + shared mutation lock を要求;
- exact runtime/Handoff/state/authority/trust inventory が round-trip;
- authoritative terminal/recovery evidence を持たない deliberately quarantined operation が backup -> staged restore -> activation/restart 後も quarantined;
- snapshot 時点の replay/tombstone state が保持;
- browser/ephemeral/log/socket/lock/stale-key material が除外;
- corrupt / incomplete / extra-file / symlink / permission / digest / runtime-identity / schema / authority mismatch が fail closed;
- restore が mutation owner/epoch を変更せず settlement を製造しない;
- clean supported-profile activation が healthy `v2_status` / `v2_doctor` に到達;
- EN/JA docs と release packaging に verified workflow が含まれる.

backup file と manifest は evidence/recovery material のままで、単独で principal/device/capability/settlement/replay authority にはなりません。
