# Deployment

V2 Hub + V2 Agent is the recommended deployment. The later V1 sections remain for `v1_gateway` regression/reference only.

Get the V2 trust/key path working with [`GETTING_STARTED.md`](GETTING_STARTED.md) before exposing either listener.

## V2-M1 deployment boundary

V2-M1 has a separate production candidate from the V1 loopback gateway. The accepted single-device topology is:

```text
MCP client
    |
    | HTTPS + deployment authentication
    v
external IdP / reviewed TLS reverse proxy / authenticated tunnel
    |
    | verified identity (provider credential stops at northbound authentication)
    v
v2_hub northbound MCP (default deployment: 127.0.0.1:8081)
    |
    | AuthenticatedClientPrincipal -> stable device -> exact DeviceCapability grant
    |
    | outbound-Agent gRPC bidi over TLS
    v
v2_agent
    +-- direct process / shell / bounded filesystem
    +-- optional Cua MCP GUI adapter
```

The Agent-facing gRPC listener is a separate Hub service port (example 7443). Agents connect outbound and authenticate again at the application layer with the enrolled Ed25519 device identity. A public deployment must restrict this port at the host/cloud firewall and apply the deployment's normal TCP/TLS connection controls. The in-process session limits begin after transport acceptance and are defense in depth, not a raw handshake-flood defense.

Northbound authentication and CUMG authorization are deliberately separate. The current packaged Hub validates OAuth bearer tokens through RFC 7662 introspection, then constructs `AuthenticatedClientPrincipal { issuer, subject }`. RFC 7662 is one adapter, not a requirement on the CUMG core. The packaged runtime also supports a reviewed trusted-proxy/tunnel fixed-principal adapter for explicitly single-principal deployments; generic OIDC/JWT validation remains the preferred future direction for multi-principal signed-token deployments. After that point, only `DeviceCapabilityAuthorizer` decides `principal -> device -> exact capability`.

OIDC/JWT validation does not require a CUMG user database merely to identify the caller: signature plus configured issuer/audience/time/subject claims establish the principal. Authorization data is separate. The current default authorization store is the integrity-protected `CUMG_V2_NORTHBOUND_POLICY_FILE`, loaded into an in-process exact-tuple policy. A future database or external policy engine may sit behind `DeviceCapabilityAuthorizer` without changing the execution-safety state machine.

For a trusted authenticated proxy/tunnel, constrain the Hub listener to loopback. An explicitly single-principal deployment sets `CUMG_V2_TRUSTED_PROXY_ISSUER`, `CUMG_V2_TRUSTED_PROXY_SUBJECT`, and the required `CUMG_V2_TRUSTED_PROXY_SECRET_FILE`; these are mutually exclusive with the OAuth introspection settings. Generate a random header-safe secret of at least 32 bytes (for example 32 random bytes encoded as hex), store it as a private regular file, and provision the same value independently to the reviewed local proxy. The proxy **must overwrite**, not append or pass through, `X-CUMG-Trusted-Proxy-Token` on every request it forwards to the loopback Hub. The Hub validates and strips that token before global request admission, so an unrelated local process cannot consume the normal northbound concurrency/rate budget merely by reaching `127.0.0.1`.

After the local trust gate, trusted-proxy traffic has a separate peer ceiling: `CUMG_V2_TRUSTED_PROXY_MAX_PEER_CONCURRENCY` defaults to `4` and `CUMG_V2_TRUSTED_PROXY_MAX_PEER_REQUESTS_PER_MINUTE` defaults to `60`. Both must remain below the global defaults (`16` and `120`) so global headroom is preserved. Peer concurrency rejection is HTTP 503 and peer rate rejection is HTTP 429. The peer key is the verified loopback source IP; it is overload isolation, not user identity. The fixed CUMG principal still comes only from operator configuration, and caller `clientInfo` remains audit-only. The adapter strips common Authorization/Cloudflare identity headers before MCP dispatch. If the deployment needs per-user CUMG policy, use a signed-token/OIDC-style adapter that conveys a tamper-resistant authenticated identity. Never trust a caller-provided `X-User`/similar header merely because the listener is called a proxy mode.

### First-class Handoff runtime and local operator control

Handoff is a **first-class but optional** CUMG integration. A deployment that does not configure the Agent Handoff runtime continues to support its ordinary authorized process/shell and GUI capabilities without Handoff. The three managed runtime settings below are therefore all-or-none: omitting all of them disables the optional Handoff runtime; configuring only a subset is rejected. When enabled, Handoff becomes authoritative at the Agent/Human control boundary rather than a best-effort external helper: CUMG admission and the Agent-local final gate must honor suspended Human authority, and there is no fallback that bypasses Handoff merely because its runtime or transport is unavailable. This optionality is a deployment/capability choice, not permission to weaken authority fencing after Handoff has been enabled.

The normal OS-Window Handoff runtime is Agent-owned because capture, Human input, TCC/Accessibility, WebRTC, TURN, and the exact controllable surface belong to the controlled device. Configure `CUMG_V2_HANDOFF_RUNTIME_COMMAND`, `CUMG_V2_HANDOFF_RUNTIME_SCRIPT`, and `CUMG_V2_HANDOFF_RUNTIME_ENV_FILE` on `v2_agent` together. The command and script must be absolute regular files. The env file must be an absolute private regular file, must not be a symlink, and is passed only to the Agent-local managed Node Handoff child after the Agent clears the child environment. Handoff-only transport credentials such as TURN configuration therefore do not become Hub process environment variables. `CUMG_V2_HANDOFF_RUNTIME_TIMEOUT_SECS` bounds each Agent-local stdio control exchange. Supplying these managed-runtime variables to `v2_hub` is rejected; the legacy `CUMG_V2_OPERATOR_HANDOFF_SOCKET` remains compatibility/regression-only.

