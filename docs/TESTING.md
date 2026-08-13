# Testing

The test strategy separates deterministic repository checks, protocol/backend compatibility, documentation integrity, and real desktop GUI execution.

## Normal CI

`.github/workflows/ci.yml` runs with read-only repository permissions.

### Rust and deterministic validation

```bash
cargo fmt --check
cargo check --locked --all-targets
cargo test --locked
python3 -m py_compile \
  scripts/cua_gateway_smoke.py \
  scripts/cua_desktop_e2e.py \
  scripts/mock_mcp_backend.py \
  scripts/v1_quality_gate.py \
  scripts/v1_conformance.py
cargo build --locked
python3 scripts/v1_quality_gate.py
python3 scripts/v1_conformance.py
```

`cargo test --locked` includes a downstream-cancellation test that starts the deterministic MCP fixture, begins a pending tool request, cancels it, and requires the downstream `notifications/cancelled` request ID to equal the in-flight backend tool request ID. This proves cancellation propagation rather than merely dropping the gateway-side future.

### Deterministic V1 quality fixture

`scripts/mock_mcp_backend.py` is a test-only stdio MCP backend. It never touches a desktop and exposes only:

- `noop` — immediate side-effect-free success;
- `slow` — remains pending until a downstream cancellation notification arrives.

`scripts/v1_quality_gate.py` runs the real gateway process against this backend and verifies:

1. stateless `2026-07-28` discovery;
2. policy-filtered tool discovery;
3. exactly 100 successful `tools/call` round trips through Gateway → backend MCP stdio;
4. gateway readiness after the soak;
5. `/healthz` contains the gateway-owned backend child PID, cumulative CPU seconds, and RSS bytes;
6. a five-second Linux-hosted idle sample of the gateway process stays below the regression gates of 2% CPU and 128 MiB RSS.

These thresholds are regression guards, not capacity or production-performance claims. A representative passing hosted-Linux sample measured 100 calls in 0.142 seconds, 0.000% idle gateway CPU, and 17.191 MiB gateway RSS. Exact numbers vary by runner; the configured thresholds are the pass/fail contract.

### Official MCP conformance runner

`scripts/v1_conformance.py` pins the official runner package to:

```text
@modelcontextprotocol/conformance@0.2.0-alpha.11
```

CI requires Node 22+ and asks the pinned runner to load both frozen requirement revisions tracked by this project:

```text
2025-11-25
2026-07-28
```

It then runs the V1-applicable server-boundary scenarios against a real gateway process:

- `server-initialize`;
- `tools-list`;
- `dns-rebinding-protection`.

This is deliberately **not** described as full MCP conformance certification. The upstream complete requirement sets contain prompts, resources, completion, authentication, and fixture-specific tool-content behavior that this tools-only gateway does not advertise or implement. We run the official scenarios that directly apply to the gateway boundary and retain the broader dual-protocol smoke as a compatibility claim.

### Real-Cua compatibility matrix

GitHub-hosted CI runs the gateway against pinned Cua Driver 0.19.3 on:

- Linux;
- macOS;
- Windows.

For each OS, CI:

1. verifies the pinned Cua installer SHA-256;
2. verifies the pinned platform release payload SHA-256 independently;
3. extracts the expected `cua-driver` executable from the verified payload;
4. installs Cua;
5. requires the installed executable SHA-256 to equal the executable from the verified payload;
6. builds the gateway with `cargo build --locked`;
7. runs real-Cua MCP smoke for `2025-11-25`;
8. runs real-Cua MCP smoke for `2026-07-28`.

The smoke harness exercises the actual Gateway → `cua-driver mcp` path, backend tool discovery, `/healthz`, northbound MCP lifecycle behavior, tool filtering, deny-policy behavior, backend resource telemetry availability where supported, and rejection of malicious Host/Origin values.

## Documentation CI

`.github/workflows/docs.yml` is a separate read-only workflow that runs:

```bash
python3 scripts/check_docs.py
```

The checker scans `README.md`, `CONTRIBUTING.md`, and `docs/*.md` and fails when a repository-local Markdown link points to a missing target or escapes the repository root. It deliberately does not fetch external URLs.

## What normal CI proves

Normal CI provides strong evidence for:

- Rust compilation and unit behavior;
- locked dependency resolution;
- the pinned Cua compatibility input on three operating systems;
- Gateway ↔ Cua MCP stdio connectivity;
- northbound MCP lifecycle compatibility for the two exercised protocol revisions;
- selected official conformance scenarios at the V1 server boundary;
- dynamic tool discovery/filtering;
- deny-by-default policy enforcement;
- conservative semantic tool classification;
- actual downstream cancellation notification with the correct request ID;
- Host/Origin transport guards;
- deterministic 100-call gateway/backend-MCP soak behavior;
- short-window idle gateway CPU/RSS regression limits;
- gateway-owned backend child PID/CPU/RSS health telemetry;
- repository-local documentation link integrity.

