# Cloud Run hosted Hub packaging

This directory contains provider-neutral packaging for the hosted v2_hub profile tracked by Issue #284.

The image contains the v2_hub binary and CA certificates only. Deployment-specific identity, database credentials, authorization policy, resource URIs, provider host names, project identifiers, and secret-store identifiers must be supplied outside source control at deploy time.

Required private inputs remain file-backed through the existing *_FILE interfaces. Do not bake Hub, Agent, grant-signing, PostgreSQL, OAuth, or Handoff secret bytes into the image, Docker build arguments, environment variables, revision labels, or this repository.

Cloud Run Secret Manager volumes are platform-owned and may be mounted with permissions that are intentionally broader than CUMG accepts for private key/password inputs. The image entrypoint therefore copies only the explicit hosted input allowlist from generic mount paths into the container-owned private runtime directory with mode 0600, then points the existing *_FILE variables at those copies. Hub identity, grant signer, enrolled device trust, PostgreSQL password, northbound policy, and hosted Handoff policy mounts are always required. The OAuth introspection client-secret mount is required only when OAuth introspection is the selected hosted authentication mode; OIDC/JWT deployments omit that mount. A supplied optional mount is still validated and materialized through the same private-file boundary. Missing required or unsafe supplied mounts fail before v2_hub starts; the core file-permission checks are not relaxed.

The image defaults only the platform-independent hosted safety profile:

- hosted one-port mode enabled;
- Agent session maximum 3300 seconds;
- reauthentication drain 30 seconds;
- planned shutdown drain 8 seconds;
- ephemeral local state directory reserved for non-authoritative runtime use.

Authoritative Hub state must use the configured external PostgreSQL backend. Remote TCP PostgreSQL must use CUMG_V2_POSTGRES_TLS_MODE=verify-full.

Cloud Run deployment commands and concrete provider values are intentionally kept out of this public repository. Acceptance operators should keep those values in a private local deployment workspace or managed secret/config system.

The repository-level .gcloudignore and .dockerignore use a deny-all allowlist so only the Rust build inputs and provider-neutral Cloud Run build files are uploaded or sent to the Docker daemon. The image destination is supplied as the _IMAGE substitution at submission time and is not recorded here.

## Managed-secret rotation guard

Cloud Run secret rotation must validate the raw service/revision volume graph before an old managed-secret version is disabled. Do not assume that `gcloud run deploy --update-secrets` or `--set-secrets` removes every previously generated secret-backed volume definition: an unmounted/orphan volume can still make a later revision undeployable after its referenced version is retired.

Export the private service or candidate revision as JSON and run:

```text
python3 scripts/v2_cloud_run_secret_volume_guard.py PRIVATE_MANIFEST.json --auth-mode oidc_jwt
python3 scripts/v2_cloud_run_secret_volume_guard.py PRIVATE_MANIFEST.json --auth-mode oauth_introspection
```

The guard emits only bounded PASS/FAIL codes and counts. It never prints secret names, versions, mount paths, or values. OIDC/JWT expects the six required hosted mounts; OAuth introspection expects the same six plus the introspection client secret. Every secret version must be explicitly pinned for acceptance rather than using `latest`.

The reviewed rotation order is:

1. keep the old version enabled while staging the replacement revision;
2. prove the replacement is Ready, has the newer durable writer epoch, and fences the old writer;
3. run the volume-graph guard against the raw candidate/service manifest;
4. if the guard reports orphan/duplicate/missing bindings, canonicalize the private service manifest so only volumes referenced by intended container mounts remain, then apply that reviewed manifest while serving traffic remains pinned to the accepted revision;
5. re-run the guard and confirm the graph is one-to-one and pinned;
6. disable/retire the old version only after the canonical candidate is accepted;
7. while the old version remains disabled, require a fresh no-traffic revision to become Ready and pass the guard again.

A rotation is not accepted merely because revision B started once. Rollback is also a fresh deployment/writer acquisition and must use a canonical current secret graph; it never revives an old writer epoch, Agent generation, Handoff route, or retired signer.
