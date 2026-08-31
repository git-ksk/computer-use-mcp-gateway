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

Directories containing state, secrets, trust material, or rollback assets must be owner-private (`0700`). The signer runtime directory is kept separately under the shorter `~/Library/Caches/cumg-v2/` path to stay well below macOS Unix-domain socket path limits and must also be `0700`. Secret files must be owner-private (`0600`). Do not place secret values in plist files, runtime manifests, command output, or issue logs. For coherent backup/restore of this state and the exact paired runtime, follow [`V2_BACKUP_RESTORE.md`](V2_BACKUP_RESTORE.md); a live directory copy is not an application-consistent backup.

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

## Single mutating authority

The physical desktop/Cua backend is one local mutation-authority domain. The standard single-Mac profile stores its owner state under `@ROOT@/mutation-authority` (normally `~/Library/Application Support/computer-use-mcp-gateway/mutation-authority`) and passes the same directory to every supported control plane that can reach that backend. The state contains only a closed owner role (`v1` or `v2`) and a monotonically increasing epoch. A private OS file lock serializes each effectful backend call and authority transition. Process death releases the transient lock, but it never transfers the durable owner role.

V1 direct-Cua startup requires `CUMG_MUTATION_AUTHORITY_DIR` whenever its policy can expose an effectful tool. V2 requires the same directory whenever Cua or Agent-owned Human Handoff input is configured. Observe-class Cua calls may remain available to the non-owner for diagnostics; every effectful call fails before backend dispatch unless that control plane is the durable owner. Human Handoff begin/recovery is also V2-owner gated, so Human input cannot bypass the Cua-side fence.

Authority changes are explicit CAS transitions through `v2_maint mutation-authority-switch`. The switch uses the same exclusive lock as live mutations, refuses a mismatched current owner, refuses while durable V2 quarantine exists, and requires the Agent-owned Handoff runtime to be provably idle while the lock is held. It never resolves quarantine, replays an operation, or infers ownership from process liveness. `v2_doctor` reports the privacy-bounded current owner/epoch.

Supported legacy-to-V2 cutover:

1. If an old unfenced legacy Gateway and V2 Agent are both loaded against the same backend, stop there. The upgrade preflight returns `legacy_gateway_unfenced`; it does not auto-stop either writer or guess that one is safe to retire.
2. Either retire the old legacy Gateway first, or upgrade it to an authority-aware V1 profile using the same authority directory. When an authority-aware legacy writer must remain active, initialize the domain with `owner=v1`.
3. Prove V2 quarantine is empty and Handoff is idle, then perform the explicit `v1 -> v2` CAS switch. After that transition, an authority-aware V1 may stay loaded for read-only diagnostics, but its effectful Cua calls are refused before dispatch. It may also be unloaded.
4. For rollback to an authority-aware V1, perform the inverse explicit `v2 -> v1` switch before enabling V1 mutations. Never restart an old unfenced legacy Gateway concurrently with V2.

A V2-only pre-authority installation has a narrower automatic migration lane: after normal preflight proves no legacy Gateway is loaded, the reviewed upgrade helper stops V2, creates a fresh private authority domain with `owner=v2`, adds the Agent configuration, and then restarts. An existing unreferenced authority directory causes refusal rather than adoption.

## Safe runtime upgrade

`scripts/v2-single-mac-upgrade.sh` is the reviewed upgrade helper for an already-installed single-Mac profile. The normal operator path runs the helper shipped inside a verified artifact via `v2_launchd_maintenance_job.py run-upgrade --artifact-bundle <bundle>`, so no CUMG/Handoff source checkout is required. Artifact mode validates the exact CUMG/Handoff pair and inner runtime payload before a durable maintenance transaction or service drain. The maintainer-only source mode additionally refuses unless both source checkouts are clean/pinned. Both modes refuse on live quarantine, active/recovering/faulted Handoff, unavailable required state/services, or incomplete Cua/signing inputs.

