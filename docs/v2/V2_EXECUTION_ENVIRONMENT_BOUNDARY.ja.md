# V2 execution-environment boundary

> この日本語版は [`V2_EXECUTION_ENVIRONMENT_BOUNDARY.md`](V2_EXECUTION_ENVIRONMENT_BOUNDARY.md) の翻訳です。**英語版を canonical（正典）とし、解釈に差がある場合は英語版を優先します。**

Status: **active product-boundary clarification (2026-08-29)**.

この文書は、CUMG と managed agent computer、cloud sandbox、execution-environment provider の関係を明確にします。[`V2_POSITIONING.md`](V2_POSITIONING.md) を置き換えるものではなく、#219 で現在の Cua Driver + Cloud Fleets の方向をレビューした結果、既存の境界を明示するものです。

## Decision

CUMG は **execution environment を provision する製品ではありません**。

CUMG が最適化すべき対象は、execution authority、side effect、Human intervention、local credential、recovery state が transport loss、process restart、actor 変更を跨いで維持される必要がある、**特定の stateful な interactive computer / desktop session** の委任制御です。

execution environment 自体は次のいずれでも構いません。

- 既存の物理 macOS / Windows / Linux computer
- remote workstation
- operator が用意した VM
- Cua などの provider が提供する managed cloud desktop
- CUMG の evidence contract を満たせる将来の native / hosted backend

物理か仮想か、local か hosted かは製品上の本質的な区別ではありません。CUMG が **特定の stateful desktop** に対する uncertainty-aware authority と recovery semantics を所有し、compute の provision / replacement は外部責務に保つことが境界です。

## CUMG が所有するレイヤー

```text
agent / external principal
          |
          v
        CUMG
  authorization + exact capability
  operation identity + ownership
  generation/capability fencing
  no-auto-replay
  Indeterminate + quarantine
  reconciliation / recovery
  Human authority transition
          |
          v
execution-provider / backend seam
     /          |           \
 physical    native       managed
 endpoint    backend      cloud desktop
```

CUMG は次の project-owned semantics を引き続き強化します。

1. **Operation identity and ownership.** 状態変更を伴う desktop operation には明示的な identity と authoritative owner が必要です。
2. **Ambiguous-side-effect handling.** lost response、cancellation、timeout、disconnect、restart は non-execution の証明ではありません。
3. **Durable quarantine and no replay.** effectful work の結果を証明できない場合は `Indeterminate` を維持し、reviewed な explicit recovery が authority を解決するまで unsafe reuse を拒否します。
4. **Human Handoff and handback.** Human authority は first-class transition として Agent execution を fence し、必要な verification boundary を通った後だけ Agent に戻します。
5. **Local-user recovery.** real endpoint の ambiguous state を解決する際、Agent/device identity を recovery authority にせず、別に root された local-user authority を要求できるようにします。
6. **Backend-neutral semantic capabilities.** provider 固有の identifier / API は CUMG semantic surface より下で終端します。
7. **Privacy-bounded evidence.** recovery / audit に必要な evidence は保持しますが、raw desktop、command、credential、typed content の保存をデフォルトにはしません。

これらは、既存 login state、local application、device-bound credential、user-presence mechanism、OS permission、置き換え不能な interactive state など、computer/session 自体に継続的な価値がある場合に特に重要です。

## 再利用する隣接レイヤー

managed agent-computer provider や sandbox system は、次のような責務を所有できます。

- VM / sandbox provisioning
- image distribution
- warm pool / fleet scheduling
- execution-environment replacement
- provider-specific desktop driver
- infrastructure-level isolation / tenancy
- generic policy language / provider-specific audit product

これらは正当な downstream / surrounding layer です。CUMG は CUMG execution-safety contract を保てる限り、feature parity のために再実装せず、maintained infrastructure を統合します。

したがって、隣接 provider の機能が増えたこと自体は、CUMG がその provider と同じ product category に拡大する理由にはなりません。

## Explicit non-goals

別途 evidence-backed な product-boundary review を行わない限り、CUMG は次のものを構築せず、これら自体を差別化要因として主張しません。

- VM provisioning
- Kubernetes / KubeVirt orchestration
- generic sandbox / fleet scheduling
- generic device discovery / registry / fabric
- remote-desktop product
- hosted account / dashboard product
- feature parity のための provider-specific policy engine 複製
- disposable compute 自体の product surface

**Hub** を hosted deployment することは別の論点です。たとえば #215 により Cloud Run Hub profile を正式サポートしても、Agent/desktop execution を Cloud Run に移したり、CUMG を Fleet product にしたりはしません。

## Provider seam

execution provider は CUMG core より下で replaceable に保ちます。

provider の adapter が CUMG operation state machine に必要な evidence を維持、または保守的に map できる場合だけ compatible とみなします。特に:

- provider reconnect によって古い effectful CUMG operation を暗黙 replay してはいけません。
- stale generation からの provider result が current authority を finalize してはいけません。
- cancellation support は、provider が実際に証明できない限り non-execution の証明として扱いません。
- provider-specific session / object identifier を stable northbound authority にしてはいけません。
- provider failure は CUMG contract を弱めるのではなく、保守的な `Indeterminate` を要求する場合があります。

この条件を満たせば、physical endpoint、native backend、managed cloud desktop のいずれも同じ CUMG execution-safety layer の下に置けます。

## Competitive and design implication

CUMG は「policy engine がある」「agent computer がある」こと自体を競争軸にしません。これらは既に多くの隣接実装が存在する領域です。

project 固有の価値は次の統合にあります。

> **stateful interactive desktop に対する uncertainty-aware operation ownership + durable no-replay recovery + Human authority transition**

この境界が Cua 固有の偶然ではないことを証明するには、性質の大きく異なる second computer-use backend が有効な可能性があります。#219 が、実装 Issue を分割する前に必要最小限の portability proof を判断します。

## Roadmap rule

新しい subsystem を評価するときは、次の順で判断します。

1. 特定の stateful desktop に対する operation ownership、ambiguity handling、quarantine、explicit recovery、Human authority transition、local-user recovery、backend-neutral evidence を直接強化するか。
2. 強化するなら、CUMG core または narrow な project-owned adapter に属する可能性があります。
3. そうでなければ、execution-environment provisioning、fleet/device fabric、remote-desktop transport、generic identity/policy などの maintained infrastructure concern ではないかを確認します。
4. 外部 concern なら replaceable/external に保ち、複製せず統合します。
5. この境界を変更する場合は、implementation / acceptance evidence を伴う explicit product review を必須とします。

## Related

- [`V2_POSITIONING.md`](V2_POSITIONING.md) — canonical V2 product positioning / execution-safety boundary
- [`../ARCHITECTURE.md`](../ARCHITECTURE.md) — Hub/Agent / backend/provider seam
- [`V2_THREAT_MODEL.md`](V2_THREAT_MODEL.md) — security claim / compromise boundary
- [#213](https://github.com/git-ksk/computer-use-mcp-gateway/issues/213) — Product Readiness umbrella
- [#215](https://github.com/git-ksk/computer-use-mcp-gateway/issues/215) — hosted Hub evaluation
- [#217](https://github.com/git-ksk/computer-use-mcp-gateway/issues/217) — Windows/Linux local-user recovery parity
- [#219](https://github.com/git-ksk/computer-use-mcp-gateway/issues/219) — Cua-informed authorization / product-boundary review
