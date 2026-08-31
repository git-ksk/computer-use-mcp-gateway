# V2 release artifacts and single-Mac installation

> English is canonical. [日本語版 / Japanese translation](V2_RELEASE_ARTIFACTS.ja.md)

This document defines the post-#237 distribution boundary. Release artifacts improve installation provenance; they do **not** become CUMG execution, recovery, Human-Handoff, or mutation authority.

## Distribution scope

| Platform / profile | Artifact status | Normal installation boundary |
| --- | --- | --- |
| macOS single-Mac reference profile | install-capable reviewed release candidate | source-free artifact install/upgrade; local stable Apple code signing remains required before activation |
| Linux Hub / Agent | reviewed native candidate evidence | existing source/service packaging; no official binary installer claim yet |
| Windows desktop Agent | reviewed native candidate evidence | existing source/Task Scheduler profile; no official binary installer claim yet |

A CI artifact is not automatically an official GitHub Release asset. The existing `v0.3.0` GitHub Release remains source-only. A later release may promote a reviewed artifact only through the documented release procedure; tags and published assets are never inferred from CI success.

## macOS artifact identity

The macOS bundle is self-contained for CUMG/Handoff source code. `release-artifact-manifest.json` schema v2 records:

- package version and exact 40-hex CUMG source commit;
- exact Hub/Agent application-schema version;
- platform and architecture;
- exact paired `mcp-execution-handoff` source commit;
- the closed `single-mac-artifact-v1` install profile;
- exact size and SHA-256 for every allowlisted file.

The bundle includes the paired Hub/Agent/maintenance/operator binaries, `v2_recover`, the macOS Secure Enclave helper, the single-Mac LaunchAgent templates, bounded install/upgrade tooling, and a self-contained Handoff runtime payload. The Handoff payload has a second manifest binding the same CUMG commit to the exact reviewed Handoff commit and hashing every runtime file. Unexpected files, unsafe paths, symlinks, missing production dependencies, digest drift, or commit mismatch fail verification.

The reviewed Handoff pin lives in `packaging/release/single-mac-handoff.json`. The release workflow also pins the same commit and refuses if those identities differ. Normal operators do not discover or supply a Handoff Git commit.

## Integrity before activation

The workflow emits an archive-level `.sha256` file. Verify that checksum before extraction using the release/candidate publication channel as the trust source. After fresh extraction, the bundled verifier checks the closed file manifest again:

```bash
python3 install/v2_artifact_install.py inspect --bundle-dir "$PWD"
```

Neither manifest nor checksum authorizes an operation, clears quarantine, selects a recovery decision, transfers mutation authority, or proves that a desktop side effect happened. They are distribution evidence only.

CI artifacts are not Apple-notarized public installers. The reviewed single-Mac profile instead preserves the existing TCC continuity boundary: before activation, the installer/upgrade path copies verified bytes into private staging and stable-signs the TCC-sensitive local executables/helpers with the operator's exact code-signing certificate fingerprint and Team ID. There is no ad-hoc fallback. The installed `runtime-manifest.json` records the post-signing binary digests that `v2_doctor` verifies.

## First install

A clean supported Mac means the CUMG and Handoff source repositories are absent; it does **not** mean deployment identities are generated automatically. The operator must already have:

- an interactive supported macOS user session;
- Python 3, Node.js, and the reviewed Cua Driver version;
- a valid local Apple code-signing identity matching the selected fingerprint/Team ID;
- separately provisioned owner-private Hub/grant/device/TLS/proxy secret material and matching trust/policy files;
- reviewed stable device, MCP resource, and trusted-proxy identity values.

The artifact contains `install/single-mac-profile.example.json` and a self-contained `install/README.md`. Secret/trust bytes are never embedded in the release artifact or profile.

Run the non-activating preflight first:

```bash
python3 install/v2_artifact_install.py install \
  --bundle-dir "$PWD" \
  --profile /secure/cumg/single-mac-profile.json \
  --provisioning-dir /secure/cumg/provisioning \
  --preflight-only
```

Then repeat without `--preflight-only`. The installer:

1. verifies the outer artifact, target macOS architecture, profile, private provisioning inputs, and inner Handoff payload before creating installed runtime state;
2. stages and stable-signs the TCC-sensitive CUMG/Handoff executables without changing the downloaded artifact;
3. installs the exact paired binaries and immutable Handoff runtime generation;
4. initializes only a fresh `owner=v2` mutation-authority domain; it never adopts or overwrites an existing authority domain;
5. installs the reviewed LaunchAgents and starts signer -> Hub -> Agent;
6. requires the **installed** `v2_doctor` and `v2_status` to report healthy before reporting install success.

An existing installation is refused; use the upgrade path instead. Startup/post-verification failure stops any services started by that invocation. It does not replay work or manufacture recovery success.

## Artifact-backed upgrade and rollback

For an existing reviewed single-Mac deployment, invoke the **bundled** helper through the existing one-shot launchd wrapper:

```bash
python3 install/v2_launchd_maintenance_job.py \
  run-upgrade --artifact-bundle "$PWD"
```

The wrapper still guarantees `RunAtLoad=true`, `KeepAlive=false`, a single observed run, no automatic retry, and cleanup of the temporary maintenance plist. Artifact mode verifies the complete bundle and inner Handoff pair before the durable maintenance transaction or service drain. It uses private copies of artifact binaries for local stable signing; source mode remains maintainer-only.

The existing upgrade contract remains authoritative: quarantine must be empty, Handoff must be idle, mutation authority is fenced, Hub admission drains before replacement, the old paired binaries/state/config/Handoff runtime are archived together, installed runtime identity is re-hashed, signer -> Hub -> Agent restarts, and `v2_doctor` must be healthy. Artifact mode refuses rather than reconstructing an incomplete old rollback runtime from the **new** Handoff payload.

Rollback is therefore still a version-paired recovery action. Never restore only an old binary over state written by a newer incompatible runtime. Never use rollback to clear quarantine or retry an ambiguous operation.

## Supply-chain and publication strategy

Current reproducible evidence is:

- exact CUMG and Handoff commits;
- Rust and npm lockfiles at build time;
- pinned GitHub Actions revisions;
- Dependency Review and CodeQL in protected PR CI;
- outer archive checksum, closed outer file manifest, and closed inner Handoff file manifest;
- native packaged-binary smoke from fresh extraction on Linux/macOS/Windows;
- source-free macOS installer inspection plus automated install/startup/doctor/status orchestration coverage.

For an **official** binary GitHub Release, release preparation must additionally attach a reviewed SBOM/license inventory generated from the exact Cargo/npm lockfile graph and provenance/attestation binding the published archive checksum to the protected workflow/source commits. macOS notarization is a separate distribution decision; until it is implemented and accepted, documentation must not describe the CI candidate as a notarized general-purpose installer.

These supply-chain records remain evidence. Signing, SBOM, provenance, or release metadata must never become principal/device/capability authority.
