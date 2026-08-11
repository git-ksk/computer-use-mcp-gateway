# Testing

The test strategy separates deterministic repository checks, protocol/backend compatibility, documentation integrity, and real desktop GUI execution.

## Normal CI

`.github/workflows/ci.yml` runs with read-only repository permissions.

### Rust validation

```bash
cargo fmt --check
cargo check --locked --all-targets
cargo test --locked
python3 -m py_compile scripts/cua_gateway_smoke.py scripts/cua_desktop_e2e.py
```

### Real-Cua compatibility matrix

GitHub-hosted CI runs the gateway against a pinned Cua Driver on:

- Linux
- macOS
- Windows

For each OS, CI:

1. verifies the pinned Cua installer SHA-256;
2. verifies the pinned platform release payload SHA-256 independently;
3. extracts the expected `cua-driver` executable from the verified payload;
4. installs Cua;
5. requires the installed executable SHA-256 to equal the executable from the verified payload;
6. builds the gateway with `cargo build --locked`;
7. runs real-Cua MCP smoke for `2025-11-25`;
8. runs real-Cua MCP smoke for `2026-07-28`.

The smoke harness exercises the actual Gateway → `cua-driver mcp` path, backend tool discovery, `/healthz`, northbound MCP lifecycle behavior, tool filtering, deny-policy behavior, and rejection of malicious Host/Origin values.

## Documentation CI

`.github/workflows/docs.yml` is a separate read-only workflow that runs:

```bash
python3 scripts/check_docs.py
```

The checker scans `README.md`, `CONTRIBUTING.md`, and `docs/*.md` and fails when a repository-local Markdown link points to a missing target or escapes the repository root.

It deliberately does **not** fetch external URLs. Installation commands, current MCP-client UI/configuration, and reverse-proxy behavior can change upstream, so changes to those examples should be reviewed against the authoritative upstream documentation rather than depending on a flaky network link checker.

## What normal CI proves

Normal CI provides strong evidence for:

- Rust compilation and unit behavior;
- locked dependency resolution;
- the pinned Cua compatibility input on three operating systems;
- Gateway ↔ Cua MCP stdio connectivity;
- northbound MCP lifecycle compatibility for the two exercised protocol revisions;
- dynamic tool discovery/filtering;
- deny-by-default policy enforcement;
- Host/Origin transport guards;
- repository-local documentation link integrity.

## What normal CI does not prove

GitHub-hosted macOS runners are not a substitute for a persistent logged-in desktop with real Accessibility and Screen Recording grants. Normal CI therefore does not claim to prove:

- real screenshots of a user desktop;
- real clicks/typing against a persistent GUI session;
- macOS TCC permission persistence;
- arbitrary application-specific GUI workflows;
- long-running soak/resource behavior;
- continuing correctness of external third-party setup instructions after an upstream product changes.

The dual-protocol smoke suite is also **not** an MCP conformance certification. Integration of the official MCP conformance requirement runner remains tracked in `docs/ROADMAP.md`.

## Updating the Cua compatibility pin

Treat a Cua Driver version bump as a reviewed compatibility change, not a blind dependency refresh.

When changing the CI pin:

1. select the exact upstream release version;
2. update the versioned installer SHA-256;
3. update every platform release-payload SHA-256 used by the matrix;
4. verify the asset names/architectures still match the workflow assumptions;
5. let CI independently compare the installed executable to the verified payload executable;
6. require all three OS jobs and both protocol lifecycle smokes to pass before merge;
7. review newly discovered Cua tools against the gateway and optional backend policy before widening any production allowlist.

Do not replace the pinned release URLs with a mutable `latest` or convenience installer URL in normal CI.

## Desktop E2E

`.github/workflows/desktop-e2e.yml` is intentionally:

- `workflow_dispatch` only;
- restricted to `main`;
- targeted at `[self-hosted, macOS, cua-desktop-e2e]`;
- read-only with respect to repository contents.

The dedicated runner must be:

- a test machine, not a daily-use workstation;
- logged into a real macOS GUI session;
- preconfigured with CuaDriver Accessibility and Screen Recording permissions;
- isolated from untrusted pull-request execution.

The fixture performs a real desktop path using TextEdit:

```text
launch fresh TextEdit
    → screenshot evidence
    → click editor
    → type unique marker
    → independently read accessibility state
    → verify marker
```

This lane exists because a full computer-use E2E needs the OS permission and GUI state that ephemeral hosted runners cannot reliably preserve.

## Running smoke locally

After building the gateway and installing Cua, the compatibility harness can be run directly:

```bash
cargo build --locked
MCP_PROTOCOL_VERSION=2025-11-25 python3 scripts/cua_gateway_smoke.py
MCP_PROTOCOL_VERSION=2026-07-28 python3 scripts/cua_gateway_smoke.py
python3 scripts/check_docs.py
```

The Cua smoke script starts its own gateway process/configuration for the smoke scenario. Do not point it at a production desktop or reuse production credentials.

## Future coverage

Still tracked for V1 hardening/dogfood:

- official MCP conformance requirement runner integration;
- first successful dedicated-Mac desktop E2E execution;
- idle CPU/RAM benchmark;
- 100-call soak test;
- remote Cloudflare Access/Tunnel dogfood for this gateway;
- ChatGPT remote MCP dogfood for this gateway.