## What normal CI does not prove

GitHub-hosted runners are not a substitute for the final operator-controlled acceptance checks. Normal CI does not claim to prove:

- real screenshots/clicks/typing on a persistent TCC-granted macOS desktop;
- macOS TCC permission persistence on the dedicated test machine;
- arbitrary application-specific GUI workflows;
- a production-duration soak or production capacity/performance benchmark;
- a real Cloudflare Access/Tunnel deployment using the operator's account;
- a real ChatGPT remote MCP connection through that authenticated deployment;
- continuing correctness of third-party setup instructions after an upstream product changes;
- full MCP conformance certification beyond the applicable official scenarios described above.

The remaining V1 operator checks are in [`V1_ACCEPTANCE.md`](V1_ACCEPTANCE.md).

## Backend resource telemetry semantics

`/healthz` reports metrics for the **gateway-owned backend child process** it spawned, when the platform can query them:

- PID;
- cumulative CPU seconds;
- resident RSS bytes.

On macOS, Cua may proxy through its supported application/daemon lifecycle. These values therefore describe the direct child owned by the gateway, not necessarily the aggregate resource usage of every Cua process on the machine.

## Updating the Cua compatibility pin

Treat a Cua Driver version bump as a reviewed compatibility change, not a blind dependency refresh.

When changing the CI pin:

1. select the exact upstream release version;
2. update the versioned installer SHA-256;
3. update every platform release-payload SHA-256 used by the matrix;
4. verify the asset names/architectures still match the workflow assumptions;
5. let CI independently compare the installed executable to the verified payload executable;
6. require all three OS jobs and both protocol lifecycle smokes to pass before merge;
7. review newly discovered Cua tools against the exact-name policy and semantic classification before widening any production allowlist.

Do not replace pinned release URLs with a mutable `latest` or convenience installer URL in normal CI.

## Desktop E2E

`.github/workflows/desktop-e2e.yml` is intentionally:

- `workflow_dispatch` only;
- restricted to `main`;
- targeted at `[self-hosted, macOS, cua-desktop-e2e]`;
- read-only with respect to repository contents.

The dedicated runner must be a test machine, logged into a real macOS GUI session, preconfigured with CuaDriver Accessibility and Screen Recording permissions, and isolated from untrusted pull-request execution.

The workflow performs two physical acceptance fixtures from trusted `main`:

```text
V1 desktop fixture:
launch fresh TextEdit
    → screenshot evidence
    → click editor
    → type unique marker
    → independently read accessibility state
    → verify marker

V2 P1 execution-safety fixture:
real Cua state-changing operation
    → cancellation propagated but outcome unprovable
    → indeterminate + exact durable quarantine
    → Hub and Agent restart
    → newer Agent generation reconnects
    → old operation remains quarantined with no terminal receipt/replay
    → competing principal remains blocked
    → explicit audited resolution
    → safe reuse with a new operation ID
```

The final P1 physical acceptance passed on 2026-08-13 against `main` commit `e4eb464` in Desktop E2E run `31655691675`. The dedicated runner may be registered ephemerally, but the workflow must remain manual, `main`-only, and isolated from untrusted pull-request execution. See [`V1_ACCEPTANCE.md`](V1_ACCEPTANCE.md) for the original desktop procedure and [`V2_P0_EXECUTION_SAFETY.md`](V2_P0_EXECUTION_SAFETY.md) for the V2 invariant.

The trusted lane also establishes an ordinary visible macOS Desktop immediately before the V1 TextEdit fixture. Cua `launch_app` starts the application in the background; if the runner is sitting on a different Space, the launched TextEdit window can be off-Space and `get_window_state` can return an empty Accessibility element set even though launch/window/screenshot calls themselves succeed. This is a physical-lane precondition only and does not alter the V2 execution-safety state machine.

## V2-M1 final acceptance

The V2-M1 gate is recorded in [`V2_M1_ACCEPTANCE.md`](V2_M1_ACCEPTANCE.md). Run the deterministic portion explicitly on Rust 1.88 even if another toolchain is the local default:

```bash
cargo +1.88.0 fmt --check
cargo +1.88.0 check --locked --all-targets
cargo +1.88.0 test --locked --all-targets
cargo +1.88.0 clippy --locked --all-targets
python3 scripts/check_docs.py
```

The real-Cua cancellation test is intentionally ignored in normal test discovery because it performs a real desktop action. On the dedicated, logged-in, TCC-granted macOS acceptance machine:

```bash
CUMG_V2_CUA_CANCEL_E2E_ACK=1 CUMG_V2_CUA_COMMAND="$HOME/.local/bin/cua-driver" cargo +1.88.0 test --locked   --test v2_m1_cua_cancellation_e2e -- --ignored --nocapture
```

