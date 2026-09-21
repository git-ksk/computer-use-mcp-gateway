# V2 Linux cgroup-v2 process containment

Issue #267 adds an optional Linux-only containment backend for bounded `execute_process` and `shell` operations. It strengthens descendant cleanup without changing the portable Unix process-group contract.

## Activation and fail-closed boundary

The backend is enabled only by an explicit operator setting:

- `CUMG_V2_LINUX_CGROUP_V2_ROOT=/absolute/cgroup/path`, or
- `--linux-cgroup-v2-root /absolute/cgroup/path`.

Omission means the existing truthful Unix process-group behavior remains in use. There is no auto-detection fallback and no claim that a cgroup-v2 mount alone grants CUMG authority.

When configured, Agent startup refuses the stronger backend unless all of the following hold:

- the Agent is non-root;
- the path is absolute, canonical, non-symlink, and on a cgroup-v2 filesystem;
- the cgroup is an empty `domain` cgroup with no child cgroups;
- `cgroup.procs` and `cgroup.kill` are writable to the Agent;
- the Agent can create a child cgroup and write its `cgroup.procs` / `cgroup.kill`;
- the parent cgroup's `cgroup.procs` is not writable to the Agent.

The final check is deliberate. Linux cgroup-v2 delegation containment prevents a non-root delegatee from migrating a process outside the delegated subtree when it lacks write authority to the common ancestor's `cgroup.procs`. CUMG rejects broader migration authority rather than advertising the stronger guarantee.

## Execution contract

The configured subtree is dedicated to CUMG process/shell containment and is serialized to one bounded operation at a time.

For each operation CUMG:

1. creates a fresh child cgroup;
2. pre-opens that child's `cgroup.procs`;
3. in the post-fork/pre-exec hook writes `0` through that already-open descriptor;
4. only then allows the requested executable or fixed shell to exec.

User code therefore does not execute before entering the cgroup domain. The existing Unix process-group wrapper remains in place as a compatible inner lifecycle primitive.

On cancellation, timeout, setup failure after spawn, and ordinary top-level completion, CUMG:

1. writes `1` to the dedicated root `cgroup.kill`;
2. waits until the root `cgroup.events` reports `populated 0`;
3. removes descendant cgroup directories before releasing the subtree.

Root-level kill is intentional. A same-UID workload may migrate between cgroups inside its delegated subtree, but that does not escape the root-level termination domain. Migration outside the delegation boundary must remain denied. Kernel `cgroup.kill` also covers concurrent forks and is protected against migrations.

If terminal proof fails after spawn, the operation remains outcome-unproven and follows the existing Indeterminate/no-replay/quarantine path. The backend is poisoned for further use rather than silently falling back to process groups.

## Deployment requirements

The cgroup root is infrastructure supplied by the operator or service manager. CUMG does not mount cgroup2, create host-global delegation, elevate privileges, or repair a bad delegation.

A generic `Delegate=yes` on the same systemd unit is not sufficient by itself: the Agent must not live inside the subtree that CUMG will kill, and the configured subtree's parent migration authority must remain unavailable to the Agent. Provision a separate, dedicated, initially empty execution subtree whose parent is still controlled by the service manager or root.

Containerized Agents have the same requirements. A writable cgroup namespace view is not enough unless the configured subtree is the reviewed delegation boundary and the parent migration path is inaccessible. Read-only or partial cgroup mounts make the stronger backend unavailable.

The backend is process-lifecycle containment only. It does not restrict filesystem, network, credentials, syscalls, or executable authority and is not a sandbox.

## Scope

- Linux bounded `execute_process` / `shell`: optional cgroup-v2 stronger cleanup.
- macOS / other Unix: unchanged process-group contract.
- Windows: unchanged Job Object contract.
- managed jobs: unchanged platform baseline in this issue.
- Playwright: still requires its separately reviewed container sandbox; cgroups do not replace it.

## Acceptance

Linux CI provisions both a writable delegated root and an unwritable control root. The real kernel tests prove:

- a `setsid()`-detached descendant is gone when the operation returns;
- a fork race leaves recursive `populated 0`;
- migration across the delegation boundary fails;
- migration inside the dedicated root cannot evade root-level termination;
- an unwritable cgroup-v2 subtree is explicitly unavailable.

References:

- Linux kernel cgroup v2 documentation: <https://docs.kernel.org/admin-guide/cgroup-v2.html>
- `cgroup_namespaces(7)`: <https://man7.org/linux/man-pages/man7/cgroup_namespaces.7.html>
