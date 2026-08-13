# V2 optional usage accounting

Status: optional runtime integration. CUMG remains the execution authority; `mcp-usage-control` is accounting authority only.

## Boundary

The recommended V2 northbound path is:

```text
MCP client
  -> OAuth bearer verification
  -> verified issuer + subject
  -> MCPUsage reserve(1)
  -> exact CUMG device/capability authorization
  -> CUMG ownership / generation / quarantine / operation admission
  -> MCPUsage markLiable()
  -> persist CUMG Dispatched
  -> gRPC/TLS Agent dispatch
  -> Agent -> MCP stdio -> Cua
```

`reserve()` cannot safely precede OAuth verification because usage identity is derived from the verified issuer and subject. The bearer token itself is stripped at the northbound boundary and is never sent to the usage sidecar.

The CUMG `operation_id` is passed unchanged as MCPUsage `operationId`. No second logical operation identity is generated.

## Authority split

CUMG alone decides:

- operation admission, ownership, generation/fencing, and replay rejection;
- whether an outcome is terminal or `indeterminate`;
- desktop quarantine;
- explicit resolution;
- whether a business operation may ever run again.

MCPUsage decides only runtime quota/accounting admission and settlement. Usage state is never part of the CUMG execution-safety state machine. A usage error cannot clear quarantine, convert `indeterminate` to terminal, or authorize replay.

## Memory semantics

The bundled sidecar uses `MemoryUsageStore` with one cumulative runtime budget per verified principal.

- one logical tool operation reserves exactly 1 unit;
- successful/effect-possible execution settles 1 unit;
- a proven pre-dispatch or proven no-effect path settles 0 units;
- there are no capability weights in this integration;
- state is process-local and non-durable;
- restarting the usage sidecar resets usage/quota/replay accounting state;
- the optional packaged systemd lifecycle couples the sidecar to Hub restart, so an explicit Hub restart also recreates the Memory store;
- manually running the sidecar independently means a Hub-only restart does not magically erase the still-running sidecar process; restart both if runtime-reset semantics are desired.

This is runtime/session quota. It is not a financial ledger, durable billing system, cross-instance quota authority, or production accounting record. A future Redis/Firestore/Cloudflare Durable Object implementation can replace the local controller behind the same CUMG-owned seam.

## Settlement mapping

| CUMG outcome/evidence | Usage settlement |
| --- | --- |
| authorization/admission deny before Agent dispatch | 0 |
| cancellation proven before dispatch | 0 |
| successful execution | 1 |
| dispatched operation with a narrowly proven no-effect result | 0 |
| `indeterminate` | 1 |
| timeout/disconnect after effect became possible | 1 |
| cancellation after dispatch/effect became possible | 1 |
| other dispatched mutable-operation failure | 1 conservatively |

Current post-dispatch zero settlement is intentionally narrow: an observation/read operation that returns a verified remote error can be classified as no state-changing effect. Mutable operations do not receive that refund merely because they returned an error.

`indeterminate -> refund -> retry` is forbidden. Accounting reconciliation and business-operation replay are separate concerns.

## Failure semantics

| Failure | Behavior |
| --- | --- |
| reserve timeout / ambiguous ACK | fail the northbound call; do not dispatch and do not create an unrelated second reservation automatically |
| quota deny / duplicate operationId | fail before CUMG execution admission |
| `markLiable()` failure / ambiguous ACK | fail closed before Agent-visible dispatch; cancel the already-admitted CUMG operation through the existing pre-dispatch cancellation transition |
| settlement timeout / ambiguous ACK | preserve the authoritative CUMG result/quarantine; log a bounded accounting failure and do not replay the business operation |
| sidecar crash before dispatch | fail closed; no Agent dispatch |
| sidecar crash after dispatch | CUMG execution state remains authoritative; accounting may be lost because MemoryUsageStore is non-durable |
| Hub crash after persisted dispatch | existing CUMG restart recovery may restore `indeterminate`/quarantine; usage state cannot clear it |
| Agent reconnect | existing generation fencing/no-auto-replay rules remain authoritative |
| sidecar restart | runtime quota/accounting state resets; CUMG durable safety state does not |
| competing principal | existing CUMG operation ownership remains authoritative |

A `markLiable()` ACK can be lost after the sidecar committed the transition. CUMG still refuses to start metered work unless the Hub observed a successful transition. If CUMG therefore proves no Agent dispatch occurred, a zero settlement is safe when the reservation can still be reached. If the sidecar is unavailable, no speculative accounting recovery is allowed to alter CUMG execution state.

## Local sidecar

The private bridge lives at [`../integrations/mcp-usage-control-sidecar/`](../integrations/mcp-usage-control-sidecar/). It is not a general or public language-neutral MCPUsage protocol.

The sidecar binds only to literal `127.0.0.1` or `::1`. The Rust client likewise rejects non-loopback hosts, HTTPS redirects, credentials in the URL, query strings, and non-root base paths. The systemd unit additionally restricts IP traffic to localhost.

Only these fields cross the bridge:

- verified OAuth issuer and subject;
- CUMG operation ID;
- bounded tool name;
- opaque reservation ID;
- bounded settlement outcome;
- `actualUnits` of 0 or 1.

Tool arguments/results, screenshots, file contents, shell text, bearer tokens, OAuth introspection credentials, and private keys are excluded. The sidecar rejects unexpected reserve fields so an accidental `args`/payload addition fails closed rather than silently widening the privacy boundary.

## Enable or disable

With no `CUMG_V2_USAGE_ENDPOINT`, V2 constructs `NoopUsageController`; Node and `mcp-usage-control` are not required and the existing V2 execution path remains compatible.

When enabled:

```text
CUMG_V2_USAGE_ENDPOINT=http://127.0.0.1:8787/
CUMG_V2_USAGE_TIMEOUT_SECS=2
```

The sidecar requires a positive `CUMG_USAGE_LIMIT_PER_PRINCIPAL`. See its README and the systemd examples under `packaging/systemd/`.
