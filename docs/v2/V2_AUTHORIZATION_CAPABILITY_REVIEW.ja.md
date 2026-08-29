# V2 Authorization / Capability Boundary Review

Status: #219 の reviewed design decision（2026-08-29）。

この文書は、current Cua Driver を adjacent reference として使った CUMG の authorization / capability review を記録します。Cua compatibility plan ではなく、`V2_POSITIONING.ja.md` と `V2_EXECUTION_ENVIRONMENT_BOUNDARY.ja.md` の product boundary を変更しません。

## Decision summary

CUMG は execution-safety control plane という境界を維持します。

> 特定の stateful interactive computer を authenticated Agent / Human authority 間で委任し、operation ownership、ambiguity state、recovery authority、handoff continuity を失わない。

CUMG を AI-PC、sandbox、fleet、remote-desktop、generic policy-engine product にはしません。

この review から新たに分割する implementation follow-up は2件だけです。

- #221: 既存 authorization boundary へ typed backend-neutral semantic constraints を追加する;
- #222: second real computer-use backend で GUI portability evidence を作る。

multi-principal caller identity は #139、Windows/Linux local-user recovery は #217、hosted Hub は #215 が引き続き owner です。

## Reference baseline

2026-08-29 時点の Cua Driver docs と source、`trycua/cua@63c700d78aec868e7151c8d982263a4f7f146ade` を review baseline としました。

その baseline で参考にした Cua の性質は次です。

- public SDK/MCP/daemon path が platform dispatch 前に1つの native authorization coordinator を通る;
- configured managed policy と user policy は narrow-only で、両方の allow が必要;
- configured policy が missing/invalid の場合、action endpoint bind 前に fail closed;
- policy snapshot は runtime lifetime 中 immutable;
- 一部 tool arguments へ constraint を設定できる;
- caller identity/authentication は policy engine の責務外;
- optional host authorization は trusted embedding boundary で、Agent tool ではない;
- Computer History は opt-in / encrypted / local / metadata-bounded で、raw screenshot、typed text、clipboard、raw arguments/results、path、window title、URL を保存しない。

これらは reference property であり、CUMG API requirement ではありません。

## Current CUMG boundary

CUMG には、1つの policy engine に潰してはいけない authority separation がすでにあります。

1. **Northbound caller identity** — verified authentication adapter が `AuthenticatedClientPrincipal { issuer, subject }` を生成する。
2. **Exact semantic authorization** — `DeviceCapabilityAuthorizer` が principal -> stable device -> exact `DeviceCapability` を判定する。
3. **Grant-signing ceiling** — packaged external signer が stable device、exact capability、TTL、clock skew を独立に制限する。
4. **Agent/device identity** — Agent が enrolled device key と current authenticated generation を証明する。
5. **Hub transport identity** — Agent device identity / grant key と独立して pin される。
6. **Execution ownership/state** — CUMG が operation identity、owner、dispatch fence、durable terminal/ambiguous state、quarantine、no-auto-replay を所有する。
7. **Human Handoff authority** — `HandoffCoordinator` が CUMG admission と canonical `mcp-execution-handoff` authority FSM を合成し、CUMG は FSM を重複実装しない。
8. **Local-user recovery authority** — separate user-presence verifier が exact quarantine を resolve できる。Agent/device identity だけでは不可。
9. **Execution provider/backend** — Cua や別 provider は semantic adapter の下にあり、settlement authority を持たない。

ordinary MCP execution path は command dispatch 前に exact capability authorization を実施します。tool discovery は filtered view に過ぎず、authorization として扱いません。Handoff admission は additional authority gate であり exact capability authorization の代替ではありません。

## Adopt / adapt / reject

| Reference concept | Decision | CUMG disposition |
| --- | --- | --- |
| provider dispatch 前の1つの logical authorization point | **Adopt** | ordinary northbound execution は既存 exact CUMG authorization を必須とする。typed semantic constraint は narrow できるが bypass できない。 |
| configured missing/invalid authorization state の fail closed | **Adopt** | deployment が constraint/authority source を宣言した場合、invalid/unavailable を silent fallback しない。 |
| 1 runtime authority generation 内の immutable authorization snapshot | **Adopt** | authority widening は reviewed restart/revision/generation transition を必要とし、Agent-facing hot-widen path を作らない。 |
| Agent が administrator authority を widen できない | **Adopt** | caller、Agent、provider、narrower session policy は operator ceiling と intersection するだけにする。 |
| per-capability argument constraints | **Adapt** | threat/operational value がある typed backend-neutral semantic ceiling のみ #221 で追加する。provider/tool argument policy を permanent contract にしない。 |
| managed/admin + user/session policy layering | **Adapt** | narrow-only composition だけを model 化する。この性質のために generic policy-language stack は不要。 |
| policy decision audit | **Adapt** | stable な decision/reason/snapshot metadata のみ privacy-bounded に残す。raw arguments、text、URL、screenshot、credential、policy content は normal audit にしない。 |
| trusted host residual authorization callback | **core CUMG FSM として Reject** | CUMG には distinct Handoff / local-user recovery authority がある。将来 hook を追加する場合も concrete boundary が必要で、second Human consent/handoff FSM は作らない。 |
| `standard` / `bounded` / `unrestricted` product modes | **Reject** | explicit exact capability authorization と fail-closed deployment config を維持し、northbound unrestricted mode は作らない。 |
| YAML/Rego/OPA feature parity | **Reject** | generic policy language は replaceable infrastructure。external engine は CUMG semantic を維持する authorizer seam 実装としてのみ利用可能。 |
| general activity product としての Computer History | **Reject** | execution safety / recovery / bounded operation に必要な evidence だけを保持する。一方 privacy-minimizing metadata posture は維持する価値がある。 |
| Fleet/sandbox/VM provisioning | **Reject** | execution environment は downstream infrastructure。CUMG core は disposable compute を schedule/provision しない。 |
| separate provider/backend provenance | **Adapt** | provider identity を caller/Human/device authority と分離する。#222 では northbound provider ID を作らず bounded provenance を evidence として扱える。 |
| second real computer-use backend | **Adapt / evidence として必要** | deterministic reference executor は core state-machine replaceability を示すが、real GUI semantic neutrality は示さない。#222 が small real-backend portability proof を所有する。 |

