# V2 Ephemeral Data References

Status: v0.5.0 の #313 foundation に、current v0.5 branch で #83 process/shell output consumer を実装した状態です。

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

## #83 process/shell output consumer

#83 consumer でも通常の inline contract は変更しません。stdout / stderr は通常の process/shell result では各 16 KiB 上限のままです。

stream がこの inline cap を超え、dedicated Agent ephemeral-data parent が設定されている場合:

- stdout / stderr を別々に、display decode 前の raw byte として retain します。
- retain 上限は 1 stream あたり 4 MiB、1 operation 合計では最大 8 MiB です。
- retention ceiling 到達後も supervised pipe は EOF まで drain し、超過 byte は蓄積せず discard します。
- Agent から Hub へは opaque private locator と bounded metadata だけを返します。
- Hub は別の public `output_ref` を mint し、owner、device、generation、capability revision、source operation、stream kind、TTL に bind します。
- `read_process_output` は Shell/ExecuteProcess authority を継承せず、独立した exact Observe capability です。
- follow-up read は default 8 KiB、最大 64 KiB です。
- offset / length は retained prefix 上の raw-byte offset とし、northbound byte は base64 encode するため、UTF-16 / invalid UTF-8 でも offset semantics は曖昧になりません。
- `complete=true` は retained prefix が stream 全体を含むことを示し、`complete=false` は stream が 4 MiB ceiling を超え、その後続 byte が意図的に unavailable であることを示します。

dedicated ephemeral-data parent が未設定なら、process/shell execution は従来どおり 16 KiB inline result のみで動作し、live output ref は作りません。packaged path selection、readiness/permission preflight、upgrade/schema integration は #314 の責務です。

durable `get_operation` が persist するのは従来の bounded inline process/shell result だけです。live public ref、Agent locator、retained extended-output byte は persist しません。

## Recovery / quarantine

`ReadProcessOutput` は read-only recovery evidence として分類します。Hub は先に public ref を resolve し、retrieval を exact source operation に bind します。quarantine 中に admit できるのは、その quarantine が同じ source operation に属する場合だけです。この read が quarantine を settle したり terminal evidence を manufacture したり、replay authorization や mutation-resume barrier bypass に使われることはありません。

## Non-goals

これは次のものではありません。

- generic blob store
- bearer-capability system
- public host-filesystem handle API
- durable object storage
- ordinary Agent grant への principal identity 追加理由
- exact DeviceCapability authorization の代替
