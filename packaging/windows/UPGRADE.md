# Windows V2 paired upgrade

`v2_windows_upgrade.py` is the reviewed fail-closed upgrade boundary for the Windows V2 persistence profile. It upgrades the Windows release-candidate runtime as one exact artifact identity instead of replacing Hub and Agent independently.

## Contract

The command verifies the extracted Windows release-candidate manifest and SHA-256 records through `v2_release_candidate.py`, then stages the complete Windows runtime set (`v2_hub`, `v2_agent`, `v2_maint`, `v2_keyctl`, `v2_tls_check`). Before stopping the active runtime it probes the staged Hub/Agent `--help` surfaces and refuses candidate configs missing any required CLI flag. This catches migrations such as the v0.4.0 `--allowed-file-root` requirement before service drain.

Activation is paired and bounded: stop Agent, stop Hub, install the reviewed configs and complete runtime set, start Hub, verify its loopback listeners, start Agent, require a stable Agent PID and a fresh Hub `v2_agent_session_accepted` event, then optionally run an operator-provided authenticated external-route smoke script. Caddy/proxy, enrollment, trust, secrets and durable Hub/Agent state are not replaced.

If activation or a health gate fails after service drain, the command restores the previous Hub/Agent configs and every pre-existing Windows runtime binary as one rollback set, restarts Hub then Agent, and verifies recovery. A rollback failure becomes `operator_action_required`; a subsequent upgrade is refused until the prior transaction is investigated.

## Operation

Run with `--preflight-only` first. Preflight verifies artifact identity and config compatibility without stopping Hub or Agent. Remove it only after preflight succeeds. Use `--external-smoke-script` when the deployment has an authenticated route smoke; completion then requires that script to exit zero.

The durable operator record is under `v2-windows-shell\state\upgrade\windows-upgrade-status.json`. Staged candidates and rollback assets live under `staging\<transaction>` and `backup\<transaction>`. Rollback assets remain after success for operator-controlled recovery/cleanup.

Do not hand-copy only `v2_agent.exe` or only `v2_hub.exe` across a minor schema boundary. The upgrader stops only the reviewed scheduled tasks and only PID-file children whose executable identity matches the reviewed config.