If the exact target `runtime-<cumg>-<handoff>` generation already exists, the helper never overwrites or repairs it in place. It may reuse that generation only after validating its owner-private path, exact source-commit pair, manifest schema and complete file set, every recorded SHA-256, absence of symlinks, required runtime import/dependencies, and stable Handoff helper code signatures. Any mismatch refuses before service shutdown. Failure cleanup deletes only a generation created by the current invocation, never a pre-existing verified generation. This bounded reuse path allows a paired binary/manifest cutover to resume after a prior generation-staging step completed without authorizing state edits, replay, or quarantine changes.

The known single-Mac Hub/Agent launchd label families are mutually exclusive. Preflight refuses if two Hub labels, two Agent labels, or a Hub and Agent from different known families are loaded at the same time. During a reviewed cutover, after the configured services are drained/unloaded and before they are restarted, the helper boots out and disables the alternate known Hub/Agent labels. Their plist files are deliberately preserved for rollback/forensics; the guard never deletes them and never changes quarantine or replay state.

The online-recovery Secure Enclave helper is also a bounded one-shot subprocess. `v2_recover` allows at most 60 seconds for helper completion; an unresponsive LocalAuthentication path is killed/reaped and returns `recovery_helper_timeout`. User denial/cancellation remains `recovery_user_presence_denied`, while a clearly unavailable LocalAuthentication facility is `recovery_helper_auth_unavailable`. None of these failures publishes an authorization or changes quarantine. Before retrying after timeout/cancellation, verify the current challenge again with `v2_recover status` and make a fresh explicit attempt; never infer a recovery decision from a timeout.


The normal reviewed install/upgrade path is now artifact-backed as documented in [`V2_RELEASE_ARTIFACTS.md`](V2_RELEASE_ARTIFACTS.md). The historical source-build path remains maintainer-only and intentionally bounded. `CUMG_V2_CARGO_BUILD_JOBS` defaults to `2` and accepts only `1..8`; `CUMG_V2_MIN_BUILD_FREE_MIB` defaults to `6144`. Preflight reads free space without creating a probe file and refuses before the maintenance transaction, service drain, mutation-authority migration, or install if that floor is not met. A later Cargo `ENOSPC`/`No space left on device` failure is recorded as `build_storage_exhausted` and exits before any installed runtime or mutation authority is changed. The helper never deletes Git WIP/untracked files as capacity recovery.

After preflight succeeds, the real upgrade creates an owner-private, atomically replaced `v2/maintenance/upgrade-transaction.json`. The record is operational evidence only: it cannot clear quarantine, resume/retry the upgrade, transfer mutation authority, restore rollback state, or replay desktop work. It records the exact CUMG/Handoff source commits, current phase, bounded status, target runtime generation, rollback bundle identity, mutation-authority owner/epoch, and explicit completion gates. `completed` is accepted only after runtime-manifest verification, safe launchd topology, V2 mutation authority, quarantine=0, paired Handoff runtime, service restart, healthy `v2_doctor`, rollback creation, and cleanup have all been recorded.

If the invoking MCP/client disconnects, inspect the durable record rather than reconstructing state manually:

```bash
v2_maint upgrade-status
```

`in_progress` means only that the last durable phase was recorded; it is **not** permission to start another upgrade. Check the one-shot launchd job with `scripts/v2_launchd_maintenance_job.py inspect`. If that job is still active, let the same invocation finish. If it is no longer active while the transaction remains `in_progress`, treat the transaction as incomplete and inspect before any new run. `failed_before_install` means the transaction failed before the install boundary; `failed_closed_after_stop` means the operator must assume services may intentionally be stopped and inspect the recorded rollback asset before recovery; `operator_action_required` means an attempted restore/cleanup/status update could not establish a clean automatic conclusion. A new transaction refuses to overwrite an `in_progress`, `failed_closed_after_stop`, or `operator_action_required` record.

