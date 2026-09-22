# CUMG single-Mac artifact install

This directory is part of a reviewed macOS CUMG release-candidate bundle. It is source-free: normal install/upgrade must not require a CUMG or Handoff Git checkout.

Before extraction, verify the downloaded archive against its sibling `.sha256` file from the same reviewed release/candidate publication channel. After extraction, inspect the bounded artifact identity:

```bash
python3 install/v2_artifact_install.py inspect --bundle-dir "$PWD"
```

The macOS artifact is paired to the exact CUMG and Handoff source commits recorded in `release-artifact-manifest.json`. The bundled Handoff runtime has its own inner manifest. Any hash/path/pairing mismatch fails before activation.

## First install

Prerequisites remain explicit operator/deployment inputs, not artifact authority:

- a supported macOS interactive user session;
- Python 3, Node.js, and the reviewed Cua Driver version;
- a valid Apple code-signing identity matching the exact fingerprint and Team ID in the profile;
- separately provisioned owner-private CUMG secret/trust material;
- reviewed stable device/resource/proxy identity values.

Copy `install/single-mac-profile.example.json`, replace every placeholder, and prepare a private provisioning directory containing:

```text
provisioning/
  secrets/
    hub.key
    grant.key
    device.key
    tls-server.key
    trusted-proxy.key
  trust/
    hub.pub
    grant.pub
    device.pub
    tls-root.der
    tls-server.pem
    grant-signer-policy.json
    northbound-policy.json
```

Secret files must be owner-private. The installer never creates or guesses these authorities.

Run a non-activating readiness check first:

```bash
python3 install/v2_artifact_install.py install \
  --bundle-dir "$PWD" \
  --profile /secure/cumg/single-mac-profile.json \
  --provisioning-dir /secure/cumg/provisioning \
  --preflight-only
```

Then run the same command without `--preflight-only`. It verifies artifact identity before staging, stable-signs the local TCC-sensitive binaries/helpers with the reviewed identity, installs the paired runtime, initializes only the fresh mutation-authority domain, starts signer -> Hub -> Agent, and requires installed `v2_doctor` plus `v2_status` to become healthy. It never resolves quarantine, replays an operation, invents a recovery decision, or derives execution authority from artifact metadata.

## Upgrade

For an existing reviewed single-Mac profile, use the bundled one-shot maintenance wrapper:

```bash
python3 install/v2_launchd_maintenance_job.py run-upgrade --artifact-bundle "$PWD"
```

This reuses the durable upgrade transaction, service drain, exact rollback bundle, mutation-authority fences, post-upgrade doctor, Handoff runtime retention, and no-auto-retry behavior of the reviewed upgrade path. The historical source-build mode remains maintainer-only.

## Verified backup / restore

The macOS candidate also includes `install/v2_backup_restore.py`. This is the reviewed disaster-recovery workflow for the installed single-Mac profile; it is separate from the release-paired upgrade rollback bundle.

Create a backup only after the reviewed Hub/Agent/signer and conflicting legacy writer are stopped. The tool acquires the shared mutation-authority lock without changing owner/epoch, validates the exact installed CUMG/Handoff runtime identity and durable state, excludes runtime/transfer staging and coordination locks, and publishes an owner-private verified backup set. Record the emitted `manifest_sha256` **outside** the backup set.

Restore is deliberately two-phase. `restore` verifies that external digest and creates a non-running staged profile; `activate` re-verifies the same digest/stage, promotes only into the exact clean profile path, installs the reviewed LaunchAgents, and then starts signer -> Hub -> Agent. Backup/restore never clears quarantine, manufactures settlement, replays work, or changes mutation-authority owner/epoch. A quarantine may disappear after activation only through the existing authoritative recovery/reconciliation contract, such as an exact persisted backend receipt. See `docs/v2/V2_BACKUP_RESTORE.md` for the full boundary and freshness limitation.

The command sequence is `backup` -> record the emitted external `manifest_sha256` -> `verify` -> `restore` -> retain the printed `stage_dir` -> `activate`. Refusals return only a bounded `code` and `next_action`; they never print secret values or raw checkpoint payloads.
## Deferred cleanup remediation

If an otherwise healthy upgrade ends specifically as `operator_action_required / cleanup / cleanup_safety_refusal`, do not rerun the upgrade or edit the durable transaction by hand. The bundle includes `install/v2_deferred_cleanup_recovery.py`, which first supports a read-only plan and then an explicit `--health-confirmed --apply` path. It accepts only that exact deferred-cleanup terminal state, revalidates the active runtime plus package/source/Hub-Agent/control/capability identity, runs the normal fail-closed Handoff runtime cleanup, and completes the same transaction only after cleanup succeeds. Any other transaction state or identity mismatch is refused. See `docs/v2/V2_SINGLE_MAC_PRODUCTION.md` in the repository for the full operator procedure.
