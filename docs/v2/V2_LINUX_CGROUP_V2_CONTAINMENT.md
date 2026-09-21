# V2 Linux cgroup-v2 process containment

Issue #267 adds an optional Linux-only containment backend for bounded `execute_process` and `shell` operations. It strengthens descendant cleanup without changing the portable Unix process-group contract.

## Activation and fail-closed boundary

The backend is enabled only by an explicit operator setting:

- `CUMG_V2_LINUX_CGROUP_V2_ROOT=/absolute/cgroup/path`, or
- `--linux-cgroup-v2-root /absolute/cgroup/path`.

Omission keeps the existing truthful Unix process-group behavior. CUMG never infers stronger authority from cgroup-v2 mount presence alone.

When configured, Agent startup refuses the backend unless all of the following hold:

- the Agent is non-root;
- the host exposes one normal cgroup-v2 mount at `/sys/fs/cgroup`;
- the configured path is absolute, canonical, non-symlink, and a `domain` cgroup;
- the **Agent process is already a member of that configured cgroup**;
- the configured cgroup has no child cgroups at startup;
- its directory and `cgroup.procs` support delegated operation-child creation/migration;
- a fresh child exposes writable `cgroup.procs` and `cgroup.kill`;
- the configured cgroup's parent `cgroup.procs` is not writable to the Agent;
- unprivileged user namespaces plus cgroup and mount namespaces are available;
- a startup probe can enter a child cgroup, establish the private cgroup view, exec, and clean the child cgroup completely.

The placement rule is important. Linux cgroup-v2 delegation requires write authority to the common ancestor when migrating a process. Therefore a non-root Agent cannot safely pull a process from outside the delegated subtree while simultaneously proving that it cannot push the process back outside. The service manager/operator must place the Agent into the delegated root before CUMG starts.

## Execution contract

The configured root belongs to the Agent service. Bounded process/shell execution is serialized to one operation cgroup at a time.

For each operation CUMG:

1. creates a fresh child cgroup;
2. pre-opens that child cgroup's `cgroup.procs`;
3. forks;
4. in the post-fork/pre-exec hook, writes `0` through the pre-opened descriptor so the child enters the operation cgroup;
5. before any requested code executes, creates a new user namespace, cgroup namespace, and mount namespace;
6. installs one-to-one UID/GID mappings for the current Agent identity;
7. makes mount propagation private and mounts a new **read-only cgroup2 view** at `/sys/fs/cgroup`, rooted by the new cgroup namespace at the operation cgroup;
8. sets `no_new_privs`;
9. only then execs the requested executable or fixed shell.

The operation therefore starts inside the cgroup before effectful user code runs. The private cgroup namespace plus private read-only cgroup mount prevents same-UID command code from reaching the Agent's outer cgroup hierarchy or migrating back to the Agent root. The existing Unix process-group wrapper remains as an inner lifecycle primitive.

On cancellation, timeout, setup failure after spawn, and ordinary top-level completion, CUMG:

1. writes `1` to the exact operation cgroup's `cgroup.kill`;
2. waits until that operation root's `cgroup.events` reports `populated 0`;
3. removes descendant cgroups, then removes the operation cgroup itself.

This exact operation-root kill covers descendants that call `setsid()`, concurrent fork races, and descendants that create or move within cgroups below the private operation root.

If terminal proof fails after spawn, the operation remains outcome-unproven and follows the existing Indeterminate/no-replay/quarantine path. The backend is poisoned for further use rather than silently falling back to process groups.

## Deployment requirements

The service manager/operator owns provisioning. CUMG does not mount the host cgroup hierarchy, create host-global delegation, elevate privileges, or move the Agent into its delegation.

A suitable deployment must place the Agent process into a dedicated delegated cgroup before launching the Agent binary, then grant only that cgroup to the Agent. The parent migration boundary stays service-manager/root controlled.

The kernel must permit unprivileged user namespaces because CUMG uses a child user namespace only to create the child-owned cgroup and mount namespaces needed to hide the outer cgroup hierarchy. If host policy disables unprivileged user namespaces, the stronger backend is unavailable and configured startup fails closed.

Containerized/namespaced deployments are not assumed equivalent. The current implementation deliberately requires the reviewed single `/sys/fs/cgroup` mount layout and otherwise refuses stronger mode.

The backend remains process-lifecycle containment, not a general sandbox. It does not isolate network, ordinary filesystem access, credentials, or arbitrary syscalls. The namespace setup is only part of proving the cgroup execution boundary. It also means set-user-ID privilege gain is not available inside this optional backend because `no_new_privs` is set.

## Scope

- Linux bounded `execute_process` / `shell`: optional cgroup-v2 stronger cleanup.
- macOS / other Unix: unchanged process-group contract.
- Windows: unchanged Job Object contract.
- managed jobs: unchanged platform baseline in this issue.
- Playwright: still requires its separately reviewed container sandbox; cgroups do not replace it.

## Acceptance

Linux CI provisions a writable delegated root and an unwritable control root, then places the test process into the writable root using the privileged test harness before dropping back to the normal runner identity. Real-kernel tests prove:

- a `setsid()`-detached descendant is gone when the operation returns;
- a fork race leaves no operation cgroup behind;
- migration toward the parent hierarchy is denied;
- reaching the outer hierarchy through another process's `/proc/<pid>/root` is denied;
- the private cgroup mount is read-only to command code;
- an unwritable cgroup-v2 root is explicitly unavailable.

References:

- Linux kernel cgroup v2 documentation: <https://docs.kernel.org/admin-guide/cgroup-v2.html>
- `cgroup_namespaces(7)`: <https://man7.org/linux/man-pages/man7/cgroup_namespaces.7.html>
- `user_namespaces(7)`: <https://man7.org/linux/man-pages/man7/user_namespaces.7.html>
