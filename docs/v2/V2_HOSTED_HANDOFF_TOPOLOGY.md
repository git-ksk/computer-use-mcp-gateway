# V2 hosted Handoff topology

Status: **architecture decision for Issue #275; hosted implementation/acceptance remains pending**.

This document defines how CUMG should compose its future hosted Hub with `mcp-execution-handoff` without moving physical mutation authority into a replaceable cloud process. It extends the accepted Agent-owned Handoff boundary from #152 and composes with the Cloud Run Hub support gate in #215.

Cloud Run is a reference hosted runtime, not a protocol requirement. The same boundary should remain valid for another replaceable public Hub runtime.

## Decision summary

The target architecture separates three authorities that must not be collapsed:

1. **CUMG execution-safety authority** lives in the hosted Hub and its external durable state.
2. **Handoff mutation authority** for a physical Window or PTY lives in the controlled Agent's canonical Handoff runtime.
3. **Hosted Human-session routing** may live in a public Handoff routing/gateway plane, but routing state is not mutation authority.

The hosted Hub may keep a conservative signed Handoff dispatch fence. That fence may deny execution earlier, but it can never make Agent execution more permissive than the Agent-local Handoff runtime.

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
      - capture/input and final pre-execution Handoff gate
          |                         ^
          | WebRTC direct/TURN     | authenticated outbound
          v                         | hosted routing/WSS channel
        Human <------------- Hosted Handoff routing plane
