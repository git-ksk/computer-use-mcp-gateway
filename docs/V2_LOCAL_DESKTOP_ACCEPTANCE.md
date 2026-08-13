# V2 local desktop acceptance

Physical macOS desktop acceptance is local-only. The public repository does not use a self-hosted macOS runner for this check.

Normal CI stays on GitHub-hosted Linux, macOS, and Windows runners. Physical GUI validation is an operator-controlled acceptance step for trusted code.

## Run

Prerequisites:

- Rust 1.88.0
- `cua-driver` installed with the required macOS permissions
- the exact trusted commit checked out locally

Run:

```bash
bash scripts/v2_desktop_acceptance.sh
```

The wrapper verifies Cua permissions, runs the real TextEdit screenshot/click/type/readback fixture, then runs the V2 real-Cua ambiguity/restart/reconnect/no-auto-replay/explicit-resolution regression.

The historical V1 gateway binary is built only for the mature TextEdit physical fixture. The recommended runtime remains V2 Hub + V2 Agent.

A successful run ends with:

```text
PASS local physical desktop acceptance
```

Changes affecting the Computer Use adapter or V2 desktop execution boundary should pass this local acceptance before release. CUMG remains the authority for operation ownership, generation fencing, durable indeterminate quarantine, explicit resolution, and no-auto-replay.