For live operator lifecycle control, configure the Unix-only `CUMG_V2_HANDOFF_CONTROL_SOCKET` on the Hub in a private, non-symlink directory that is not group/world writable. The Hub creates the socket with mode `0600`; the directory/socket filesystem ownership is the authorization boundary. This endpoint is **not MCP** and is never added to normal tool discovery. The Hub is only a signed/session-fenced relay for the Agent-owned Handoff coordinator; it does not run a second Handoff state machine. The closed commands are `status`, `begin`, `recover_reissue`, `recover_rebind`, `rebind_live`, `abandon_expired_recovery`, `request_resume`, and `cancel_before_human`. The local caller cannot supply principal, device, PID, window ID, generation, or capability-revision authority. `begin` uses only the last still-valid exact Window admitted by CUMG. After an authenticated Agent generation rollover, `rebind_live` requires a fresh CUMG observation of the same exact OS Window and the Agent-local runtime itself proves continuity from the prior binding; it never restores Agent authority. Recovery remains explicit and uses fresh CUMG observation plus the prior-owner proof required by the signed Handoff checkpoint.

Invoke `v2_handoff_ctl` directly from the trusted local operator environment. Do not tunnel lifecycle control through CUMG's northbound `shell` or `execute_process`: that caller is itself a tracked Agent operation and must remain subject to the Agent idle fence. If another operation is still active, the local control plane returns `handoff_agent_not_idle`; wait for a durable terminal result before making a new explicit operator request. If that operation instead becomes indeterminate, reconcile the CUMG quarantine first; `begin` returns `handoff_device_quarantined` and never opens Human control across that fence. CUMG never retries or replays the Handoff transition automatically.

The paired local CLI uses the same narrow CUMG operator protocol:

```bash
export CUMG_V2_HANDOFF_CONTROL_SOCKET=/run/user/$(id -u)/cumg-v2/handoff-control.sock
v2_handoff_ctl status
v2_handoff_ctl begin
# after an authenticated Agent generation rollover, first make a fresh exact-Window observation:
v2_handoff_ctl rebind-live
# after Human Done and fresh Agent-local CUMG verification reaches ready_to_resume:
v2_handoff_ctl request-resume
```

For restart/context/generation recovery, use `recover-reissue` only for a non-expired checkpoint with the same owner proof. A recovered `human_active` checkpoint uses a deliberately two-phase `recover-rebind`: first make a current exact-Window request so CUMG has a fresh session-bound target selection, then call `recover-rebind --prior-context-id ctx_...` (plus `--prior-generation` / `--prior-capability-revision` when rollover requires them). If the signed prior-owner digest matches, that first call **only** arms a 60-second evidence lease; recovery remains authoritative and all ordinary Agent Window commands remain denied. Within that lease, run an exact `verify_ui_state` for the selected process/window with `include_screenshot=false` and a `window_exists: true` predicate. Only a satisfied verification may mark the lease observed. Repeating the same `recover-rebind` proof can then reissue the Human intervention into `awaiting_human`. Wrong prior proof, wrong principal/device/window, stale lease, unsatisfied verification, or any non-verification Window command stays fail-closed. The evidence lease is ephemeral and never written to the signed checkpoint. Neither recovery command restores Agent authority, resumes, or replays the old Agent operation.

If an **expired** recovery can no longer be rebound because the prior exact surface no longer exists, the local operator may explicitly abandon only that Handoff recovery tombstone with `abandon-expired-recovery --expected-epoch N`. The Agent accepts this only when recovery is expired, no intervention is active, the runtime is not faulted, and the epoch exactly matches. Durable checkpoint deletion must succeed before the recovery lock is released. Abandonment does **not** mark the prior semantic action successful, does not replay or resume it, and does not alter the CUMG operation ledger, quarantine, or recovery evidence. The operator control response may contain the current Human takeover locator; treat it as local capability material and do not place it in logs, issues, shell history, or generic diagnostics.

### Authenticated Agent session lifetime

`v2_hub` bounds every authenticated Agent transport with `CUMG_V2_MAX_AGENT_SESSION_LIFETIME_SECS` (default `3600`). `CUMG_V2_AGENT_SESSION_REAUTH_DRAIN_SECS` (default `30`) reserves the final part of that lifetime for a controlled reauthentication drain. The drain value must be non-zero and strictly smaller than the hard lifetime.

When the reauthentication window begins, the Hub emits `v2_agent_session_reauth_requested` / `cumg.v2.agent_session_reauth_requested`, pauses **new** operation admission for that session, and lets already-admitted work settle. If the pending set drains before the hard deadline, the Hub closes the stream normally; the Agent's existing reconnect lifecycle performs a fresh hello/challenge/proof/accepted handshake and advances to a new generation. This normal path does not create a quarantine.

The hard lifetime is not advisory. If already-dispatched work is still unsettled when the deadline arrives, the Hub emits the high-visibility `v2_agent_session_lifetime_exceeded` event plus `cumg.v2.agent_session_lifetime_exceeded` and closes the transport. Existing execution-safety cleanup then fails closed: work whose side effect cannot be proven terminal may become `Indeterminate` and quarantine exactly as with any other connection loss. Increasing the lifetime or drain window must never be used to auto-replay or clear such ambiguity.

### Planned Hub shutdown and restart

`v2_hub` treats `SIGINT`, `SIGTERM`, and `SIGHUP` as planned shutdown signals. On the first signal it closes the operation-admission gate, keeps the Agent transport alive, and waits up to `CUMG_V2_DRAIN_TIMEOUT_SECS` (default `30`) for work that was already admitted to reach a durable terminal or indeterminate state. Requests that have not crossed the dispatch boundary are rejected/cancelled rather than starting new side effects during the drain.

After the drain, the Hub requests closure of the live Agent session and only then signals the gRPC, northbound HTTP, and private Handoff-control servers to stop. The Agent owns managed Handoff-runtime shutdown: loss or shutdown of that process revokes the live Human transport while preserving its signed checkpoint for explicit recovery; it never auto-restores Agent authority. If the bounded drain timeout expires, the same ordering continues with the existing fail-closed restart behavior: any work that had crossed the dispatch boundary without terminal proof remains eligible for `Indeterminate` + quarantine on restart. The timeout never authorizes replay, clears ambiguity, or automatically restarts Handoff authority.

Configure the service manager's stop/kill timeout to be **longer** than `CUMG_V2_DRAIN_TIMEOUT_SECS`; otherwise the supervisor can kill the Hub before its own bounded drain completes. The packaged systemd unit uses `TimeoutStopSec=45s` for the default 30-second drain. Apply the same ordering to operator-maintained launchd or other service-manager definitions.

### Read-only quarantine inspection

When a caller receives `device_indeterminate`, inspect the durable checkpoint before choosing any recovery action:

```bash
v2_maint inspect-quarantine \
  --state-dir /var/lib/cumg-v2/hub
```

