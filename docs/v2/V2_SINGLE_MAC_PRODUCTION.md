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

Replace `@HOME@`, `@ROOT@`, `@RUN_ROOT@`, and `@BINARY_DIR@`. Replace the explicit `REPLACE_*` values with the deployment's reviewed resource URI, trusted-proxy identity, and stable device ID. Never substitute a public Hub/MCP bind address; the single-Mac profile is intentionally loopback-only.

Create the signer runtime directory before loading the signer:

```bash
ROOT="$HOME/Library/Application Support/computer-use-mcp-gateway"
RUN_ROOT="$HOME/Library/Caches/cumg-v2"
install -d -m 700 "$RUN_ROOT"
```

Start order is signer -> Hub -> Agent. The Agent stays in the logged-in user session so macOS TCC attribution remains explicit.

The example signer policy is intentionally minimal. Expand it only with exact `DeviceCapability` values that the reviewed northbound policy requires. There is no wildcard capability and no signer fallback.

## Safe runtime upgrade

`scripts/v2-single-mac-upgrade.sh` is the reviewed upgrade helper for an already-installed single-Mac profile. It refuses to proceed when:

- the source checkout is not clean `main == origin/main`;
- the installed paired `v2_maint` cannot inspect authoritative state;
- a live quarantine exists;
- required LaunchAgents/state directories are absent;
- the reviewed Cua version is not explicitly supplied;
- the external signer profile is incomplete.

A successful upgrade performs this sequence:

1. preflight source/state/service health;
2. build `v2_hub`, `v2_agent`, `v2_maint`, `v2_doctor`, and `v2_grant_signer` from one commit;
3. archive old binaries, service configuration, and—after drain—the authoritative stopped Hub/Agent state;
4. signal the Hub first so admission closes and already-admitted work can drain while the Agent is still connected;
5. unload Agent and signer only after Hub shutdown;
6. re-check stopped authoritative state and refuse the upgrade if drain produced a quarantine;
7. atomically replace the version-paired runtime binaries;
8. write `runtime-manifest.json` containing only package version, source commit, binary names, and SHA-256 digests;
9. start signer -> Hub -> Agent;
10. run `v2_doctor`; a failed post-start doctor stops the profile fail-closed and does not automatically combine old binaries with newer state.

Example:

```bash
CUMG_V2_EXPECTED_CUA_VERSION=0.19.3 \
  scripts/v2-single-mac-upgrade.sh --preflight-only

CUMG_V2_EXPECTED_CUA_VERSION=0.19.3 \
  scripts/v2-single-mac-upgrade.sh
```

The helper prints the rollback asset directory. That directory is evidence and an explicit old-binary/old-state pair. Do not restore only the old binaries over state that a newer runtime has already advanced. Recovery remains an explicit operator action.

## `v2_doctor`

`v2_doctor` is read-only. It never resolves quarantine, dispatches work, reads secret contents, or prints raw command/result/desktop data.

For the standard profile it checks:

- runtime manifest schema, source commit, and exact SHA-256 identity of `v2_hub`, `v2_agent`, `v2_maint`, `v2_doctor`, and `v2_grant_signer`;
- authoritative Hub checkpoint readability and current registry/capability schema;
- exactly one enrolled single-Mac device and current generation;
- Agent checkpoint readability and exact Hub/Agent generation pairing;
- live quarantine count;
- Hub/Agent/external-signer LaunchAgent running state and the Agent -> loopback Hub transport being established;
- private signer socket shape/parent permissions;
- server certificate and pinned Agent trust-root validity;
- actual Cua Driver version against the explicit reviewed pin.

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
- the runtime manifest verifies every installed paired binary;
- a harmless northbound semantic smoke reaches a durable terminal `Completed` state;
- the old binary/state rollback pair remains retained until the operator-selected bake period completes.

Also test at least one refused upgrade (for example dirty/diverged source or a live quarantine) and verify that no binary replacement occurs.
