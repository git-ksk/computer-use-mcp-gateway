# V2 Hosted Handoff topology

Status: **Issue #275 の architecture decision。hosted implementation / acceptance は pending**。

この文書は、将来の hosted CUMG Hub と `mcp-execution-handoff` を、物理 mutation authority を replaceable な cloud process へ移さずに構成する方法を定義します。#152 で受け入れ済みの Agent-owned Handoff boundary を拡張し、#215 の Cloud Run Hub support gate と合成します。

Cloud Run は reference hosted runtime であり、protocol requirement ではありません。同じ boundary は別の replaceable public Hub runtime でも維持できる必要があります。

## Decision summary

target architecture では、次の3つの authority を明確に分離し、同一視しません。

1. **CUMG execution-safety authority** は hosted Hub と external durable state が所有する。
2. **物理 Window / PTY の Handoff mutation authority** は controlled Agent 上の canonical Handoff runtime が所有する。
3. **hosted Human-session routing** は public Handoff routing/gateway plane が所有してよいが、routing state 自体は mutation authority ではない。

hosted Hub は conservative な signed Handoff dispatch fence を保持できます。この fence は早い段階で deny するためだけに使い、Agent-local Handoff runtime より execution を permissive にしてはいけません。

```text
MCP client / authenticated operator
              |
              v
      Hosted CUMG Hub
      - principal/device/capability authorization
      - operation ledger / quarantine / replay barrier
      - hosted writer epoch + durable revision
      - conservative Handoff dispatch fence
              |
              | authenticated outbound Agent channel
              v
      CUMG Agent / execution worker
      - Cua / PTY execution
      - canonical Handoff FSM/checkpoint
      - exact Target Surface authority
      - Desktop Session / viewer-generation boundary
      - capture/input + final pre-execution Handoff gate
          |                         ^
          | WebRTC direct/TURN     | authenticated outbound
          v                         | hosted routing/WSS channel
        Human <------------- Hosted Handoff routing plane
```

初期実装では CUMG Hub と hosted Handoff routing plane を同じ deployment に置いても構いません。ただし schema と authority meaning は分離し、後から process/service を分けても security model が変わらないようにします。

## canonical Handoff authority を Agent に残す理由

physical #152 acceptance では、初期の Hub-owned runtime placement が修正されました。capture、input、TCC/Accessibility、exact Window/PTY state、最終 mutation point は controlled device の属性であり、Hub host の属性ではありません。

canonical Handoff authority を Agent に残すと、次の性質を維持できます。

- Hub replacement が hosted state から Agent/Human authority を復元・生成できない。
- Hub の Handoff view が stale でも、Agent が Cua/PTY execution 直前に final gate を実行するため local mutation を許可できない。
- Human media/input と OS permission boundary が controlled device に残り、CUMG Hub の責務にならない。
- CUMG が `mcp-execution-handoff` authority FSM の2つ目の実装を持たない。

これは CUMG consumer 向けの upstream Handoff composition です。他の Handoff consumer すべてに同じ deployment shape を要求するものではありません。

## Responsibility split

### Hosted CUMG Hub

hosted Hub は次を authoritative に所有します。

- authenticated northbound principal;
- stable device identity と current authenticated Agent generation;
- exact capability authorization と semantic constraints;
- operation identity / admission / dispatch binding;
- durable terminal evidence と `Indeterminate` state;
- quarantine / reconciliation / permanent replay denial;
- #215 の hosted durable-state revision と writer epoch;
- bounded signed Agent status から得る conservative Handoff dispatch fence。

Hub は Handoff media/input、transport credential、local display/session continuity、canonical Agent/Human mutation authority を所有しません。

### Controlled Agent / execution worker

Agent は次を authoritative に所有します。

- canonical `mcp-execution-handoff` FSM と intervention epoch;
- Handoff checkpoint / recovery semantics;
- `agent | human | none` mutation authority;
- exact Window / Terminal Target Surface admission と revalidation;
- support 済みの場合の local Desktop Session / Display Backend continuity;
- local target に対する Human viewer / transport attachment;
- WebRTC/WSS host-side capture と Human input mechanics;
- OS-local Screen Recording / Accessibility / input permission boundary;
- Cua/PTY mutation 直前の final Handoff authority validation。

### Hosted Handoff routing plane

public routing plane が所有できるのは hosted-session concern に限定します。

