# V2 second-backend portability acceptance

Status: **accepted for v0.8.0 on 2026-09-25** for Issue #222. PR #385 is merged and all required protected CI, including Linux/macOS/Windows Cua smoke and release-candidate bundle checks, is green. Final publication follows the normal release-PR/tag procedure.

## Provider selection

The second real backend is **macos-mcp 0.4.0** running on the trusted macOS host through its stdio MCP transport.

It is materially different from the existing Cua 0.19.3 backend: macos-mcp is a Python/FastMCP server backed by native macOS Accessibility/Quartz semantics and exposes provider tools such as `Snapshot`, `Click`, and `Move`, rather than the Cua tool/result shapes. CUMG does not proxy those names northbound. `MacosMcpAdapter` terminates them below `ComputerUseBackendAdapter` and advertises only reviewed CUMG `DeviceCapability`s.

The reviewed portability slice is intentionally narrow:

| CUMG semantic capability | macos-mcp private mapping | Class | Status |
| --- | --- | --- | --- |
| `ListApplications` | `Snapshot(use_vision=false)` reduced to bounded application count | Observe | implemented for portability proof |
| `MovePointer` | `Move(loc, drag=false)` | Interact | implemented for portability proof |
| `PointerClick` | `Click(loc, button, clicks)` | Interact | implemented for portability proof |

All other CUMG GUI/browser capabilities remain explicitly unsupported by this adapter. This is not a feature-parity claim and does not make macos-mcp a replacement for the default Cua adapter.

## Authority and privacy boundary

- Operation ID, owner, Agent generation, capability revision, dispatch grant, quarantine, reconciliation, and replay policy remain CUMG-owned.
- The provider name/version is provenance only; it is not authorization authority and is not a settlement oracle.
- No macos-mcp tool name, provider-private object ID, session token, or raw provider response becomes a northbound capability identifier.
- `Snapshot` text may contain application/window/control names. The adapter consumes it privately and returns only the bounded CUMG `Applications { count }` result for this slice.
- Effectful provider response text is discarded after validating the CUMG result shape.
- No screenshot, typed text, URL, clipboard data, provider token, or provider-private opaque ID is added to normal audit for this proof.
- Handoff remains the existing optional authority integration; macos-mcp does not introduce a second Human-authority or remote-desktop state machine.

## Deterministic contract evidence

The branch includes tests proving:

- the adapter advertises only `ListApplications`, `PointerClick`, and `MovePointer`;
- unsupported or wider pointer semantics are rejected instead of emulated;
- the FastMCP `list[str]` stdio representation is decoded under a 64 KiB snapshot-text ceiling and application counts are bounded;
- provider payload text is reduced before becoming a CUMG result;
- for effectful work, provider cancellation, timeout, and response loss become `CancellationPropagatedIndeterminate`, `TimedOutIndeterminate`, or `BackendOutcomeIndeterminate`; the same failures on observation remain definite backend errors;
- a stale Agent generation or capability revision is rejected by the Hub session fence before anything enters the dispatch channel;
- a signed `BackendOutcomeIndeterminate` result for an already-dispatched `PointerClick` becomes durable `Indeterminate` with reason `BackendOutcomeUnproven`, creates desktop quarantine, records no auto-resolution, survives Hub restart, and never re-enqueues/replays the old operation.

Key regression tests:

- `macos_mcp_advertises_only_reviewed_portability_slice`
- `macos_mcp_maps_only_exact_reviewed_command_shapes`
- `macos_mcp_snapshot_parser_is_bounded_and_payload_reducing`
- `macos_mcp_effectful_normalization_never_exposes_provider_payload`
- `macos_mcp_post_dispatch_failures_are_indeterminate_only_for_effectful_work`
- `second_backend_session_fence_rejects_stale_generation_or_revision_without_dispatch`
- `second_backend_post_dispatch_unproven_result_is_durable_quarantine_without_replay`

These tests use the existing CUMG operation and Hub/Agent state machines. There is no provider-specific operation lifecycle or settlement path.

## Trusted-Mac physical/provider-backed evidence on 2026-09-25

The installed `$HOME/.local/bin/macos-mcp` corresponds to project version 0.4.0. Its CLI was independently checked to support `serve --transport stdio`, and the source registration confirmed the reviewed `Snapshot`, `Click`, and `Move` tool shapes.

The acknowledgement-gated ignored test was run against the real provider:

```text
CUMG_V2_MACOS_MCP_E2E_ACK=1 \
CUMG_V2_MACOS_MCP_COMMAND="$HOME/.local/bin/macos-mcp" \
CUMG_V2_MACOS_MCP_VERSION=0.4.0 \
cargo test real_macos_mcp_portability_acceptance --lib -- --ignored --nocapture
```

Result: **1 passed, 0 failed**.

The run performed:

1. real stdio MCP initialization against macos-mcp;
2. real `Snapshot(use_vision=false)` and CUMG `ListApplications` normalization with a non-zero observed count;
3. discovery of the real Finder Dock `AXDockItem` coordinate from the provider snapshot;
4. real macOS cursor movement through the CUMG `MovePointer` semantic capability;
5. real left click through the CUMG `PointerClick` semantic capability.

The fixture deliberately uses the Finder Dock item because macos-mcp intentionally excludes bundle-less helper processes such as a standalone `osascript` dialog from its snapshot model. The acceptance adapted the fixture; it did not weaken the provider or CUMG semantics.

## Ambiguity and reconnect rule

The physical success run does not manufacture a live response-loss incident merely to obtain evidence. Instead, production adapter classification is tested directly for cancellation/timeout/response-loss, and the Hub integration test starts from a command that has already crossed the outbound dispatch boundary before receiving the signed unproven provider result.

That path proves:

```text
provider result unproven after dispatch
-> BackendOutcomeIndeterminate
-> durable CUMG Indeterminate
-> desktop quarantine
-> Hub restart/reconnect
-> quarantine remains
-> old operation is not replayed
```

A provider reconnect is therefore transport recovery only. It is never proof that the prior effect did not occur.

## Cua regression requirement

macos-mcp is additional portability evidence, not a change to the default backend contract. Before merge, the final branch must keep the complete Rust library tests, format/check/clippy, docs checks, and repository Cua smoke/packaging CI green on the unchanged Cua path.

## Scope boundary

This acceptance does **not** add:

- generic provider discovery or a provider marketplace;
- VM/sandbox/fleet provisioning;
- raw MCP passthrough;
- macos-mcp-specific northbound tools;
- feature parity with Cua;
- a second operation ledger, recovery authority, or Handoff state machine.

The proof is complete only while the second backend continues to adapt to CUMG semantics rather than weakening CUMG semantics to match the provider.
