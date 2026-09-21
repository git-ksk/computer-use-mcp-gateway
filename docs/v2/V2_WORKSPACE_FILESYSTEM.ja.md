# V2 least-privilege workspace filesystem observation

> 英語版が canonical です。[English](V2_WORKSPACE_FILESYSTEM.md)

Issue #105 は shell authority を追加せず read-only filesystem lane を実用化します。bounded stateless file range、deterministic bounded directory continuation、将来 #107 mutation と共有する capability-rooted path primitive を追加します。

## Authority boundary

- observation root は `allowed_file_roots` だけで、cwd root を継承しません。
- ambient canonicalization は configured root の選択だけに使います。actual target は open 済み `cap_std::fs::Dir` から relative に reopen し、canonicalize-then-open pathname を authority proof にしません。
- symlink/reparse escape は fail closed です。`v2_workspace_path` の shared primitive に Unix replacement-race test と Windows junction replacement test を持ちます。
- 残余 assumption: configured root path は operator-controlled な startup policy であり、`FilesystemPolicy` 構築中に adversarial に差し替えられない前提です。#105 は accepted root を open capability として固定し、その後の operation を閉じ込めます。#314 で root trust の packaged permissions/ACL preflight を統合しました。
- `ExecuteProcess` / `Shell` を sandbox 化するものではなく、両者は引き続き別の Dangerous capability です。

## Stateless file range

`read_file` は required `path`、default `0` の optional unsigned `offset`、最大 8 KiB の optional positive `max_bytes` を受け取ります。result は `offset`、`bytes`、`truncated`、`next_offset` を返し、overflow は fail closed、EOF は empty/non-truncated です。

各 call は独立した observation です。`next_offset` は convenience cursor であって snapshot token ではありません。call 間で file が変わり得るため、range を連結して同一 immutable file version と仮定しません。

## Deterministic directory continuation

`list_directory` は required `path` と optional `after` を受け取ります。entry は UTF-8 name で sort してから page 化し、result は `after` を echo して `truncated` / `next_cursor` を返します。次 page は `next_cursor` を `after` に渡します。

cursor は stateless で、call 間の contents が安定していることを前提にします。1 call は最大 4,096 entries を scan、最大 256 entries を返し、serialized entry に conservative な 24 KiB budget を持ちます。scan budget 超過は `filesystem_directory_scan_limit_exceeded` で fail closed し、単一 entry が result budget に収まらない場合も fail closed です。

raw file contents、requested path、continuation value を telemetry に追加せず、workspace-path Debug output は redact します。

## Schema integration

#105 は typed `DeviceCommand` / `DeviceResult` filesystem shape を変更しますが、`CONTROL_SCHEMA_VERSION`、`CAPABILITY_SCHEMA_VERSION`、`HUB_AGENT_SCHEMA_VERSION` は単独で bump しません。#314 で final migration pairing、mixed-version refusal、packaged readiness、v0.4.0 -> v0.5.0 upgrade acceptance を control schema 10 / capability schema 6 として統合しました。

result/command matching は file response を requested offset/byte limit に、directory response を requested cursor に bind し、食い違う continuation response を拒否します。