Use `--device-id DEVICE_ID` to restrict output in a multi-device state directory. Inspection reads only the latest atomically committed checkpoint and does **not** take the Hub's exclusive maintenance lock, so it may run while the Hub is serving. It never resolves quarantine, signs recovery, dispatches device work, or rewrites the checkpoint. The output is a point-in-time durable view; a later normal Hub checkpoint may advance after the inspection returns.

Each unresolved entry identifies the exact `blocking_operation_id`, stable device/generation, capability/semantic operation class, conservative `read_only` or `effectful` class, bounded workflow/client correlation labels when supplied, fingerprint presence, bounded text-input evidence shape when present, dispatch-binding presence (never its value), target/effect/verification classes, durable dispatch marker/timestamps, indeterminate timestamp/reason, available evidence class, `evidence_status`, explicit `reconciliation_status` (`auto_reconciling`, `operator_required`, or `unrecoverable_evidence_gap`), `manual_audit_required`, and a bounded recovery disposition. It deliberately omits authenticated owner issuer/subject, the request fingerprint/key value, and every raw command, argv, cwd, environment value, typed text, URL, clipboard value, screenshot, backend identifier, result payload, credential, or secret. Correlation labels are audit-only and do not authenticate the caller or authorize recovery. The state-directory filesystem permissions are the authorization boundary for this local operator surface; no equivalent cross-principal northbound inspection endpoint is exposed.

A northbound tool request rejected by an existing quarantine returns `code=device_indeterminate`, `blocking_operation_id=op_...`, and `retry_safe=false`. `blocking_operation_id` always names the **earlier ambiguous operation that is already quarantining the device**, not a newly generated ID for the rejected request. Do not replay that operation. `confirmed_not_executed` is valid only with independent evidence that no side effect occurred; `confirmed_completed` is valid only with independent evidence that the intended side effect completed; otherwise leave quarantine intact.

For text-input ambiguity, `confirmed_effect_applied_uncommitted` is a third explicit offline decision: use it only when independent evidence proves the input was delivered but a distinct submit/commit action did not occur. It is not equivalent to `confirmed_not_executed`, and it never makes the old operation retry-safe. `v2_maint resolve --decision confirmed_effect_applied_uncommitted` rejects capabilities outside the bounded text-input set. Execution-safety schema v6 persists this distinction and refuses downgrade to v5 when such a record exists.

For shell/process/text-input deployments that want privacy-preserving candidate matching, provision one private file of at least 32 bytes and configure the Hub with `CUMG_V2_AUDIT_FINGERPRINT_SECRET_FILE`. The file must be readable only by the Hub/operator account and must not be committed or copied into logs. The Hub HMACs the canonical shell/process request or typed-text payload before dispatch; no raw request or typed text is persisted for this purpose. `type_text` also persists its bounded shape envelope even when the HMAC key is not configured; in that case candidate equality is unavailable. To compare a locally-held candidate request, place that candidate JSON in a private file and run:

```bash
v2_maint compare-quarantine-request \
  --state-dir /var/lib/cumg-v2/hub \
  --operation-id op_... \
  --tool shell \
  --request-file /secure/tmp/candidate.json \
  --fingerprint-secret-file /secure/path/audit-fingerprint.key
```

For a quarantined `type_text` operation, use the same command with `--tool type_text`; the private candidate JSON contains only `{"text":"..."}` plus optional audit/operation correlation fields, which are ignored for payload comparison.

Execution-safety schema v7 persists the text-input evidence envelope and rejects downgrade to v6 when an envelope exists. The envelope inspection reports only its versioned shape and whether a fingerprint is present; it never prints typed text or the HMAC value.

The command prints only `"same_request"`, `"different_request"`, or `"unavailable"`; it does not print the candidate, stored fingerprint, key identifier, or key. A different/rotated key intentionally yields `unavailable`, not `different_request`. Matching proves only request correlation. It never proves completion, clears quarantine, changes retry safety, or authorizes replay. Arbitrary shell text is never parsed to infer idempotency or postconditions. Remove the temporary candidate file according to the deployment's sensitive-file handling policy after use.

### Authoritative self-reconciliation after reconnect/restart

Execution-safety schema v4 can settle a narrow subset of `Indeterminate` operations automatically, but **it never retries or replays them**. The Hub persists an effectful operation's exact dispatch binding before the Agent-visible command is sent: stable device ID, original generation, exact capability, capability revision, and the one-shot grant identifier. The identifier is an opaque one-shot correlation fence, not a bearer grant/token, and normal inspection never prints it.

The Agent keeps a maximum of 64 payload-free terminal-evidence entries. An entry is created only after the normal execution path has reached a definite terminal result already accepted by CUMG's existing evidence contract. Raw command/argv/cwd/env/stdout/stderr/result/credential material is never copied into this journal. On each newly authenticated generation the Agent signs the complete bounded journal against the fresh Hub/Agent nonces and sends it before accepting new work.

For an existing transient quarantine, inspection initially shows `reconciliation_status=auto_reconciling`. The Hub may change it to `auto_resolved` only when the signed report exactly matches the quarantined operation's stable device, original generation, operation ID, capability revision, exact capability, and dispatch fence and uses a supported authoritative terminal evidence/state pair. The Hub first commits a candidate terminal checkpoint and only then swaps the live controller/clears quarantine. A checkpoint failure leaves the live quarantine unchanged. Duplicate evidence for an already-terminal operation is ignored idempotently; it does not create a second settlement.

If the complete Agent journal has no exact proof for an auto-reconciling quarantine, the status becomes `unrecoverable_evidence_gap` and quarantine remains. A signed but stale/wrong/mismatched claim becomes `operator_required` or fails closed. Backend-provided ambiguity, cancellation without terminal proof, correlation/fingerprint equality, shell-text heuristics, and observational postconditions do not qualify as self-reconciliation evidence. In those cases use the offline operator procedure below only after independent evidence is available.

To inspect bounded automatic-resolution history without owner principal, raw payload, fingerprint/key, or dispatch-fence values:

```bash
v2_maint inspect-reconciliation-history \
  --state-dir /var/lib/cumg-v2/hub
```

Add `--device-id DEVICE_ID` to filter by stable device. Auto-resolution history and retired-indeterminate history are each bounded to 64 entries and report only safe fields such as operation/device/generation, capability, terminal/evidence or retirement class, resolution/retirement timestamp, and `replayed=false`.

