# V2 least-privilege workspace filesystem observation

> English is canonical. [日本語版](V2_WORKSPACE_FILESYSTEM.ja.md)

Issue #105 extends the read-only filesystem lane without granting shell authority. It adds bounded stateless file ranges, deterministic bounded directory continuation, and a capability-rooted path primitive shared with future #107 mutation.

## Authority boundary

- `allowed_file_roots` remain the only observation roots; cwd roots are not inherited.
- Ambient canonicalization only selects the configured root. The actual target is reopened relative to an already-open `cap_std::fs::Dir`, so the authority proof is capability-rooted rather than canonicalize-then-open by pathname.
- Symlink/reparse escapes fail closed. Unix replacement-race tests and a Windows junction replacement test cover the shared primitive in `v2_workspace_path`.
- Residual assumption: configured root paths are operator-controlled startup policy and are not adversarially replaced while `FilesystemPolicy` is being constructed. #105 anchors each accepted root as an open capability for later operations; packaged permissions/ACL preflight for root trust remains #314-owned.
- This does not sandbox `ExecuteProcess` or `Shell`; they remain separately authorized Dangerous capabilities.

## Stateless file ranges

`read_file` accepts required `path`, optional unsigned `offset` defaulting to `0`, and optional positive `max_bytes` capped at 8 KiB. Results return `offset`, `bytes`, `truncated`, and `next_offset`. Overflow fails closed; EOF returns empty/non-truncated.

Each call is an independent observation. `next_offset` is a convenience cursor, not a snapshot token. A file may change between calls, so callers must not assume concatenated ranges are one immutable file version.

## Deterministic directory continuation

`list_directory` accepts required `path` and optional `after`. Entries are sorted by UTF-8 name before paging. Results echo `after` and return `truncated` plus `next_cursor`; pass `next_cursor` as the next `after`.

The cursor is stateless and assumes stable contents between calls. One call scans at most 4,096 entries, returns at most 256 entries, and applies a conservative 24 KiB serialized-entry budget. Scan-budget overflow fails closed with `filesystem_directory_scan_limit_exceeded`; an entry that cannot fit the result budget also fails closed.

Raw file contents, requested paths, and continuation values are not added to telemetry; workspace-path Debug output is redacted.

## Schema integration

#105 changes typed `DeviceCommand` / `DeviceResult` filesystem shapes but does not independently bump `CONTROL_SCHEMA_VERSION`, `CAPABILITY_SCHEMA_VERSION`, or `HUB_AGENT_SCHEMA_VERSION`. Final migration pairings, mixed-version refusal, packaged readiness, and v0.4.0 -> v0.5.0 upgrade acceptance remain owned by #314.

Result/command matching binds file responses to requested offset/byte limits and directory responses to the requested cursor, so mismatched continuation responses are rejected.