- authenticated worker connection / routing identity;
- stable device reference と current Agent generation;
- routing/fencing に必要な intervention id / Handoff epoch;
- short-lived operator-session expiry;
- viewer / concrete transport generation;
- bounded readiness / revocation state;
- selected Handoff transport に必要な signaling / WSS routing。

これは **routing/session state であり execution authority ではありません**。process restart 後に Agent/Human mutation authority を復元する根拠として使ってはいけません。

## 3種類の durable-state meaning

hosted composition では、同じ database technology を使う場合でも次の store を論理的に分離します。

| State | Authority meaning | Example fields | mutation authority を復元できるか |
| --- | --- | --- | --- |
| CUMG authoritative state | execution ownership / ambiguity / replay safety | operation、device generation、dispatch、terminal evidence、quarantine、tombstone、writer epoch、revision | Handoff authority は復元しない。CUMG execution-safety state のみ復元 |
| Agent Handoff checkpoint | local Handoff lifecycle の bounded recovery hint | intervention、epoch、status、principal binding、expiry | **不可。** recovery は `reissue_and_revalidate` のみ |
| Hosted routing/session state | current Human session を current worker/intervention へrouteする | worker/device、Agent generation、intervention/epoch、viewer/transport generation、expiry | **不可** |

routing availability、worker heartbeat、新しい Hub instance、生存している database record を、以前の Human/Agent authority がまだ有効である証明として扱いません。

## Generation model

hosted CUMG では次の lifetime を独立したものとして扱います。

1. **CUMG Agent generation** — fresh authenticated Agent session 後に advance。
2. **Handoff intervention epoch** — 1つの Agent/Human authority lifecycle を fence。
3. **Desktop/application session** — support 済みの場合の persistent local application/display continuity。
4. **Human viewer generation** — 1つの current viewer attachment。
5. **Transport generation** — 1つの具体的 WebRTC/WSS transport attempt/attachment。
6. **Hosted Hub writer epoch/revision** — competing replaceable Hub instance を fence。

viewer reconnect / managed transport fallback では (4)/(5) だけを rotate でき、(3) を再生成したり、(2) を変えたり、(1) を advance してはいけません。Hub replacement による (6) の advance も Handoff authority change を意味しません。すべての stale generation は fail closed します。

CUMG が upstream Handoff pin を更新した後、この model は v0.4.1 Desktop Session / Display Backend separation を利用します。

## Hosted operator control

現在の `CUMG_V2_HANDOFF_CONTROL_SOCKET` は single-host/VM では適切な operator boundary ですが、hosted profile は Hub instance の filesystem access に依存できません。

したがって hosted profile では、local CLI と同じ narrow lifecycle intent を持つ closed authenticated operator-control surface が必要です。#277 adapter では review 対象の HTTP resource family を、short-lived context 発行用 `/operator/v1/handoff/context` と lifecycle command 用 `/operator/v1/handoff/control` に固定します。この router は OAuth 保護された operator resource であり、MCP tool は登録しません。

context handle 自体を target authority として扱いません。process-memory-only で bounded CUMG selection lifetime 内に失効し、発行 operator principal / action に bind したうえで、exact fresh CUMG selection + Agent generation/capability revision に再照合します。fresh selection では handle を rotate し、stale / cross-principal / cross-action / expired / generation-mismatch は fail closed します。hosted caller から raw PID/window identity は一切受け付けません。

必須条件:

- lifecycle control は ordinary northbound MCP tool discovery と分離する;
- operator authentication を明示する;
- authorization は exact `principal -> device -> handoff-control action` とする;
- caller が arbitrary PID/window authority を指定できない;
- `begin` / recovery は current device generation / capability revision に bind された fresh CUMG-authorized interaction/surface context だけを使う;
- raw OS target identity を hosted durable state に保持するより、short-lived opaque surface/context handle を優先する;
- Hub は bounded signed/fenced command だけを Agent-owned Handoff runtime へrelayする;
- locator/session capability material は short-lived とし、normal log / audit / durable CUMG state から除外する。

local/single-host deployment では Unix control socket を operator adapter として維持できます。hosted adapter と local adapter は、別々の Handoff semantics を定義せず同じ internal typed operator command model へ収束させます。

## Human data plane / connectivity

CUMG は transport/provider-blind のままにします。