This feature requires capability schema v5 on both Hub and Agent. Upgrade the pair together. Mixed old/new peers fail the capability-schema handshake closed rather than attempting a partially compatible session. Execution-safety checkpoint restore remains compatible with schemas v1/v2/v3/v4/v5 when those checkpoints stay within their representational limits; downgrading a state that already contains v4 dispatch/reconciliation metadata, v5 retirement state, or v6 partial-input resolution state is intentionally rejected.

### Unknown-outcome retirement for permanently unknowable legacy ambiguity

Execution-safety schema v5 adds a separate retirement path for a narrow class of legacy `Indeterminate` operations whose historical outcome can no longer be established truthfully. This is **not** a resolution and does not mean completed, failed, cancelled, or not-executed. `inspect-quarantine` reports `execution_outcome=indeterminate`, the current durable device generation, `retirement_eligibility`, the reviewed `retirement_policy`, and a bounded `recommended_action`. The initial policy allows only `scroll` and pointer movement; every other capability, including shell/process and other higher-impact effects, remains ineligible by default. Eligibility also requires a recorded dispatch, `operator_required` or `unrecoverable_evidence_gap`, and a durable device generation strictly newer than the original operation generation.

If and only if inspection reports `retirement_eligibility=eligible`, install `v2_hub` and `v2_maint` from the same reviewed schema-v5 build, stop `v2_hub` completely, and use the exclusive offline maintenance path:

```bash
v2_maint retire-indeterminate \
  --state-dir /var/lib/cumg-v2/hub \
  --operation-id op_... \
  --policy transient_ui_interaction_v1 \
  --reason "legacy transient UI outcome permanently unknowable; retired without replay"
```

The policy must be named explicitly so an existing runbook can never opt into a future retirement policy merely because the binary gained one. The reason is bounded audit metadata; never include raw commands, results, desktop content, URLs, credentials, tokens, or secrets. A successful retirement appends a schema-v5 execution checkpoint before the prior checkpoint stops being authoritative. The operation itself remains `Indeterminate` with no terminal receipt, its exact operation ID remains permanently non-replayable, and the retirement record preserves `outcome=unknown`, original/authorizing generations, capability/policy, prior reconciliation state, local-maintenance authority, timestamp, and `replayed=false`. Only the device quarantine is removed. The old command is never reconstructed, re-signed, resumed, retried, or dispatched. Any later requested work uses a fresh operation ID and ordinary authorization/admission.

Retirement intentionally creates state that execution-safety schema v4 cannot represent. After a successful retirement, do not roll back to an older Hub binary while keeping the new checkpoint. A binary rollback requires restoring the pre-retirement checkpoint as well, which also restores the original quarantine. A failed checkpoint publication leaves the previous quarantined checkpoint authoritative. `inspect-reconciliation-history` includes a privacy-bounded `retired_indeterminate` history entry and never prints the operator reason text.

Do not use retirement as a substitute for independent evidence. When authoritative/independent evidence proves a terminal outcome, use self-reconciliation or explicit `confirmed_completed` / `confirmed_not_executed` resolution instead. Retirement exists only for the distinct case where the outcome remains permanently unknown but the reviewed low-impact operation can be abandoned without replay. The `Scroll`/`MovePointer` allowlist is a risk policy, not a claim that every application treats those inputs as side-effect-free or idempotent. An application may attach its own state changes to input events; authorizing retirement explicitly accepts that historical uncertainty. The strictly newer Agent generation invalidates prior interaction contexts/scoped refs, and quarantine has already cancelled queued pre-ambiguity work, so later effectful work must be newly admitted rather than continuing the old operation chain.

### Offline quarantine resolution

Reconnect or restart **alone** never clears a durable `Indeterminate` quarantine. If schema-v4 authoritative self-reconciliation did not produce an exact terminal proof (`operator_required` or `unrecoverable_evidence_gap`), then after an operator establishes independent evidence for the exact ambiguous operation, stop `v2_hub` completely and use the offline maintenance CLI instead of editing checkpoint JSON:

```bash
v2_maint resolve \
  --state-dir /var/lib/cumg-v2/hub \
  --operation-id op_... \
  --decision confirmed_not_executed \
  --evidence "ticket-1234: operator verified no side effect"
```

`confirmed_completed` is also available when the side effect is positively confirmed. Evidence is required and remains subject to `MAX_RESOLUTION_EVIDENCE_BYTES`; keep it to bounded audit metadata and never place commands, results, desktop content, credentials, tokens, or secrets in it.

The Hub and maintenance CLI take the same exclusive state-directory lock. `v2_maint` therefore fails closed while any `SingleDeviceHub` instance still owns that state directory. Before applying the in-memory resolution transition, maintenance verifies that the restored execution state can still be represented by the execution-safety schema of the authoritative input checkpoint. It validates the post-resolution candidate again before publishing bytes. Maintenance preserves the input checkpoint's outer state schema, registry snapshot, and execution-safety writer contract instead of silently upgrading durable state while the intended Hub is offline. If the source writer contract cannot represent the candidate state, recovery fails before checkpoint publication with a bounded persistence-compatibility error; do not edit the checkpoint or force a downgrade. Use a `v2_maint` paired with the deployed Hub release, or upgrade the Hub through the documented compatibility path before retrying recovery.

A successful resolution invokes the existing authoritative `resolve_indeterminate` transition and appends a new checkpoint through the same private-pending-file/fsync/atomic-publication persistence path. The resulting `ResolutionRecord` remains durable even after terminal operation tombstones are pruned on later generations. Restart the Hub only after the CLI exits successfully. For packaged deployments, install `v2_hub` and `v2_maint` from the same reviewed build/release artifact. Running `cargo run --bin v2_maint` from a newer checkout against state owned by an older deployed Hub is not a supported shortcut unless that checkout is the exact source used for the deployed Hub.

### Initial Agent enrollment

The production runtime does not expose unauthenticated or mutable network enrollment. Prepare each fresh fixed Agent offline with `v2_keyctl prepare-agent-enrollment` from a protected directory:

```bash
v2_keyctl prepare-agent-enrollment \
  --output-dir /secure/cumg-enroll/desktop-01 \
  --hub-public /secure/cumg-trust/hub.pub \
  --grant-public /secure/cumg-trust/grant.pub \
  --tls-root-der /secure/cumg-trust/tls-root.der
```

