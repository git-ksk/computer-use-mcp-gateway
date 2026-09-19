# V2 Ephemeral Data References

Status: v0.5.0 の #313 foundation です。

この日本語版は V2_EPHEMERAL_DATA_REFS.md の翻訳です。英語版を canonical とします。

## Boundary

V2 の bounded ephemeral data ref は、通常の least-privilege workspace response に直接載せるには大きすぎる、または continuation state が必要な data にだけ使います。最初の consumer は truncated process/shell output（#83）と、必要な場合だけ deterministic directory continuation（#105）を想定します。

この ref は意図的に **non-authoritative** です。

- device capability を authorize しません。
- operation settlement、quarantine clear、effectful action completion の proof にはなりません。
- checkpoint / recovery / release / backup の truth ではありません。
- Agent filesystem path を公開しません。

## Split authority

authenticated northbound principal は Hub に存在します。通常の southbound grant は意図的にその principal identity を持ちません。そのため public ref の authorization は Hub-side に置きます。

Hub public-ref registry は owner、device、generation、capability revision、operation/kind、expiry、opaque Agent locator を bind します。Agent private data store は opaque locator と bounded bytes を device、generation、capability revision、operation、kind、local lifetime に bind します。

public ref と Agent locator は別の random value です。どちらも possession だけでは authority になりません。caller は通常の northbound authentication / authorization を通過した後でのみ Hub が public ref を resolve し、exact retrieval operation を dispatch します。

## Lifecycle / resource limit

foundation は短い TTL と count/byte ceiling を持ちます。expiry 判定には trusted Hub/Agent runtime または session-derived time を使い、northbound caller が authoritative clock を指定することはありません。Agent staging は private、regular-file-only、per-object / global byte bounded で、expiry、explicit removal、clean shutdown、または Agent-store startup で削除します。専用 storage parent は authoritative Agent checkpoint / rollback backup tree の外に置き、startup cleanup はその配下の固定 ephemeral child だけを削除します。startup cleanup により Agent restart 後は outstanding locator がすべて stale になり得ます。

これは意図した挙動です。durable get_operation に live public ref / Agent locator を保存してはいけません。authoritative operation record と独立に stale になるためです。

resource exhaustion は fail closed します。通常の inline output/file limit を引き上げたり、filesystem/process authority を広げたりしません。

Windows を含む packaged deployment では、authoritative Agent state/rollback tree とは別の専用 ephemeral root を作り、その親 directory の reviewed ACL boundary を継承させます。別の ACL authority model をここで増やさず、exact path / ACL / backup exclusion の preflight は v0.5 release integration gate（#314）が担当します。

## Privacy / observability

raw staged bytes、Agent path、public ref、private locator は default telemetry に出しません。expiry、quota、generation、revision、operation、kind mismatch は payload / host path を含まない stable error category だけを公開できます。wrong-owner は unknown ref と同じ stale category に潰し、cross-principal existence oracle を作りません。

## Recovery / quarantine

将来 read-only output retrieval を exact original owner/operation に対する bounded recovery evidence として admit する場合も、read lane のままです。retrieved bytes は terminal evidence を manufacture せず、replay authorization、quarantine clear、mutation-resume barrier bypass に使いません。

## Non-goals

これは次のものではありません。

- generic blob store
- bearer-capability system
- public host-filesystem handle API
- durable object storage
- ordinary Agent grant への principal identity 追加理由
- exact DeviceCapability authorization の代替