upstream `mcp-execution-handoff#19` が provider-neutral WebRTC connectivity/relay を所有します。CUMG は STUN/TURN provider を選択せず、provider credential を semantic request / policy / log / checkpoint に持ち込みません。

hosted HTTP ingress と Human media/input path は別責務です。

```text
Human browser
  -> HTTPS hosted ingress -> Handoff signaling/session routing

Human browser
  -> WebRTC direct when viable
  -> Handoff-managed TURN relay when required
  -> Agent/worker

or

Human browser
  -> hosted WSS Handoff route
  -> authenticated worker channel
  -> Agent/worker
```

Hub が public だからという理由だけで frame を CUMG execution-safety Hub に流しません。review 済み Handoff transport が必要とする場合は hosted Handoff routing service が WSS transport data を運べますが、その service は CUMG execution-safety state の外側に置きます。

viewer disconnect、transport fallback、route loss は Human `Done` ではなく、Agent resume でも Human input replay permission でもありません。

## Dispatch invariant

protected effectful operation は、applicable な gate がすべて一致した場合だけ dispatch できます。

```text
CUMG principal/device/capability authorization
AND current hosted writer epoch + durable state revision
AND current authenticated Agent generation
AND no unresolved CUMG quarantine
AND current capability revision / semantic constraints
AND conservative Hub Handoff fence permits Agent
AND Agent-local canonical Handoff authority permits Agent
```

最後の check は backend mutation 直前に必須です。current durable writer epoch を持たない stale/compromised hosted instance は #215 により dispatch boundary へ到達できてはいけません。

Hub-side Handoff cache は fail-closed 専用です。stale/unknown status は work を deny できますが、permissive な cached status だけで Agent-local gate を迂回して execution してはいけません。

## Restart / partition behavior

### Hosted Hub replacement

- external durable backend から CUMG authoritative state のみrestoreする;
- mutation authority 前に fresh writer epoch を取得する;
- protected execution 前に authenticated Agent から fresh bounded Handoff status をsyncする;
- hosted routing state から Human viewer / locator / transport generation / Handoff mutation authority を復元しない。

### Agent/Handoff restart

- old ephemeral Agent/Human authority は失われる;
- Handoff checkpoint recovery は `reissue_and_revalidate` のまま;
- old locator/capability/viewer/transport generation は invalid のまま;
- explicit recovery/verification が fresh lifecycle を許可するまで protected execution をdenyする。

### Hub-Agent partition

- Hub connectivity loss で `human` / `none` authority を `agent` へ戻さない;
- surviving Human transport が automatic Agent resume を起こしてはいけない;
- reconnect は fresh Agent authentication/generation semantics と fresh bounded Handoff sync を行う。

### Human viewer loss

- viewer disconnect は Done ではない;
- supported boundary が維持されるなら Desktop/application continuity は local に残せる;
- reconnect/fallback は fresh viewer/transport generation を取得する;
- generation を跨いで Human input を replay しない。

## Privacy / secret boundary

以下は CUMG durable state、normal audit、generic hosted routing metadata に入れません。

- frame / screenshot / video / audio;
- raw Human input / entered text;
- password / OTP / MFA / challenge answer;
- browser cookie / target-service token / credential;
- opaque bounded context で代替できる場合の raw PID/window identity;
- ICE candidate / address / SDP;
- TURN username/password/token / provider API credential;
- live locator/capability/reconnect-handle value。

operational diagnostics は bounded / categorical / content-minimizing のままにします。

## Cloud Run composition

この設計は #215 Cloud Run support claim の dependency であり、#215 の代替ではありません。

Handoff-enabled Cloud Run profile では追加で次を必須とします。

- Hub-local Unix socket に依存しない operator control;
- Agent に public inbound listener を要求しない outbound Agent/worker connectivity;
- CUMG effect dispatch と同じ durable writer-epoch/revision fence と Handoff dispatch check の合成;
- CUMG authoritative state と独立して survive/fail できる bounded hosted session/routing state;
- old Hub instance / stale Human route が mutation できない restart/revision-rollout test;
- hosted topology 上で Human active -> Agent deny -> Done -> fresh verification -> explicit resume を通す physical Agent acceptance。

single-host/VM profile は引き続き有効で、hosted operator adapter の採用を必須にしません。

## Upstream Handoff adoption

