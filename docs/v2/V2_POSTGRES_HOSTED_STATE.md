# V2 PostgreSQL hosted Hub state

Status: implementation-enabling contract for Issue #391. This backend enables #284 Cloud Run acceptance; it does not by itself change the Cloud Run NO-GO support decision.

## Purpose

Cloud Run instances are replaceable and their writable filesystem is not authoritative state. The hosted Hub therefore uses PostgreSQL for the complete authoritative Hub snapshot while VM/single-host deployments keep the existing local CheckpointStore.

The PostgreSQL backend implements the same #283 contract:

- one complete HubPersistentState payload per stable state key;
- monotonic logical revision;
- monotonic writer epoch;
- compare-and-commit only when both expected revision and epoch match;
- durable commit before live authority advances;
- exact read-after-commit verification;
- stale writers cannot mutate or dispatch;
- ambiguous COMMIT or post-COMMIT verification is ReadAfterCommitMismatch and permanently fences that writer;
- provider unavailability before COMMIT is fail-closed and does not publish candidate state.

## Runtime topology

The hosted v2_hub requires the PostgreSQL settings when CUMG_V2_HOSTED_PROFILE=true.

- CUMG_V2_POSTGRES_HOST: TCP host or Unix socket directory. Cloud SQL may use the mounted `/cloudsql/PROJECT:REGION:INSTANCE` directory; a remote PostgreSQL service uses its reviewed DNS hostname with `CUMG_V2_POSTGRES_TLS_MODE=verify-full`.
- CUMG_V2_POSTGRES_PORT: defaults to 5432.
- CUMG_V2_POSTGRES_DATABASE: database name.
- CUMG_V2_POSTGRES_USER: runtime DB role.
- CUMG_V2_POSTGRES_PASSWORD_FILE: optional private password file. Password bytes are never accepted as an inline CUMG environment variable.
- CUMG_V2_POSTGRES_STATE_KEY: stable deployment-owned row key. It must remain unchanged across Hub revision rollout and Agent key rotation.
- CUMG_V2_POSTGRES_TLS_MODE: `disable` or `verify-full`. Unix sockets and local trust-authenticated fixtures use explicit `disable`; remote TCP PostgreSQL should use `verify-full`.
- CUMG_V2_POSTGRES_TLS_CA_PEM_FILE: optional bounded PEM CA bundle for `verify-full`. The bundle supplements the built-in public WebPKI roots and is never emitted through Debug/error/log output.
- CUMG_V2_POSTGRES_CONNECT_TIMEOUT_SECS: bounded connect timeout.
- CUMG_V2_POSTGRES_QUERY_TIMEOUT_SECS: bounded statement and lock timeout.

The serving process does not create or migrate the database schema.

### Remote TCP TLS

Remote TCP providers use the same PostgreSQL state contract; CUMG does not accept provider-specific project identifiers or raw DSNs. `verify-full` uses rustls and validates both the certificate chain and the configured PostgreSQL host name. Public WebPKI roots are trusted by default; deployments using a private database CA may add that CA through the bounded file-backed `CUMG_V2_POSTGRES_TLS_CA_PEM_FILE`.

CUMG deliberately exposes no insecure "encrypt without verification" mode. An untrusted CA, malformed CA bundle, or hostname mismatch fails the provider connection closed as unavailable before schema access. The CA file is trust material, not an application principal or writer identity.

Unix-socket transports, including the Cloud SQL mounted `/cloudsql/...` path, remain `disable`: the database TLS layer is not stacked onto the Unix socket, and contradictory `verify-full` configuration is rejected.

## Migration and least privilege

Apply packaging/postgres/001_hosted_hub_state.sql using a separate migration identity before starting the hosted Hub.

The serving role needs only:

- CONNECT on the database;
- USAGE on the target schema;
- SELECT, INSERT, UPDATE on cumg_hub_state.

