# アーキテクチャ

> この日本語版は [`ARCHITECTURE.md`](ARCHITECTURE.md) の翻訳です。**英語版を canonical（正典）とし、解釈に差がある場合は英語版を優先します。**

V2 Hub + V2 Agent が推奨 runtime です。V1 は regression/reference 用の `v1_gateway` として保持されています。

## V1 legacy/reference

gateway は MCP server であると同時に MCP client です。

```text
Northbound                                  Southbound

MCP client
    |
    | MCP Streamable HTTP /mcp
    v
+-----------------------------+
| computer-use-mcp-gateway    |
|                             |
|  Host / Origin guards       |
|       |                     |
|  policy / audit             |
|       |                     |
|  dynamic tool snapshot      |
|       |                     |
|  backend abstraction        |
+-------|---------------------+
        |
        | MCP stdio
        v
   cua-driver mcp
```

### Responsibilities

**Gateway**
- MCP transport boundary
- Host/Origin transport guard
- backend lifecycle
- dynamic tool discovery と cached policy-filtered snapshot
- request forwarding
- deny-by-default exact-name policy enforcement
- audit/inspection 用 semantic tool risk classification
- upstream cancellation forwarding
- health / audit metadata

**Backend adapter**
- child-process connection lifecycle
- connection / operation timeout
- bounded reconnect/backoff
- 1つの physical desktop に対する operation serialization
- 実際の in-flight request ID を使う downstream MCP cancellation
- failed / cancelled state-changing call の no automatic replay
- platform が対応する場合の gateway-owned backend child PID/CPU/RSS telemetry

**Cua backend**
- screenshot
- accessibility/UI tree
- click/type/scroll
- window/application control
- platform permission

## Backend abstraction

V1 は Cua から始まりますが、public gateway surface に Cua semantics を hard-code しません。

```text
Backend
  connect()
  health()
  resource_metrics()
  list_tools()
  call_tool(..., cancellation)
  shutdown()
```

最初の implementation は `CuaBackend` です。

## State model

MCP `2026-07-28` は protocol-level HTTP session を削除します。そのため public request handler は HTTP MCP session に保存された client state に依存できません。

application-level state は MCP transport session から独立しています。

- `Gateway` は exact-name policy、semantic classifier、shared policy-filtered tool snapshot を所有する。
- `CuaBackend` は current MCP client service、direct child PID、synchronization lock を所有する。
- `tools/list` は backend discovery を refresh する。refresh が失敗した場合、last policy-filtered cached snapshot を返せる場合がある。
- policy-allowed `tools/call` が current snapshot に存在しない場合は1回 refresh し、それでも discovery が tool を確認できなければ fail closed する。
- cursor/focus/UI snapshot state が shared mutable desktop state なので、backend operation は serialize する。

## Tool classification

V1 は authorization と semantic classification を分離します。

exact tool name は `CUMG_ALLOW_TOOLS` / `CUMG_DENY_TOOLS` による enforcement boundary のままです。それとは独立して discovered/called tool を次に classify します。

- `observe`
- `interact`
- `system`
- `dangerous`

known Cua-compatible name は explicit に map します。unknown/new backend tool name は review されるまで `dangerous` と classify します。この classification は audit metadata / discovery count に含めますが、exact-name allowlist を黙って broaden するものではありません。

## Failure / cancellation model

read-only tool discovery は transport failure 後に reconnect / retry できます。state-changing computer-use call は desktop が既に action を部分適用している可能性があるため、安全に replay できません。そのため failed call は error として返し、recovery は later request のために試みます。

northbound MCP request が cancel されると gateway は signal を `CuaBackend` に forward します。backend は rmcp の cancellable-request API で downstream call を作成し、actual downstream request ID を保持し、その ID で `notifications/cancelled` を送ります。cancelled call は error として返し、replay しません。

per-call timeout も同じ no-replay safety rule に従います。backend は in-flight request に downstream cancellation notification を送り、その後、可能なら later request 用に connection を修復します。

## Health / resource telemetry

`/healthz` は backend readiness と、gateway が直接所有する backend child process の optional `backend_resources` snapshot を返します。

```json
{
  "status": "ok",
  "backend": "ready",
  "backend_resources": {
    "pid": 12345,
    "cpu_seconds": 0.12,
    "rss_bytes": 17817600
  }
}
```

`cpu_seconds` は cumulative process CPU time、`rss_bytes` は resident memory です。platform/process lookup が snapshot を提供できない場合、readiness decision を変えず `backend_resources` が `null` になる場合があります。

macOS では Cua が supported application/daemon lifecycle を介して proxy する場合があるため、これら metric は gateway が所有する direct child を示し、全 Cua process の aggregate resource use を表しません。

## Security boundary