Single-Mac maintenance is explicitly one-shot. Do **not** use `launchctl submit` for an upgrade/recovery command: launchd can infer persistence/relaunch behavior from an underspecified submitted job. `scripts/v2_launchd_maintenance_job.py run-upgrade` is the reviewed launchd wrapper for the upgrade helper. It writes an owner-private temporary plist with `RunAtLoad=true` and `KeepAlive=false`, forwards only the closed non-secret environment allowlist needed by the upgrade helper, verifies the job's launchd `runs` count never exceeds one even when the upgrade exits non-zero, and always boots the job out and deletes its temporary plist before returning. It never retries a failed upgrade.

Before any upgrade, both the wrapper and `v2-single-mac-upgrade.sh` inspect the current GUI launchd domain for known current/legacy CUMG maintenance labels. Any loaded job other than the wrapper's exact current label fails preflight with `stale_maintenance_jobs`; an active job is never auto-terminated. Use `scripts/v2_launchd_maintenance_job.py inspect` for privacy-bounded state/runs/last-exit diagnostics. After confirming a stale job is not running, `cleanup-stale` may boot it out and remove only a matching private temporary plist. The cleanup path refuses while any matching maintenance job is active.

For signing, prefer the exact 40-hex `CUMG_V2_MACOS_CODESIGN_FINGERPRINT`. The display-name `CUMG_V2_MACOS_CODESIGN_IDENTITY` remains a compatibility fallback only when it resolves to exactly one valid certificate. The helper verifies the selected certificate's exact Team ID **before signing**, then verifies the stable identifier/Team-ID designated requirement after signing. There is no ad-hoc fallback. The same reviewed identity signs `v2_recover` with stable identifier `com.github.git-ksk.cumg-v2-recover`; this provides stable artifact identity for deployment/audit, while Secure Enclave user presence remains the actual recovery-authorization boundary.


A successful upgrade performs this sequence:

1. prove CUMG and Handoff source provenance, quarantine=0, loaded services, no conflicting known Hub/Agent launchd family, no stale CUMG maintenance job other than the exact current one-shot wrapper, and an idle Agent-owned Handoff status without printing locator/owner data;
2. build all paired CUMG binaries (including the local-user `v2_recover` CLI and its CryptoKit Secure Enclave helper) from one merged CUMG commit and stage a private `runtime-<cumg>-<handoff>` Handoff generation from the exact reviewed Handoff `dist`, `package.json`, and lockfile plus the CUMG runtime host script; install only lockfile-pinned production dependencies with lifecycle scripts disabled, remove npm command-shim `.bin` links because runtime generations are symlink-free, reject any remaining dependency symlink, and prove the staged entrypoint imports under the configured Node executable before any service is stopped;
3. copy and stable-sign Handoff host helper(s) into that new generation; the live helper is not modified in place;
4. create a private rollback bundle containing old binaries/configuration, the Handoff env file, helper copies, and a self-contained old Handoff generation including its runtime dependencies; an archive missing those dependencies remains an external-runtime reference and must not permit cleanup of that runtime; authoritative Hub/Agent state is copied only after drain;
5. signal Hub first to close admission and drain, then unload Hub/Agent/signer, boot out and disable alternate known Hub/Agent labels without deleting their plists, and re-check stopped quarantine state;
6. while stopped, atomically retarget the private Handoff env and Agent plist to the staged generation, then atomically replace the paired CUMG binaries;
7. write `runtime-manifest.json` schema 3 with the merged CUMG source commit, exact Hub/Agent application-schema version, package version, and binary SHA-256 identities;
8. start signer -> Hub -> Agent, re-check that no conflicting known launchd family is active, and run `v2_doctor`, including the read-only Handoff status check;
9. only after doctor is healthy, prune eligible unreferenced `runtime-*` code directories. Active runtime, legacy externally referenced rollback runtime, a bounded recent set, and any unsafe/symlink-bearing candidate are protected/refused. Checkpoint/key/env/audit/control/rollback data are outside the cleanup candidate set.

