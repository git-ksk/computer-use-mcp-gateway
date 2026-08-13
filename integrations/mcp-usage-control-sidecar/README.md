# CUMG MCPUsage sidecar

This private integration bridge lets the Rust V2 Hub use `mcp-usage-control` without adding a TypeScript dependency to the Rust process or adding CUMG-specific code to `mcp-usage-control`.

It is intentionally loopback-only and language-local. It is **not** a public or general-purpose MCPUsage API.

The sidecar uses `MemoryUsageStore`, so all quota/replay/accounting state disappears when the sidecar process restarts. It is suitable for runtime/session quota only, not durable billing, financial ledgers, or cross-instance enforcement.

Required environment:

- `CUMG_USAGE_LIMIT_PER_PRINCIPAL`: positive integer runtime quota.
- `CUMG_MCP_USAGE_CONTROL_MODULE`: optional ESM module specifier; defaults to `mcp-usage-control` when the package is locally installed/linked.

Optional environment:

- `CUMG_USAGE_BIND=127.0.0.1:8787`
- `CUMG_USAGE_RESERVATION_TTL_MS=60000`
- `CUMG_USAGE_MAX_RETAINED_OPERATIONS=10000`
- `CUMG_USAGE_MAX_RETAINED_BUDGET_KEYS=10000`

The Rust Hub points `CUMG_V2_USAGE_ENDPOINT` at the sidecar root, for example `http://127.0.0.1:8787/`. Only verified OAuth issuer+subject, CUMG `operation_id`, tool name, opaque reservation ID, bounded settlement outcome, and 0/1 units cross the bridge. Bearer tokens and tool arguments/results do not.
