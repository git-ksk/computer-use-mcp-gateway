# Changelog

## Unreleased — v0.7.0

### Recovery target identity (#289)

- execution-safety durable schema **v14** adds private recovery-target metadata for application-targeted effectful operations; v13 and earlier reviewed state remains readable, while a v14 operation carrying recovery-target state cannot be lossily downgraded to v13;
- `launch_application` persists its bounded identifier/name selector as recovery metadata; `terminate_application` now requires a bounded `application` value alongside `process_id` so a later local operator can identify the caller-selected application after an ambiguous termination; the Agent control command remains PID-only and live Hub-Agent/control schema versions are unchanged;
- recovery target metadata is checkpoint-local and observational only. Normal northbound MCP, `get_operation`, generic quarantine JSON, logs, metrics, and unified status do not disclose it. The local `v2_maint incident-brief` surface may reveal it solely to support independent Human verification and never treats it as settlement or replay authority.

**Migration:** v0.7 clients calling `terminate_application` must pass `application` using the bounded application identity selected during prior observation (for example the `application` field returned by `list_windows`). Existing v0.6 clients that send only `process_id` receive an input-schema failure and must refresh MCP tool discovery/schema before using this effectful operation.

### Authoritative backend execution receipts (#290)

- Agent/backend adapters can opt into a reviewed payload-free durable receipt contract carrying exact operation/device-generation/capability-revision/capability/dispatch-grant/provider provenance, monotonic sequence, relevant application target binding, and terminal outcome. Missing, stale, malformed, conflicting, cross-operation, target-mismatched, or unsupported receipts never become terminal evidence;
- exact receipts persist in Agent M1 checkpoint schema **v6**, survive restart, and are converted into the existing `AgentTerminalEvidence` so the signed #124 self-reconciliation path remains the only automatic settlement state machine. Hub execution-safety stays at **v14** and live control/capability/Hub-Agent schemas do not change;
- historical Agent checkpoint schema v5 remains readable when it has no receipt state; receipt-bearing v6 state cannot be lossily represented as v5 and fails closed on downgrade/corruption;
- the default adapter and current Cua MCP adapter do not claim a reviewed durable receipt source, so backend response loss before a definite Agent result remains `Indeterminate` / operator-required. Provider/log/current-state heuristics remain observational only;
- `v2_maint audit-reconciliation` and `incident-brief` correlate bounded receipt provenance and #289 target binding while keeping external diagnostics separately labeled `observational_only`; generic MCP/status/log/metric surfaces remain payload/private-target safe.

## v0.6.0 — 2026-09-22

Managed Developer Execution release. This release adds separately authorized managed developer jobs, a provider-isolated Playwright/E2E capability, and optional reviewed Linux cgroup-v2 process containment while preserving exact authorization, bounded execution, `Indeterminate` quarantine, and permanent no-auto-replay semantics.

### Managed developer execution

