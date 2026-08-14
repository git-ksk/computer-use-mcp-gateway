# V2 Production Cutover and Rollback

This runbook covers a controlled V1 -> V2 production routing change. It is deliberately stricter than a local V2 acceptance run: V1 stays available as the immediate rollback target until the external client path has proved V2.

## 1. Preconditions

Do not change the public route unless all of the following are true:

- repository and deployed V2 binaries are identified by an exact commit/hash;
- Hub and Agent are supervised and listening only on their intended loopback/local endpoints;
- the Agent advertises the reviewed capability schema/revision and the Hub reports no unresolved quarantine;
- every persisted operation is terminal, or any ambiguity remains explicitly quarantined;
- the northbound exact principal -> device -> capability policy has been reviewed;
- `Host` and `Origin` guards are enabled; an unrelated Host and unrelated browser Origin are rejected;
- Cua is the reviewed compatibility version and `CUMG_V2_CUA_BACKEND_VERSION` is set to that exact version rather than `external`;
- the authenticated reverse proxy/Access policy still identifies the intended principal;
- V1 remains healthy and reachable on its existing origin.

`ExecuteProcess` and `Shell` are intentionally powerful `Dangerous` capabilities, not sandboxes. Allowed cwd roots do not confine arbitrary argv or shell syntax. Grant them only when remote process/shell execution is part of the production contract; do not add them merely to make `tools/list` look complete.

## 2. Persistent-state compatibility

Current V2 fails closed when an older control/capability checkpoint cannot be safely mixed with the current schema. Never delete or reset V2 state merely to make a new binary boot.

A state reset is permitted only after all three conditions are proved:

1. there is no unresolved/quarantined ambiguous operation that would be forgotten;
2. every recorded operation is terminal;
3. the complete old Hub/Agent state, binaries, policy, and service configuration have been archived for incident/rollback analysis.

After a justified reset, start Hub before Agent and confirm the fresh generation/schema/revision before any public route change.

## 3. Reverse-proxy change

Treat the reverse-proxy configuration as concurrent mutable state. Immediately before editing it:

1. fetch the latest full tunnel/proxy configuration;
2. assert the target hostname still points at V1;
3. assert authentication/Access settings and unrelated ingress rules are unchanged;
4. preserve the fetched configuration as the route rollback source.

For Cloudflare Tunnel deployments whose API replaces the complete configuration document, submit the latest full document and change only the target ingress origin fields required for V2. Do not reuse a previously observed config version as an optimistic assumption.

Keep V1 running. A routing change and a V1 shutdown are separate operations.

## 4. Immediate external V2 smoke

After the route changes, refresh/reconnect MCP discovery in the external client. Some MCP clients cache tool schemas across an existing conversation or connection.

Verify through the actual authenticated public path:

- tool discovery is the expected policy-filtered V2 semantic surface and contains no raw Cua/CDP/generic backend escape hatch;
- `get_screen_size` succeeds;
- `list_apps` succeeds;
- `list_windows` succeeds where supported;
- Hub durable state records the requests as terminal;
- quarantine remains empty;
- the V2 Hub, not V1, received the requests.

Do not use a V1-only operator tool such as `health_report` as the V2 success criterion.

## 5. Transport-security negative smoke

Before declaring the route stable, retain the security invariants that were already required in V1:

- unexpected `Host` -> rejected;
- unexpected browser `Origin` -> rejected;
- the canonical public Origin -> accepted when an Origin header is present;
- requests without a browser Origin remain usable for ordinary non-browser MCP clients;
- reverse-proxy authentication/Access remains required.

Never disable Host/Origin validation to work around proxy forwarding.

## 6. Rollback trigger and procedure

Rollback immediately on authentication failure, missing/incorrect tool discovery after a clean reconnect, semantic smoke failure, unexpected quarantine, V2 crash/restart loop, transport-guard regression, or evidence that requests reached the wrong local service.

Rollback routing only first:

1. refetch the current full proxy/tunnel configuration;
2. restore only the target hostname's origin/Host-rewrite fields to the known V1 values;
3. preserve authentication/Access and unrelated ingress rules;
4. verify the external V1 semantic smoke;
5. leave V2 stopped or isolated for diagnosis as appropriate.

Do not restore an old incompatible V2 checkpoint into a new V2 binary as a shortcut. The archived pre-cutover state is evidence and an explicit old-binary rollback asset, not permission to mix schema generations.

## 7. Bake and V1 retirement

Only consider stopping/disabling V1 after the external V2 path has remained healthy for an operator-chosen bake period and there is no unresolved V2 quarantine. Preserve the pre-cutover rollback archive through closeout.

Record at closeout:

- deployed commit and binary hashes;
- Cua compatibility version;
- exact granted capability set;
- proxy config revision before/after;
- external smoke results;
- final quarantine/operation state;
- whether V1 was retained or retired.