Example:

```bash
export CUMG_V2_EXPECTED_CUA_VERSION=0.19.3
export CUMG_V2_MACOS_CODESIGN_FINGERPRINT=0123456789ABCDEF0123456789ABCDEF01234567
export CUMG_V2_MACOS_TEAM_ID=ABCDEFGHIJ
export CUMG_V2_HANDOFF_SOURCE_ROOT="$HOME/x-code/mcp-execution-handoff"
export CUMG_V2_EXPECTED_HANDOFF_COMMIT=<reviewed-40-hex-commit>

scripts/v2-single-mac-upgrade.sh --preflight-only
python3 scripts/v2_launchd_maintenance_job.py run-upgrade
```

The preflight may be run directly because it does not stop/restart services. The actual cutover must use the reviewed one-shot wrapper rather than an ad-hoc launchd job. A non-zero upgrade exit is returned after the temporary job has been booted out and its plist cleaned; it is not retried. After the first pinned cutover, `CUMG_V2_HANDOFF_SOURCE_ROOT` must remain an explicit reviewed checkout; the runtime's `CUMG_V2_HANDOFF_ROOT` points to immutable staged code and is no longer a development source checkout.

The rollback bundle is evidence and an explicit old-binary/old-state/Handoff-code set. Do not restore old binaries alone over state advanced by a newer runtime. Post-start failure stops the new profile fail-closed and requires explicit operator recovery.

Expired-recovery abandonment also writes a private append-only JSONL audit record before deleting the signed checkpoint. The record contains only timestamp, recovery epoch, prior closed recovery status, and a bounded result code. It intentionally excludes locator, process/window/context/intervention IDs, principals, action digests, TURN credentials, Human input, and payloads. If the audit append fails, abandonment is refused and recovery remains authoritative.

## Unified operator status

Use `v2_status` as the normal first diagnostic for this profile. With the reviewed LaunchAgents installed, it discovers only the exact non-secret configuration fields it needs from the Hub/Agent plists (Handoff control socket, Cua command/version, mutation-authority path) and otherwise uses the same bounded single-Mac defaults as `v2_doctor`:

```bash
"$HOME/Library/Application Support/computer-use-mcp-gateway/bin/v2_status"
"$HOME/Library/Application Support/computer-use-mcp-gateway/bin/v2_status" --json
```

JSON schema v1 is the stable machine-readable contract. It reports overall operator status, Agent/control-plane connectivity, backend status, the five #226 readiness lanes, quarantine/replay-safety state and #233 incident-review availability, privacy-bounded Handoff lifecycle, mutation-authority owner/epoch, verified runtime identity, #234 maintenance status/phase, one stable `primary_reason`, and one supported `next_action`. It intentionally omits takeover locators, intervention/recovery IDs or epochs, device/principal identity, commands/argv/cwd/env, typed text, URLs, screenshots, clipboard data, credentials, grants, and fingerprints.

`v2_status` is composition only. A `ready` lane does not authorize another lane; the command cannot resolve quarantine, switch mutation authority, resume/cancel Handoff, retry/resume an upgrade, or replay an operation. `review_incident` means use the #233 incident-brief flow, `complete_recovery` means the existing explicit Handoff/recovery flow, `inspect_upgrade` means `v2_maint upgrade-status`, and configuration/backend codes lead to `v2_doctor` or the backend diagnostics. Unknown or mismatched evidence fails closed as `unknown`, `unavailable`, or `action_required`, never healthy-by-default.

## `v2_doctor`

`v2_doctor` is read-only. It never resolves quarantine, dispatches work, reads secret contents, or prints raw command/result/desktop data.

