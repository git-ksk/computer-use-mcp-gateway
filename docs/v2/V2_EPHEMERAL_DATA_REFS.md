# V2 Ephemeral Data References

Status: v0.5.0 foundation from #313, with the #83 process/shell output consumer implemented in the current v0.5 branch.

## Boundary

V2 uses bounded ephemeral data references only for data that is too large or too stateful to carry directly in a normal least-privilege workspace response. The first consumers are expected to be truncated process/shell output (#83) and, only where needed, deterministic directory continuation (#105).

These references are deliberately **non-authoritative**:

- they do not authorize a device capability;
- they do not settle an operation, clear quarantine, or prove that an effectful action completed;
- they are not checkpoint, recovery, release, or backup truth;
- they do not expose Agent filesystem paths.

## Split authority

The authenticated northbound principal exists at the Hub. Ordinary southbound grants intentionally do not carry that principal identity. Therefore public reference authorization remains Hub-side.

The Hub public-ref registry binds owner, device, generation, capability revision, operation/kind, expiry, and an opaque Agent locator. The Agent private data store binds its opaque locator and bounded bytes to device, generation, capability revision, operation, kind, and local lifetime.

The public ref and Agent locator are different random values. Possession of either value is not sufficient authority. A caller must pass normal northbound authentication/authorization before the Hub resolves the public ref and dispatches an exact retrieval operation.

## Lifecycle and resource limits

The foundation uses short-lived TTLs and explicit count/byte ceilings. Expiry decisions must use trusted Hub/Agent runtime or session-derived time; northbound callers never supply the authoritative clock. Agent staging is private, regular-file-only, bounded per object and globally, and deleted on expiry, explicit removal, clean shutdown, or Agent-store startup. Its dedicated storage parent must live outside the authoritative Agent checkpoint and rollback-backup tree, and startup cleanup removes only the fixed ephemeral child beneath that parent. Startup deletion means an Agent restart may invalidate every outstanding locator.

That restart behavior is intentional. Durable get_operation must not persist a live public ref or Agent locator because those values can become stale independently of the authoritative operation record.

Resource exhaustion fails closed. It never raises the ordinary inline output/file limits or widens filesystem/process authority.

On Windows and the other packaged profiles, deployment creates a dedicated ephemeral root outside the authoritative Agent state/rollback tree and inherits the reviewed ACL/permission boundary from its parent. The v0.5 release-integration gate (#314) owns the exact path, ACL/permission preflight, and backup exclusion rather than defining a second independent authority model here.

## Privacy and observability

Raw staged bytes, Agent paths, public refs, and private locators are excluded from default telemetry. Stable error categories may identify expiry, quota, generation, revision, operation, or kind mismatch without including payload or host path detail. Wrong-owner resolution collapses to the same stale category as an unknown ref so the public ref cannot become a cross-principal existence oracle.

## #83 process/shell output consumer

The #83 consumer keeps the existing normal inline contract unchanged: stdout and stderr each remain capped at 16 KiB in the ordinary process/shell result.

When one stream exceeds that inline cap and a dedicated Agent ephemeral-data parent is configured:

- stdout and stderr are retained separately as raw bytes before display decoding;
- at most 4 MiB per stream is retained, so one operation can retain at most 8 MiB across both streams;
- the supervised pipe is still drained to EOF after the retention ceiling is reached; excess bytes are discarded rather than accumulated;
- the Agent returns only an opaque private locator plus bounded metadata to the Hub;
- the Hub mints a different public `output_ref` bound to owner, device, generation, capability revision, source operation, stream kind, and TTL;
- `read_process_output` is a separate exact Observe capability rather than inherited Shell/ExecuteProcess authority;
- each follow-up read defaults to 8 KiB and is capped at 64 KiB;
- offsets and lengths are raw-byte offsets over the retained prefix, and northbound bytes are base64 encoded so UTF-16 or invalid UTF-8 output cannot make offsets ambiguous;
- `complete=true` means the retained prefix contains the complete stream; `complete=false` means the stream exceeded the 4 MiB retention ceiling and later bytes are intentionally unavailable.

If the dedicated ephemeral-data parent is not configured, process/shell execution remains available with the existing 16 KiB inline result and no live output refs. #314 owns packaged path selection, readiness/permission preflight, and upgrade/schema integration.

Durable `get_operation` persists only the existing bounded inline process/shell result. It never persists a live public ref, Agent locator, or retained extended-output bytes.

## Recovery and quarantine

`ReadProcessOutput` is classified as read-only recovery evidence. The Hub resolves the public ref first and binds retrieval to its exact source operation. While a quarantine exists, retrieval is admitted only when that quarantine belongs to the same source operation. The read cannot settle quarantine, manufacture terminal evidence, authorize replay, or bypass mutation-resume barriers.

## Non-goals

This is not:

- a generic blob store;
- a bearer-capability system;
- a public host-filesystem handle API;
- durable object storage;
- permission to add principal identity to ordinary Agent grants;
- a replacement for exact DeviceCapability authorization.
