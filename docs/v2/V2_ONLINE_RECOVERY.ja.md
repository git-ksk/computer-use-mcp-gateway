# V2 ローカルユーザー承認型オンラインリカバリ

> 英語版 [`V2_ONLINE_RECOVERY.md`](V2_ONLINE_RECOVERY.md) が正本です。

状態: **明示的な recovery key provisioning を前提に実装済み。trusted physical macOS Secure Enclave user-presence acceptance は完了済み。Linux FIDO2/CTAP2 provider は automated contract coverage を持つ実装候補だが、独立した UV-capable authenticator 実機 acceptance が pass するまで Linux support は claim しない。**

この機能は、desktop が durable `Indeterminate` quarantine に入った際、通常運用でHubを停止してoffline maintenanceを行わなくても、端末ユーザーの明示承認により安全に復帰できるようにするものです。既存の no-auto-replay と persistence-gated recovery は維持します。

## セキュリティ境界

Agent device identity は recovery authority ではありません。侵害されたAgentはローカルdesktop/backend状態について虚偽を報告できるため、Agent device keyだけでquarantineを解除してはいけません。

そこでmacOSでは、Agent keyとは別にP-256 recovery keyを小さなstable-signed CryptoKit helperでSecure Enclaveへ生成し、`userPresence + privateKeyUsage` 制約を付けます。永続化するのはSecure Enclaveのsealed `dataRepresentation`だけで、private key本体はSecure Enclave外へ出ません。sealed fileはCUMGがowner-privateに管理し、Hubには公開鍵だけをpinします。初回provisioningは既存sealed-key pathを再利用せず拒否し、sealed fileとHub側public-key fileの両方でpath/symlink/permissionをfail-closedに検証します。

accepted production provider は引き続き macOS Secure Enclave です。Windows Hello は #227 で implementation / CI complete・physical acceptance pending、Linux は #228 で optional FIDO2/CTAP2 実装候補を持ちます。pending platform に弱い software-key fallback は追加せず、provider の明示 prerequisite を満たさない deployment は既存 offline maintenance を使用します。

recovery core には **provider-neutral な WebAuthn/CTAP ES256 verifier contract** も含まれます。これは bounded な credential/public-key document、CUMG client-data への exact challenge binding、RP ID hash、credential ID、ES256 signature、UP/UV 両flagを検証する共有暗号部品です。この verifier の merge/test だけでは Windows Hello、Linux FIDO2、その他platform providerのsupportを**主張しません**。各platform providerはnative provisioning/assertionを別途実装し、supportをclaimする前にそれぞれphysical user-presence acceptanceをpassする必要があります。

Linux candidate は `libfido2-dev` を build dependency にせず、distro で install された libfido2 command-line tools を optional runtime provider として使用します。CUMG は explicit な root-owned tool directory、explicit な `/dev/...` authenticator path、libfido2 tools 1.17.0 以上、CTAP2/FIDO2、ES256、signed user presence、さらに `pin`（Client PIN / PIN-UV Auth）または `builtin`（authenticator内蔵UV）の明示UV modeを要求します。PINをconfig/env/argv/log/CUMG stdinへ渡さず、`pin` modeではlibfido2がcontrolling ttyから取得できる場合だけ進みます。U2F用 `-u` は使用せず、shared Hub verifierもsigned UP/UV両bitがないassertionを独立して拒否します。

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
        | exact Human choice:
        | historical resolution OR current-state acceptance
        | platform recovery-provider user-verification署名
        v
Agentは署名済みRecoveryAuthorizationを中継するだけ
        |
        v
Hubがrecovery key/challenge/current quarantineを検証
        |
        | resolve_indeterminate OR reviewed tombstone retirement
        | + durable checkpoint
        v
Hub署名 RecoveryResolved ACK
        |
        v
