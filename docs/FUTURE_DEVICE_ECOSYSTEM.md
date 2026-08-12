# Future device ecosystem (optional)

Status: **optional post-V2 direction; not a V2 acceptance requirement**.

This note records a possible long-term extension without changing the current V2 product boundary in [`V2_POSITIONING.md`](V2_POSITIONING.md).

## Idea

If the desktop execution-safety core proves portable, CUMG could later act as a thin **execution-safety control plane for heterogeneous agent-controlled devices** while delegating actual device control to maintained backends and ecosystems.

The reusable waist would remain:

```text
principal
   |
authority / ownership
   |
operation ID + device identity + generation fencing
   |
state-changing effect
   |
terminal evidence OR durable indeterminate
   |
quarantine / explicit resolution / no auto-replay
```

The device-specific implementation stays below that boundary.

## Possible optional adapter families

- desktop: Cua, native process/shell, and other maintained computer-use backends;
- mobile: Android/iOS automation runtimes such as Appium-, ADB-, Accessibility-, XCUITest-, or WebDriverAgent-backed systems;
- IoT/home automation: maintained gateways or protocol adapters rather than project-owned device drivers;
- robotics/physical systems: ROS/ROSClaw-class runtimes or other maintained embodied-device control layers;
- browser or remote execution environments where the same ambiguity/evidence model is meaningful.

These are **integration candidates, not commitments**.

## Scope guard

Do not turn CUMG into a generic device registry, fleet manager, automation platform, protocol replacement, or collection of native device drivers.

Prefer:

```text
CUMG safety/control semantics
        |
replaceable adapter
        |
existing maintained runtime / device ecosystem
```

rather than reimplementing the runtime below the adapter.

Any future adapter must preserve the authoritative CUMG invariants:

- exact operation identity;
- explicit principal/device ownership;
- device/session/generation fencing;
- no ownership inheritance across ambiguous work;
- durable `indeterminate` when execution cannot be proven terminal;
- quarantine that survives reconnect/restart;
- explicit auditable resolution;
- no automatic replay of ambiguous state-changing operations;
- backend evidence must justify terminal classification rather than CUMG guessing from liveness.

## When to revisit

Revisit this direction only after the current core milestones are accepted:

1. multi-device invariant proof;
2. second-backend portability proof;
3. V2 release hardening.

The first practical expansion should be an adapter proof against an existing maintained ecosystem, not a new broad device-control implementation.

Until then, the canonical V2 positioning remains **uncertainty-aware execution safety for delegated control of stateful interactive desktops**.