## Typed semantic constraints: smallest useful model

implementation owner は #221 です。この review では policy DSL を規定しません。

authorization sequence は概念的に次を維持します。

```text
verified caller principal
        |
exact principal/device/DeviceCapability authorization
        |
optional typed semantic constraint intersection
        |
Handoff/session authority admission where applicable
        |
durable CUMG operation admission + dispatch fence
        |
Agent revalidation / independent grant ceiling
        |
backend adapter / execution provider
```

semantic constraint は backend replacement 後も意味が変わらない場合だけ採用できます。initial candidate は、より小さい text-input byte ceiling、reviewed normalized application identity set、reviewed browser origin/scheme set です。process/filesystem/root limit は既存 dedicated security control と合成し、argument matcher を OS sandbox のように表現してはいけません。

provider dispatch 前の constraint denial は definite refusal であり `Indeterminate` ではありません。provider dispatch 後は既存 ambiguity model を一切弱めません。timeout、reconnect、cancellation、malformed completion、generic provider error は、CUMG が authoritative と明示した non-execution evidence がない限り未実行証明になりません。

## Handoff / recovery composition

authorization は Agent principal が semantic capability を request 可能かを判定します。interactive surface の current owner は別問題です。

Human Handoff 中は次を維持します。

- exact principal/device/capability authorization は引き続き必要;
- capability が allow でも `HandoffCoordinator` は Agent authority を suspend できる;
- canonical Handoff runtime が Agent/Human authority epoch、`Done -> verifying`、resume policy を所有する;
- CUMG は safe handback に必要な semantic postcondition/evidence requirement を所有する;
- transport reconnect だけで Agent authority を作ったり ambiguous operation を settle したりできない。

local-user quarantine recovery は separate authority のままです。policy allow decision で quarantine clear、historical truth rewrite、replay authorization はできません。

## Identity separation

次の identity/authority を混同しません。

- northbound authenticated Agent/service principal;
- Human Handoff authority/epoch;
- stable device identity / current Agent generation;
- Hub transport identity;
- grant-signing authority;
- local-user recovery verifier;
- downstream adapter/provider provenance。

#139 が拡張するのは最初の項目だけです。#222 は bounded provider provenance を追加する可能性がありますが、他 authority の意味は変えません。

## Second-backend portability evidence

real GUI/computer-use semantics が heterogeneous backend で実証済みと主張する前に #222 の evidence が必要です。compile-time interface や deterministic fake だけではその claim はできません。

valid evidence は materially different な real backend と small overlapping semantic slice を使い、observation、effectful input、stale generation/revision rejection、deliberately ambiguous な post-dispatch failure を含めます。ambiguous failure は durable `Indeterminate` + quarantine となり、reconnect 後も残り、auto-replay されないことを証明します。

provider は physical endpoint、operator-managed VM、managed cloud desktop のいずれでも構いません。provider provisioning/fleet lifecycle は CUMG core の外です。

## Audit boundary

normal authorization/execution evidence には safety に必要な fixed category / bounded identifier、たとえば capability、decision/reason category、operation identity、generation/revision、policy/constraint snapshot version/digest を含められます。

normal evidence に次の raw content を追加しません。

- screenshot/video/audio;
- typed text/keystroke / clipboard content;
- URL / arbitrary command arguments/results;
- policy diagnostic のためだけの filesystem path;
- credential/token;
- provider-private response payload / opaque ID;
- policy source content。

これは既存 V2 threat model と一致します。observability は denial/recovery state の理解に使えても、second content-retention product になってはいけません。

## Product boundary reaffirmed

この review は次を admission しません。

- VM provisioning / KubeVirt/Kubernetes orchestration;
- generic sandbox/fleet scheduling;
- generic device registry/fabric;
- remote-desktop product;
- hosted account/dashboard SaaS;
- northbound goal としての Cua API/tool/identifier compatibility;
- second Handoff FSM/WebRTC/TURN implementation;
- policy-engine feature count を differentiation とすること;
- `Indeterminate` / quarantine / reconciliation / no-auto-replay の弱体化。

Cua Cloud Fleets、E2B、Daytona、その他 provider は、compatible provider/adapter boundary で上記 semantics を維持できる場合だけ downstream execution infrastructure として利用できます。

## References

- Cua permission policies: <https://cua.ai/docs/reference/cua-driver/permission-policies>
- Cua policy enforcement/trust model: <https://cua.ai/docs/concepts/how-permission-policies-work>
- Cua Computer History: <https://cua.ai/docs/how-to-guides/driver/use-computer-history>
- CUMG positioning: [`V2_POSITIONING.ja.md`](V2_POSITIONING.ja.md)
- Execution-environment boundary: [`V2_EXECUTION_ENVIRONMENT_BOUNDARY.ja.md`](V2_EXECUTION_ENVIRONMENT_BOUNDARY.ja.md)
- Threat model: [`V2_THREAT_MODEL.md`](V2_THREAT_MODEL.md)
