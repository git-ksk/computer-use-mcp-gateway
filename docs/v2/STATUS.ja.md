# V2 ステータス

> この日本語版は [`STATUS.md`](STATUS.md) の翻訳です。**英語版を canonical（正典）とし、解釈に差がある場合は英語版を優先します。**

2026-08-31 時点の状況:

- **Desktop semantic path:** complete / accepted。same-context の native element click/type/key targeting と、real-Cua による background AX element-action evidence を含みます。
- **Browser core semantic path:** prepare、bind、inspect、navigate、click、type、dialog、pointer semantics まで complete / accepted です。
- **Browser transfer:** complete / accepted。upload/download は scoped CUMG ref と Agent-private の bounded staging を使用し、任意の host path を northbound に公開しません。private staging を確立できない場合も privacy を保ったまま、Agent startup が失敗した正確な bounded stage / I/O class を host-local に記録します。
- **Optional first-class Human Handoff:** CUMG では bounded Window / Terminal-PTY coordination を accepted 済みです。Window は upstream `WindowHandoffAdapter`、Terminal は upstream `TerminalHandoffAdapter` を使い、PTY/process containment と content-free verification は CUMG に残します。canonical Handoff runtime/checkpoint と Human surface は操作対象 Agent が所有し、Hub は CUMG authorization、ledger、quarantine、conservative pre-dispatch fence を保持します。runtime/transport loss 時は Handoff を迂回せず fail closed します。upstream Window #85 には upstream 自身の first-class same-LAN direct physical rerun が残りますが、CUMG #152 は closed のままです。
- **Current execution-safety boundary:** durable execution-safety schema は **v8** です。effectful work の結果が曖昧なら `Indeterminate` として device を quarantine し、automatic retry / replay は行いません。schema v6 は text/input delivery が適用されたが commit されていない状態を、本当の no-effect resolution と区別します。schema v7 は payload-free / privacy-preserving な text-input evidence envelope と optional keyed candidate matching を追加します。schema v8 は closed recovery-evidence read lane を追加し、明示 allowlist 済みの non-mutating evidence read だけを quarantine 中にも許可します。generic shell/process とすべての mutation/activation/write capability は引き続き拒否されます。
- **Operator inspection / reconciliation:** `v2_maint inspect-quarantine` は read-only で、raw payload / owner identity を公開せず、以前から device を block している正確な `blocking_operation_id` と bounded recovery metadata を返します。`v2_maint audit-reconciliation` は最新の Hub/Agent durable state を相関し、authoritative terminal evidence、legacy/non-authoritative marker、observational correlation を明示的に区別します。どちらも quarantine を clear せず replay authority にもなりません。`compare-quarantine-request` は correlation-only で、返せるのは `same_request`、`different_request`、`unavailable` だけです。
- **Operator-ready incident brief (#233):** `v2_maint incident-brief` は exact #133 reconciliation audit に bounded quarantine metadata と optional closed-schema diagnostic observation を合成します。external finding は `observational_only` / `unavailable` に強制され、authority を名乗ることも `supported_decisions` を広げることもできません。矛盾は CUMG evidence を上書きせず明示され、shared mutation owner/epoch も available/unavailable/not-requested として read-only に表示できます。JSON / compact text output は raw request/result/desktop/log payload を含みません。
- **Authoritative self-reconciliation / retirement:** execution-safety schema v4 は exact effectful dispatch fence を persist し、fresh authenticated generation 上の exact signed Agent terminal proof が揃う場合だけ self-resolve できます。v5 は permanently unknowable な legacy ambiguity のうち狭い allowlist だけを対象に、exact operation ID を permanent non-replayable のまま維持する別の offline retirement path を追加します。automatic settlement、manual resolution、retirement は意図的に別状態で、いずれも persistence-gated です。
- **Offline recovery compatibility:** authority-bearing `v2_maint resolve` は Hub の完全停止を要求し、authoritative checkpoint の既存 durable writer contract を維持します。maintenance binary は publication 前に representability を検証し、deployed Hub と version-paired artifact として install する必要があります。新しい checkout の maintenance binary を古い deployed Hub 所有 state に対して任意に使う運用は support しません。Issue #100 は complete です。trusted physical macOS の Secure Enclave/user-presence acceptance で実 ambiguous desktop operation を replay せず resolve し、Hub restart 後も durable resolution が authoritative のまま維持されることを確認しました。
- **Privacy-safe northbound failures:** live control schema は **v9** です。working-directory denial、timeout、program/environment policy、spawn failure、cancellation、`agent_offline`、`device_indeterminate` など expected execution-policy/runtime failure を bounded client-visible category として維持しつつ、raw path、command、environment value、device identity、provider text、OS error string は公開しません。unknown internal failure は引き続き generic に fail closed します。
- **Host reliability / diagnostics:** `v2_doctor` は restart-safety state を変更せず、authoritative に証明できる in-band diagnostic self-observation と本当の blocking quarantine を区別します。Browser staging startup failure は bounded local stage/I/O diagnostic を出します。controlled `StorageFull` injection により、Agent checkpoint persistence の容量枯渇が Agent を fail-closed exit させ、remote では `agent_offline` として見えることを確認済みです。失敗前の committed checkpoint / replay barrier は authoritative のまま維持され、容量復旧後は通常の service-manager reconnect で回復します。`v2_doctor` は state/temp filesystem の容量について coarse / read-only signal だけを公開します。
- **Process/shell response-loss recovery:** `execute_process` / `shell` は caller が保持できる stable `operation_id` を受け取り、Hub は replay せず、Agent liveness に依存しない owner/capability-scoped read-only `get_operation` を公開します。proven terminal output は northbound delivery より先に persist し、Agent generation rollover 後も bounded recovery archive（最大8件 / encoded total 256 KiB）で保持します。unknown/evicted reference になっても original operation が retry-safe になったことを意味しません。
- **Local-user online quarantine recovery:** explicit recovery-key provisioning の背後で実装済みです。Agent device key は recovery authority ではなく、fresh Hub-signed challenge は separately pinned P-256 endpoint recovery key だけで解決できます。初期 macOS signer は user-presence access control 付き Secure Enclave key を使用します。automated protocol/persistence coverage と trusted physical acceptance は完了し、local user presence 後にのみ authorization が publish され、ambiguous operation は durably resolve、restart 後も resolution を維持し、旧 operation は replay されませんでした。
- **Lane-scoped operator readiness (#226):** `v2_doctor` は existing authenticated state / capability / quarantine authority から導出する privacy-bounded readiness summary を追加します。control-plane、Computer Use observation、filesystem observation、generic effectful execution、browser-effectful execution を区別し、durable quarantine がある場合は independently healthy な observation lane を隠さず effectful lane だけを `indeterminate_fenced` と表示します。evidence 欠落・不整合は green と推測せず `unavailable` / `unknown` にします。この summary は diagnostic-only で、resolve / retry / replay / dispatch / authority grant はできません。
- **現在の productization priority:** current single-Mac reference path では macOS online recovery で必要な安全な回復 authority は成立しています。直近は operator/product integration（#226/#233/#234 -> #235 -> #236/#237）を優先します。cross-platform recovery parity は #217 配下の `0.4.0` work として維持し、#227 Windows Hello/WebAuthn と #228 Linux FIDO2 UV は、Product Readiness sequence が一貫するか concrete production evidence により前倒しが必要になるまで intentionally defer します。
- **V1 compatibility:** V1 は regression/reference と既存 deployment 向けに引き続き利用できます。V2 作業中も V1 regression/conformance coverage は必須です。残る #14/#15 は upstream Cua に blocked された observation として扱い、active CUMG release blocker にはしません。

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
- [Issue #100](https://github.com/git-ksk/computer-use-mcp-gateway/issues/100) / merged PR #101 — trusted physical macOS Secure Enclave/user-presence acceptance が pass。Hub restart 後も durable resolution を維持し、ambiguous operation の replay なしを確認済み。

## 履歴記録

初期 prototype と progress record は [`../archive/v2/`](../archive/v2/) に archive されています。設計の provenance を保持するための記録であり、現在の実行可能な setup instruction ではありません。
