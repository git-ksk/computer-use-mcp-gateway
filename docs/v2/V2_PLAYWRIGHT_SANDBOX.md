# V2 Playwright Sandbox

> English is canonical. [日本語版 / Japanese translation](V2_PLAYWRIGHT_SANDBOX.ja.md)

Issue #114 adds governed Playwright/E2E execution without widening generic process or browser authority. Playwright configuration and tests execute as Node.js code, so they are treated as arbitrary code inside an external isolation provider rather than as a safe command allowlist.

## Authority boundary

The northbound surface is separate from generic managed jobs:

- PlaywrightTestControl — Dangerous; required for playwright_test_start and playwright_test_stop.
- PlaywrightTestObserve — Observe; required for playwright_test_status and playwright_test_output.

Neither capability is implied by ExecuteProcess, Shell, ManagedJobControl, ManagedJobObserve, browser authority, cwd/workspace roots, or a class-only grant. Generic managed jobs use job_ public refs; Playwright tests use a separate pwtest_ registry. Cross-registry lookup fails closed.

## Provider admission

The initial provider is an operator-configured Docker/Podman-compatible local container runtime. CUMG does not install or start the runtime, pull images, provision VMs, or create a fleet.

Configuration is an all-or-nothing tuple:

- absolute runtime executable path;
- immutable image reference pinned as ...@sha256:<64 lowercase hex>;
- one or more Playwright-specific approved workspace roots.

A runtime path that is itself a symlink is rejected. The runtime must be a regular executable. Partial configuration is rejected. When no provider is configured, Playwright capability is absent rather than emulated through generic process authority.

Before advertising the capability, the Agent validates the provider configuration, inspects the digest-pinned image, enumerates containers carrying both io.cumg.playwright=v1 and the exact owner label derived from stable device identity plus Agent state directory, removes only those owned leftovers, proves the owned set is empty, and only then prunes old private artifacts. Failure anywhere in startup recovery fails the provider closed. Containers owned by another Agent are never selected by the recovery label.

## Typed request

playwright_test_start accepts a typed request rather than raw CLI/runtime arguments:

- approved workspace;
- up to 32 relative test paths, each up to 256 bytes;
- optional project up to 128 bytes;
- optional grep up to 512 bytes;
- optional workers from 1 through 16;
- bounded hard lifetime up to 30 minutes.

The request rejects absolute test paths, parent traversal, option-looking paths beginning with a dash, NUL, arbitrary environment, raw reporter/output paths, raw runtime arguments, host browser profiles, CDP/connect endpoints, Docker socket access, SSH agent access, credential-store mounts, and arbitrary host paths.

## Fixed container contract

The provider is executed directly with argv, never through a shell. The generated run profile is fixed to the reviewed sandbox boundary:

- run --rm --init;
- CUMG-managed container name;
- --pull=never;
- CUMG owner labels;
- --network none;
- --read-only;
- --cap-drop ALL;
- --security-opt no-new-privileges;
- --pids-limit 512;
- --memory 2g;
- --cpus 2;
- --shm-size 1g;
- --user pwuser;
- bounded writable tmpfs for /tmp and /home/pwuser;
- approved workspace mounted read-only at /workspace;
- one Agent-private run directory mounted writable at /artifacts;
- fixed HOME=/home/pwuser, CI=1, and PLAYWRIGHT_BROWSERS_PATH=/ms-playwright;
- fixed executable /workspace/node_modules/.bin/playwright;
- fixed reporter=line;
- fixed output=/artifacts/test-results.

The Playwright process policy has an empty inherited-environment allowlist. Host Agent/Gateway HOME, user identity, SSH agent, credentials, and arbitrary environment values are not inherited.

Concurrency is bounded to two active Playwright jobs. Output uses the managed-job rolling-buffer primitive with at most 4 MiB retained per stream and at most 64 KiB returned by one output read.

## Network boundary

The initial network profile is only none. Loopback inside the same container remains available, so a repository can launch its project-local web server in that container and test it via localhost.