```

The CUMG Hub and hosted Handoff routing plane may initially be deployed together, but their schemas and authority meanings must remain distinct so they can be separated later without changing the security model.

## Why the Agent keeps canonical Handoff authority

Physical #152 acceptance corrected an earlier Hub-owned runtime placement. Capture, input, TCC/Accessibility, exact Window/PTY state and the final mutation point are properties of the controlled device, not of the Hub host.

Keeping canonical Handoff authority on the Agent has four important consequences:

- Hub replacement cannot restore or invent Agent/Human authority from hosted state.
- A stale Hub Handoff view cannot authorize a local mutation because the Agent performs the final gate immediately before Cua/PTY execution.
- Human media/input and OS permission boundaries remain on the controlled device instead of becoming CUMG Hub responsibilities.
- CUMG does not implement a second copy of the `mcp-execution-handoff` authority FSM.

This is a CUMG consumer composition of upstream Handoff. It does not require every other Handoff consumer to use the same deployment shape.

## Responsibility split

### Hosted CUMG Hub

The hosted Hub remains authoritative for:

- authenticated northbound principal;
- stable device identity and current authenticated Agent generation;
- exact capability authorization and semantic constraints;
- operation identity, admission and dispatch binding;
- durable terminal evidence and `Indeterminate` state;
- quarantine, reconciliation and permanent replay denial;
- hosted durable-state revision and writer epoch from #215;
- a conservative Handoff dispatch fence derived from bounded signed Agent status.

The Hub does **not** own Handoff media/input, transport credentials, local display/session continuity, or canonical Agent/Human mutation authority.

### Controlled Agent / execution worker

The Agent remains authoritative for:

- canonical `mcp-execution-handoff` FSM and intervention epoch;
- Handoff checkpoint/recovery semantics;
- `agent | human | none` mutation authority;
- exact Window / Terminal Target Surface admission and revalidation;
- local Desktop Session / Display Backend continuity where supported;
- Human viewer/transport attachment to the local target;
- WebRTC/WSS host-side capture and Human input mechanics;
- OS-local Screen Recording, Accessibility and input permission boundaries;
- final Handoff authority validation immediately before Cua/PTY mutation.

### Hosted Handoff routing plane

A public routing plane may own only hosted-session concerns such as:

- authenticated worker connection/routing identity;
- stable device reference and current Agent generation;
- intervention id and Handoff epoch needed for routing/fencing;
- short-lived operator-session expiry;
- viewer and concrete transport generation;
- bounded readiness/revocation state;
- signaling or WSS routing required by the selected Handoff transport.

This state is **routing/session state, not execution authority**. It must never be used to restore Agent or Human mutation authority after a process restart.

## Three durable-state meanings

Hosted composition must keep these stores logically distinct even if one database technology backs more than one of them.

| State | Authority meaning | Example fields | Can restore mutation authority? |
| --- | --- | --- | --- |
| CUMG authoritative state | execution ownership / ambiguity / replay safety | operation, device generation, dispatch, terminal evidence, quarantine, tombstone, writer epoch, revision | No Handoff authority; restores only CUMG execution-safety state |
| Agent Handoff checkpoint | bounded recovery hint for local Handoff lifecycle | intervention, epoch, status, principal binding, expiry | **No.** Recovery is `reissue_and_revalidate` only |
| Hosted routing/session state | route one current Human session to one current worker/intervention | worker/device, Agent generation, intervention/epoch, viewer/transport generation, expiry | **No** |

Do not make routing availability, a worker heartbeat, a new Hub instance, or a surviving database record proof that a previous Human/Agent authority is still valid.

## Generation model

Hosted CUMG must treat the following lifetimes as independent:

1. **CUMG Agent generation** — advances after a fresh authenticated Agent session.
2. **Handoff intervention epoch** — fences one Agent/Human authority lifecycle.
3. **Desktop/application session** — persistent local application/display continuity where supported.
4. **Human viewer generation** — one current viewer attachment.
5. **Transport generation** — one concrete WebRTC/WSS transport attempt/attachment.
6. **Hosted Hub writer epoch/revision** — fences competing replaceable Hub instances.

A viewer reconnect or managed transport fallback may rotate (4) and (5) without recreating (3), changing (2), or advancing (1). Hub replacement may advance (6) without implying any Handoff authority change. Every stale generation fails closed.

This model consumes the upstream v0.4.1 Desktop Session / Display Backend separation when CUMG updates its pinned Handoff revision.

## Hosted operator control

The current `CUMG_V2_HANDOFF_CONTROL_SOCKET` remains a good single-host/VM operator boundary, but hosted operation cannot depend on filesystem access to the Hub instance.

A hosted profile therefore needs a closed authenticated operator-control surface with the same narrow lifecycle intent as the local CLI. The #277 adapter now fixes the reviewed HTTP resource family as `/operator/v1/handoff/context` for short-lived context issuance and `/operator/v1/handoff/control` for lifecycle commands. This router is an OAuth-protected operator resource and does not register MCP tools.

The context handle is not target authority by itself. It is process-memory-only, expires within the bounded CUMG selection lifetime, binds the issuing operator principal and action, and is checked again against the exact fresh CUMG selection plus Agent generation/capability revision. Fresh selection rotates the handle; stale, cross-principal, cross-action, expired, or generation-mismatched handles fail closed. Raw PID/window identity is never accepted from the hosted caller.

Required properties:

- lifecycle control remains separate from ordinary northbound MCP tool discovery;
- operator authentication is explicit;
- authorization is exact `principal -> device -> handoff-control action`;
- callers cannot submit arbitrary PID/window authority;
- `begin`/recovery uses a fresh CUMG-authorized interaction/surface context bound to the current device generation and capability revision;
- prefer a short-lived opaque surface/context handle over durable hosted storage of raw OS target identity;
- the Hub relays only bounded signed/fenced commands to the Agent-owned Handoff runtime;
- locator/session capability material is short-lived and excluded from normal logs, audit and durable CUMG state.

For a local/single-host deployment, the Unix control socket may remain the operator adapter. Hosted and local adapters should converge on the same internal typed operator command model rather than defining two Handoff semantics.

## Human data plane and connectivity

CUMG remains transport/provider-blind.

Upstream `mcp-execution-handoff#19` owns provider-neutral WebRTC connectivity/relay. CUMG must not select STUN/TURN providers or carry provider credentials in semantic requests, policy, logs or checkpoints.

