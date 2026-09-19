# V2 Ephemeral Data References

Status: v0.5.0 foundation for #313.

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

## Recovery and quarantine

A future read-only output retrieval may be admitted as bounded recovery evidence for the exact original owner/operation. If so, it remains a read lane only. Retrieved bytes cannot manufacture terminal evidence, authorize replay, clear quarantine, or bypass mutation-resume barriers.

## Non-goals

This is not:

- a generic blob store;
- a bearer-capability system;
- a public host-filesystem handle API;
- durable object storage;
- permission to add principal identity to ordinary Agent grants;
- a replacement for exact DeviceCapability authorization.