- managed jobs use separate `ManagedJobControl` / `ManagedJobObserve` authority, stable opaque `job_` refs, bounded concurrency/output/lease/hard lifetime, explicit renew/status/output/stop, and proven process-domain termination; generic shell/process background escape is not promoted into a supported persistence mechanism (#106);
- asynchronous managed-job termination ambiguity persists Agent fail-closed state and requires explicit offline operator recovery rather than silently reopening execution;
- Playwright/E2E uses separate `PlaywrightTestControl` / `PlaywrightTestObserve` authority and `pwtest_` refs; its Docker/Podman-compatible provider is optional and advertised only after complete config, digest-pinned image inspection, and owner-scoped orphan recovery (#114).

### Playwright sandbox boundary

- the fixed provider profile uses direct argv, `--pull=never`, network none, read-only root/workspace, private writable artifacts, dropped capabilities, `no-new-privileges`, fixed non-root `pwuser`, bounded pids/memory/CPU/shm, tmpfs home/tmp, sanitized environment, fixed Playwright binary/reporter/output, and no inherited host Docker/SSH authority;
- provider-container absence is required before terminal stop/completion/expiry; unproven provider cleanup becomes `Indeterminate`/no-replay rather than terminal success;
- the Playwright container remains the filesystem/network sandbox boundary. Host process-group or Linux cgroup containment alone is not represented as equivalent isolation.

### Linux process containment hardening

- optional Linux cgroup-v2 containment is enabled only with an explicit reviewed delegated root that already contains the Agent; mount presence alone never grants stronger authority (#267);
- each bounded process/shell operation enters a fresh operation cgroup before exec, establishes a user+cgroup namespace boundary on an `nsdelegate` cgroup-v2 host mount, and terminates via exact operation `cgroup.kill` plus `populated 0` proof;
- Linux CI proves deliberate `setsid()` detachment, concurrent fork cleanup, migration containment, and explicit fail-closed behavior when delegation is unavailable; macOS/portable-Unix process-group and Windows Job Object claims are unchanged.

### Compatibility, upgrade, and release identity

- live schemas are control **12**, capability advertisement **8**, persisted device registry **8**, and Hub-Agent transport **6**; historical v0.5.0 is 10/6/8/6 and historical #106 development used 11/7;
- persisted v0.5 registry/capability state is accepted through the reviewed migration, stale advertisements are discarded, and a fresh current Agent advertisement is required; live mixed old/new control/capability versions fail closed;
- v0.5.0 -> v0.6.0 is a paired runtime upgrade. Rollback after v0.6 writes current capability state requires the pre-upgrade v0.5 checkpoint and version-paired v0.5 binaries/config rather than asking v0.5 to interpret v0.6 state;
- optional Playwright/cgroup configuration defaults absent, so upgrade does not silently enable new execution authority.

### Acceptance and support boundary

- #106, #114, and #267 are closed before the final #335 release gate; deterministic CI, CodeQL, dependency review, docs/link checks, Linux/macOS/Windows release-candidate build/verify/smoke, schema migration, and no-replay/recovery regressions remain required on the exact release snapshot;
- managed-job lifecycle is dogfooded on the trusted physical macOS process-control domain; Linux cgroup-v2 stronger containment is accepted only on the reviewed delegated Linux CI host; Playwright support remains conditional on a successfully probed configured provider;
- #139 physical signed-token support, #217 cross-platform recovery parity, and #228 physical Linux FIDO2 UV support remain explicitly withheld/open; hosted Cloud Run/Handoff remains unsupported;
- the GitHub Release remains source-only unless reviewed binary assets plus required SBOM/license/provenance evidence are attached. CI release-candidate archives remain release evidence, not official binary installers.

## v0.5.0 — 2026-09-21

Least-privilege Workspace release. This release adds bounded workspace observation/mutation, retrievable truncated output, private ephemeral continuation data, execution-budget hardening, and challenge-scoped guided recovery without widening shell authority or weakening `Indeterminate` quarantine/no-replay semantics.

### Workspace and least privilege

- live schemas are pinned to control **10**, capability advertisement **6**, persisted device registry **8**, and Hub-Agent transport **6**; the reviewed registry migration accepts historical pairings `2/2`, `3/3`, `4..6/4`, `7/5`, and current `8/6`, while impossible or mixed live versions fail closed;
- bounded filesystem observation, retrievable truncated process/shell output, and atomic workspace mutation are exact capabilities rather than extensions of generic shell authority (#105/#83/#107);
- workspace mutation defaults to disabled and is advertised only when an explicit mutation executor exists; enabling it requires an explicit allowed write root, deny paths remain deny-wins, and cwd/read roots never become write roots by fallback;
- Agent ephemeral bytes are private, bounded, non-authoritative, outside authoritative state/rollback trees, and excluded from rollback assets.

### Execution budget and ambiguity

- the reviewed packaged Cua timeout remains **30 s** and the conservative effective execution budget is **24 s** for duration-bearing commands;
- impossible paced input is rejected before backend dispatch as terminal/retry-safe; a real post-dispatch backend timeout remains `Indeterminate`, quarantines the device, and is never automatically retried or replayed (#319);
- the Hub-Agent schema bump to 6 covers the signed payload-free `IndeterminateAck` / backend-timeout cause evidence added after v0.4.0 without turning that evidence into completion or replay authority.

### Guided recovery lifecycle

- guided recovery plan schema **v3** makes the interactive Human review challenge-scoped: expiry or exact binding/generation rollover invalidates the old prompt and stale selection context, requires a fresh signed review plus a fresh Human choice, and never carries an unsigned historical assertion across challenges (#323);
- pre-auth stale review explicitly reports that user-presence authentication did not start and leaves authorization unpublished, quarantine retained, and the old operation unreplayed; challenge identity is re-checked immediately before signing and before authorization publication;
- terminal guided recovery still requires the exact durable Hub acknowledgement and now makes recovery completion, exact quarantine clearance, effectful-execution readiness, normal recovery mode, and `old_operation_replayed=false` explicit.

### Packaging, upgrade, and runtime identity

- release-manifest schema **v3** records exact Hub-Agent/control/capability schemas, package/source identity, platform/architecture, and closed file digests;
- installed runtime-manifest schema **v4** records the same three nested schema identities plus exact source/package/binary hashes; `v2_doctor` rejects any individual nested-schema mismatch as `runtime_manifest / invalid_schema_or_identity`;
- macOS generic LaunchAgent, single-Mac LaunchAgent, Linux env, and Windows reviewed config all carry an explicit ephemeral parent and explicit workspace-mutation default of disabled;
- v0.4 -> v0.5 upgrade migration preserves least privilege: missing mutation mode migrates to disabled only when no write policy exists, inconsistent configuration fails closed, and ephemeral data is never promoted into authoritative backup/rollback state;
- Windows paired-upgrade preflight requires a dedicated ephemeral parent, validates mutation mode and absolute write policy, and keeps ephemeral content out of rollback backups.

### Acceptance boundary

- macOS physical Cua acceptance for the execution-budget hardening was already completed under #319;
- Linux/Windows release-candidate, packaging, upgrade, and capability-advertisement checks are automated evidence and are not represented as new physical Cua smoke results;
- #308 Windows npm/CSPRNG recurrence was not reproducible on the affected SKDT runtime during release recheck; live Gateway `node`, repeated `npm --version`, `npm run typecheck`, and Node crypto all passed without Agent restart/duplication, and PR #326 adds an exact Windows CI recurrence guard through the cleared-environment `ProcessExecutor` path while the historical transient root cause remains open for follow-up;
- the GitHub Release remains source-only unless reviewed binary assets, SBOM/license inventory, and provenance/attestation are explicitly attached; CI archives remain release evidence rather than official binary installers;
- `Indeterminate`, quarantine, no-auto-replay, payload/path/text-free diagnostics, exact capability authorization, and mixed-version fail-closed behavior remain unchanged security invariants;
- the dedicated release/v0.5.0 PR reruns the standing Product Readiness gates against the exact release commit before the immutable tag/source-only GitHub pre-release is created.


## v0.4.0 — 2026-09-19

V2 Recovery, Identity & Semantic Authorization release. This release consolidates the post-v0.3 recovery/reconciliation hardening, provider-neutral multi-principal identity, and narrow typed semantic authorization into one reviewed minor release without widening unaccepted platform/provider support claims.

### Recovery and execution safety

- durable recovery/reconciliation and operator guidance were tightened across current-state acceptance, historical Human resolution, runtime/tool skew detection, recovery-key readiness, replay-tombstone handling, long-lived PointerClick recovery, exact completion acknowledgement, and packaged recovery-key discovery (#103/#115/#136/#137/#253/#254/#255/#256/#305/#309/#310);
- execution-safety durable schema v13 adds explicit mutation-resume barriers/records for acknowledged-unknown PointerClick recovery; schema v12 retains bounded semantic-authorization admission evidence (snapshot revision/digest plus constraint kind/rule ID). Older supported state remains readable, while downgrade fails closed whenever it would discard v13 mutation-resume state or v12 semantic evidence;
- permanent no-auto-replay, `Indeterminate` quarantine, exact operation ownership, and pre-dispatch cancellation semantics remain authoritative.

### Identity and authorization

- provider-neutral signed OIDC/JWT caller identity verifies exact issuer/audience, asymmetric algorithm allowlists, pinned HTTPS JWKS, bounded cache/unknown-`kid` refresh, and maps only verified `issuer + subject` into the existing exact principal/device/capability authorizer (#139 / PR #269);
- typed semantic authorization adds narrow-only constraints at the finalized command boundary: a UTF-8 byte ceiling for `TypeText` and normalized requested-origin allowlists for `BrowserNavigate` (#221 / PR #271);
- semantic decisions are bound to an immutable revision+digest snapshot, recorded without raw text/URL/policy payloads, and fenced again before provider dispatch; stale snapshot identity cancels before dispatch rather than becoming indeterminate;
- no generic expression language, regex policy escape hatch, caller-controlled hot reload, or backend-private authorization namespace is introduced.

### Product and operability

- filesystem observation roots are separated from process working-directory roots (#104);
- Windows v0.4.0 dogfood gains a fail-closed version-paired upgrade path with release-manifest verification, candidate config preflight, bounded Hub/Agent health gates, and pair rollback (#293/#294);
- Windows candidate operation now surfaces bounded Hub-Agent schema incompatibility separately from transport/auth failures and exponentially backs off repeated rapid child exits instead of creating a tight restart storm (#294);
- reproducible V1 latency/concurrency benchmarking is available as informational product evidence (#111);
- the Unix explicit-session-detachment investigation is closed with the portable process-group guarantee documented; stronger optional Linux cgroup-v2 containment remains future #267 work (#96);
- the `0.4.0` roadmap now treats Cloud Run #215 as design-complete but unsupported future hosted work rather than a release claim.

### Compatibility and support claims

- `v0.4.0` is a pre-1.0 minor compatibility boundary and must be deployed as a version-paired Hub/Agent/maintenance/recovery/Handoff set; mixed/incompatible schema or durable-state representations continue to fail closed;
- the GitHub Release remains source-only unless reviewed binary assets, SBOM/license inventory, and provenance/attestation are explicitly attached; CI release-candidate artifacts are evidence, not automatically supported installers;
- Windows Hello recovery is release-supported after trusted physical interactive-desktop acceptance passed on #227; generic signed-token support remains withheld until #139 physical/dogfood acceptance is recorded, Linux FIDO2 UV recovery remains withheld until #228 physical acceptance, and #217 remains the cross-platform parity umbrella;
- Cloud Run remains unsupported, and Linux/Windows CI artifacts do not become official binary-installer claims.

### Acceptance evidence

- #221 merged after local full regression (`530 passed / 0 failed`, six existing physical-only tests ignored), warning-free all-target clippy, synchronized EN/JA docs, and all 15 GitHub checks green;
- #227 trusted physical Windows Hello acceptance passed on PR #252: cancel preserved the exact quarantine, approval produced durable_completion=verified, a fresh unrelated ScreenGeometry succeeded, Hub restart preserved the terminal resolution, and the old operation remained permanently non-replayed (ONLINE_RECOVERY_PHYSICAL_PASS operation_replayed=false);
- #305/#307 trusted physical macOS recovery dogfood cleared a long-lived PointerClick quarantine through explicit two-stage local-user authorization without replay; #309/#310 then closed completion-reporting and packaged recovery-key diagnostic gaps discovered by that run;
- the dedicated release/v0.4.0 PR reruns the standing Product Readiness gate against the exact release commit before the immutable tag/source-only GitHub pre-release is created.

## v0.3.0 — 2026-08-27

V2 Production Hardening / Operational Readiness release. The final #100 trusted physical-macOS Secure Enclave/user-presence acceptance passed on merged release-candidate code: a real ambiguous desktop operation was resolved through local-user-authorized online recovery, the durable quarantine cleared only after verified authorization, Hub restart preserved the resolution, and the old operation was never replayed. The stale release PR #99 was superseded by a fresh release snapshot from current `main`.

### Execution safety and recovery

- privacy-preserving, read-only quarantine inspection with explicit `blocking_operation_id`, plus candidate correlation that never becomes completion/replay authority (#116);
- version-paired Hub/`v2_maint` offline recovery with pre-publication durable writer-compatibility checks (#117);
- exact signed self-reconciliation for supported terminal evidence, bounded unknown-outcome retirement for reviewed low-impact legacy ambiguity, and a first-class cross-Hub/Agent reconciliation-readiness audit without raw checkpoint archaeology (#133);
- partial text/input effect resolution, privacy-preserving evidence envelopes, and the execution-safety schema-v8 restricted recovery-evidence read lane while mutation remains quarantined (#179/#181/#180);
- permanent no-auto-replay and persistence-gated quarantine semantics remain unchanged across reconnect, restart, recovery, retirement, and evidence collection.

### Human Handoff and runtime boundary

- first-class optional Handoff coordination is integrated into the controlled Agent for Window and Terminal/PTY surfaces while the Hub retains CUMG authorization/ledger/quarantine and conservative dispatch fencing (#152);
- legacy/current launchd runtime coexistence fails closed instead of allowing ambiguous double-runtime ownership (#157);
- Handoff unavailability does not silently bypass the coordinator once the deployment enables it.

### Diagnostics and operability

- live control schema v9 carries bounded privacy-safe execution failure classes through Agent -> Hub -> northbound MCP without exposing host paths, commands, environment values, device identity, or raw OS/provider errors (#141);
- `v2_doctor` distinguishes an exact in-band diagnostic self-observation from a real blocking quarantine without mutating restart-safety state (#194);
- browser staging startup reports bounded local initialization stage/I/O classes while preserving fail-closed private staging (#143);
- controlled `StorageFull` fault injection confirms durable Agent checkpoint exhaustion can surface remotely as `agent_offline`; failed publication preserves the prior committed checkpoint/replay barriers, normal service-manager restart/authenticated reconnect succeeds after writable capacity returns, and doctor exposes coarse read-only state/temp capacity warnings (#112).

### Acceptance status

- merged-main regression coverage includes warning-free Rust gates, V1 quality/conformance preservation, pinned-Cua Linux/macOS/Windows smoke, privacy/no-replay durability tests, and the previously accepted physical Desktop/Window/Terminal Handoff evidence applicable to their respective changes;
- #100 trusted physical local-user online recovery passed with Secure Enclave user presence, durable resolution across Hub restart, quarantine remaining clear, and `operation_replayed=false`; deferred stabilization/enhancement issues remain outside the v0.3.0 release gate.

## v0.2.0 — 2026-08-13

V2 complete.

- uncertainty-aware execution safety with authoritative operation ownership and generation fencing;
- durable `indeterminate` quarantine with explicit auditable resolution and no automatic replay;
- restart/reconnect-safe persistence and fixed-set multi-device invariant proof;
- backend portability/replacement seams without duplicating the execution-safety state machine;
- standard OAuth northbound and TLS/gRPC southbound boundaries;
- payload-safe structured observability and OpenTelemetry support;
- 10k-operation, reconnect/generation-churn, and RSS-plateau regression coverage;
- trusted real-Cua desktop acceptance on merged `main`.

Non-goals remain generic fleet/platform infrastructure, a generic device fabric, remote desktop, and a generic delegated-authorization protocol.

## v0.1.0 — 2026-08-11

V1 complete: hardened MCP-to-computer-use gateway with local/remote transport, deny-by-default tool policy, cancellation/serialization/resource regression coverage, and trusted real-desktop acceptance.
