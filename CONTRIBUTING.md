# Contributing

> English is the canonical documentation. [日本語版 / Japanese translation](CONTRIBUTING.ja.md)

## Principles

1. Keep the public MCP boundary backend-agnostic.
2. Do not reimplement desktop automation unless a backend cannot provide a required capability.
3. Preserve backend MCP content/result envelopes when proxying.
4. Security checks happen before backend execution.
5. Never add automatic replay of a failed, timed-out, or cancelled computer-use call without a proven idempotency contract.
6. Treat one physical desktop as shared mutable state; preserve operation serialization unless the concurrency model changes explicitly.
7. Do not log raw tool arguments, results, screenshots, clipboard contents, or credentials by default.
8. Keep semantic tool classes separate from exact-name authorization; unknown tools must remain conservatively classified.
9. Idle resource usage and deterministic soak behavior are V1 regression gates.
10. Keep newcomer-facing commands copy-pasteable and repository-local Markdown links valid.

## Before a PR

Run the deterministic checks used by normal CI:

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
python3 scripts/check_docs.py
```

When Node 20+ and network access are available, also run:

```bash
python3 scripts/v1_conformance.py
```

The conformance script downloads the exact pinned official runner package through `npx`. It validates only the V1-applicable official server-boundary scenarios documented in [`docs/TESTING.md`](docs/TESTING.md); do not describe that as full MCP conformance certification.

`Cargo.lock` is part of the binary application's reproducibility contract. Dependency changes must update it intentionally; do not bypass `--locked` in normal validation.

The docs link checker validates repository-local Markdown targets without fetching external websites. When changing an external installation/client/deployment command, verify it against the current authoritative upstream documentation as part of review.

## Compatibility changes

Changes to MCP lifecycle handling, cancellation, Cua integration, policy filtering/classification, Host/Origin validation, reconnect behavior, health telemetry, or CI supply-chain pins should include or update the relevant deterministic or real-Cua coverage. See [`docs/TESTING.md`](docs/TESTING.md).

Do not run untrusted pull-request code on a TCC-granted self-hosted desktop runner. The desktop E2E workflow is manual and `main`-only by design.
