# V2 ステータス

> この日本語版は [`STATUS.md`](STATUS.md) の翻訳です。**英語版を canonical（正典）とし、解釈に差がある場合は英語版を優先します。**

2026-08-19 時点の状況:

- **Desktop semantic path:** complete / accepted。same-context の native element click/type/key targeting と、real-Cua による background AX element-action evidence を含みます。
- **Browser core semantic path:** prepare、bind、inspect、navigate、click、type、dialog、pointer semantics まで complete / accepted です。
- **Browser transfer:** complete / accepted。upload/download は scoped CUMG ref と Agent-private の bounded staging を使用し、任意の host path を northbound に公開しません。
- **Post-dispatch ambiguity hardening:** mutating command を dispatch した後に generic backend error、malformed/unprovable completion、response loss が発生した場合、adapter/Agent 境界では `BackendOutcomeIndeterminate` と分類し、Hub は reason `BackendOutcomeUnproven` を持つ durable `Indeterminate` として永続化します。その desktop に既に queue されていた work は cancel され、explicit かつ persistence-gated な resolution が完了するまで device は quarantine されたままです。automatic retry / replay は行いません。read-only command の backend failure は definite failure のまま扱える場合があります。northbound に返す operational failure は bounded MCP `CallToolResult`（`isError=true`、closed CUMG code）であり、transport/provider/`ExceptionGroup` の形を漏らしません。issue #47 の observable side-effect case は real-Cua browser-alert acceptance で検証しています。
- **Process/shell response-loss recovery:** `execute_process` / `shell` は caller が保持できる stable `operation_id` を受け取り、Hub は replay せず、Agent liveness に依存しない owner/capability-scoped read-only `get_operation` を公開します。proven terminal output は northbound delivery より先に persist し、Agent generation rollover 後も bounded recovery archive（最大8件 / encoded total 256 KiB）で保持します。unknown/evicted reference になっても original operation が retry-safe になったことを意味しません。
- **Privacy-preserving audit correlation:** execution-safety schema v3 は dispatch 前に bounded な workflow/client correlation label と optional な keyed shell/process request fingerprint を persist します。`inspect-quarantine` は raw request/result/credential payload を公開せず correlation / reconciliation guidance を示し、`compare-quarantine-request` が返せるのは `same_request`、`different_request`、`unavailable` だけです。correlation/fingerprint evidence は operation settlement、quarantine clear、replay authorization には使えず、schema v1/v2 restore も fail-closed compatibility を維持します。
- **Authoritative self-reconciliation:** execution-safety schema v4 は exact effectful dispatch fence と reconciliation state を persist します。Agent は ordinary execution が authoritative terminal result に到達した後だけ payload-free terminal proof を最大64件 journal し、fresh authenticated generation で再署名して報告します。original operation は再実行しません。Hub は operation/device/original-generation/capability-revision/capability/grant-fence が全て exact match する場合だけ self-resolve し、live quarantine を clear する前に terminal candidate を persist します。それ以外は `operator_required` または `unrecoverable_evidence_gap` を記録します。`v2_maint inspect-reconciliation-history` で bounded / safe な `auto_resolved` history を確認できます。capability schema v5 により old/new Hub-Agent mix は handshake で fail closed します。schema v1/v2/v3 durable execution state は表現可能な範囲で引き続き読めます。
- **V1 production:** V2 development branch による変更はありません。V2 の作業中も V1 regression / conformance coverage は必須です。

## 有効な契約

- [`V2_POSITIONING.ja.md`](V2_POSITIONING.ja.md) — canonical product boundary の日本語版。
- [`V2_P0_EXECUTION_SAFETY.md`](V2_P0_EXECUTION_SAFETY.md) — uncertainty-aware execution と no-auto-replay invariant。
- [`V2_OPERATION_RECOVERY.ja.md`](V2_OPERATION_RECOVERY.ja.md) — northbound response loss 後の durable / bounded process・shell result recovery。
- [`V2_INTERACTION_CONTEXT.md`](V2_INTERACTION_CONTEXT.md) — scoped interaction state と backend-reference ownership。
- [`V2_GUI_SEMANTIC_CAPABILITIES.ja.md`](V2_GUI_SEMANTIC_CAPABILITIES.ja.md) — Desktop semantic surface。
- [`V2_BROWSER_SEMANTIC_CAPABILITIES.md`](V2_BROWSER_SEMANTIC_CAPABILITIES.md) — Browser semantic surface と transfer boundary。
- [`V2_CUA_PARITY_MATRIX.ja.md`](V2_CUA_PARITY_MATRIX.ja.md) — Cua compatibility/parity classification。
- [`V2_THREAT_MODEL.ja.md`](V2_THREAT_MODEL.ja.md) — security claim と non-claim。
- [`V2_STANDARDIZATION.md`](V2_STANDARDIZATION.md) と [`V2_P2_REPLACEMENT_SEAMS.md`](V2_P2_REPLACEMENT_SEAMS.md) — maintained OSS / standards の replacement boundary。

## Acceptance evidence

- [`acceptance/V2_BROWSER_CORE_ACCEPTANCE.md`](acceptance/V2_BROWSER_CORE_ACCEPTANCE.md) — Browser core closeout。
- [`acceptance/V2_BROWSER_TRANSFER_ACCEPTANCE.md`](acceptance/V2_BROWSER_TRANSFER_ACCEPTANCE.md) — Browser transfer contract、threat control、automated coverage、trusted-Mac real-Cua evidence。
- [`acceptance/V2_LOCAL_DESKTOP_ACCEPTANCE.md`](acceptance/V2_LOCAL_DESKTOP_ACCEPTANCE.md) — trusted physical Desktop acceptance の procedure/evidence。
- [`acceptance/V2_M1_ACCEPTANCE.md`](acceptance/V2_M1_ACCEPTANCE.md) — 以前の secure-Agent milestone acceptance を evidence として保持。

## 履歴記録

初期 prototype と progress record は [`../archive/v2/`](../archive/v2/) に archive されています。設計の provenance を保持するための記録であり、現在の実行可能な setup instruction ではありません。
