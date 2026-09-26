# Cloud Run hosted Hub packaging

This directory contains provider-neutral packaging for the hosted v2_hub profile tracked by Issue #284.

The image contains the v2_hub binary and CA certificates only. Deployment-specific identity, database credentials, authorization policy, resource URIs, provider host names, project identifiers, and secret-store identifiers must be supplied outside source control at deploy time.

Required private inputs remain file-backed through the existing *_FILE interfaces. Do not bake Hub, Agent, grant-signing, PostgreSQL, OAuth, or Handoff secret bytes into the image, Docker build arguments, environment variables, revision labels, or this repository.

Cloud Run Secret Manager volumes are platform-owned and may be mounted with permissions that are intentionally broader than CUMG accepts for private key/password inputs. The image entrypoint therefore copies only the explicit hosted input allowlist from generic mount paths into the container-owned private runtime directory with mode 0600, then points the existing *_FILE variables at those copies. Missing or unsafe mounted inputs fail before v2_hub starts; the core file-permission checks are not relaxed.

The image defaults only the platform-independent hosted safety profile:

- hosted one-port mode enabled;
- Agent session maximum 3300 seconds;
- reauthentication drain 30 seconds;
- planned shutdown drain 8 seconds;
- ephemeral local state directory reserved for non-authoritative runtime use.

Authoritative Hub state must use the configured external PostgreSQL backend. Remote TCP PostgreSQL must use CUMG_V2_POSTGRES_TLS_MODE=verify-full.

Cloud Run deployment commands and concrete provider values are intentionally kept out of this public repository. Acceptance operators should keep those values in a private local deployment workspace or managed secret/config system.

The repository-level .gcloudignore and .dockerignore use a deny-all allowlist so only the Rust build inputs and provider-neutral Cloud Run build files are uploaded or sent to the Docker daemon. The image destination is supplied as the _IMAGE substitution at submission time and is not recorded here.