The current post-M1/P0 version of this acceptance is stronger than the original M1 cancellation smoke. It launches the TextEdit fixture through the Agent-native shell executor, performs the real Cua desktop action under the same operation-owner boundary, observes `IndeterminateAfterPropagation`, verifies durable quarantine, rejects another principal, explicitly resolves the exact ambiguous operation, and then proves the desktop can be reused without replaying the old action. A merely delivered cancellation notification is not sufficient evidence of non-execution.

### V2 P0 execution-safety regression

The detailed invariant is recorded in [`V2_P0_EXECUTION_SAFETY.md`](V2_P0_EXECUTION_SAFETY.md). In addition to the repository-wide gate above, the focused deterministic regressions are:

```bash
cargo test v2_execution_safety --lib
cargo test --test v2_m1_desktop_boundary_e2e
cargo test --test v2_m1_partition_recovery
```

`v2_m1_desktop_boundary_e2e` uses a deterministic Cua-shaped stdio fixture and a real Hub-Agent TLS/gRPC session. It proves native shell and GUI-adapter commands share one owner/fence/quarantine boundary, includes a forced checkpoint-write failure during resolution, and requires rollback to quarantine before a successful retry.

`v2_m1_partition_recovery` drops the Agent after dispatch while a native shell side effect can still complete locally, reconnects a new Agent generation, requires the exact old operation to remain quarantined, rejects a competing principal, and only permits new work after explicit resolution.

Packaging/lifecycle acceptance also includes `plutil -lint` on the LaunchAgent template, shell syntax plus valid/mismatched certificate fixtures for the ACME deploy hook, and throwaway `v2_keyctl` generation/rotation outside the repository. The systemd unit must additionally be run through `systemd-analyze verify` on the target Linux distribution or Linux release CI because the macOS desktop acceptance host cannot execute systemd's semantic verifier.

## Running smoke locally

After building the gateway and installing Cua:

```bash
cargo build --locked
MCP_PROTOCOL_VERSION=2025-11-25 python3 scripts/cua_gateway_smoke.py
MCP_PROTOCOL_VERSION=2026-07-28 python3 scripts/cua_gateway_smoke.py
python3 scripts/v1_quality_gate.py
python3 scripts/v1_conformance.py
python3 scripts/check_docs.py
```

The Cua smoke script starts its own gateway process/configuration. Do not point test harnesses at a production desktop or reuse production credentials.

### V2 P1 multi-device and backend-portability proof

In addition to the repository-wide locked gate, run:

```bash
cargo test --locked --test v2_p1_invariants
cargo test --locked --test v2_p1_backend_portability
cargo test --locked --test v2_p1_multi_device_e2e
```

`v2_p1_invariants` proves independent A/B ownership/quarantine state, restart normalization, queue cancellation on the quarantined device, wrong-owner/late/duplicate settlement rejection, and property-based stale-generation isolation.

`v2_p1_backend_portability` uses a deterministic process-like second executor whose cancellation semantics can prove not-started or clean termination and can also deliberately become post-commit indeterminate. It verifies that both this executor and Cua ambiguity use the same P0 operation ID/owner/generation/quarantine/resolution/no-replay model.

`v2_p1_multi_device_e2e` composes two existing `SingleDeviceHub` services in one process with independent durable state. Device A uses the Cua-shaped GUI fixture and becomes unknown/quarantined; Device B simultaneously runs native shell work under a different principal; A reconnect and partition leave B usable; Hub reconstruction restores A/B independently; explicit A resolution permits a new operation while marker counts prove the old GUI action was not replayed.

These deterministic lanes are supplemented by the main-only physical acceptance above. P1 no longer carries a physical real-Cua residual: the final P1 code passed run `31655691675` on 2026-08-13. Future changes to the Computer Use adapter seam must rerun the same trusted desktop workflow before claiming equivalent physical regression.

### V2 P2 replacement-seam regression

P2 adds two focused unit regressions in addition to the existing P1 suites:

- `v2_m1_northbound::tests::replaceable_authorizer_keeps_exact_principal_device_capability_boundary` proves a replacement authorizer still denies wrong principal, wrong device, and wrong capability;
- `v2_m1_agent::tests::custom_computer_use_backend_is_injected_without_changing_native_capabilities` proves an alternate Computer Use backend can be injected without replacing Agent-native capabilities or the surrounding execution gate.

The existing `v2_p1_backend_portability` test remains the semantic guard: backend-specific cancellation behavior must converge on the same authoritative operation/quarantine/resolution model. Any P2 Computer Use backend change also requires the final main-only real-Cua regression because compile-time interface compatibility is not evidence of physical cancellation behavior. See [`V2_P2_REPLACEMENT_SEAMS.md`](V2_P2_REPLACEMENT_SEAMS.md).

P2 final physical acceptance passed on 2026-08-13 against trusted `main` commit `bb6712d` in Desktop E2E run `31658602133`. This run exercised the workflow-owned Desktop precondition, the V1 physical screenshot/click/type/Accessibility fixture, and the V2 real-Cua restart/no-replay fixture in one main-only job.
