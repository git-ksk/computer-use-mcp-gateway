# Contributing

## Principles

1. Keep the public MCP boundary backend-agnostic.
2. Do not reimplement desktop automation unless a backend cannot provide a required capability.
3. Preserve backend MCP content/result envelopes when proxying.
4. Security checks happen before backend execution.
5. Never add automatic replay of a failed state-changing computer-use call without a proven idempotency contract.
6. Treat one physical desktop as shared mutable state; preserve operation serialization unless the concurrency model changes explicitly.
7. Do not log raw tool arguments, results, screenshots, clipboard contents, or credentials by default.
8. Idle resource usage is a product requirement; benchmark it before declaring the relevant roadmap item complete.

## Before a PR

Run the checks used by normal CI:

```bash
cargo fmt --check
cargo check --locked --all-targets
cargo test --locked
python3 -m py_compile scripts/cua_gateway_smoke.py scripts/cua_desktop_e2e.py
```

`Cargo.lock` is part of the binary application's reproducibility contract. Dependency changes must update it intentionally; do not bypass `--locked` in normal validation.

## Compatibility changes

Changes to MCP lifecycle handling, Cua integration, policy filtering, Host/Origin validation, reconnect behavior, or CI supply-chain pins should include or update the relevant smoke coverage. See [`docs/TESTING.md`](docs/TESTING.md).

Do not run untrusted pull-request code on a TCC-granted self-hosted desktop runner. The desktop E2E workflow is manual and `main`-only by design.
