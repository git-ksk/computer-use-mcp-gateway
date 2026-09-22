# V2 single-Mac verified backup and restore

> English is canonical. [日本語版 / Japanese translation](V2_BACKUP_RESTORE.ja.md)

This runbook defines the reviewed backup/restore contract for the supported macOS single-Mac profile. The workflow is deliberately fail-closed. A backup preserves durable runtime truth that existed at one snapshot boundary; it never settles quarantine, retries or replays an operation, transfers mutation authority, proves the absence of work after the snapshot, or turns copied files into new authorization.

The automated workflow is implemented by `v2_backup_restore.py`.

## Security model and non-claims

A verified backup has three independent properties:

1. **coherent snapshot** — Hub/Agent writers and effectful mutation are stopped while the durable set is copied;
2. **integrity/identity** — every included file is bounded, hashed, permission-checked, and tied to one exact CUMG/Handoff runtime identity;
3. **external anchoring** — the SHA-256 of the canonical backup manifest is recorded outside the backup set and is required for restore/activation.

The manifest digest is intentionally external. A digest stored only beside the files cannot detect an attacker who rewrites both the backup and its manifest.

A backup **cannot prove freshness**. Work may have happened after the snapshot and therefore may not appear in the restored replay history. Restore consequently has two phases: staged restore first, explicit activation second. Operators must choose a snapshot whose lineage is acceptable for the intended recovery. If there may have been effectful work after the snapshot, reconcile that operational history before authorizing new effectful work. The backup tool does not manufacture missing tombstones or infer that such work did not happen.

## Supported restore topology

The automated workflow supports the reviewed single-user, single-Mac profile only:

- the same absolute install root recorded by the backup;
- the same LaunchAgent directory recorded by the backup;
- owner-private CUMG state/configuration;
- authority-bearing referenced files inside the install root;
- the exact active immutable Handoff runtime generation;
- the exact installed CUMG binary set named by `runtime-manifest.json`.

Changing username/home path, migrating to another path layout, or restoring authority-bearing files from arbitrary external paths is not an automated restore. Re-provision those cases through the reviewed install/enrollment flow instead of rewriting backup contents.

A Secure Enclave sealed recovery file is not portable recovery authority. Restoring it to another Mac does not migrate the non-exportable private key.

## Authoritative backup inventory

The verified backup contains only reviewed durable classes.

### CUMG runtime identity

- `runtime-manifest.json`;
- exactly the binaries named by that manifest;
- their original bytes, modes, SHA-256 values, and valid macOS code signatures;
- the active Handoff `runtime-*` generation selected by `v2/handoff/managed-runtime.env`;
- the active generation's `runtime-generation-manifest.json` and exact file tree;
- `v2/handoff/managed-runtime.env` as an opaque sensitive file.

The backup manifest records bounded identity metadata such as CUMG source commit, Handoff source commit, package version, schema versions, and active runtime generation. It never copies secret environment values into manifest/log fields.

### Hub and Agent durable state

Hub state includes:

- committed `hub-%020d.json` checkpoints;
- the active reviewed online-recovery verifier file, where configured.

Agent state includes:

- committed `agent-%020d.json` checkpoints;
- reviewed online-recovery handoff files: challenge, authorization, and resolved evidence, when present.

The workflow copies the committed checkpoint history as files; it does not deserialize and rewrite checkpoint JSON. The latest checkpoint must be readable under the supported compatibility contract before backup and before activation.

The following are explicitly non-authoritative and excluded even when they exist below a state directory:

- `.cumg-v2-state.lock` and pending checkpoint files;
- `browser-upload-staging` and `browser-download-staging`;
- Playwright/runtime artifacts;
- transfer payloads and ephemeral-data stores;
- stale `*.pre-rotate-*` recovery-key copies;
- logs, audit streams, sockets, PIDs, caches, and temporary files.

An unknown file that is neither an approved durable class nor an explicitly reviewed non-authoritative class causes the automated backup to fail closed. This prevents new durable state from being silently omitted after future schema changes.

