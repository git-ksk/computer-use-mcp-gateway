# V2 ローカルユーザー承認型オンラインリカバリ

> 英語版 [`V2_ONLINE_RECOVERY.md`](V2_ONLINE_RECOVERY.md) が正本です。

状態: **明示的な recovery key provisioning を前提に実装済み。Secure Enclave の user-presence 実機acceptanceはリリース前ゲートとして残る。**

この機能は、desktop が durable `Indeterminate` quarantine に入った際、通常運用でHubを停止してoffline maintenanceを行わなくても、端末ユーザーの明示承認により安全に復帰できるようにするものです。既存の no-auto-replay と persistence-gated recovery は維持します。

## セキュリティ境界

Agent device identity は recovery authority ではありません。侵害されたAgentはローカルdesktop/backend状態について虚偽を報告できるため、Agent device keyだけでquarantineを解除してはいけません。

そこでmacOSでは、Agent keyとは別にP-256 recovery keyを小さなstable-signed CryptoKit helperでSecure Enclaveへ生成し、`userPresence + privateKeyUsage` 制約を付けます。永続化するのはSecure Enclaveのsealed `dataRepresentation`だけで、private key本体はSecure Enclave外へ出ません。sealed fileはCUMGがowner-privateに管理し、Hubには公開鍵だけをpinします。初回provisioningは既存sealed-key pathを再利用せず拒否し、sealed fileとHub側public-key fileの両方でpath/symlink/permissionをfail-closedに検証します。

初期実装のローカル承認providerはmacOSのみです。Windows/Linuxに弱いsoftware-key fallbackは追加せず、同等のuser-presence providerがレビューされるまでは既存のoffline maintenanceを使用します。

## フロー

```text
Hub durable quarantine
        |
        | Hub署名 fresh challenge
        v
AgentがHub署名/current generationを検証
        |
        | private local handoff
        v
cumg-v2-recover status / ユーザーがdesktopを確認
        |
        | exact decision
        | Secure Enclave user-presence署名
        v
Agentは署名済みRecoveryAuthorizationを中継するだけ
        |
        v
Hubがrecovery key/challenge/current quarantineを検証
        |
        | resolve_indeterminate + durable checkpoint
        v
Hub署名 RecoveryResolved ACK
        |
        v
handoff削除。以後は新しいoperation IDのみ実行可能
```

challengeは stable device ID、曖昧operation ID、実行時のhistorical generation、現在のauthenticated Agent generation、現在のquarantine fingerprint、fresh nonce、issued/expiry にbindingされます。有効期限は300秒（5分）です。reconnect/generation変更時はhandoffを破棄し、新しいchallengeが必要です。

historical generationとcurrent generationは別物です。generationはstale-session fenceであり、recovery ownershipではありません。

## 監査とプライバシー

通常のV2 checkpointはraw GUI command、screenshot、backend response、clipboard、credential等を保存しません。そのため、任意のGUI side effectを事後にAgentが一般的な方法で自動証明することはできません。

初期CLIは正直に `audit_assessment=inconclusive` とし、ローカルユーザーが実desktopを確認して `confirmed_completed` または `confirmed_not_executed` を選択します。将来、特定capabilityで安全に証明可能な場合のみ `completed` / `not_executed` の自動auditを追加できます。

Hubへ送るevidenceは最大1KiBのmetadataのみです。screenshot、raw command/result、credential、typed secret、無関係なdesktop contentを含めません。

## Hubの解除条件

Hubは次の条件をすべて満たす場合だけonline resolutionを受理します。

1. recovery verifierが明示的にprovision済み
2. current live Agent generationがchallenge/authorizationと一致
3. challengeが未期限切れ
4. P-256署名が正しい
5. stable device ID / exact operation IDが一致
6. historical quarantine generationが一致
7. durable quarantine fingerprintが変化していない
8. evidenceとrequest shapeがbound内

解除は既存 `resolve_indeterminate` を利用し、checkpoint永続化に失敗した場合はin-memory stateもquarantine snapshotへrollbackします。durable成功前にACKは返しません。

`confirmed_not_executed` でも旧operationは再実行しません。再試行は必ず新しいoperation IDです。同じaccepted request IDの再送は同一live recovery exchange内でidempotentにACKできますが、異なるdecisionへの変更はできません。

## macOS provisioning

`v2_recover` と stable-signed `v2_recovery_enclave_helper` を `v2_agent` と一緒にinstallします。Agentを動かすログインユーザーでowner-private recovery directoryを作り、一度だけSecure Enclave recovery keyを作成します。

```bash
install -d -m 700 "$HOME/Library/Application Support/cumg-v2-agent/recovery"
v2_recover init-key \
  --key-file "$HOME/Library/Application Support/cumg-v2-agent/recovery/recovery-key.sealed" \
  --secure-enclave-helper "$HOME/Library/Application Support/computer-use-mcp-gateway/bin/v2_recovery_enclave_helper" \
  --public-key-out "$HOME/Library/Application Support/cumg-v2-agent/recovery/recovery-public-key.p256"
```

exportされるのは公開鍵だけです。operator-authenticated provisioning channelでHubへ移し、次へ配置します。

```text
<HUB_STATE_DIR>/recovery-public-key.p256
```

Hubは起動時にverifierを読むため、初回provisioning後はHub再起動が必要です。verifier未設定ならonline recoveryが無効になるだけで、quarantine自体が弱くなることはありません。

