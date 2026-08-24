# V2 single-Mac production profile

This profile is for one trusted, logged-in macOS development workstation that runs the V2 Hub, external grant signer, Agent, and Cua backend on the same machine. It does not replace the Linux/systemd deployment model and does not widen CUMG's network trust boundary.

## Security boundary

The reviewed shape is:

```text
reviewed proxy/tunnel
        |
127.0.0.1 northbound MCP
        |
V2 Hub (LaunchAgent, loopback Hub transport)
        |
private Unix socket
        |
external grant signer (LaunchAgent)

V2 Agent (logged-in user LaunchAgent)
        |
Cua Driver (pinned version)
```

The Hub transport and MCP listener are loopback-only. Public TLS/origin policy remains owned by the reviewed proxy/tunnel. The Hub configuration receives no grant private-key path or material; it receives only signed exact-capability grants from the external signer and has no in-process fallback in this profile. Because these LaunchAgents run as the same logged-in macOS user, this is a process/configuration separation, **not** an OS-enforced key-custody boundary against a compromised same-user Hub process. The Linux/systemd profile with a separate signer user remains the stronger custody boundary.

Secrets and durable state live outside the repository under:

```text
~/Library/Application Support/computer-use-mcp-gateway/
```

Directories containing state, secrets, trust material, or rollback assets must be owner-private (`0700`). The signer runtime directory is kept separately under the shorter `~/Library/Caches/cumg-v2/` path to stay well below macOS Unix-domain socket path limits and must also be `0700`. Secret files must be owner-private (`0600`). Do not place secret values in plist files, runtime manifests, command output, or issue logs.

## Reviewed LaunchAgents

Templates are under `packaging/launchd/single-mac/`:

- `com.github.git-ksk.cumg-v2-grant-signer.plist`
- `com.github.git-ksk.cumg-v2-hub.plist`
- `com.github.git-ksk.cumg-v2-agent.plist`

Replace `@HOME@`, `@ROOT@`, `@RUN_ROOT@`, `@BINARY_DIR@`, `@HANDOFF_CONTROL_SOCKET@`, `@HANDOFF_RUNTIME_COMMAND@`, `@HANDOFF_RUNTIME_SCRIPT@`, and `@HANDOFF_RUNTIME_ENV_FILE@`. Replace the explicit `REPLACE_*` values with the deployment's reviewed resource URI, trusted-proxy identity, and stable device ID. The Hub template contains only the private operator-control relay; the Agent template owns the canonical Handoff runtime, WebRTC/capture/input surface, and its private transport env file. Never substitute a public Hub/MCP bind address; the single-Mac profile is intentionally loopback-only.

Create the signer runtime directory before loading the signer:

```bash
ROOT="$HOME/Library/Application Support/computer-use-mcp-gateway"
RUN_ROOT="$HOME/Library/Caches/cumg-v2"
install -d -m 700 "$RUN_ROOT"
```

Start order is signer -> Hub -> Agent. The Agent stays in the logged-in user session because Cua and Agent-owned Handoff capture/input require explicit macOS TCC attribution on the controlled device.

The example signer policy is intentionally minimal. Expand it only with exact `DeviceCapability` values that the reviewed northbound policy requires. There is no wildcard capability and no signer fallback.

## Safe runtime upgrade

`scripts/v2-single-mac-upgrade.sh` is the reviewed upgrade helper for an already-installed single-Mac profile. It refuses before replacement when the CUMG checkout is not clean `main == origin/main`, the reviewed Handoff checkout is not clean `main == origin/main` at the exact `CUMG_V2_EXPECTED_HANDOFF_COMMIT`, live quarantine exists, Handoff is active/recovering/faulted, required state/services are unavailable, or the Cua/signing inputs are incomplete.

For signing, prefer the exact 40-hex `CUMG_V2_MACOS_CODESIGN_FINGERPRINT`. The display-name `CUMG_V2_MACOS_CODESIGN_IDENTITY` remains a compatibility fallback only when it resolves to exactly one valid certificate. The helper verifies the selected certificate's exact Team ID **before signing**, then verifies the stable identifier/Team-ID designated requirement after signing. There is no ad-hoc fallback.

A successful upgrade performs this sequence:

