# V2 least-privilege workspace mutation

> English is canonical. See V2_WORKSPACE_MUTATION.ja.md for Japanese.

Issue #107 adds one bounded Agent-native mutation primitive so coding workflows can update an approved workspace file without granting shell or execute_process.

## Authority boundary

- allowed_write_roots are operator-configured independently from read-only filesystem roots and process cwd roots. There is no fallback or inheritance.
- denied_write_subpaths are explicit exclusions inside writable roots; deny wins over allow.
- Empty writable-root configuration disables WriteWorkspaceFile advertisement entirely.
- Northbound authorization remains exact principal/device/WriteWorkspaceFile. A generic Dangerous grant, Shell grant, or another principal does not authorize it.
- Writable roots are device/operator-global in this v0.5 slice. This does not claim per-principal path/root isolation.
- The primitive performs filesystem work only. It does not spawn a process, open network authority, or inherit shell/keychain/SSH-agent credentials.

## Mutation contract

The MCP tool is write_workspace_file.

The raw payload is base64-encoded and capped at 32 KiB, and the requested UTF-8 path is capped at 4,096 bytes. A carrier test fixes the maximum typed command plus signed Hub/grant envelope below the ordinary 64 KiB application-message boundary. The Hub validates base64, byte count, and SHA-256 before creating the typed command; the Agent decodes and verifies the same byte count and digest again.

Exactly one precondition is required:

- create: expected_absent=true
- replace: expected_sha256=<64 lowercase hex chars>

Blind overwrite is not supported.

The signed command binds path, payload, expected byte count, payload SHA-256, and precondition. The signed result returns only a bounded receipt (bytes_written, content_sha256, created); command/result matching rejects mismatched receipts.

## Atomic publication

The target is never opened for in-place writing.

1. Resolve and reopen the intended parent through the shared #105 capability-rooted path primitive.
2. Reject denied paths, symlinks/reparse-style targets, non-regular targets, and hard-link targets. Existing targets are rechecked against their canonical object so Windows case aliases fail closed; Windows file/directory identity uses stable Win32 handle information.
3. For replacement, hash the existing regular file with a 4 MiB bound and verify the expected SHA-256.
4. Write a fresh same-parent temporary file, preserve existing permissions for replacement, and sync the staged file.
5. Re-prove deny policy and destination identity/content immediately before publication.
6. Create uses same-directory hard-link publication, which atomically fails if the destination appeared after the earlier check; the temporary name is then removed.
7. Replace uses same-parent atomic rename over the re-proven destination.
8. Preflight parent-directory sync support before publication. Filesystems that support it must sync the parent after publication. Only explicit Unsupported / directory-fsync InvalidInput is treated as unavailable; other preflight errors fail closed before effect, while a post-publication sync failure after successful preflight is Indeterminate.

A publication or flush outcome that cannot be proven is Indeterminate. The Agent reconnects without a terminal result so the existing Hub execution-safety path quarantines the operation. The initial slice never automatically retries or infers success from resulting content.

The CAS check is intentionally local to this API. It re-proves the target immediately before publication, but it does not claim a global filesystem transaction against an unrelated process that can mutate the same inode concurrently.

## Privacy and recovery

Default telemetry and durable recovery contain no raw file content, requested path, configured root/deny path, or OS error detail. Stable error codes and bounded receipts are sufficient for diagnostics.

The mutation payload is not copied into durable recovery. If result delivery is lost after dispatch, callers must use the existing operation_id/get_operation/quarantine workflow and must not replay blindly.

## Release integration

Issue #107 intentionally left the v0.5 release integration to #314. #314 now completes:

- final control/capability/Hub-Agent schema review and version bumps
- mixed-version fail-closed behavior
- macOS launchd, Linux systemd, and Windows packaged writable-root/deny configuration
- readiness/doctor/status diagnostics without exposing sensitive absolute policy paths
- upgrade/rollback acceptance and release-candidate packaging evidence
