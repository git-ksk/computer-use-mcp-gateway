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

The fixture performs:

```text
launch fresh TextEdit
    → screenshot evidence
    → click editor
    → type unique marker
    → independently read accessibility state
    → verify marker
```

Execution of this lane is intentionally left to the operator. See [`V1_ACCEPTANCE.md`](V1_ACCEPTANCE.md).

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

The test must observe `IndeterminateAfterPropagation`, resolve the original Hub operation to `DeviceIndeterminate`, and reject subsequent work through the same device quarantine. A merely delivered cancellation notification is not sufficient evidence of non-execution.

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