### Mutation authority

`mutation-authority.json` is durable and is preserved byte-for-byte. Its `owner` and positive `epoch` are recorded in the manifest.

`mutation-authority.lock` is coordination state, not backup truth. Backup holds the existing lock exclusively for the complete snapshot window. Restore creates a fresh owner-private lock file; it never restores the old lock inode or increments/changes the authority epoch.

### Provisioned trust and secrets

The workflow inventories the reviewed LaunchAgent environment and copies only regular owner-private authority-bearing files referenced from inside the install root, including the configured Hub/grant/TLS/trusted-proxy/device/recovery material.

References to sockets, executable prerequisites, run/cache roots, or OS services are configuration, not backup payload. Authority-bearing file references outside the install root are refused by the automated workflow.

The backup must be protected according to its most sensitive included secret.

## Coherent backup procedure

1. Run bounded read-only `v2_status`/`v2_doctor` inspection. Quarantine is allowed and must not be cleared for backup.
2. Stop Agent, Hub, grant signer, and any conflicting legacy effectful writer through the reviewed lifecycle. Handoff must be idle or represented by its durable recovery/checkpoint state.
3. Require the reviewed LaunchAgent family to be unloaded.
4. Acquire the shared mutation-authority lock exclusively without changing owner/epoch. If it is busy, fail.
5. Validate runtime manifest, installed binary hashes/signatures, active Handoff generation, state permissions/schema, LaunchAgent configuration, and referenced secret/trust files.
6. Copy the approved durable inventory into a new owner-private staging directory. Symlinks, special files, weak permissions, oversize files, or unexpected durable paths fail closed.
7. Build a canonical manifest containing exact relative paths, sizes, modes, hashes, runtime identity, checkpoint sequence/schema summaries, mutation owner/epoch, and the excluded-class policy. The manifest contains no secret payloads.
8. Atomically publish the completed backup directory.
9. Print the canonical `manifest_sha256`. Store that value outside the backup set.
10. Restart the original deployment separately and require its ordinary post-start health checks. Backup creation itself changes no durable authority state.

A live directory copy is not a supported snapshot.

Typical packaged invocation after the reviewed services are stopped:

```bash
ROOT="$HOME/Library/Application Support/computer-use-mcp-gateway"
LAUNCH_AGENTS="$HOME/Library/LaunchAgents"
BACKUP="/secure/cumg-backups/cumg-v2-$(date +%Y%m%d-%H%M%S)"

python3 install/v2_backup_restore.py backup \
  --install-root "$ROOT" \
  --launch-agent-dir "$LAUNCH_AGENTS" \
  --output "$BACKUP"
```

Record the emitted `manifest_sha256` outside `$BACKUP`. Do not store the only copy of that digest inside the backup directory.

## Verification

`verify` performs no restore and no state mutation. It requires the externally recorded `manifest_sha256` and rejects:

- digest mismatch;
- missing, additional, symlinked, special, weak-permission, or modified files;
- unsupported/newer backup manifest or checkpoint schema;
- mixed CUMG/Handoff runtime identity;
- invalid runtime-generation file sets;
- invalid installed binary code signatures;
- mutation owner/epoch mismatch;
- unsafe or external authority-bearing references.

An unanchored inspection may display bounded metadata, but it is not sufficient for restore or activation.

```bash
python3 install/v2_backup_restore.py verify \
  --backup "$BACKUP" \
  --expected-manifest-sha256 "$MANIFEST_SHA256"
```

`inspect --backup "$BACKUP"` is intentionally unanchored and read-only; it is useful only for bounded inventory display.

## Staged restore

Restore targets must be clean: no active reviewed/legacy services, no existing CUMG install root, no destination LaunchAgent files, and no competing mutation-authority state.

The `restore` phase:

1. verifies the complete backup against the external manifest digest;
2. revalidates the exact runtime/state/trust pairing;
3. creates a private staging root adjacent to the intended install root;
4. restores approved files with exact bytes/modes;
5. creates fresh coordination lock files instead of copying lock inodes;
6. verifies the staged tree again;
7. leaves reviewed LaunchAgents uninstalled/unloaded and performs no effectful activation.