handoff削除。以後は新しいoperation IDのみ実行可能
```

challengeは stable device ID、曖昧operation ID、実行時のhistorical generation、現在のauthenticated Agent generation、現在のquarantine fingerprint、fresh nonce、issued/expiry にbindingされます。有効期限は300秒（5分）です。reconnect/generation変更時はhandoffを破棄し、新しいchallengeが必要です。

historical generationとcurrent generationは別物です。generationはstale-session fenceであり、recovery ownershipではありません。

online-recovery schema v2 の signed authorization は `confirmed_completed` / `confirmed_not_executed` に加えて `current_state_accepted` を持ちます。後者では `transient_ui_interaction_v1` policy 自体も署名対象です。decision / policy / operation / device / generation / fingerprint / nonce / expiry / assessment / evidence の変更は signature または exact challenge match を壊します。legacy schema v1 は従来 historical resolution shape のみ受理し、current-state acceptance authority は持てません。

## 監査とプライバシー

通常のV2 checkpointはraw GUI command、screenshot、backend response、clipboard、credential等を保存しません。そのため、任意のGUI side effectを事後にAgentが一般的な方法で自動証明することはできません。

generic post-hoc evidence で history を証明できない場合、CLI は正直に `audit_assessment=inconclusive` とします。その後の Human choice は意図的に2種類へ分離します。`resolve` は `confirmed_completed` / `confirmed_not_executed` という historical claim 専用です。`accept-current-state` は historical claim を一切行わず、「現在のscreenを確認し、このstateを continuation point として受け入れる」ことだけを意味します。このpathでは historical operation は `Indeterminate` のままです。将来、特定capabilityで安全に証明可能な場合のみ `completed` / `not_executed` の自動auditを追加できます。

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

historical `confirmed_completed` / `confirmed_not_executed` は既存 `resolve_indeterminate` を利用します。`current_state_accepted` は別物で、online-recovery schema v2、`audit_assessment=inconclusive`、exact `transient_ui_interaction_v1` policy、authorization と exact match する current authenticated registry generation、original ambiguous dispatch より strictly newer generation を追加で要求します。既存 retirement policy が capability を独立に `Scroll` / `MovePointer` のみに限定し、`Shell`、process、filesystem、text/input、browser mutation、その他すべては valid local-user signature があっても fail closed します。

current-state acceptance は terminal resolution ではなく permanent retired-indeterminate replay tombstone を再利用します。durable operation は `Indeterminate`、terminal receipt/result は無し、audit は `outcome=unknown`、`disposition=current_state_accepted`、`authority=local_user_presence`、reviewed policy/generation、bounded Human evidence metadata、`replayed=false` を記録します。解除するのは device quarantine だけです。

どちらの transition も persistence-gated で、checkpoint 永続化に失敗した場合は in-memory execution state を quarantine snapshot へ rollback し、success ACK は返しません。旧operationのresume/replayは一切行わず、後続workは必ず新しいoperation IDです。同一 accepted request ID の exact retransmission は同じ live recovery exchange 内で idempotent に ACK できますが、decision/policy/evidence の conflict は置換できません。

## Linux FIDO2 実装候補

このレーンは review / automated CI 用の実装まで進めていますが、#228 の physical Linux acceptance を記録するまでは **pre-support** です。libfido2 1.17.0+ tools を root 管理の distro/package path へ install し、使用する authenticator device を明示指定します。CUMG は device を自動選択・silent switch しません。`pin` mode は configured Client PIN、`builtin` mode は authenticator-integrated `uv` を要求し、mode間の silent fallback はしません。

専用 ES256 recovery credential を provision します。

```bash
v2_recover init-linux-fido2 \
  --tool-dir /usr/bin \
  --device /dev/hidraw0 \
  --uv-mode pin \
  --verifier-out "$HOME/.config/cumg/recovery-webauthn-verifier.json"
