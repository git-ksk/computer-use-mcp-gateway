# Backend passthrough contract

V1 is a policy-controlled MCP gateway, not a semantic reimplementation of Cua Driver. After exact-name policy checks, the gateway preserves backend tool-call semantics unless a behavior is explicitly documented as gateway-owned.

## Tool arguments and sessions

The gateway forwards the MCP tool `arguments` object to the configured backend without removing, adding, or rewriting backend-specific fields such as `session`.

This matters for Cua Driver session-aware tools. If the same Cua tool behaves differently when a `session` argument is present, that behavior belongs to the Cua/backend session contract unless a gateway regression test demonstrates argument mutation.

For example, a read-only call such as `get_screen_size` may be accepted by Cua without a session but rejected with a backend error such as `desktop_escalation_required` when a session is supplied. The gateway must not bypass that backend escalation requirement by silently dropping or rewriting the session field.

When diagnosing session behavior, compare the same tool and arguments with and without `session`, and compare direct Cua behavior with Gateway → Cua behavior when a trusted desktop environment is available.

## Discovery and application identity

The gateway also forwards backend tool results without inventing application/process identity normalization. It does not reconcile `list_apps`, `list_windows`, accessibility snapshots, or other Cua discovery results by guessing that display names, bundle names, executable names, and PIDs refer to the same application.

Consequently, if Cua reports an application as `running=false` / `pid=0` in one discovery surface while a different Cua surface reports a live PID for a similarly named application, V1 preserves those backend results rather than fabricating a corrected PID.

This fail-conservative rule avoids turning heuristics such as `CuaDriver.app` vs `Cua Driver` into authoritative process identity. Backend adapters in a future control-plane design may normalize identity only behind explicit, versioned conformance rules.

## Regression coverage

`scripts/v1_passthrough_contract.py` verifies that:

- a `session` field and nested tool arguments reach the backend unchanged;
- deliberately inconsistent backend application/window identity data is returned unchanged;
- the gateway therefore does not become the source of backend session escalation or discovery identity semantics.

The contract test uses the deterministic mock backend and never touches the desktop.