A staged restore therefore cannot become a running mutation authority merely because files were copied.

```bash
python3 install/v2_backup_restore.py restore \
  --backup "$BACKUP" \
  --expected-manifest-sha256 "$MANIFEST_SHA256"
```

The command prints the exact `stage_dir`; retain that value for activation. If restore is interrupted before activation, delete only the tool-created staging root after verifying its marker/digest, then retry. Do not partially promote individual files.

## Explicit activation

Activation requires the same external manifest digest and the exact staged restore produced by the tool.

Before promotion it rechecks:

- destination paths are still clean;
- no conflicting service/writer is loaded;
- backup/staged hashes and runtime identity are unchanged;
- mutation owner/epoch equal the snapshot;
- staged unresolved quarantine/replay state still matches the backup exactly;
- active Handoff runtime identity is exact;
- the intended local prerequisites exist.

Activation then atomically promotes the staged install root, installs the reviewed LaunchAgents, and starts signer -> Hub -> Agent. The Agent must establish a fresh authenticated generation and advertise current capabilities; restored liveness is never inherited.

```bash
python3 install/v2_backup_restore.py activate \
  --backup "$BACKUP" \
  --expected-manifest-sha256 "$MANIFEST_SHA256" \
  --stage-dir "$STAGE_DIR"
```

Post-activation requires:

- expected mutation owner/epoch;
- any quarantine present in the staged snapshot either remains quarantined or is removed only by the existing authoritative recovery/reconciliation contract (for example an exact persisted #290 backend receipt), never by backup/restore itself;
- if no persistent quarantine remains, `v2_status` and `v2_doctor` must be healthy;
- if persistent quarantine remains, `v2_status` must report only the expected `previous_operation_outcome_unknown / review_incident` action and `v2_doctor` may be `unsafe` only because of the preserved live-quarantine/recovery boundary; any unrelated error fails activation acceptance;
- no automatic retry/replay of a pre-restore ambiguous operation;
- a harmless read-only semantic smoke before any deliberate effectful action.

If startup or health acceptance fails, stop the newly activated services and leave the restored state intact for bounded diagnosis. Do not auto-clear quarantine or roll forward state to make health green.

## Snapshot freshness and rollback boundary

The external manifest digest proves which backup set was selected; it does not prove that the set is the newest state ever produced by that deployment. This is a fundamental snapshot property, not something CUMG can infer from copied files.

Therefore:

- a general backup is disaster-recovery material, not an idempotency oracle for time after the snapshot;
- the release-paired upgrade rollback bundle remains the preferred immediate rollback mechanism for a failed upgrade;
- backup restore does not consume or rewrite upgrade transaction records;
- upgrade rollback assets are not promoted into current runtime authority merely because they are present near the install;
- if an operator cannot establish an acceptable snapshot lineage after possible post-backup effectful work, keep the restored profile non-effectful and use the normal recovery/re-provisioning process.

## Acceptance gate

Issue #347 is complete only when automated regression demonstrates:

- coherent backup requires stopped writers plus the shared mutation lock;
- exact runtime/Handoff/state/authority/trust inventory round-trips;
- a deliberately quarantined operation with no authoritative terminal/recovery evidence remains quarantined across backup -> staged restore -> activation/restart;
- replay/tombstone state present at snapshot remains present;
- browser/ephemeral/log/socket/lock/stale-key material is excluded;
- corrupt, incomplete, extra-file, symlink, permission, digest, runtime-identity, schema, and authority mismatches fail closed;
- restore never changes mutation owner/epoch or manufactures settlement;
- clean supported-profile activation reaches healthy `v2_status`/`v2_doctor`;
- EN/JA docs and release packaging contain the verified workflow.

Backup files and their manifest remain evidence/recovery material. They are never principal, device, capability, settlement, or replay authority by themselves.