External-origin networking is intentionally deferred. URL filtering or Playwright CLI argument validation is not a network sandbox. A future external-network profile requires enforceable provider-level policy below CUMG.

## Lifecycle and terminal proof

A container runtime client process ending is not proof that the provider container stopped. Playwright terminal states therefore add provider-aware proof to the managed-job process-domain proof.

For explicit stop, CUMG records stop intent first, requests provider container rm -f, reaps or terminates the attached runtime client, requests provider cleanup again, and queries provider container ps -a for the exact CUMG-managed name. The state becomes stopped only after the exact container is proven absent.

Natural completion and hard expiry similarly require cleanup plus provider absence before exposing a terminal success state. The stop-intent-first ordering prevents an explicit stop from racing into ordinary completion.

If provider absence cannot be proven, the runner records provider termination as unproven. The effectful operation is surfaced as PlaywrightProviderOutcomeUnproven, the Agent uses the existing Indeterminate / reconnect / quarantine semantics, and CUMG does not auto-replay the test. Asynchronous provider ambiguity also activates the Agent fail-closed safety boundary.

## Restart recovery

A crashed Agent can leave a provider container alive even if the host runtime-client process disappeared. Startup recovery therefore runs before capability advertisement and is owner-scoped by labels. Ordinary process-group or Windows Job Object cleanup is never treated as sufficient evidence about the external container.

This is also why --rm is insufficient by itself: cleanup still requires a provider query proving that the owned container is absent.

## Artifacts

Artifacts live under an Agent-private Playwright root inside the Agent state directory. The parent is private; only the per-run directory is writable from the container.

Northbound APIs never reveal the host path. Status exposes only bounded artifact_count and artifact_total_bytes. Inspection rejects symlinks and enforces bounded file, directory, and total-byte traversal.

Artifacts are removed only when provider terminality has been proven. If cleanup is ambiguous, artifacts are retained so CUMG does not destroy evidence or an active mount.

## Privacy and telemetry

Default telemetry must not record raw test paths, grep values, workspace or artifact host paths, provider container identifiers, pwtest_ public refs, or raw output bodies. Fixed capability/reason categories and bounded operational metadata remain subject to the existing audit policy.

## Platform claim and non-claims

The sandbox boundary is provided by the configured container runtime, not by CUMG host process supervision.

- macOS/Windows: the selected runtime may use a VM internally; CUMG treats that as external provider infrastructure.
- Linux: container isolation remains required for this feature. Optional #267 cgroup-v2 containment strengthens host descendant cleanup but is not a filesystem/network sandbox and is not a substitute for the provider.
- macOS sandbox-exec / SBPL is not part of the product contract.

CUMG does not claim containment beyond the guarantees of the selected reviewed execution provider when the provider runtime, kernel, Agent, or workspace dependency is compromised.

## Schema compatibility

Current live v0.6 values for #114 are:

- CONTROL_SCHEMA_VERSION = 12;
- capability schema 8;
- HUB_AGENT_SCHEMA_VERSION = 6;
- persisted device registry schema 8.

Historical registry/capability pairings accepted only for restore are 2/2, 3/3, 4..=6/4, 7/5, released-v0.5 8/6, and historical #106 v0.6 8/7. The current live pairing is 8/8. Historical advertisements are discarded during restore and a fresh current advertisement is required before dispatch.

## Acceptance focus

Before merge/release acceptance, evidence must cover provider-absent and partial-config fail closed, digest/runtime/workspace validation, fixed isolated argv and no host environment inheritance, exact capability split and job_/pwtest_ namespace separation, explicit stop/natural completion/hard-expiry provider absence proof, provider-proof failure becoming Indeterminate rather than terminal success, startup orphan recovery before capability advertisement, artifact symlink/traversal rejection, privacy-bounded status, no automatic replay after response loss or provider ambiguity, and Linux/macOS/Windows compile/CI compatibility for the compiled surface.

External-origin networking remains out of scope for the initial profile.