The command refuses an existing output directory, requires a non-group/world-writable parent on Unix, validates that the TLS root is a currently-valid X.509 DER certificate, and creates:

- `agent/secrets/device.key` — new private device identity;
- `agent/trust/hub.pub`, `agent/trust/grant.pub`, `agent/trust/tls-root.der` — normalized Agent trust inputs;
- `hub/device.pub` — the public key to register as `CUMG_V2_DEVICE_PUBLIC_KEY_FILE`;
- `enrollment.json` — non-secret relative paths plus the stable `device_id`.

Transfer `agent/` over an authenticated operator channel and preserve the device-secret permissions. Register `hub/device.pub` on the intended Hub and copy the manifest `device_id` into the Agent configuration before starting either side. The CLI never prints private key bytes. An existing Hub checkpoint for a different device intentionally fails trust matching; this command is not a hot-swap/discovery mechanism. Use signed device-key rotation for the same logical device or a separate fixed Hub/device entry for another device.

### Linux Hub

Use `packaging/systemd/cumg-v2-hub.service`, `packaging/systemd/cumg-v2-grant-signer.service`, and `packaging/systemd/hub.env.example` as templates. Install the operator maintenance binary `v2_maint` from the same reviewed build/release artifact as `/usr/local/bin/v2_hub`; offline recovery relies on the durable-state compatibility contract shared by that pair. Production packaging deliberately separates grant-key custody from the Hub process: the Hub unit receives the Hub Ed25519 key and ACME TLS key, while the dedicated signer unit receives the grant Ed25519 key. Provision them independently outside the repository:

```bash
sudo systemd-creds encrypt --name=hub-secret \
  /secure/admin/hub.key /etc/credstore.encrypted/hub-secret
sudo systemd-creds encrypt --name=grant-secret \
  /secure/admin/grant.key /etc/credstore.encrypted/grant-secret
```

Create the signer service account (example `cumg-v2-signer`) with primary group `cumg-v2`, install `packaging/systemd/grant-signer-policy.example.json` as `/etc/cumg-v2/policy/grant-signer.json`, replace its device ID/capability allowlist, and keep the policy non-group/other-writable. The signer owns its runtime directory and exposes a mode-0660 Unix socket to the Hub group. The Hub unit itself contains neither `LoadCredentialEncrypted=grant-secret` nor `CUMG_V2_GRANT_SECRET_FILE`; it pins `/etc/cumg-v2/trust/grant.pub` and talks to the signer socket. Keep recovery/rotation copies in the operator's normal secret manager, not the checkout.

The signer is not a raw signing oracle: requests are bounded typed grant fields; the signer generates the grant ID/canonical payload itself and independently checks exact device capability, short TTL, and bounded issue-time skew. External mode has no local fallback. See [`v2/V2_GRANT_SIGNING.md`](v2/V2_GRANT_SIGNING.md) for the protocol, failure semantics, and residual risk. A consciously single-host/development deployment may instead configure only `CUMG_V2_GRANT_SECRET_FILE`; do not combine it with external signer variables.

For northbound OAuth introspection, use the optional encrypted-credential drop-in in `packaging/systemd/cumg-v2-hub-oauth-credential.conf.example` rather than putting the client secret in `hub.env`. For trusted-proxy mode, use `packaging/systemd/cumg-v2-hub-trusted-proxy-credential.conf.example`; provision the same random secret separately to the proxy/tunnel and never place the value itself in `hub.env`.


### TLS renewal

Keep certificate issuance/renewal with the deployment's ACME client. Do not point `v2_hub` directly at a symlinked ACME `live/` private key because the Hub secret loader intentionally rejects symlinks. Configure the ACME deploy hook to run:

```bash
scripts/v2-install-renewed-tls.sh   ACME_CERT_PEM ACME_KEY_PEM   /etc/cumg-v2/tls/server.pem /etc/cumg-v2/tls/server.key
sudo systemctl try-restart cumg-v2-hub.service
```

The hook validates that the certificate and private key parse and match before same-directory atomic replacement. The deployed key is mode 0600. Application Hub/device/grant identity rotation is independent; follow `packaging/README.md` and use `v2_keyctl` for continuity documents.

Install `v2_tls_check` beside the Hub and enable `packaging/systemd/cumg-v2-tls-expiry.service` plus `.timer`. The timer runs daily and uses a 30-day warning window. Healthy checks emit `CUMG_TLS_EXPIRY_OK`; an expiring, expired, not-yet-valid, malformed, unsafe-path, or writable certificate emits `CUMG_TLS_EXPIRY_ALERT` and fails the oneshot service. Alert on either the failed unit or that marker in the journal. The checker does not renew or replace trust automatically.

#### Private pinned-root compromise

A suspected TLS server-private-key leak is not an ordinary renewal event. Without CRL/OCSP enforcement, an Agent that retains the old private root can still validate the old leaf until its validity ends. The independent Ed25519 Hub authentication means possession of the TLS key alone is **not** CUMG command authority, but the TLS confidentiality boundary is compromised. For the private pinned-root model, perform a maintenance cutover:

1. Stop affected Agents so no operation straddles the trust change.
2. Create a replacement private root and server certificate/key using the deployment's protected PKI process; validate the chain and key before staging.
3. Install the replacement regular-file server certificate/key on the Hub and transfer the replacement DER root to every Agent over the authenticated provisioning channel. Keep the existing Hub/device/grant Ed25519 identities unchanged.
4. Start the Hub, then start the Agents. Verify a fresh authenticated Agent generation and normal command execution.
5. Remove/archive the compromised TLS material according to the operator's incident procedure and confirm `v2_tls_check` is healthy on both server certificate and Agent root.

The TLS regression `v2_m1_tls::tests::private_root_compromise_cutover_requires_agent_trust_reprovisioning` proves that an Agent pinned to the old root rejects the replacement chain until its trust root is explicitly reprovisioned, and accepts the replacement afterward. CUMG intentionally does not invent CRL/OCSP or silently broaden trust to make this cutover seamless.

### Linux Agent

Install `packaging/systemd/cumg-v2-agent.service` as a **user service** and customize `packaging/systemd/agent.env.example` outside the repository. The template intentionally avoids a filesystem namespace that would silently change the explicitly configured process/filesystem capability semantics. Store the device secret as a regular 0600 file and keep Hub/grant/TLS trust anchors non-group/other-writable.

