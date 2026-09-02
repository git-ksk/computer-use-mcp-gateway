# V2 Unix process-containment investigation

Status: **investigation complete for issue #96; current portable contract unchanged**.

## Decision

CUMG will **not** claim OS-wide descendant containment for the existing Unix `execute_process` / `shell` capability and will not attempt to emulate that guarantee with command-text filters, recursive PID scans, process-name matching, or best-effort lineage polling.

The current portable contract remains:

- each Unix operation starts in a dedicated process group;
- cancellation, timeout, and ordinary top-level completion terminate descendants that remain in that group;
- deliberate detachment into another session/process group is outside the guarantee;
- persistent work must use a separately authorized managed-job/service lifecycle rather than `nohup`, `setsid`, daemonization, or equivalent escape from the bounded operation.

This is a lifecycle boundary, not a sandbox against a caller already authorized for the exact `Dangerous` process/shell capability.

## macOS evidence

A trusted macOS probe on 2026-09-03 reproduced the known escape against the same process-group model used by CUMG:

1. launch a top-level shell as a new process-group leader;
2. fork a child that calls `setsid()` and remains alive;
3. let the top-level shell exit;
4. send `SIGKILL` to the original process group;
5. verify the detached child is still alive, then explicitly terminate that test child.

Observed result:

```text
top_level_rc=0 ... detached_alive_after_killpg=true
```

This matches the macOS `setsid(2)` contract: a successful caller becomes the leader of a **new session and new process group**.

The current macOS SDK exposes `proc_listchildpids()` and PID metadata, but those are snapshot/relationship inspection APIs rather than a descendant container. Once a parent exits, children can be reparented, so recursive child enumeration cannot provide a race-free kill guarantee. `EVFILT_PROC` still defines `NOTE_FORK`, but the SDK explicitly marks the historical `NOTE_TRACK` / `NOTE_CHILD` fork-lineage tracking flags unsupported since macOS 10.5. CUMG therefore must not claim that polling these APIs closes the gap.

For intentionally persistent macOS background work, Apple's supported lifecycle is `launchd` / Service Management. That is compatible with the product direction of #106: persistent jobs should be a separate explicit lifecycle, not an escape from a bounded shell operation.

## Linux evidence and stronger optional direction

Linux cgroup v2 provides a materially stronger primitive when the CUMG Agent is given a properly delegated cgroup subtree. The kernel delegation model makes resource restrictions hierarchical, and a delegated subtree can contain its descendants even when a process changes POSIX session/process-group membership. A cgroup-backed execution domain can therefore be stronger than `killpg` for deliberate `setsid()` escape.

That does **not** make cgroup v2 a portable drop-in replacement for the current Unix baseline:

- delegation and writable cgroup ownership must be provisioned by the service manager/deployment;
- CUMG must not assume `/sys/fs/cgroup` is writable merely because cgroup v2 is mounted;
- service-manager/systemd policy, containers, and cgroup namespaces can change what subtree is delegated;
- the implementation must prove that stale/escaped processes cannot migrate outside the delegated operation subtree;
- cancellation/timeout still needs terminal proof before returning a proven terminal result.

Linux cgroup containment is therefore split into a Linux-specific implementation issue rather than silently widening the guarantee of the existing process-group backend.

## Rejected approaches

The investigation rejects the following as authoritative containment mechanisms:

- filtering shell text for `setsid`, `nohup`, `daemon`, `launchctl`, `systemctl`, or spelling variants;
- recursively scanning current child PIDs and killing what is visible;
- matching executable names or command lines;
- broad same-user process killing;
- assuming a normal parent/child relation remains after daemonization or reparenting;
- treating successful delivery of a signal as proof that all side effects/processes stopped.

Each is incomplete, race-prone, overly broad, or changes the security boundary without proof.

## Consequences for planned work

### #106 managed long-running jobs

#106 should own intentionally persistent developer processes. A managed job requires a distinct exact capability, stable job identity, explicit start/status/output/stop lifecycle, bounded concurrency/storage, and platform-specific termination proof. On macOS this may use a reviewed service/helper lifecycle where appropriate; on Linux a cgroup-backed execution domain may provide stronger containment.

### #114 Playwright / E2E sandbox

#114 must not depend on the ordinary process-group boundary as its sandbox. Browser test execution needs a separately reviewed OS/container/VM isolation boundary, ephemeral profile/home, bounded filesystem/network authority, and explicit cleanup semantics.

## Support claim

No released Unix support claim changes from this investigation. The documented current guarantee remains truthful and tested for ordinary descendants. A future stronger platform-specific backend may advertise a stronger guarantee only after its deployment prerequisites and regression/physical evidence are explicit.

## References

- Apple `setsid(2)`: <https://developer.apple.com/library/archive/documentation/System/Conceptual/ManPages_iPhoneOS/man2/setsid.2.html>
- Apple Service Management: <https://developer.apple.com/documentation/servicemanagement/>
- Apple launchd daemon/agent guidance: <https://developer.apple.com/library/archive/documentation/MacOSX/Conceptual/BPSystemStartup/Chapters/CreatingLaunchdJobs.html>
- Linux cgroup v2: <https://docs.kernel.org/admin-guide/cgroup-v2.html>
