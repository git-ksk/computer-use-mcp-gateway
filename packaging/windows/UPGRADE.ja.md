# Windows V2 ペアアップグレード

`v2_windows_upgrade.py` は Windows V2 persistence profile 向けの fail-closed なアップグレード境界です。Hub と Agent を個別に差し替えず、Windows release-candidate の完全なランタイム一式を同一 artifact identity として更新します。

## 契約

`v2_release_candidate.py` で抽出済み Windows candidate の manifest と SHA-256 を検証し、`v2_hub`、`v2_agent`、`v2_maint`、`v2_keyctl`、`v2_tls_check` をまとめて stage します。稼働中ランタイムを停止する前に、stage 済み Hub/Agent の `--help` から必須 CLI flag を取得し、candidate config に不足があれば拒否します。v0.4.0 の `--allowed-file-root` のような migration は service drain 前に検出されます。

activation は Agent 停止、Hub 停止、reviewed config と完全なランタイム一式の配置、Hub 起動と loopback listener 確認、Agent 起動、安定した Agent PID と新しい Hub の `v2_agent_session_accepted` 確認、必要なら認証済み外部 route smoke、の順で行います。Caddy/proxy、enrollment、trust、secret、Hub/Agent の durable state は置き換えません。

service drain 後に activation または health gate が失敗した場合、以前の Hub/Agent config と更新前に存在した全 Windows runtime binary を一組として復元し、Hub → Agent の順で再起動して復旧を確認します。rollback 自体が失敗した場合は `operator_action_required` として fail closed し、調査が終わるまで次の upgrade を拒否します。

## 運用

最初は必ず `--preflight-only` で実行します。preflight は artifact identity と config compatibility を検証しますが Hub/Agent は停止しません。成功確認後のみ `--preflight-only` を外します。認証済み外部 route smoke がある環境では `--external-smoke-script` を指定し、その script が exit 0 になるまで upgrade 完了扱いにしません。

durable な運用記録は `v2-windows-shell\state\upgrade\windows-upgrade-status.json` に保存されます。stage は `staging\<transaction>`、rollback asset は `backup\<transaction>` に保存されます。成功後も rollback asset は運用者が明示的に整理するまで保持します。

minor schema boundary をまたいで `v2_agent.exe` だけ、または `v2_hub.exe` だけを手動コピーしないでください。upgrader が停止するのは reviewed scheduled task と、reviewed config の executable identity に一致する PID-file child のみです。