quarantine発生後の canonical operator workflow は `v2_recover guide` です。

```bash
v2_recover guide \
  --hub-state-dir "<HUB_STATE_DIR>" \
  --agent-state-dir "$HOME/Library/Application Support/cumg-v2-agent/state" \
  --hub-public-key-file "$HOME/Library/Application Support/cumg-v2-agent/trust/hub.pub" \
  --key-file "$HOME/Library/Application Support/cumg-v2-agent/recovery/recovery-key.sealed" \
  --secure-enclave-helper "$HOME/Library/Application Support/computer-use-mcp-gateway/bin/v2_recovery_enclave_helper" \
  --mutation-authority-dir "<MUTATION_AUTHORITY_DIR>" \
  --wait-secs 60
```

`guide` は orchestration / UX だけを追加し、新しい recovery authority は作りません。最初に Hub 署名 challenge を検証し、既存 #233 incident brief を生成して、`IncidentBrief.cumg.supported_decisions` を変更せず pure recovery plan にコピーします。observational diagnostics は説明材料として表示できますが、supported decision を作成・追加できません。authoritative decision set が空なら `keep_quarantine` で終了し、user-presence signing / authorization publication は実行しません。

CUMG が support する decision がある場合、authority-bearing guided path は interactive Human terminal を必須にし、その closed set からの明示選択か cancel だけを受け付けます。Agent/LLM から pipe した入力では recovery decision を実行できません。Human review 後、CLI は署名前に signed challenge と incident brief を直ちに再読します。operation/device/original-generation/current-generation/fingerprint/nonce binding、incident state、supported-decision set のどれかが変わっていれば fail closed で再reviewを要求します。

fresh validation 後だけ、既存 Secure Enclave helper が macOS user presence を要求します。deny/cancel/timeout/authentication unavailable では authorization を publish せず、quarantine は維持されます。guided path でも既存 online-recovery protocol をそのまま使用し、old operation の retry/replay authority は追加しません。

`authorization=published` は completion ではありません。guided recovery は必ず #109 semantics で exact Hub-signed `RecoveryResolved` ACK を待ち、request/device/current-generation/operation/decision binding を検証した後だけ `durable_completion=verified` と `old_operation_replayed=false` を表示します。その後、exact quarantine の read-only check と既存 `v2_status` JSON を実行します。すべて healthy なら `recovery_outcome=verified_healthy`、recovery 自体は durable に完了しているが無関係な Handoff/runtime/mutation-authority/backend/recovery-mode 問題が残る場合は `recovery_outcome=verified_with_unrelated_status_problem` と bounded `v2_status` reason を別に表示し、durable recovery 成功を消しません。

Agent-assisted explanation / UI composition には read-only JSON planning mode を使えます。

```bash
v2_recover guide \
  --hub-state-dir "<HUB_STATE_DIR>" \
  --agent-state-dir "$HOME/Library/Application Support/cumg-v2-agent/state" \
  --hub-public-key-file "$HOME/Library/Application Support/cumg-v2-agent/trust/hub.pub" \
  --json
```

JSON mode は prompt/sign/publish/quarantine clear/replay を一切行いません。private challenge fingerprint/nonce、credential/key/raw command/argv/clipboard/screenshot/principal material も出力しません。そのため Agent/UI は「今 CUMG が何を support しているか」を説明できますが、recovery decision の authority は持ちません。

低レベルの `v2_recover status` / `resolve` / `confirm` は advanced diagnostics / break-glass 用 primitive として残します。`resolve` は従来どおり macOS user presence を要求し、`confirm` は publish 済み request を exact safe metadata で再確認できます。ACK 欠落、timeout、stale receipt、mismatch は成功ではなく、retry/replay authority にもなりません。通常 operator flow では `guide` を優先します。

## 障害時

sealed Secure Enclave recovery-key representation紛失、Agent接続不能、Secure Enclave利用不能、online protocol故障などでは、既存 `v2_maint` をbreak-glassとして残します。device key侵害が疑われる場合でも、それだけでrecovery authorityにはなりません。

## リリース前acceptance

自動テストに加え、trusted physical Macで以下を確認します。

1. 曖昧なdesktop operationがdurable quarantineになる
2. Agent reconnectでは解除されない
3. Hubを停止しない
4. local state dirにfresh challengeが届く
5. `v2_recover resolve` でSecure Enclave由来の実user-presence promptが出る
6. 拒否時はquarantine維持
7. 承認後のみHubがdurable resolveして署名ACKを返す
8. 新しいoperationが成功する
9. 旧operationは一度もreplayされない

## Protocol互換性とchallenge更新

Online recoveryは現在の `HUB_AGENT_SCHEMA_VERSION = 4` application protocolの一部です。schema validationはfail-closedのままなので、このreleaseを有効にする際はHubとAgentを協調して更新し、mixed-version rolling compatibilityには依存しません。V1 gatewayの動作は変わりません。

Recovery challengeは300秒（5分）で期限切れになります。desktopがquarantineのままなら、通常のauthenticated Agent heartbeatを契機にHubがpending challengeを再確認し、期限切れ後はfresh nonceを持つchallengeを再発行します。承認時間切れだけを理由にHub/Agentを再起動する必要はありません。fresh challenge受信時は以前のlocal authorization handoffを無効化します。