Install `packaging/systemd/cumg-v2-agent-tls-expiry.service` and `.timer` in the same user systemd scope. It reads `CUMG_V2_TLS_ROOT_DER_FILE` from the Agent environment file and checks that pinned root daily. Treat a failed oneshot / `CUMG_TLS_EXPIRY_ALERT` journal entry as operator-action-required; it is never permission to replace the trust anchor automatically.

### macOS Agent

Customize `packaging/launchd/com.github.git-ksk.cumg-v2-agent.plist`, replacing `@BINARY@` and `@HOME@`, then install it as a user LaunchAgent. Cua-backed GUI automation must run in the logged-in user session so Accessibility/Screen Recording TCC attribution remains explicit. Secret/trust files live outside the repository under the user's Application Support tree with restrictive permissions. Keep `CUMG_V2_CUA_BACKEND_VERSION` pinned to the exact reviewed Cua version; a concrete value is checked against the MCP handshake on initial connection and reconnect.

Install `packaging/launchd/com.github.git-ksk.cumg-v2-tls-expiry.plist` alongside it after replacing `@BINARY@` with `v2_tls_check` and `@HOME@`. It checks the DER trust root at load and every 24 hours, writing the same `CUMG_TLS_EXPIRY_OK` / `CUMG_TLS_EXPIRY_ALERT` markers to the configured log files. Route the alert log/non-zero job status into the operator monitoring used on that Mac.

For an existing V1 production endpoint moving to V2, follow the guarded [`V2 production cutover runbook`](v2/V2_PRODUCTION_CUTOVER.md). Do not treat a successful local V2 start as permission to stop V1.

### Overload and observability

The Hub defaults to bounded Agent sessions/session starts and bounded northbound MCP request concurrency/rate. Excess Agent sessions use gRPC `RESOURCE_EXHAUSTED`; excess northbound requests use HTTP 429 or 503. Keep external firewall/reverse-proxy limits as the outer control.

OTLP is opt-in through standard OpenTelemetry variables. `OTEL_EXPORTER_OTLP_ENDPOINT` enables traces and metrics; `OTEL_EXPORTER_OTLP_TRACES_ENDPOINT` and `OTEL_EXPORTER_OTLP_METRICS_ENDPOINT` enable individual signals; `OTEL_SDK_DISABLED=true` disables export. The packaged build uses the standard OTLP `grpc` transport. Default telemetry intentionally excludes command/result bodies, argv, stdout/stderr, file contents, screenshots, clipboard data, bearer tokens, OAuth introspection secrets, grants, signatures and private key material. Protocol failures log only a message kind plus safe error metadata, never the full signed protocol object.

V2 structured events use control-plane correlation fields only when available. The principal incident keys are `operation_id`, `device_id` and `generation`; `capability`, `outcome`, `error_code`, `indeterminate_reason`, `reconnect_attempt` and `backend` add bounded diagnostic context. Authenticated principal issuer/subject is not emitted by default. Northbound audit events additionally record bounded MCP `clientInfo` name/version/description as `client_name`, `client_version`, and `client_description`. These values are caller-supplied **audit metadata only** (`identity_source=mcp_client_info_untrusted`): they never select the authenticated principal, authorize a capability, change operation ownership, or cross the Hub/Agent trust boundary. `v2_northbound_operation_requested` carries the same `operation_id` used by downstream Hub execution events so tooling/human callers can be correlated without treating their claimed client name as identity. The main event families cover Agent session start/accept/supersede/end/reconnect/exhaustion, northbound client initialization/request correlation, operation admission/dispatch/terminal failure or completion, cancellation request/acknowledgement, indeterminate/quarantine/manual resolution/auto reconciliation, persistence failure, authorization failure, overload rejection, backend ambiguity/timeout and stale result/session rejection.

OTel counters intentionally expose only closed, low-cardinality attribute domains:

- `cumg.v2.agent_session_started`;
- `cumg.v2.agent_session_rejected{reason}`;
- `cumg.v2.reconnect_attempt` and `cumg.v2.reconnect_exhausted`;
- `cumg.v2.operation_completed{capability,outcome}`;
- `cumg.v2.operation_indeterminate{reason}`;
- `cumg.v2.quarantine_created` and `cumg.v2.quarantine_resolved`;
- `cumg.v2.persistence_failure{component}`;
- `cumg.v2.auth_failure{reason}`;
- `cumg.v2.backend_failure{reason}`;
- `cumg.v2.stale_result_rejected`;
- `cumg.v2.northbound_request_rejected{reason}`.

Never add `operation_id`, `device_id`, principal/subject, request path, command/tool name or other unbounded values as metric attributes. Those belong in structured logs/traces only when required for incident correlation. Collector, proxy and service-manager logging must preserve the same payload-free boundary; do not enable HTTP/gRPC body capture or Authorization-header logging around the process.

#### Quarantine alerting recipe

Every newly-created quarantine emits a dedicated `ERROR`-level `v2_quarantine_created` event with bounded correlation fields (`operation_id`, `device_id`, `generation`, capability, and indeterminate reason) in addition to incrementing `cumg.v2.quarantine_created`. Treat either signal as operator-action-required; neither signal changes or resolves the quarantine. On systemd, a minimal local watch is:

```bash
journalctl -u cumg-v2-hub.service --priority=err --grep=v2_quarantine_created --follow
```

For OTLP-backed monitoring, alert whenever the **increase/delta of `cumg.v2.quarantine_created` is greater than zero** over the collector's shortest reliable alert window (for example one to five minutes), and page or otherwise notify the operator responsible for the device. Metric exporters may translate the meter name to backend-specific syntax; alert on the exported counter corresponding to this exact OpenTelemetry meter rather than adding `operation_id` or `device_id` labels. Use the paired `v2_quarantine_created` error event to recover those incident identifiers, then follow the offline resolution procedure above. Clear the operational alert only after a durable terminal settlement exists: either explicit operator resolution (`v2_quarantine_resolved` / `cumg.v2.quarantine_resolved`) or an `v2_operation_auto_resolved` event for the exact quarantined operation. Reconnect or process restart alone is not resolution; `operator_required` and `unrecoverable_evidence_gap` remain alerting conditions.

