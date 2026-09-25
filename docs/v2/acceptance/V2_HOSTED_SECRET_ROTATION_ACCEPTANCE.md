# V2 Hosted Secret / Key Rotation Acceptance

Status: **automated core green; real hosted revision A/B acceptance remains required by #353/#284 before Cloud Run support can leave NO-GO**.

This contract keeps secret rotation separate from execution authority. Capacity settings, session affinity, secret versions, and process lifetime are not writer authority; the durable Hub writer epoch/revision remains the hosted mutation fence.

## Rotation matrix

| Boundary | Stable invariant | Rotation rule | Safety effect |
| --- | --- | --- | --- |
| Hub application key (`CUMG_V2_HUB_SECRET_FILE`) | one Agent trust continuity chain | old+new signed continuity proof with monotonic rotation epoch | never restores an old Hub writer lease |
| Agent device key | stable logical `device_id` | old+new proof; invalidate current session; reconnect on newer device generation | terminal / `Indeterminate` / quarantine / replay state remains unchanged |
| Grant-signing key | grant capability contract and max lifetime | bounded old/new verifier overlap, then retire old | retired signer cannot authorize new grants; no operation replay |
| OAuth introspection client secret | issuer/resource/principal mapping | rotate as process credential through reviewed managed-secret provisioning | authentication credential only; never execution authority or durable Hub state |
| OIDC/JWKS signing keys | exact issuer/audience/resource/algorithm policy | external IdP `kid` rotation through bounded JWKS refresh/cache | stale/unknown keys fail authentication; token loss is not execution success |
| Audit fingerprint secret | non-authoritative correlation | independent rotation | cross-key comparison becomes unavailable, never settlement/replay authority |
| Handoff viewer/transport material | Agent generation and intervention epoch stay distinct | viewer/transport generations rotate independently | disconnect/fallback is not Human `Done` and cannot restore Agent authority |
| Recovery private authority | separate local/operator recovery boundary | separately reviewed administrative rotation | hosted service identity cannot substitute for it |
| Container TLS private key | absent from hosted profile | public TLS terminates outside the Hub process | hosted mode continues to reject Hub TLS key/cert files |

Secret-store/workload identity, OAuth credentials, Hub/device identity, grant signing, Handoff authority, and durable writer authority are separate trust domains. Rotating one must not widen another.

## Automated acceptance

`tests/v2_hosted_secret_rotation.rs` composes existing production primitives and proves:

1. two hosted revisions get successive writer epochs; the old epoch cannot commit;
2. device-key rotation invalidates the old session/key while an already ambiguous operation remains exact `Indeterminate` + quarantine and rejects replay after restart;
3. Hub trust continuity and grant-signing overlap/retirement rotate independently;
4. viewer/transport replacement rotates only those Handoff generations, leaving Agent generation/intervention epoch unchanged and stale transport unusable;
5. authoritative Hub checkpoint serialization contains no private Hub/device/grant key material;
6. OAuth introspection config debug output redacts the client-secret value.

Existing `v2_m1_hub_service` session-reauthentication, `v2_m1_partition_recovery`, #283 writer-fence/replacement, and #277 Handoff routing regressions remain part of the evidence set.

## Hosted rollout contract

A real hosted deployment must inject secret bytes through a reviewed managed-secret boundary compatible with the existing `*_SECRET_FILE` interfaces. Secret values must not appear in ordinary environment variables, revision labels, image layers, command lines, source control, runtime manifests, durable Hub checkpoints, Handoff routing state, normal audit, or telemetry.

Revision B must receive the new credential, acquire a newer durable writer epoch before authoritative mutation, and prove revision A cannot commit/dispatch afterward even if A still owns an old Agent stream. Expiring/revoking the old credential or session must not become completion or replay. `Indeterminate`, quarantine, and replay barriers must survive replacement. Rollback must acquire a fresh writer epoch; it never revives an old lease, Agent generation, Handoff route, or retired grant signer.

## Real hosted evidence still required

#353 stays open until #284 records a real revision A/B rollout with the selected external durable-state provider and managed-secret mechanism, concurrent old/new revisions, at least one old credential/session expiry or revocation, an interrupted effectful operation, Handoff generation separation, rollback/recovery steps, and log/OTLP inspection proving an injected sentinel secret value never appears.

Until that artifact is green, Cloud Run remains unsupported.
