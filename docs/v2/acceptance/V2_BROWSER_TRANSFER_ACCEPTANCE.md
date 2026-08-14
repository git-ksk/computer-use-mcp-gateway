# V2 browser transfer acceptance

Status: **Browser transfer complete** on 2026-08-15 for the reviewed `feat/v2-browser-semantic-parity` branch. This acceptance does not merge PR #44 or change V1 production/Cloudflare routing.

## Contract

Browser transfer is a dedicated dangerous-capability boundary, not a generic filesystem API.

Upload uses two northbound semantics:

1. `browser_stage_upload_file` accepts a path-safe logical filename plus at most 16 MiB of bounded base64 data and returns a context-scoped CUMG file ref.
2. `browser_upload_file` consumes one to 32 CUMG file refs and one current upload-capable browser element ref.

The Agent materializes upload bytes only below its private state directory. The staging broker creates private directories/files, rejects symlinks/directories/replacements, re-proves canonical regular-file identity and exact size immediately before dispatch, and passes the resulting path only across the southbound Cua adapter boundary. No caller-selected host path is accepted or returned northbound. Public upload refs are principal/context/device-generation/capability-revision bound and atomically consumed before dispatch.

Download uses `browser_download_file` with a current click-capable page ref, a path-safe logical destination name, a mandatory byte ceiling, and an explicit overwrite flag. There is no northbound destination directory/path. The Agent creates a private per-operation download root and the adapter maps the exact `BrowserDownload` authorization to Cua's MCP-host approval bit. Cua writes only below that private root and returns an opaque download id plus byte count. The Agent then independently re-proves that the completed object is a direct regular file in the exact operation directory, rejects symlink/path escape and partial/oversized results, performs a bounded read, and returns a context-scoped result ref plus logical name, byte count, and bounded base64 bytes. The Cua/source URL, server filename, Cua download id, and host path do not become northbound authority.

The current Cua 0.19.3 semantic snapshot has no separate `download` action. CUMG therefore requires the exact dangerous `BrowserDownload` capability and a fresh ref carrying `click` authority; the adapter calls Cua `browser_download`, which independently proves that activation started and completed a download.

## Lifecycle and execution safety

- Upload/download refs are bound to the `InteractionContext`, stable device, Agent generation, capability revision, and ref kind.
- Upload refs are one-shot. Multi-ref consumption validates the complete set before removing any ref.
- Agent-private staging is removed after definite completion/failure and on context close, generation rollover, shutdown, or broker drop.
- A definite unsafe download completion is discarded rather than surfaced.
- Cancellation or provider timeout after dispatch remains `indeterminate`; private staging is quarantined until context teardown because the backend may still be reading/writing it.
- No transfer outcome weakens the existing durable quarantine, explicit-resolution, or no-auto-replay state machine.
- The per-file transfer ceiling is 16 MiB, upload sets are bounded to 32 files, and active/completed download staging is bounded to 32 entries per context. The 16 MiB payload plus base64/envelope headroom fits the reviewed 28 MiB large-message carrier.
- Logical download-name collisions fail unless overwrite is explicitly requested. Replacement of an existing logical result occurs only after the new result has been safely finalized.

## Automated acceptance

The transfer tests cover:

- upload happy path, safe basename/data bounds, missing/replaced/oversized files, directory/symlink escape, stale/cross-context/generation/revision handles, atomic one-shot public refs, invalid target/result shapes, backend failure, timeout, and cancellation;
- download happy path, unsafe name/id, collision/overwrite, stale/cross-context/generation/revision state, direct-file/symlink/directory proof, exact byte-count/partial rejection, oversize, backend failure, timeout, and cancellation;
- exact capability discovery and Cua advertisement;
- the reviewed large-message transport budget;
- Browser core, Desktop, execution-safety, V1 regression/conformance, docs, and backend-passthrough regressions through the repository gates.

The deterministic Cua-shaped fixture verifies both transfer directions and proves that cancellation/timeout is surfaced as indeterminate rather than an ordinary safe failure.

## Trusted-Mac real-Cua evidence on 2026-08-15

The installed Cua Driver 0.19.3 reported Accessibility and Screen Recording already granted. No permission bypass, unrestricted mode, existing-profile bypass, Cloudflare change, or production listener was used.

A disposable Google Chrome profile was launched with its own temporary user-data directory and DevTools endpoint against a loopback test page containing only a file input and a deterministic download link. Cua produced an exact native-window/browser binding and fresh semantic refs. The ignored, acknowledgement-gated `real_cua_browser_transfer_acceptance` test then exercised the actual `CuaMcpAdapter` and Agent-private staging boundary:

- a harmless staged text payload was assigned to the exact live upload ref through real `browser_set_input_files`;
- a deterministic `artifact.txt` was downloaded through the exact live click ref using real `browser_download`;
- the Agent-side broker revalidated the Cua result and the returned bytes exactly matched `deterministic-browser-download\n`;
- an intentionally stale download ref was refused by real Cua and did not become a completed transfer;
- the Cua interaction session and all temporary browser/server/staging state were explicitly cleaned up.

Transfer-specific cancellation and timeout are covered by the deterministic adapter fixture because forcing a live transfer into ambiguity is unnecessary for the physical success/fail-closed proof; the repository's existing trusted real-Cua cancellation acceptance separately proves that the same Cua MCP cancellation path enters durable CUMG quarantine.

## Closeout

Browser transfer is complete only while all of the following remain true:

- `BrowserUploadFile` and `BrowserDownload` are advertised only by adapters that implement these exact semantics;
- raw host paths, Cua refs/ids, CDP ids, source URLs, server filenames, and provider approval artifacts remain outside the northbound contract;
- no generic filesystem or raw-Cua fallback is introduced;
- ambiguous transfer execution remains indeterminate and non-replayable;
- V1 production and Cloudflare routing remain unchanged until the separate final V2 cutover decision.

PR #44 remains Draft and unmerged after this acceptance.