gateway は internet authentication service ではありません。remote deployment では process を loopback に置き、その前段に authenticated TLS termination を配置します。`/mcp` boundary ではさらに Host authority と browser Origin value を検証します。

tool exposure は deny-by-default です。Cua 自身の policy engine は argument-aware な second capability ceiling を提供できます。

## V2 runtime

実際の northbound runtime は現在 V2 Hub です。default binary と explicit `v2_hub` binary は同じ entrypoint を共有し、`v2_agent` は別の outbound desktop process のままです。旧 single-process V1 entrypoint は `v1_gateway` として保持されています。

quota、billing、usage accounting は CUMG core の外側にある deployment-layer の責務です。reverse proxy、MCP edge、その他 operator-controlled component で Hub 到達前に制御できますが、その component は CUMG の operation identity、authorization、generation fencing、durable execution state、quarantine、replay admission、recovery を変更できません。

authoritative operation record は execution-safety schema v11 を使用します。schema v5 は schema-v4 の exact dispatch / reconciliation semantics をすべて維持しつつ、policy-eligible かつ permanently unknowable な `Indeterminate` operation のための独立した durable retirement ledger を追加します。schema v3 で導入した bounded audit correlation label と optional な keyed shell/process request fingerprint は引き続き non-authoritative であり、owner/device/generation/capability fence、terminal state、retry semantics、replay admission を変更できません。schema v4 はさらに effectful operation の exact pre-send dispatch binding（既存 authoritative な operation/device/original-generation/capability に加え capability revision + one-shot grant ID）と explicit reconciliation status を persist します。Agent は別の bounded / payload-free terminal-evidence journal を保持し、fresh authenticated session 後にだけ報告します。Hub が `Indeterminate` record を self-reconcile できるのは binding/evidence が exact equality の場合だけです。candidate terminal checkpoint を commit してから live controller を swap するため、persistence failure 時は quarantine が残ります。read-only maintenance は label、fingerprint presence/comparison、reconciliation status、bounded auto-resolution history を公開できますが、raw request/result、fingerprint/key value、owner principal、dispatch-fence value は公開しません。schema v1/v2/v3/v4/v5/v6/v7/v8/v9/v10 checkpoint は元の representational limit 内で引き続き読め、新しい state を失う downgrade は拒否します。retirement は historical execution outcome を変更せず、operation は `Indeterminate` のまま、exact ID は permanent non-replayable のままです。解除するのは device quarantine だけで、現在 eligible なのは strictly newer な durable device generation が original session を fence した後の `Scroll` / `MovePointer` に限定します。従来の `retired` disposition は引き続き exclusive offline local maintenance のみに存在し、versioned retirement policy の明示指定を要求します。schema v10 は同じ permanent tombstone primitive に、同じ reviewed `Scroll` / `MovePointer` allowlist だけを対象とする `current_state_accepted` を追加しますが、利用できるのは separately authenticated local-user-presence online recovery だけです。terminal receipt / historical outcome は作らず、どちらの path も northbound MCP capability ではありません。schema v11 は permanent replay denial と detailed retirement history を分離します。exact retired operation ID は lossless compact tombstone index に最大4096件まで保持し、詳細 retirement record は64件で oldest-first rotation します。詳細 record が rotation したときは対応する full retired operation detail も compact しますが replay deny は残ります。tombstone corruption、legacy/current encoding 混在、capacity exhaustion は fail closed します。

execution-safety schema v6 は v5 の retirement contract を維持したまま、text input の ambiguity に対する bounded な manual resolution `confirmed_effect_applied_uncommitted` を追加します。これは `type_text` / browser text input に限り、input side effect は発生したが独立した submit/commit action は発生していないことを independent evidence で確認できた場合だけ使用できます。元 operation は terminal replay tombstone となり retry/replay は一切許可せず、`confirmed_not_executed` と `confirmed_completed` のどちらとも明確に区別されます。この v6-only resolution を含む checkpoint は、その区別を失う v5 への downgrade を拒否します。

execution-safety schema v7 は `type_text` のための versioned / privacy-preserving evidence envelope を追加します。durable envelope が保持するのは byte数・文字数・行数、末尾 newline の有無、独立した submit 要求の有無、authorized target の process/window class、delivery mode、delay といった bounded shape metadata だけで、入力 plaintext は保存しません。deployment-private な audit fingerprint key が設定されている場合だけ、domain-separated HMAC-SHA256 fingerprint も保持し、local に保有する candidate text を same/different/unavailable として比較できます。HMAC/key 自体は公開しません。key 未設定でも shape envelope は persist しますが candidate equality は unavailable です。この evidence は non-authoritative であり、execution/completion の proof、quarantine clear、replay authorization にはなりません。v7 envelope を含む checkpoint は、その reconciliation evidence を失う v6 への downgrade を拒否します。

