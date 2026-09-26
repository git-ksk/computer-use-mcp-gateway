# V2 hosted Cloud Run acceptance

Status: **pending real hosted execution** for Issue #284. This is the bounded public evidence record for the v0.9.0 Cloud Run support gate; creating it does not make Cloud Run supported.

Canonical design: [../V2_CLOUD_RUN_HUB.md](../V2_CLOUD_RUN_HUB.md)

## Evidence boundary

Public evidence must bind exact tested software and safety outcomes without publishing credentials, raw endpoint/project identifiers, secret-store locators, Handoff locator/session material, network addresses, frames, Human input, or application content. Provider-specific deployment commands and secret-bearing artifacts stay in the private operator workspace; the public record may retain bounded metadata and cryptographic digests.

## Exact run identity

| Field | Required evidence |
| --- | --- |
| CUMG | exact 40-hex `main` commit + release-manifest digest |
| Handoff | exact reviewed commit + package version |
| Cloud Run image | immutable image digest, never only a tag |
| schemas | control / capability / registry / Hub-Agent / execution-safety / Agent-M1 versions |
| PostgreSQL | provider class, major version, region class, connection mode; endpoint/project names private |
| deployment config | digest of reviewed non-secret config |
| secrets | opaque version/fingerprint only; never values or secret-store paths |

## Required real-hosted scenarios

Every row needs a run identifier, timestamp window, exact tested revisions, and bounded outcome.

- one-port h2c ingress keeps MCP / Agent gRPC / hosted Handoff authentication boundaries separate;
- revision B acquires a newer durable writer epoch and still-live A cannot commit or dispatch;
- pre-dispatch termination never dispatches later;
- post-dispatch interruption without terminal proof restores exact `Indeterminate` + quarantine;
- the interrupted old operation remains permanently non-replayed;
- 3300s proactive Agent rotation performs fresh authentication + generation advance;
- forced request/transport loss is never successful completion;
- hosted drain closes new admission, is <=8s, and forced termination cannot manufacture success/replay;
- PostgreSQL outage prevents authoritative mutation/dispatch and recovery requires a valid durable read + fresh epoch;
- secret/key rollout never creates two authorities; expired/revoked credentials do not settle/replay work;
- reviewed logs/OTLP contain no sentinel secret value or prohibited payload/locator material;
- backup/restore preserves revision lineage, quarantine and replay barriers, then requires a fresh writer epoch;
- Hub-Agent partition never restores Agent authority or clears quarantine;
- physical hosted Handoff proves Human active -> Agent deny -> Done -> fresh verification -> explicit resume;
- viewer/WebRTC/WSS/TURN generation replacement never replays Human input or changes Agent/Handoff authority epochs;
- rollback is a fresh deployment/writer acquisition and revives no old writer lease, generation, route lease or signer.

## Public run record

```text
date:
cumg_commit:
image_digest:
handoff_commit:
handoff_package:
postgres_provider_class:
postgres_major:
region_class:
connection_mode:
cloud_run_request_timeout_seconds:
cloud_run_concurrency:
cloud_run_min_instances:
cloud_run_max_instances:
private_config_digest:
private_evidence_bundle_digest:
revision_a_opaque_id:
revision_b_opaque_id:
result: PASS | FAIL
failed_rows:
notes:
```

`revision_*_opaque_id` must be a bounded public label/digest, not a Cloud project/revision URL or secret-store locator.

## Acceptance authority

Deterministic core tests are necessary but cannot substitute for these real-hosted rows. Cloud Run remains **NO-GO** until all required rows are green and #215/#277/#353/#284 close with evidence. Upstream `mcp-execution-handoff#19` and `#12` must also be accepted before hosted Handoff can be green.
