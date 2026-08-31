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