execution-safety schema v8 は、すでに quarantine 中の device に対する独立した `recovery_evidence_read` lane を追加します。eligible capability は、既存 semantics で non-mutating と証明済みの closed allowlist（`list_applications`、screen geometry/screenshot、approved root 内の file/directory read、window/UI inspection、clipboard read、pointer position、region capture、browser inspect/bind）に限定します。generic `shell` / `execute_process`、activation、input、pointer mutation、write、launch/termination、browser navigation/type/download/upload、および allowlist 外の capability は引き続き拒否します。この lane は元の quarantine を clear / supersede せず、ambiguous operation の replay も行いません。lane marker は durable / auditable です。transport、cancellation、restart、backend result delivery が unprovable になった場合は evidence read 自体を `recovery_read_interrupted` の Failed として終端し、2つ目の quarantine は作りません。persistent quarantine が存在する間、`v2_doctor` は `restricted_read_only` を明示します。v8-only lane/evidence state を含む checkpoint は v7 への downgrade を拒否します。

execution-safety schema v9 は existing stable operation identity をすべての effectful northbound Desktop/Browser capability へ拡張します。caller は dispatch 前に operation ID を保持し、後から Agent へ contact せず work を replay しない owner-scoped / exact-capability-authorized `get_operation` を利用できます。process/shell は bounded output recovery を維持し、それ以外の effectful capability は payload-free terminal marker と already-authoritative な terminal state/receipt だけを persist します。raw screenshot、typed text、URL、clipboard content、backend result payload、GUI state を recovery storage へコピーしません。bounded recovery archive は最大8件 / 256 KiB のままです。v9 marker を含む checkpoint の v8 downgrade は拒否します。

execution-safety schema v10 は reviewed low-impact GUI ambiguity のため独立した `current_state_accepted` retirement disposition を追加します。v5 permanent replay tombstone を再利用しますが historical settlement は行わず、operation は `Indeterminate`、terminal receipt なし、exact old operation ID は permanent non-replayable のままです。online path は separately provisioned local-user recovery key、fresh Hub-signed challenge、exact quarantine fingerprint、exact current authenticated/durable generation、original dispatch より strictly newer generation を必須にします。eligible capability は `Scroll` / `MovePointer` だけです。durable audit は `outcome=unknown`、`disposition=current_state_accepted`、`authority=local_user_presence`、bounded evidence metadata、`replayed=false` を記録します。この v10 distinction を含む checkpoint の v9 downgrade は拒否します。schema v11 は compact permanent replay-deny index と bounded recent retirement history を分離し、detail rotation 後の state は tombstone を失わず v10 で表現できないため downgrade を拒否します。

## V2 accepted boundary

V2 は **「V1 + multi-machine routing」ではありません。** completed competitor-gap PoC と GO/NO-GO review により、accepted boundary は **stateful interactive desktop の delegated control に対する uncertainty-aware execution safety** に絞られました。Hub/Agent topology はその safety boundary の implementation vehicle であり、generic fleet/device-fabric product ではありません。

```text
MCP Client
   |
   | MCP
   v
Hub
   |
   | authenticated, typed, backend-neutral command/grant protocol
   v
outbound Agent
   |
   +-- direct process/shell executor
   +-- bounded filesystem read/list capabilities
   +-- GUI/computer-use adapter
        +-- Cua MCP backend
        +-- future native GUI backend
```

差別化される control semantics は次を中心にします。

- cryptographic device identity / enrollment
- expiry/revocation/replay rule を持つ short-lived capability grant
- explicit operation ID / per-device lease ownership
- fail-closed cancellation/reconnect behavior
- raw desktop-content logging を伴わない policy-decision evidence
- backend-neutral capability contract

transport は implementation choice であり product boundary ではありません。M1 production candidate は **gRPC bidirectional streaming over TLS** です。以前の raw TLS transport は regression/reference implementation として残ります。application command/grant schema は transport-neutral のままなので、将来 WebSocket、QUIC、その他 transport adapter に変えても semantics は再定義されません。最初の gRPC migration slice では Protobuf が RPC/carrier framing を所有し、existing independently signed application message は bounded carrier 内で unchanged のままです。transport migration と security-protocol rewrite を同時に coupling しないための設計です。