Hosted HTTP ingress and Human media/input are different paths:

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

Do not route frames through the CUMG execution-safety Hub merely because the Hub is public. A hosted Handoff routing service may carry WSS transport data if the reviewed Handoff transport requires it, but that service remains outside CUMG execution-safety state.

Viewer disconnect, transport fallback or route loss is never Human `Done`, never Agent resume and never permission to replay Human input.

## Dispatch invariant

A protected effectful operation may dispatch only when all applicable gates agree:

```text
CUMG principal/device/capability authorization
AND current hosted writer epoch + durable state revision
AND current authenticated Agent generation
AND no unresolved CUMG quarantine
AND current capability revision / semantic constraints
AND conservative Hub Handoff fence permits Agent
AND Agent-local canonical Handoff authority permits Agent
```

The last check is mandatory immediately before backend mutation. A stale or compromised hosted instance that lacks the current durable writer epoch must also be unable to reach dispatch under #215.

A Hub-side Handoff cache is fail-closed only: stale/unknown status may deny work, but a permissive cached status is never sufficient to execute without the Agent-local gate.

## Restart and partition behavior

### Hosted Hub replacement

- restore only CUMG authoritative state through the external durable backend;
- acquire a fresh writer epoch before mutation authority;
- synchronize fresh bounded Handoff status from the authenticated Agent before protected execution;
- never reconstruct a Human viewer, locator, transport generation or Handoff mutation authority from hosted routing state.

### Agent/Handoff restart

- old ephemeral Agent/Human authority is lost;
- Handoff checkpoint recovery remains `reissue_and_revalidate`;
- old locators/capabilities/viewer/transport generations remain invalid;
- protected execution remains denied until explicit recovery/verification permits a fresh lifecycle.

### Hub-Agent partition

- loss of Hub connectivity never converts `human` or `none` authority back to `agent`;
- a surviving Human transport must not cause automatic Agent resume;
- reconnect performs fresh Agent authentication/generation semantics and fresh bounded Handoff synchronization.

### Human viewer loss

- viewer disconnect is not Done;
- Desktop/application continuity may remain local when its supported boundary survives;
- a reconnect/fallback gets a fresh viewer/transport generation;
- no Human input is replayed across generations.

## Privacy and secret boundary

The following do not belong in CUMG durable state, normal audit or generic hosted routing metadata:

- frames, screenshots, video/audio;
- raw Human input or entered text;
- passwords, OTP/MFA/challenge answers;
- browser cookies, target-service tokens or credentials;
- raw PID/window identity when an opaque bounded context can be used instead;
- ICE candidates, addresses or SDP;
- TURN usernames/passwords/tokens or provider API credentials;
- live locator/capability/reconnect-handle values.

Operational diagnostics remain bounded, categorical and content-minimizing.

## Cloud Run composition

This design is a dependency of the #215 Cloud Run support claim, not a replacement for #215.

A hosted Handoff-enabled Cloud Run profile additionally requires:

- operator control that does not depend on a Hub-local Unix socket;
- outbound Agent/worker connectivity with no required public inbound listener on the Agent;
- Handoff dispatch checks composed with the same durable writer-epoch/revision fence used for CUMG effect dispatch;
- bounded hosted session/routing state that survives or fails independently of CUMG authoritative state;
- restart/revision-rollout tests proving that old Hub instances and stale Human routes cannot mutate;
- physical Agent acceptance covering Human active -> Agent deny -> Done -> fresh verification -> explicit resume through the hosted topology.

The single-host/VM profile remains valid and does not need to adopt the hosted operator adapter.

## Upstream Handoff adoption

The current CUMG artifact manifest pins Handoff commit `9a621d12524632fd717e5f8d84a42c29946ab662` and labels the packaged dependency `0.3.0`.