```

bounded verifier document だけを authenticated administrative channel で Hub へ移し、`<HUB_STATE_DIR>/recovery-webauthn-verifier.json` として install して Hub を restart します。authenticator private key と PIN は Hub/Agent state になりません。fresh valid challenge に対しては同じ provider parameters で実行します。

```bash
v2_recover resolve-linux-fido2 \
  --state-dir "$HOME/.local/state/cumg-v2-agent" \
  --hub-public-key-file "$HOME/.config/cumg/hub.pub" \
  --tool-dir /usr/bin \
  --device /dev/hidraw0 \
  --uv-mode pin \
  --verifier-file "$HOME/.config/cumg/recovery-webauthn-verifier.json" \
  --decision confirmed-completed \
  --evidence "local user inspected the current desktop" \
  --wait-secs 30
```

reviewed low-impact current-state path は同じ provider/verifier 引数で `accept-current-state-linux-fido2` を使用します。tool/device 欠落、CTAP2/ES256/UP/UV不足、PIN/biometric cancel/failure、malformed tool output、wrong credential/RP/challenge/signature、signed UP/UV不足はいずれも authorization publication 前に fail closed します。unsupported Linux deployment は offline `v2_maint` を継続使用します。

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

低レベルの `v2_recover status` / `resolve` / `confirm` は advanced diagnostics / break-glass 用 primitive として残します。historical outcome を証明できない reviewed low-impact `Scroll` / `MovePointer` ambiguity で、local Human が現在のscreenを continuation point として明示的に受け入れる場合だけ、別コマンドを使用します。

```bash
v2_recover accept-current-state \
  --state-dir "$HOME/Library/Application Support/cumg-v2-agent/state" \
  --hub-public-key-file "$HOME/Library/Application Support/cumg-v2-agent/trust/hub.pub" \
  --key-file "$HOME/Library/Application Support/cumg-v2-agent/recovery/recovery-key.sealed" \
  --secure-enclave-helper "$HOME/Library/Application Support/computer-use-mcp-gateway/bin/v2_recovery_enclave_helper" \
  --evidence "local human inspected and accepted the current screen as the continuation point" \
  --wait-secs 30
```

user-presence prompt 前に `historical_execution_outcome=indeterminate`、`operator_observation=current_state_accepted`、`operational_disposition=current_state_accepted`、reviewed retirement policy、`old_operation_replayed=false` を明示します。screen observation 自体は authority ではなく、separately provisioned recovery-key signature と Hub 側の全 policy/generation/quarantine check が必要です。`resolve` / `accept-current-state` はどちらも macOS user presence を要求し、`confirm` は decision/policy を含む exact safe metadata で publish 済み request を再確認します。`guide` の authoritative historical decision closed set は暗黙に current-state acceptance で拡張しません。ACK 欠落、timeout、stale receipt、mismatch は成功ではなく retry/replay authority にもなりません。

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

#137 は新しい cryptographic/user-presence provider を追加せず、`accept-current-state` も既にaccept済みの Secure Enclave helper/key/challenge/signature/relay path をそのまま再利用します。新しい safety claim は decision/policy semantics、generation/capability gate、persistence rollback、durable unknown-outcome tombstone、wording distinction であり、deterministic regression で固定します。user-presence provider または authority boundary 自体を変更する場合は新しい physical acceptance が必要です。

## Protocol互換性とchallenge更新

Online recoveryは現在の `HUB_AGENT_SCHEMA_VERSION = 5` application protocolの一部です。schema validationはfail-closedのままなので、このreleaseを有効にする際はHubとAgentを協調して更新し、mixed-version rolling compatibilityには依存しません。V1 gatewayの動作は変わりません。

Recovery challengeは300秒（5分）で期限切れになります。desktopがquarantineのままなら、通常のauthenticated Agent heartbeatを契機にHubがpending challengeを再確認し、期限切れ後はfresh nonceを持つchallengeを再発行します。承認時間切れだけを理由にHub/Agentを再起動する必要はありません。fresh challenge受信時は以前のlocal authorization handoffを無効化します。
