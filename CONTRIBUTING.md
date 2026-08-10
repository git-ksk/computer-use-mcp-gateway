# Contributing

## Principles

1. Keep the public MCP boundary backend-agnostic.
2. Do not reimplement desktop automation unless a backend cannot provide a required capability.
3. Preserve image and structured MCP content exactly when proxying.
4. Security checks happen before backend execution.
5. Idle resource usage is a product requirement; benchmark it.

## Before a PR

```bash
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-features
```