1. prove CUMG and Handoff source provenance, quarantine=0, loaded services, and an idle Agent-owned Handoff status without printing locator/owner data;
2. build all paired CUMG binaries from one merged CUMG commit and stage a private `runtime-<cumg>-<handoff>` Handoff generation from the exact reviewed Handoff `dist`, `package.json`, and lockfile plus the CUMG runtime host script; install only lockfile-pinned production dependencies with lifecycle scripts disabled, remove npm command-shim `.bin` links because runtime generations are symlink-free, reject any remaining dependency symlink, and prove the staged entrypoint imports under the configured Node executable before any service is stopped;
3. copy and stable-sign Handoff host helper(s) into that new generation; the live helper is not modified in place;
4. create a private rollback bundle containing old binaries/configuration, the Handoff env file, helper copies, and a self-contained old Handoff generation including its runtime dependencies; an archive missing those dependencies remains an external-runtime reference and must not permit cleanup of that runtime; authoritative Hub/Agent state is copied only after drain;
5. signal Hub first to close admission and drain, then unload Hub/Agent/signer and re-check stopped quarantine state;
6. while stopped, atomically retarget the private Handoff env and Agent plist to the staged generation, then atomically replace the paired CUMG binaries;
7. write `runtime-manifest.json` schema 2 with the merged CUMG source commit, exact Hub/Agent application-schema version, package version, and binary SHA-256 identities;
8. start signer -> Hub -> Agent and run `v2_doctor`, including the read-only Handoff status check;
9. only after doctor is healthy, prune eligible unreferenced `runtime-*` code directories. Active runtime, legacy externally referenced rollback runtime, a bounded recent set, and any unsafe/symlink-bearing candidate are protected/refused. Checkpoint/key/env/audit/control/rollback data are outside the cleanup candidate set.

Example:

```bash
CUMG_V2_EXPECTED_CUA_VERSION=0.19.3 \
CUMG_V2_MACOS_CODESIGN_FINGERPRINT=0123456789ABCDEF0123456789ABCDEF01234567 \
CUMG_V2_MACOS_TEAM_ID=ABCDEFGHIJ \
CUMG_V2_HANDOFF_SOURCE_ROOT="$HOME/x-code/mcp-execution-handoff" \
CUMG_V2_EXPECTED_HANDOFF_COMMIT=<reviewed-40-hex-commit> \
  scripts/v2-single-mac-upgrade.sh --preflight-only
```

Run the same reviewed environment without `--preflight-only` for the cutover. After the first pinned cutover, `CUMG_V2_HANDOFF_SOURCE_ROOT` must remain an explicit reviewed checkout; the runtime's `CUMG_V2_HANDOFF_ROOT` points to immutable staged code and is no longer a development source checkout.

The rollback bundle is evidence and an explicit old-binary/old-state/Handoff-code set. Do not restore old binaries alone over state advanced by a newer runtime. Post-start failure stops the new profile fail-closed and requires explicit operator recovery.

Expired-recovery abandonment also writes a private append-only JSONL audit record before deleting the signed checkpoint. The record contains only timestamp, recovery epoch, prior closed recovery status, and a bounded result code. It intentionally excludes locator, process/window/context/intervention IDs, principals, action digests, TURN credentials, Human input, and payloads. If the audit append fails, abandonment is refused and recovery remains authoritative.

## `v2_doctor`

`v2_doctor` is read-only. It never resolves quarantine, dispatches work, reads secret contents, or prints raw command/result/desktop data.

For the standard profile it checks:

- runtime manifest schema 2, exact Hub/Agent application-schema version, source commit, and exact SHA-256 identity of `v2_hub`, `v2_agent`, `v2_maint`, `v2_doctor`, and `v2_grant_signer`;
- authoritative Hub checkpoint readability and current registry/capability schema;
- exactly one enrolled single-Mac device and current generation;
- Agent checkpoint readability and exact Hub/Agent generation pairing;
- live quarantine count;
- Hub/Agent/external-signer LaunchAgent running state and the Agent -> loopback Hub transport being established;
- private signer socket shape/parent permissions;
- server certificate and pinned Agent trust-root validity;
- actual Cua Driver version against the explicit reviewed pin;
- Agent-owned Handoff status through the private control socket. Output is limited to bounded guidance (`idle`, exact recover-reissue, exact recover-rebind-or-abandon-if-prior-surface-absent, active, or faulted) and never emits the locator or owner/intervention identifiers.

JSON output is suitable for local operator automation:

```bash
"$HOME/Library/Application Support/computer-use-mcp-gateway/bin/v2_doctor" \
  --expected-cua-version 0.19.3 \
  --json
```

Exit status is `0` for healthy, `1` for degraded/warning, and `2` for unsafe/error. A non-zero result must not be treated as permission to replay or clear an indeterminate operation.

## Acceptance

Before declaring a single-Mac upgrade healthy, require all of the following:

- `v2_doctor` reports `overall=healthy`;
- a fresh authenticated Agent generation is present after restart;
- live quarantine remains zero;
- the schema-2 runtime manifest verifies every installed paired binary and the exact Hub/Agent application schema;
- Handoff reports idle with no recovery/resume/fault;
- a harmless northbound semantic smoke reaches a durable terminal `Completed` state;
- the old binary/state rollback pair remains retained until the operator-selected bake period completes.

Also test at least one refused upgrade (for example dirty/diverged source or a live quarantine) and verify that no binary replacement occurs.
