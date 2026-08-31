# V2 single-Mac backup and restore

> English is canonical. [日本語版 / Japanese translation](V2_BACKUP_RESTORE.ja.md)

This runbook covers backup and restore for the reviewed macOS single-Mac profile. It is deliberately conservative: backup/restore preserves durable truth and paired runtime identity; it never settles quarantine, retries an operation, replays work, transfers mutation authority, or treats a copied checkpoint as proof that a desktop side effect did or did not occur.

## What is authoritative

The normal install root is:

```text
~/Library/Application Support/computer-use-mcp-gateway/
```

A recoverable backup must preserve, as one coherent versioned set:

- `v2/state/hub` and `v2/state/agent` durable checkpoints;
- `mutation-authority` state;
- installed paired binaries and `runtime-manifest.json`;
- the active immutable Handoff `runtime-*` generation and its managed runtime environment file;
- current reviewed LaunchAgent plists and non-secret configuration required to reconstruct the profile;
- owner-private secret/trust/key files referenced by that deployment, including the Hub/grant/TLS/trusted-proxy material where configured;
- retained rollback assets that are still part of the deployment's supported rollback boundary.

If a deployment intentionally stores a referenced secret, trust anchor, Handoff runtime file, or configuration outside the normal install root, the backup inventory must include that exact external path as well. Do not assume the install root is complete merely because the default profile uses it.

The cache/run directory `~/Library/Caches/cumg-v2/` contains runtime socket state and is **not** authoritative backup state. Recreate it as an owner-private directory when starting the restored services.

## Backup procedure

1. Require `v2_status`/`v2_doctor` to be readable and record only their bounded status/evidence. A non-zero result does not authorize cleanup or replay.
2. If quarantine exists, preserve it exactly. Do not resolve, retire, or clear an ambiguous operation merely to obtain a "clean" backup.
3. Drain and stop the reviewed services through the documented lifecycle so Hub/Agent checkpoint writers are no longer active. Do not copy a live state directory as though it were an atomic application snapshot.
4. With Hub, Agent, and signer stopped, copy the authoritative inventory above into an owner-private backup destination while preserving bytes, filenames, permissions, and directory structure. Use an operator-approved backup mechanism; do not transform or hand-edit checkpoint JSON.
5. Record the installed `runtime-manifest.json`, exact artifact/archive checksum when available, CUMG/Handoff source identities, package version, target architecture, Cua version, and the backup creation time as separate non-authoritative inventory metadata.
6. Protect the backup according to the most sensitive included material. A backup containing secrets or sealed recovery-key material is sensitive even if CUMG logs are payload-free.
7. Restart signer -> Hub -> Agent and require the normal post-start `v2_status`/`v2_doctor` checks. Creating a backup must not leave the deployment in a different authority/quarantine state.

For upgrades, the upgrade helper's paired rollback bundle remains the preferred immediate rollback asset. A general backup does not replace that release-paired rollback contract.

## Restore procedure

Restore only onto the intended trusted machine/profile and only after identifying the exact backup set.

1. Keep Hub, Agent, signer, and conflicting legacy writers stopped. Do not allow any effectful backend writer to run while restore is in progress.
2. Re-establish the required local prerequisites separately: supported macOS user session, reviewed Cua version, Node/Python runtime, Apple code-signing identity/TCC permissions, proxy/tunnel policy, and any external trust anchor not contained in the backup. A filesystem backup does not recreate OS authorization.
3. Restore the complete paired backup set, including durable Hub/Agent state, mutation-authority state, installed runtime identity, active Handoff generation, configuration, and required secret/trust files. Do not restore only old binaries over newer state or only old state under newer incompatible binaries.
4. Preserve owner-private directory/file permissions. Refuse symlinked, group/world-writable, missing, or unexpected trust/secret/state paths rather than repairing them implicitly.
5. Verify the restored `runtime-manifest.json` and use the version-paired `v2_maint`/runtime from that same backup or reviewed compatible artifact. Never use an arbitrary newer checkout to mutate restored old state.
6. Start signer -> Hub -> Agent. Require a fresh authenticated Agent generation and current capability advertisement; restored liveness is never inherited from a checkpoint.
7. Run `v2_status` and `v2_doctor`. Any restored unresolved quarantine must remain unresolved. Mixed/unsupported schema or runtime identity must fail closed.
8. Perform a harmless read-only semantic smoke before any deliberately authorized effectful action. If an effectful operation was ambiguous at backup time, follow the normal incident/recovery flow; never retry it because the machine was restored.

## Secure Enclave recovery key limitation

`v2_recovery_enclave_helper` stores a bounded sealed representation of a non-exportable Secure Enclave key. Backing up that sealed file may preserve local recovery metadata, but it does **not** make the private key portable to another Mac or Secure Enclave. A machine replacement therefore requires a newly provisioned endpoint recovery key and corresponding Hub trust update through the reviewed provisioning flow; do not claim that copying the sealed file migrates recovery authority.

## Restore acceptance

A restore is acceptable only when all of the following are true:

- exact runtime/manifest verification succeeds;
- Hub/Agent durable state is readable under the supported compatibility contract;
- mutation authority has the expected owner/epoch and was not inferred from process liveness;
- a fresh Agent session is authenticated after restore;
- unresolved quarantine count/identity is preserved rather than silently cleared;
- Handoff is either safely idle or remains in its explicit recovery state;
- `v2_status`/`v2_doctor` provide bounded actionable state;
- no pre-restore ambiguous operation was replayed.

Backup archives and inventory metadata are evidence and recovery material only. They never become principal/device/capability or operation-settlement authority.