Upstream v0.4.1 adds the Desktop Session / Display Backend boundary and later roadmap work separates provider-neutral connectivity (#19) from hosted worker topology (#12). CUMG should adopt a reviewed upstream release boundary separately from the current Windows #227 / CUMG `0.4.0` closeout.

That consumer update must include:

- exact source pin and package/manifest update;
- Window and Terminal first-class adapter compatibility;
- Desktop Session/viewer-generation invariant tests where applicable;
- deterministic stale-generation/restart tests;
- packaging/rollback compatibility;
- physical acceptance for the exact OS/transport support rows being claimed.

Do not update a release-critical dependency solely to make this design document look current.

## Implementation sequence

The preferred sequencing is:

1. **Freeze this architecture (#275).** Keep Agent-owned canonical Handoff authority and hosted routing non-authoritative.
2. **Adopt a reviewed newer Handoff pin separately in #276.** Consume v0.4.1-or-newer boundaries with CUMG integration/acceptance evidence.
3. **Consume upstream provider-neutral connectivity (#19).** Remove any consumer-visible provider-specific relay assumptions.
4. **Implement the hosted operator/routing adapter in #277.** PR #281 now provides the transport-neutral authorization core, principal/action-bound opaque context handles, and a separate OAuth HTTP router with no MCP tool surface. Production `v2_hub` listener composition remains intentionally deferred rather than creating a temporary extra public port.
5. **Implement #215 durable Hub/writer fencing and one-port hosted ingress.** Compose the reviewed #277 router into that one-port ingress and Handoff checks into the commit-before-dispatch boundary.
6. **Run hosted failure/physical acceptance.** Include Hub replacement, stale writer, Agent reconnect/restart, viewer reconnect, WSS/WebRTC fallback and the complete Human Done/verification/resume lifecycle.

Steps 2-4 may develop in parallel where their interfaces are already fixed, but no hosted support claim may bypass #215's durable-state/fencing gate.

## Acceptance matrix

Before Handoff-enabled hosted CUMG is supported, evidence must prove at least:

| Scenario | Required result |
| --- | --- |
| stale Cloud Run revision still has an old Agent stream | writer/revision fence denies dispatch |
| Hub restarts during Human active | no Agent/Human authority reconstructed by Hub; fresh Agent status required |
| Agent restarts during Human active | `reissue_and_revalidate`; no stale Human or Agent authority |
| viewer reload/reconnect | new viewer/transport generation only; no Human-input replay |
| WebRTC direct -> WSS/TURN fallback | abandoned generation fenced before new input generation |
| Hub-Agent partition | Human/none authority never becomes Agent authority automatically |
| Human Done | mutable Human transport fenced before consumer verification |
| verification succeeds | `ready_to_resume`; still no implicit Agent replay |
| explicit resume | only a fresh admitted action may regain Agent mutation authority |
| route/session database survives but worker is stale | routing record cannot restore mutation authority |
| different Agent/device claims the route | exact worker/device/generation binding rejects it |

## Non-goals

This design does not add:

- a generic fleet manager, device discovery service or dashboard;
- automatic failover of a live/ambiguous intervention to another device;
- whole-desktop authority or implicit Window -> Desktop escalation;
- CUMG-owned WebRTC/STUN/TURN provider logic;
- a second Handoff FSM in the Hub;
- remote-desktop primitives exposed as ordinary MCP tools;
- permission for Human `Done` to approve or replay a consequential Agent action.

## Related work

- CUMG #152 — Agent-owned first-class Handoff architecture and physical acceptance.
- CUMG #215 — Cloud Run Hub durable state, writer fencing, hosted ingress and support gate.
- CUMG #275 — this hosted Handoff composition decision.
- CUMG #276 — reviewed Handoff v0.4.1+ consumer pin / Desktop Session boundary adoption.
- CUMG #277 — hosted Handoff operator control / routing adapter.
- `mcp-execution-handoff#19` — provider-neutral connectivity.
- `mcp-execution-handoff#12` — hosted control-plane / execution-worker topology.
