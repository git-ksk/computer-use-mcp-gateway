# V2 local desktop acceptance

Physical macOS desktop acceptance is local-only. The public repository does not use a self-hosted macOS runner for this check.

Normal CI stays on GitHub-hosted Linux, macOS, and Windows runners. Physical GUI validation is an operator-controlled acceptance step for trusted code.

## Run

Prerequisites:

- Rust 1.88.0
- `cua-driver` installed with the required macOS permissions
- the exact trusted commit checked out locally

Run:

```bash
CUMG_DESKTOP_E2E_ACK=1 \
CUMG_V2_CUA_CANCEL_E2E_ACK=1 \
CUMG_V2_NATIVE_ELEMENT_E2E_ACK=1 \
CUMG_V2_CUA_COMMAND="$(command -v cua-driver)" \
bash scripts/v2_desktop_acceptance.sh
```

All three ACK variables are mandatory. The wrapper fails closed before any desktop action if an acknowledgement is missing.

The wrapper verifies Cua permissions, runs the real TextEdit screenshot/click/type/readback fixture,
then runs `real_cua_native_element_action_acceptance`: a fresh Calculator instance is inspected for the
exact `7` button, the resulting backend element token is exercised through the V2 background element
press path, and before/after exact-window screenshots prove the visual effect. The wrapper then runs
the V2 real-Cua ambiguity/restart/reconnect/no-auto-replay/explicit-resolution regression.


## Post-effect backend-error regression (issue #47)

Changes to Cua error translation or the V2 execution-safety boundary should also run the isolated browser-alert acceptance:

```bash
CUMG_V2_ISSUE47_E2E_ACK=1 \
CUMG_V2_CUA_COMMAND="$(command -v cua-driver)" \
bash scripts/v2_issue47_browser_alert_acceptance.sh
```

The wrapper creates a loopback-only HTML fixture and an isolated temporary Chrome profile, binds the exact native browser window, clicks a button whose handler opens `alert("GATEWAY_ALERT_OK")`, and verifies both sides of the regression: the alert is visibly present while the Cua call finishes with a provider error, and the V2 adapter classifies that post-dispatch outcome as `BackendOutcomeIndeterminate`. The disposable browser/profile/server are removed on exit. No automatic retry is permitted.

The historical V1 gateway binary is built only for the mature TextEdit physical fixture. The recommended runtime remains V2 Hub + V2 Agent.

A successful run ends with:

```text
PASS local physical desktop acceptance
```

Changes affecting the Computer Use adapter or V2 desktop execution boundary should pass this local acceptance before release. CUMG remains the authority for operation ownership, generation fencing, durable indeterminate quarantine, explicit resolution, and no-auto-replay.