現在の CUMG artifact manifest は Handoff commit `9a621d12524632fd717e5f8d84a42c29946ab662` を pin し、packaged dependency を `0.3.0` と記録しています。

upstream v0.4.1 は Desktop Session / Display Backend boundary を追加し、その後の roadmap では provider-neutral connectivity (#19) と hosted worker topology (#12) を分離しています。CUMG は current Windows #227 / CUMG `0.4.0` closeout と分離した lane で reviewed upstream release boundary を採用します。

consumer update に必要な evidence:

- exact source pin と package/manifest update;
- Window / Terminal first-class adapter compatibility;
- applicable な Desktop Session/viewer-generation invariant test;
- deterministic stale-generation/restart test;
- packaging/rollback compatibility;
- claim する exact OS/transport support row に必要な physical acceptance。

この設計文書を最新に見せるためだけに release-critical dependency を更新してはいけません。

## Implementation sequence

推奨順序:

1. **この architecture を固定 (#275)。** Agent-owned canonical Handoff authority と non-authoritative hosted routing を維持する。
2. **#276 で review 済み newer Handoff pin を別laneで採用。** v0.4.1-or-newer boundary を CUMG integration/acceptance evidence 付きでconsumeする。
3. **upstream provider-neutral connectivity (#19) をconsume。** consumer-visible provider-specific relay assumption を除去する。
4. **#277 で hosted operator/routing adapter を実装。** PR #281 で transport-neutral authorization core、principal/action-bound opaque context handle、MCP tool surface を持たない separate OAuth HTTP router まで実装済み。temporary な追加public portは作らず、production `v2_hub` listener composition は意図的に次段へ残す。
5. **#215 durable Hub/writer fencing + one-port hosted ingress を実装。** review 済み #277 router を one-port ingress にcomposeし、commit-before-dispatch boundary に Handoff check を合成する。
6. **hosted failure / physical acceptance。** Hub replacement、stale writer、Agent reconnect/restart、viewer reconnect、WSS/WebRTC fallback、Human Done/verification/resume lifecycle を含める。

interface が固定できている範囲では step 2-4 を並行実装できます。ただし hosted support claim が #215 durable-state/fencing gate を迂回することはできません。

## Acceptance matrix

Handoff-enabled hosted CUMG をsupportする前に最低限次を証明します。

| Scenario | Required result |
| --- | --- |
| stale Cloud Run revision が old Agent stream を保持 | writer/revision fence が dispatch deny |
| Human active 中に Hub restart | Hub が Agent/Human authority を復元せず、fresh Agent status 必須 |
| Human active 中に Agent restart | `reissue_and_revalidate`、stale Human/Agent authority なし |
| viewer reload/reconnect | viewer/transport generation のみ更新、Human-input replayなし |
| WebRTC direct -> WSS/TURN fallback | abandoned generation をfenceしてから new input generation |
| Hub-Agent partition | Human/none authority が自動で Agent authorityにならない |
| Human Done | consumer verification 前に mutable Human transport をfence |
| verification success | `ready_to_resume`、implicit Agent replayなし |
| explicit resume | fresh admitted action だけが Agent mutation authority を再取得可能 |
| route/session DB は生存、worker stale | routing record から mutation authority を復元不可 |
| 別Agent/deviceがroute claim | exact worker/device/generation binding で拒否 |

## Non-goals

この設計では次を追加しません。

- generic fleet manager / device discovery service / dashboard;
- live/ambiguous intervention の別deviceへのautomatic failover;
- whole-desktop authority / implicit Window -> Desktop escalation;
- CUMG-owned WebRTC/STUN/TURN provider logic;
- Hub内の2つ目のHandoff FSM;
- ordinary MCP toolとしてのremote-desktop primitive;
- Human `Done` を consequential Agent action のapproval/replay permissionとして扱うこと。

## Related work

- CUMG #152 — Agent-owned first-class Handoff architecture + physical acceptance。
- CUMG #215 — Cloud Run Hub durable state / writer fencing / hosted ingress / support gate。
- CUMG #275 — 本 hosted Handoff composition decision。
- CUMG #276 — reviewed Handoff v0.4.1+ consumer pin / Desktop Session boundary adoption。
- CUMG #277 — hosted Handoff operator control / routing adapter。
- `mcp-execution-handoff#19` — provider-neutral connectivity。
- `mcp-execution-handoff#12` — hosted control-plane / execution-worker topology。