The Hub also bounds same-generation checkpoint growth. After a successful checkpoint reaches `CUMG_V2_CHECKPOINT_GENERATION_ROLLOVER_BYTES` (default `524288`, at most half of the 1 MiB checkpoint ceiling), the Hub pauses new operation admission, lets already-admitted work settle, then closes the authenticated Agent session cleanly. The Agent reconnects with a fresh generation, and the existing generation fence makes prior signed commands stale before the Hub prunes old terminal replay/receipt records. `Indeterminate` operations and quarantine are never pruned by this rollover; a quarantined device therefore remains quarantined across every generation. This is a reliability compaction boundary, not permission to replay or forget ambiguity.

Checkpoint publication is atomic from the loader's point of view. Hub and Agent first write a private same-directory pending file, flush and fsync that complete file, then publish the sequenced final name with a no-clobber atomic link and fsync the state directory. `load_latest()` and retention discovery recognize only the exact sequenced final-name grammar, so an ENOSPC/I/O failure before publication—or a crash-leftover pending file—cannot supersede the last committed checkpoint. Pending leftovers are ignored rather than treated as fallback candidates or deleted opportunistically across processes. A malformed **committed** checkpoint still fails closed; this protocol does not add silent fallback to older committed state.

For an incident, correlate Hub and Agent by `device_id` + `generation`, then follow `operation_id`. A `v2_operation_indeterminate` event must be followed by durable quarantine until either a persistence-gated manual resolution or an exact authoritative `v2_operation_auto_resolved` settlement exists for that operation. Persistence failures expose a safe `error_code` such as `persistence_checkpoint_too_large` without a path or serialized checkpoint. Reconnect exhaustion and heartbeat timeouts are visible independently from TLS/transport connection failures. OAuth introspection unavailability is distinct from authorization denial, and a quarantine admission rejection remains `device_indeterminate` rather than being retried or auto-replayed.

See [`V2_M1_ACCEPTANCE.md`](v2/acceptance/V2_M1_ACCEPTANCE.md) for the final security gate and [`../packaging/README.md`](../packaging/README.md) for lifecycle details.

## V1 legacy/reference topology

```text
Remote MCP client
    |
    | HTTPS + authentication
    v
trusted reverse proxy / tunnel
    |
    | loopback HTTP
    v
127.0.0.1:8100/mcp
    |
    | MCP stdio
    v
cua-driver mcp
```

The gateway does not implement public authentication or TLS termination in V1. Do not expose it directly to the public internet.

## Preflight

Before adding a tunnel, all of these should work on the computer being controlled:

```bash
cua-driver --version
cua-driver doctor
cua-driver call list_apps
curl --fail http://127.0.0.1:8100/healthz
```

A local MCP client should also be able to connect to:

```text
http://127.0.0.1:8100/mcp
```

If not, stop here and use [`TROUBLESHOOTING.md`](TROUBLESHOOTING.md).

## Tool policy before remote exposure

The gateway is deny-by-default:

- empty `CUMG_ALLOW_TOOLS` exposes no tools;
- a comma-separated allowlist exposes only matching discovered tools;
- `CUMG_DENY_TOOLS` always overrides the allowlist;
- `CUMG_ALLOW_TOOLS=*` explicitly exposes every discovered backend tool.

Do not use `*` as a remote-deployment shortcut. Review exact computer-use capabilities first. Read-only tools can still disclose sensitive desktop data.

`examples/cua-policy.yaml` provides an optional second policy layer inside Cua for argument-aware restrictions.

## Backend command configuration

`CUMG_BACKEND_COMMAND` is the executable and `CUMG_BACKEND_ARGS` is the argument string. V1 splits `CUMG_BACKEND_ARGS` on ASCII whitespace; it does not implement shell-style quoting or escaping. The default `mcp` value is safe, but arguments containing embedded spaces cannot currently be represented reliably through this setting.

Do not put secrets in backend command arguments. They may be visible to local process inspection even though the gateway avoids logging argument values.

## Host and Origin rules

### Host

The default accepted Host authorities are loopback-only. A reverse proxy must therefore either:

1. intentionally rewrite/preserve the origin `Host` as a loopback authority such as `127.0.0.1:8100`; or
2. forward the public hostname and configure the gateway with the exact authority:

```text
CUMG_ALLOWED_HOSTS=computer.example.com
```

Include a port only when it is actually present in the forwarded authority.

Do not disable Host validation to make a deployment work.

### Origin

Browser-originated requests with an `Origin` header are checked independently of Host. Add only exact expected origins:

```text
CUMG_ALLOWED_ORIGINS=https://client.example.com
```

Non-browser MCP clients may omit `Origin`. Do not use wildcard origins for convenience.

## HTTP overload and health-route hardening

The gateway keeps a local defense-in-depth ceiling in front of the MCP HTTP route. `CUMG_MAX_HTTP_CONCURRENCY` defaults to `16`. Once all slots are in use, another MCP HTTP request fails immediately with HTTP `503` and `error: gateway_overloaded` instead of joining an unbounded waiter queue.

This does **not** replace the existing backend operation serialization: one physical desktop is still protected by the backend `operation_lock`. The HTTP ceiling protects the northbound process boundary from request accumulation before those serialized operations execute.

Keep reverse-proxy or Cloudflare rate limiting as an independent outer control. The local concurrency ceiling is not authentication and should not be described as a complete denial-of-service defense.

`/healthz` returns only coarse readiness by default:

```json
{"status":"ok","backend":"ready"}
```

Set `CUMG_HEALTH_DETAILS=true` only when detailed local diagnostics are intentionally required. That opt-in adds backend process metadata such as PID, cumulative CPU seconds, and RSS. Remote deployments should normally leave it disabled.

Authentication at the reverse proxy must cover the **entire public hostname**, including `/mcp`, `/healthz`, and any future auxiliary route. Do not create an unauthenticated path exception merely to make a remote health check convenient. A path-specific proxy policy that protects `/mcp` but exposes `/healthz` is not the documented deployment model.

## Cloudflare Access + Tunnel

Cloudflare is one example deployment, not a required dependency. Current Cloudflare guidance recommends creating the **Access application before publishing the tunnel route**; otherwise the hostname can be publicly reachable without the intended Access policy.

Cloudflare also recommends remotely managed tunnels for most deployments. This repository keeps a locally managed YAML example because its origin settings can be reviewed alongside the gateway. Either management model is acceptable if the resulting security properties are equivalent.