It does not require CREATE, ALTER, DROP, DELETE, TRUNCATE, ownership, superuser, or schema-migration authority.

Example operator flow:

1. create or select the database with an administrative/migration identity;
2. apply packaging/postgres/001_hosted_hub_state.sql;
3. create the runtime login/role outside the Hub process;
4. revoke unnecessary privileges and grant only CONNECT, USAGE, SELECT, INSERT, UPDATE;
5. provision the DB password through the reviewed managed-secret file boundary if password authentication is used;
6. deploy one Hub revision with the stable state key and verify initial writer epoch/revision creation;
7. only then run overlapping revision acceptance under #284.

## Bootstrap

An empty table is valid. The first hosted Hub creates exactly one row for its configured state key at revision 1 / writer epoch 1 and immediately read-verifies it.

A later Hub process acquires authority by atomically retaining the complete state while incrementing revision and writer epoch. It never treats process lifetime, Cloud Run revision name, instance count, session affinity, or database credential version as writer authority.

## Rollout and rollback

Revision B must point at the same database and stable state key as revision A. B acquires a newer writer epoch before any authoritative mutation or southbound effect dispatch. A may remain alive and may retain an old Agent stream, but all subsequent authoritative commits fail as stale.

Rollback is a new deployment operation, not revival of old process authority. The rollback revision must connect to the same state key and acquire another fresh writer epoch. It must not restore an older database row over a live newer row.

Schema rollback is allowed only when the target binary is explicitly compatible with the current Hub state schema. Otherwise keep the newer binary or perform an offline reviewed migration.

## Failure classification

Before COMMIT, connection/query/lock failures are provider unavailable and no candidate is authoritative.

After COMMIT begins, timeout/error is ambiguous and is classified as ReadAfterCommitMismatch. The writer is fenced. A replacement process must read the exact durable state and acquire a newer epoch.

After a successful COMMIT, any missing, malformed, schema-incompatible, or unequal read-back is also ReadAfterCommitMismatch.

The backend enforces the CUMG 1 MiB checkpoint payload ceiling before database mutation. The migration adds a matching database-side payload check.

## Backup and restore boundary

#391 provides the durable provider but does not claim #284 backup/restore acceptance. For hosted support, #284 must prove a real provider backup/restore or equivalent PostgreSQL backup procedure while no writer can concurrently overwrite restored authority.

A restore must preserve the complete row payload, revision, writer epoch, quarantine, and replay barriers. After restoration, the first Hub must acquire a newer writer epoch before serving.

## Automated evidence

- existing #283 deterministic writer-fence and restart-quarantine tests remain green;
- async hosted-store Hub tests prove stale live-stream dispatch denial;
- async ambiguous-commit tests prove writer fencing plus replacement restoration to Indeterminate quarantine;
- tests/v2_postgres_hub_state.rs runs against a real PostgreSQL service and proves successive epochs, stale writer rejection, exact one-winner CAS race, schema-7 durable fence round-trip, and oversize rejection before mutation;
- CI provisions a PostgreSQL 17 service for the Rust job;
- Issue #396 upgrades that fixture to TLS with ephemeral CI-only CA/server keys and proves `verify-full` success for a trusted matching certificate, rejection of an untrusted CA and hostname mismatch, and fail-closed TLS provider loss followed by verified reconnect;
- local CheckpointStore behavior and VM/single-host startup remain unchanged.

## External PostgreSQL acceptance boundary

#284 still owns the real Cloud Run / external PostgreSQL deployment evidence:

- exact PostgreSQL provider/deployment identity, version, region, and connection mode;
- runtime service-account/IAM boundary;
- managed-secret password or reviewed alternative authentication;
- concurrent Cloud Run revision A/B overlap;
- forced termination before and after dispatch;
- backup/restore;
- secret rotation/log inspection;
- partition and Handoff lifecycle;
- rollback/recovery and alerting.

Until #284/#215 are green, Cloud Run remains unsupported.