The report includes an additive `readiness` summary for operator automation. Readiness is deliberately lane-scoped rather than a second authorization model: `control_plane`, `computer_use_observation`, `filesystem_observation`, `effectful_execution`, and `browser_effectful_execution` are derived from the existing authenticated Agent/Hub state, advertised capabilities, backend diagnostics, mutation authority, and durable quarantine state. A healthy observation lane never grants effectful authority. The existing top-level `overall` remains the conservative aggregate doctor result, so it may be `unsafe` while a bounded observation lane is still `ready`; callers that need lane availability must inspect `readiness` rather than reinterpret `overall`.

When a durable blocking operation exists, supported effectful lanes report `indeterminate_fenced` with `blocking_operation_retry_safe=false` and `operator_action=inspect_reconciliation_status`; independently verified observation lanes may remain `ready`. If transport/backend/state evidence is missing or incompatible, the affected lane reports `unavailable` or `unknown` rather than being assumed healthy. `unsupported` means the authenticated device did not advertise that lane. The summary never infers `confirmed_completed`/`confirmed_not_executed`, clears quarantine, retries, replays, or dispatches work.

The doctor also checks available space on the filesystem backing Agent state and on the host temporary filesystem without creating a probe file. `critical_lt_64_mib` is a degraded operability warning; `capacity_unavailable` means the read-only capacity query itself could not be completed. These closed values intentionally omit paths and byte-exact host telemetry. A prior Agent log containing `persistence_resource_exhausted` / `storage_full` identifies a local durable-checkpoint failure that can explain a subsequent northbound `agent_offline`; restoring capacity and allowing the normal one-shot service-manager restart/reconnect is the recovery path, never operation replay or quarantine clearing.

When the doctor itself is launched through the live single-Mac Agent using `execute_process` or `shell`, its own already-dispatched operation appears in the restart-safe Hub checkpoint as `HubRestartAfterDispatch` even though the live Hub has not quarantined it. The doctor classifies that one entry as `diagnostic_self_observation=restart_safe_active_caller` only when all authoritative runtime fences agree: the doctor is a descendant of the currently loaded Agent process, the Agent-to-Hub loopback transport is established, the checkpoint contains exactly one enrolled device and one quarantine-shaped entry, that entry is process/shell work in the registry's current generation, and its durable dispatch binding is present with `auto_reconciling`. No caller-supplied operation ID participates. Any missing/mismatched condition, multiple entries, older generation, or real indeterminate reason stays a normal blocking `live_quarantine` error. This classification changes diagnostics only; restart restoration still converts dispatched work to durable `Indeterminate` and no state is resolved, cleared, replayed, or rewritten.

For the standard profile it checks:

- runtime manifest schema 3, exact Hub/Agent application-schema version, source commit, and exact SHA-256 identity of `v2_hub`, `v2_agent`, `v2_maint`, `v2_doctor`, `v2_status`, `v2_recover`, `v2_recovery_enclave_helper`, and `v2_grant_signer`;
- authoritative Hub checkpoint readability and current registry/capability schema;
- exactly one enrolled single-Mac device and current generation;
- Agent checkpoint readability and exact Hub/Agent generation pairing;
- live quarantine count;
- Hub/Agent/external-signer LaunchAgent running state, privacy-bounded current/legacy CUMG maintenance-job presence, and the Agent -> loopback Hub transport being established;
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

- `v2_status` reports `overall=healthy` and `next_action=none`;
- `v2_doctor` reports `overall=healthy`;
- a fresh authenticated Agent generation is present after restart;
- live quarantine remains zero;
- the schema-3 runtime manifest verifies every installed paired binary and the exact Hub/Agent application schema;
- Handoff reports idle with no recovery/resume/fault;
- a harmless northbound semantic smoke reaches a durable terminal `Completed` state;
- the old binary/state rollback pair remains retained until the operator-selected bake period completes.

Also test at least one refused upgrade (for example dirty/diverged source or a live quarantine) and verify that no binary replacement occurs.
