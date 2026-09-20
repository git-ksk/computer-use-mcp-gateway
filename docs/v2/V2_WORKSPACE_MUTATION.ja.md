# V2 least-privilege workspace mutation

> 英語版が canonical です。英語版は V2_WORKSPACE_MUTATION.md です。

Issue #107 は、coding workflow が shell / execute_process 権限を持たずに approved workspace file を更新できる、Agent-native の bounded mutation primitive を1つ追加します。

## Authority boundary

- allowed_write_roots は read-only filesystem root / process cwd root と完全に別の operator-configured policy です。fallback / inheritance はありません。
- denied_write_subpaths は writable root 内の明示 exclusion で、deny が allow より優先します。
- writable root が空なら WriteWorkspaceFile capability 自体を advertise しません。
- northbound authorization は principal/device/WriteWorkspaceFile の exact tuple です。generic Dangerous grant、Shell grant、別 principal では通りません。
- v0.5 の writable root は device/operator-global です。per-principal path/root isolation は主張しません。
- この primitive は filesystem operation のみで、process spawn、network authority、shell/keychain/SSH-agent credential inheritance はありません。

## Mutation contract

MCP tool は write_workspace_file です。

raw payload は base64 で 32 KiB 上限、requested UTF-8 path は 4,096 bytes 上限です。最大typed commandとsigned Hub/grant envelopeを含めても通常の64 KiB application-message boundary内に収まることをcarrier testで固定します。Hub は base64 / byte count / SHA-256 を検証して typed command を作り、Agent でも同じ byte count / digest を再検証します。

precondition は必ずどちらか1つです。

- create: expected_absent=true
- replace: expected_sha256=<64文字のlowercase hex>

blind overwrite は提供しません。

signed command は path、payload、expected byte count、payload SHA-256、precondition を bind します。signed result は bounded receipt (bytes_written, content_sha256, created) のみを返し、command/result matching で不一致を拒否します。

## Atomic publication

target path へ直接 in-place write はしません。

1. #105 と共有する capability-rooted path primitive で intended parent を resolve/reopen。
2. denied path、symlink/reparse相当、non-regular target、hard-link target を拒否。既存targetはcanonicalized実体でもdenyを再評価し、Windowsのcase aliasもfail closedにする。Windowsのfile/directory identityはstable Win32 handle情報で証明する。
3. replacement は既存 regular file を最大4 MiBまで bounded hash し、expected SHA-256を照合。
4. same-parent の fresh temp file に書き、replacementでは既存permissionを継承し、staged fileをsync。
5. publication直前に deny policy と destination identity/content を再証明。
6. create は same-directory hard-link publish。途中でdestinationが出現していれば原子的に失敗し、temp nameを削除。
7. replace は re-proven destination へ same-parent atomic rename。
8. publish後に parent directory をsync。

publish/flushの結果を証明できない場合は Indeterminate とし、Agentはterminal resultを作らずreconnectします。既存Hub execution-safety pathがquarantineし、初期sliceは自動retryもcontentからの成功推測もしません。

CAS check はこのAPI内のローカルな再証明です。publication直前にtargetを再証明しますが、同じinodeを独立processが同時に変更できる状況までglobal filesystem transactionとして保証するものではありません。

## Privacy and recovery

default telemetry / durable recovery に raw file content、requested path、configured root/deny path、OS error detailは残しません。stable error code と bounded receipt のみです。

mutation payload は durable recovery にコピーしません。dispatch後にresult deliveryが失われた場合、callerは既存の operation_id / get_operation / quarantine flowを使い、blind replayしてはいけません。

## Release integration

#107 単体では v0.5 release integration を完了しません。#314 が以下を担当します。

- control/capability/Hub-Agent schema の最終review/version bump
- mixed-version fail-closed
- macOS launchd / Linux systemd / Windows packaged writable-root/deny config
- sensitive absolute policy path を返さない readiness/doctor/status
- upgrade/rollback acceptance と release-candidate packaging evidence