Official Cloudflare references:

```text
https://developers.cloudflare.com/cloudflare-one/access-controls/applications/http-apps/self-hosted-public-app/
https://developers.cloudflare.com/tunnel/advanced/local-management/create-local-tunnel/
https://developers.cloudflare.com/tunnel/advanced/origin-parameters/
```

### 1. Create the Access application first

In Cloudflare Zero Trust, create a self-hosted Access application for your intended hostname, for example:

```text
computer.example.com
```

Create a policy that matches the intended users or machine credentials. The application/policy must protect the hostname as a whole, including `/healthz`; do not scope authentication only to `/mcp`. Do not publish the tunnel hostname first and plan to add Access later.

For automated MCP clients that cannot complete an interactive identity-provider login, a Cloudflare Access **Service Token** with a **Service Auth** policy is one option. See [`CLIENTS.md`](CLIENTS.md) for a Codex header example.

### 2. Create a tunnel

For the locally managed example:

```bash
cloudflared tunnel login
cloudflared tunnel create computer-use-mcp-gateway
```

Record the tunnel UUID and generated credentials-file path. These are deployment secrets/identifiers and must not be committed to this repository.

### 3. Prepare the tunnel configuration

Copy the repository example outside version control or into an ignored local path:

```bash
cp examples/cloudflared.yml ~/.cloudflared/computer-use-mcp-gateway.yml
```

Replace:

- `YOUR_TUNNEL_ID`;
- the credentials-file path;
- `computer.example.com`;
- optional Access `teamName` and `audTag` values when enabling local JWT validation.

The example contains:

```yaml
originRequest:
  httpHostHeader: 127.0.0.1:8100
```

Cloudflare documents `httpHostHeader` as the `Host` header sent to the local service. Keeping this rewrite means the gateway's default loopback Host allowlist can remain unchanged.

If you intentionally remove that rewrite and forward the public hostname instead, set:

```text
CUMG_ALLOWED_HOSTS=computer.example.com
```

### 4. Enable Access-token validation at the tunnel where practical

Cloudflare's **Protect with Access** origin setting makes `cloudflared` validate the Access JWT before proxying traffic to the gateway. For a locally managed config, the shape is:

```yaml
originRequest:
  httpHostHeader: 127.0.0.1:8100
  access:
    required: true
    teamName: YOUR_TEAM_NAME
    audTag:
      - YOUR_ACCESS_APPLICATION_AUD_TAG
```

This is defense in depth on top of Cloudflare Access policy evaluation. Replace placeholders with the values from your Access application.

### 5. Create the DNS route

After the Access application exists:

```bash
cloudflared tunnel route dns computer-use-mcp-gateway computer.example.com
```

### 6. Run the gateway on loopback

For example:

```bash
cargo run --locked -- --allow-tools list_apps,list_windows,get_accessibility_tree,get_screen_size
```

Do not change `CUMG_BIND` to `0.0.0.0` merely because a tunnel is being used.

### 7. Run the tunnel

```bash
cloudflared tunnel \
  --config ~/.cloudflared/computer-use-mcp-gateway.yml \
  run computer-use-mcp-gateway
```

The remote MCP URL becomes:

```text
https://computer.example.com/mcp
```

### 8. Verify authentication, then MCP

First verify that unauthenticated access is rejected by Access for both `/mcp` and `/healthz`. Then connect using the intended identity/OAuth/service-token mechanism.

For a Service Token, Cloudflare's standard request headers are:

```text
CF-Access-Client-Id
CF-Access-Client-Secret
```

Never put the secret in the repository. [`CLIENTS.md`](CLIENTS.md) shows an environment-backed Codex configuration.

## Reverse-proxy requirements

If you use something other than Cloudflare, preserve the same properties:

- HTTPS/TLS for the remote connection;
- authentication for the whole public hostname, including health/auxiliary routes, before requests reach the gateway;
- an intentional Host forwarding/rewrite policy matching `CUMG_ALLOWED_HOSTS`;
- exact Origin allowlisting when browser Origins are expected;
- an explicit request-rate/concurrency policy appropriate to the deployment in addition to the gateway's local concurrency ceiling;
- no direct route to the Cua stdio backend;
- no accidental public exposure of other local services;
- preferably an additional origin-side verification mechanism so a proxy-policy bypass does not silently become anonymous access.

The gateway should remain on loopback unless a different bind is a deliberate, reviewed network design.

## MCP clients

Local and remote client examples are in [`CLIENTS.md`](CLIENTS.md).

A remote client must be able to satisfy the authentication mechanism chosen at the reverse proxy. Browser SSO, OAuth, static bearer credentials, and service-token headers are not interchangeable; choose an auth flow the client actually supports.

## Logging

Normal gateway logs intentionally omit raw tool arguments/results. Treat reverse-proxy logs separately: headers, authentication metadata, and request bodies can leak sensitive information depending on proxy configuration.

Do not enable body logging for MCP traffic unless there is a narrowly scoped debugging need and the resulting data is handled as sensitive.

## Secrets

Do not commit:

- tunnel credential files or real tunnel IDs;
- Access service-token Client Secrets;
- production hostnames when they are intended to remain private;
- screenshots or desktop E2E artifacts containing user data;
- personal filesystem paths;
- `.env` files.

`.gitignore` excludes `.env` variants plus generated `*.key`, PKCS#12, `*.secret`, and `secrets/` material. Ignore rules are only defense in depth; production credentials belong in the selected secret store.

## Current deployment limitations

V1 has no built-in:

- public authentication;
- TLS termination;
- multi-machine routing;
- per-user desktop isolation;
- distributed locking;
- shell-style quoting for `CUMG_BACKEND_ARGS`;
- cloud control plane.

All MCP clients connected to one V1 gateway ultimately share one serialized physical desktop/backend state.

## Reviewed single-Mac macOS deployment

A trusted development Mac that intentionally co-locates Hub, external grant signer, Agent, and Cua should use the reviewed single-Mac profile rather than hand-written LaunchAgents. See [`v2/V2_SINGLE_MAC_PRODUCTION.md`](v2/V2_SINGLE_MAC_PRODUCTION.md). Its upgrade helper preserves Hub drain/quarantine semantics, archives a version-paired rollback asset, writes a payload-free runtime identity manifest, and requires a healthy read-only `v2_doctor` result after restart.
