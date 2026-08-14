# computer-use-mcp-gateway

`computer-use-mcp-gateway` (CUMG) is a Rust MCP gateway for policy-controlled computer use. The recommended V2 runtime separates a remotely reachable **Hub** from a desktop-side **Agent** and exposes bounded, backend-neutral semantic capabilities instead of making raw backend tool names or identifiers part of the northbound contract.

> **Runtime status:** V2 Hub + Agent is the recommended development/runtime path. V1 remains available as `v1_gateway` for regression/reference and existing production operation. Browser transfer (upload/download) is not implemented or advertised.

## Overview

CUMG keeps execution authority in the gateway while allowing maintained infrastructure to own generic concerns such as network-edge TLS and authentication. Its core safety rule is that an ambiguous state-changing operation is never automatically replayed after a client, Hub, Agent, transport, backend, or device reconnects.

The first reviewed computer-use backend is [Cua Driver](https://github.com/trycua/cua), pinned in CI at **0.19.3**. Cua-specific MCP names, raw browser/CDP identifiers, accessibility handles, screenshots, and provider response shapes terminate at the adapter boundary rather than becoming stable CUMG API surface.

## Architecture

```text
MCP client
    |
    | authenticated MCP
    v
V2 Hub
    |  exact principal -> device -> capability authorization
    |  operation ownership / generation fencing / quarantine
    |
    | gRPC bidirectional stream over TLS
    v
V2 Agent
    |  direct process / shell / bounded filesystem capabilities
    |  backend-neutral Desktop + Browser semantic adapter
    v
Computer-use backend (Cua Driver today)
```

The Hub owns admission, authorization, operation state, replay barriers, and durable `indeterminate` quarantine. The Agent owns the authenticated device session and local execution boundary. Optional usage accounting is separate accounting authority and cannot authorize execution, clear quarantine, or permit replay.

See [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md) and the canonical V2 boundary in [`docs/v2/V2_POSITIONING.md`](docs/v2/V2_POSITIONING.md).

## Getting Started

For a clean installation, follow [`docs/GETTING_STARTED.md`](docs/GETTING_STARTED.md). It covers prerequisites, OS permissions, backend verification, Hub/Agent configuration, local MCP connectivity, and the safe path to remote access.

Build all current targets with:

```bash
git clone https://github.com/git-ksk/computer-use-mcp-gateway.git
cd computer-use-mcp-gateway
cargo build --locked
```

The default binary is the V2 Hub:

```bash
cargo run --locked -- --help
```

The desktop-side Agent is separate:

```bash
cargo run --locked --bin v2_agent -- --help
```

V1 remains available for legacy/regression operation:

```bash
cargo run --locked --bin v1_gateway -- --allow-tools list_apps,list_windows,get_accessibility_tree,get_screen_size
```

Do not expose a backend transport directly or replace the documented trust/TLS boundaries with an unauthenticated public listener. Packaged service examples live under `packaging/`; client examples and troubleshooting are in [`docs/CLIENTS.md`](docs/CLIENTS.md) and [`docs/TROUBLESHOOTING.md`](docs/TROUBLESHOOTING.md).

## Security

CUMG is fail-closed around the capabilities that can change a desktop. In particular:

- northbound authorization is reduced to an exact authenticated principal, stable device, and exact capability;
- stale device generations, stale capability revisions, wrong-context references, consumed references, and unauthorized operations are rejected;
- ambiguous state-changing work becomes `indeterminate` and quarantines the device until explicit resolution instead of being replayed;
- raw backend IDs and generic backend escape hatches are not part of the V2 northbound semantic surface;
- backend request arguments/results, screenshots, clipboard data, bearer tokens, and private credentials are excluded from default telemetry/logging;
- remote deployments keep the Hub listener behind a reviewed TLS/authentication edge and preserve the outbound Agent connection model.

These controls do not replace OS permissions, endpoint hardening, secret custody, network controls, or deployment-specific monitoring. Read [`docs/SECURITY.md`](docs/SECURITY.md) and [`docs/v2/V2_THREAT_MODEL.md`](docs/v2/V2_THREAT_MODEL.md) before exposing a sensitive desktop remotely.

## V2 status

The active implementation is tracked by capability rather than by internal milestone names:

| Area | Status |
| --- | --- |
| Desktop semantic path | Complete / accepted |
| Browser core | Complete / accepted |
| Browser transfer (upload/download) | Not started; unsupported and unadvertised |
| V1 regression/conformance | Required and preserved |

Browser core currently covers the typed prepare, bind, inspect, navigate, click, type, dialog, and pointer paths while preserving opaque CUMG references and exact-or-refuse execution semantics. Transfer remains a separate security boundary and is intentionally not implied by Browser core completion.

See [`docs/v2/STATUS.md`](docs/v2/STATUS.md) for the current map, [`docs/v2/acceptance/V2_BROWSER_CORE_ACCEPTANCE.md`](docs/v2/acceptance/V2_BROWSER_CORE_ACCEPTANCE.md) for Browser core evidence, and [`docs/README.md`](docs/README.md) for how active specs, acceptance evidence, and archived decision records are organized.

## Testing and Deployment

Repository changes should pass the same warning-free baseline locally before relying on CI:

```bash
cargo fmt --all -- --check
cargo check --locked --all-targets
cargo test --locked
cargo clippy --locked --all-targets -- -D warnings
python3 scripts/check_docs.py
git diff --check
```

V1 compatibility remains an explicit regression boundary:

```bash
python3 scripts/v1_quality_gate.py
python3 scripts/v1_conformance.py
```

Normal CI also exercises the pinned Cua release on Linux, macOS, and Windows, selected MCP conformance scenarios, cancellation behavior, resource/soak checks, and the backend passthrough contract. Trusted physical Desktop acceptance is operator-controlled rather than granting an untrusted hosted runner GUI access.

See [`docs/TESTING.md`](docs/TESTING.md) for exact guarantees and limits. Deployment, service supervision, TLS/authentication edge requirements, credential handling, and V1 legacy configuration are documented in [`docs/DEPLOYMENT.md`](docs/DEPLOYMENT.md).

## Documentation

Start at [`docs/README.md`](docs/README.md). It separates:

- operator/contributor guides;
- active V2 specifications;
- acceptance evidence;
- historical PoC and decision records.

The repository-local documentation link checker recursively validates these directories so archived or nested documents cannot silently accumulate broken local links.

## License

MIT. This is an independent project and is not affiliated with Cua AI or the Model Context Protocol project.