Cua は重要な GUI/computer-use backend のままですが、Cua-specific tool name / wire behavior を permanent Hub-to-Agent protocol にしてはいけません。reviewed parity target と stateful interaction boundary は [`V2_CUA_PARITY_MATRIX.ja.md`](v2/V2_CUA_PARITY_MATRIX.ja.md) と [`V2_INTERACTION_CONTEXT.md`](v2/V2_INTERACTION_CONTEXT.md) にあります。V2 は backend-neutral semantic GUI vocabulary を定義し、`ListWindows`、`LaunchApplication`、`InspectWindow`、`VerifyUiState` は typed CUMG capability とし、Cua の `list_windows`、`launch_app`、`get_window_state`、`verify_state`、AX role、session helper は adapter で終端します。これは minimum-common-denominator restriction ではなく、各 backend が実装する semantic subset を advertise します。[`V2_GUI_SEMANTIC_CAPABILITIES.ja.md`](v2/V2_GUI_SEMANTIC_CAPABILITIES.ja.md) を参照してください。

Direct process/shell execution は Agent 自身が所有し、Cua 経由で terminal window を automation する形で実装してはいけません。structured argv execution は implicit shell parsing を避けますが `Dangerous` のままです。script/interpreter は arbitrary code を実行でき、cwd policy は argv filesystem access を confine しません。そのため M1 の Agent-native process grant は exact device capability に scope します。別の `ReadFile`/`ListDirectory` observation capability は approved root 配下で path を canonicalize し、returned content を bound し、symlink escape を拒否し、coarse error を返します。これら read-only operation は narrower capability surface ですが `ExecuteProcess` 自体を制限しません。free-form shell execution は exact grant と distinct command/result type を持つ separate implemented `Dangerous` capability です。固定 OS shell を intentional に invoke するため、`ExecuteProcess` を widen せず shell parsing risk を受け入れます。explicit filesystem mutation は separate future higher-risk surface のままです。


Handoffは **optional integration / first-class authority** の原則に従います。通常のCUMG process/shell executionやGUI operationはHandoff runtimeを必須とせず、deploymentからHandoff coordinatorを省略できます。一方、Handoffを設定した場合はbest-effort sidecarとして扱いません。CUMGはadmission boundaryでcoordinatorを参照し、Agentはbackend execution直前にsigned authority/surface bindingを再検証します。active Human authority中はCuaなどtarget-surface backendへdispatchする前にAgentをfenceします。Handoffをdisable/omitする場合はoptional capability自体をなくすのであって、弱いfallback pathを作りません。canonical Handoff FSM、checkpoint/recovery semantics、Human transport、capture、inputはcontrolled Agent上のHandoff runtimeが所有し、CUMG coreへ同じstate machineやWebRTC/TURN mechanicsを複製しません。

consumer boundaryはexperimental部品の手組みではなくfirst-class component利用へ移行済みです。Window handoffはupstream `WindowHandoffAdapter`、Terminal/PTY handoffはupstream `TerminalHandoffAdapter` をinstantiateします。CUMG自身が `TakeoverBroker` + Window WebRTC runtimeを組み立てず、production contractとして `ExperimentalTerminalPtyAuthority` / `ExperimentalTerminalWebRtcTakeover` を直接importしません。Rust PTY coordinationとmanaged Node Handoff runtime間のcompatibility wireはCUMG-internalに留め、authority / epoch / session / transport orderingはHandoff、exact PTY、writer drain、process/descendant containment、content-free verification、operation ledger、quarantine、replay policyはCUMGが所有します。production stagingではservice drainより前にfirst-class Window / Terminal exportをpreflightし、不完全なHandoff runtime packagingはfail closedにします。

implementation order は意図的に shell-first です。secure Agent core を確立し、direct process/shell execution を追加し、それら workflow に必要な bounded filesystem operation のみを追加し、transition 中は GUI/computer-use に Cua を保持し、その後 native GUI backend を追加します。これにより GUI backend replacement と、development/operations task に対する Agent の有用性を分離します。

M1 operator-facing `v2_agent` process は production-candidate carrier として outbound gRPC bidirectional streaming over TLS を使います。Agent-native work が async receive loop の外で動いている間も heartbeat/cancellation を受け取り、transport loss 後に bounded reconnect を行います。companion `v2_hub` process は single-device always-on-VM runtime です。enrolled Agent を authenticate し、risky transition より前に generation/admission state を persist し、heartbeat/offline state を維持し、exact-capability grant を発行し、ambiguous disconnect outcome を conservative に `indeterminate` と mark します。raw TLS carrier は deployment default ではなく regression/reference implementation のままです。

V2-M0 PoC とその後の competitor review はこの stop rule を適用しました。CUMG を別の generic remote-device orchestrator、fleet platform、device fabric、remote desktop、delegated-authorization protocol へ広げません。

milestone / explicit non-goal は [`ROADMAP.md`](ROADMAP.md)、V2 trust boundary、compromise assumption、key rotation、replay、cancellation、residual risk は [`V2_THREAT_MODEL.ja.md`](v2/V2_THREAT_MODEL.ja.md) を参照してください。
