# V2-M1 service packaging and secret lifecycle

V2-M1 uses the operating system service manager instead of implementing a daemon supervisor. The Linux Hub is a hardened systemd system service; Linux Agents are systemd user services; macOS Agents are LaunchAgents so Cua and macOS privacy/TCC attribution remain in the interactive user session.

## TLS certificate lifecycle

Use an existing ACME client (Certbot, Caddy, lego, etc.) as the certificate authority/renewal mechanism. `v2_hub` intentionally rejects symlinked secret-key inputs, so do not point it directly at an ACME client's `live/` symlink tree. Run `scripts/v2-install-renewed-tls.sh` from the ACME deploy hook to validate the certificate/key pair and atomically copy regular files into `/etc/cumg-v2/tls/`, then `systemctl try-restart cumg-v2-hub.service`. Renewal logic remains owned by ACME, not this project.

The systemd Hub unit uses `LoadCredentialEncrypted=` for the long-lived Hub Ed25519 and grant-signing keys and `LoadCredential=` for the ACME-managed TLS private key. Provision the application keys into the systemd encrypted credential store with `systemd-creds encrypt --name=hub-secret ... /etc/credstore.encrypted/hub-secret` and the equivalent `grant-secret` command; keep the rotation/recovery copy in the operator's normal secret manager rather than in the repository. The service receives only `%d/...` credential file paths. Private key bytes are never placed in environment variables, checkpoints, logs, or OTLP attributes.

`v2_tls_check` provides the common certificate/trust-anchor expiry probe. It accepts PEM server certificates and DER trust roots, rejects symlinks or group/world-writable trust material, prints `CUMG_TLS_EXPIRY_OK` while healthy, and exits non-zero with `CUMG_TLS_EXPIRY_ALERT` when the certificate is inside the warning window, expired/not-yet-valid, or malformed. The packaged Linux Hub timer checks `/etc/cumg-v2/tls/server.pem` daily with a 30-day warning window. The Linux Agent user timer and macOS LaunchAgent template perform the same daily check on the configured pinned `tls-root.der`. A non-zero oneshot/LaunchAgent result is intentionally an operational alert signal; wire failed-unit/log monitoring to the deployment's pager or notification system instead of treating it as an automatic trust change.

### Initial Agent enrollment

Enrollment remains an **offline operator-controlled trust action**, not a public enrollment endpoint. On a protected administrative host, place the already-reviewed Hub application public key, grant-verifier public key, and currently-valid TLS root in a private staging parent, then run:

```bash
v2_keyctl prepare-agent-enrollment \
  --output-dir /secure/cumg-enroll/desktop-01 \
  --hub-public /secure/cumg-trust/hub.pub \
  --grant-public /secure/cumg-trust/grant.pub \
  --tls-root-der /secure/cumg-trust/tls-root.der
```

The output directory must not already exist and its parent must not be a symlink or group/world-writable. The command validates the source trust material, generates a new device secret with create-new semantics, and writes a non-secret `enrollment.json`. Transfer only `agent/` to the target desktop over the operator's authenticated provisioning channel. Install `hub/device.pub` as that Hub instance's `CUMG_V2_DEVICE_PUBLIC_KEY_FILE`, and configure the Agent with the `device_id` printed by the command / recorded in the manifest. Private device bytes are never printed.

This flow is for a **fresh fixed-device enrollment**. Do not replace the provisioned public key underneath an existing checkpoint for a different device: Hub startup intentionally detects that as a trust mismatch. Rotate the same logical device through the dual-signed device-rotation procedure below; provision unrelated additional devices as separate fixed entries/instances rather than turning this workflow into mutable runtime discovery.

### TLS private-key/root compromise

Ordinary ACME renewal is not the compromise procedure. If a private pinned hierarchy's server private key may have escaped, the old certificate remains cryptographically valid to an Agent that still trusts the old root because CUMG does not add CRL/OCSP processing. The independent Hub Ed25519 handshake still prevents the TLS key alone from becoming CUMG command authority, but TLS confidentiality must be considered lost. Use a maintenance cutover: stop affected Agents, create a replacement private root and server certificate/key with the deployment PKI, validate the new chain/key, stage the new regular-file server identity on the Hub and the new DER root on every Agent through the authenticated provisioning channel, restart the Hub, then restart Agents and verify a fresh authenticated generation. Do **not** rotate Hub/device/grant application identities merely because the TLS hierarchy changed.

The regression `v2_m1_tls::tests::private_root_compromise_cutover_requires_agent_trust_reprovisioning` proves the boundary: the old root accepts the old chain, rejects the replacement chain, and the replacement root accepts it.

## Application-key lifecycle

`v2_keyctl` creates secrets with create-new semantics and never prints private key material.

On Linux Hub rotation, decrypt or retrieve the active old Hub key only in a protected offline/admin context, generate the dual-signed replacement, then encrypt the replacement into `/etc/credstore.encrypted/hub-secret` before restarting the service. Do not keep the temporary plaintext rotation input under the repository checkout.

- Hub identity rotation: `v2_keyctl rotate-hub --old-secret OLD --new-secret NEW --new-public NEW_PUB --rotation hub-rotation.json --epoch NEXT_EPOCH`. Stop the Agent during the cutover, deploy `NEW` to the Hub and `NEW_PUB` plus the signed rotation document to the Agent (`CUMG_V2_HUB_ROTATION_FILE`), then restart Hub and Agent. The Agent accepts the changed Hub key only if the persisted old key verifies the continuity document and the next epoch is exact.
- Device identity rotation: stop the Agent first, then run `v2_keyctl rotate-device --device-id STABLE_ID --old-secret OLD --new-secret NEW --new-public NEW_PUB --rotation device-rotation.json --epoch EPOCH` in a protected admin context. The command creates the replacement secret with create-new semantics and produces a continuity document signed by both the old and new device keys. Stage `NEW_PUB` plus `CUMG_V2_DEVICE_ROTATION_FILE` on the Hub and restart the Hub **before** starting the Agent with `NEW`; Hub startup verifies and persists the rotation while retaining the stable device id and existing replay/admission checkpoint. Start the Agent with `NEW`, confirm a fresh authenticated generation, then remove the one-shot rotation-file setting; a later Hub restart must load the already-persisted new verifier without that document. Do not fall back to the old device secret after the rotation is persisted: returning to an older key requires a new continuity rotation signed by the currently trusted key and the intended replacement.
- Grant-signing rotation: generate a new grant key, first add its public key to the Agent with `CUMG_V2_ADDITIONAL_GRANT_PUBLIC_KEY_FILES`, then switch the Hub signer. Keep both verifiers trusted for at least the maximum grant lifetime (5 minutes). After that, make the new verifier primary and remove the old verifier. The Agent's configured verifier set is authoritative on restart, so retired keys are removed from its persisted grant ledger.

TLS identity, Hub application identity, device identity, and capability-grant signing identity are deliberately separate lifecycles.

## Connection and request limits

`v2_hub` sheds excess Agent session starts with gRPC `RESOURCE_EXHAUSTED`; it does not create a second operation scheduler. Northbound MCP overload is shed before OAuth work with HTTP `429` (rate) or `503` (concurrency). These in-process limits complement the existing per-device operation admission controller.

Authenticated Agent transports are also bounded by a one-hour default hard lifetime. The Hub starts a 30-second reauthentication drain before that deadline, pauses new operation admission, and normally reconnects only after already-admitted work settles. Reaching the hard deadline forcibly closes the transport and preserves the existing fail-closed ambiguity/quarantine rules. Configure these with `CUMG_V2_MAX_AGENT_SESSION_LIFETIME_SECS` and `CUMG_V2_AGENT_SESSION_REAUTH_DRAIN_SECS`.

Raw TCP/TLS handshake floods must be bounded at the standard deployment edge (cloud firewall/security group plus a reviewed TLS-capable reverse proxy/load balancer where applicable). The Hub does not grow a bespoke pre-TLS protocol or custom handshake queue. Keep northbound MCP loopback-only behind that edge.

Trusted-proxy mode additionally requires `cumg-v2-hub-trusted-proxy-credential.conf.example`. Generate a private random secret, encrypt the Hub copy with `systemd-creds`, provision the same value through the proxy's secret mechanism, and make the proxy overwrite `X-CUMG-Trusted-Proxy-Token` on loopback forwarding. The Hub strips the token before MCP handling. Its trusted-proxy peer concurrency/rate defaults (4 / 60 requests per minute) intentionally stay below the global 16 / 120 limits to preserve headroom.

## OpenTelemetry

Local structured tracing is always available through `RUST_LOG`. OTLP is opt-in only through standard endpoint variables: `OTEL_EXPORTER_OTLP_ENDPOINT` enables both traces and metrics; the signal-specific `OTEL_EXPORTER_OTLP_TRACES_ENDPOINT` and `OTEL_EXPORTER_OTLP_METRICS_ENDPOINT` enable only their respective signal. `opentelemetry-otlp` resolves the standard `OTEL_EXPORTER_OTLP_PROTOCOL`, signal-specific protocol, headers, and timeout variables; the packaged build uses the standard OTLP `grpc` transport. `OTEL_SDK_DISABLED=true` disables export. OTLP traces cover northbound HTTP requests (method + path only) and authenticated Agent session lifetimes; rejection counters cover rate/concurrency shedding. Default telemetry contains control-plane event names, rejection reasons, and opaque operation ids only; command payloads, argv, file contents, screenshots, clipboard contents, bearer tokens, and credentials are excluded.

## Optional MCPUsage Memory sidecar

`cumg-v2-usage-sidecar.service` runs the private CUMG-owned bridge on loopback only. Install `cumg-v2-hub-usage.conf.example` as a Hub drop-in only when usage accounting is desired. `PartOf=` plus the Hub `Requires/After` drop-in couples explicit Hub restart to sidecar recreation, intentionally resetting MemoryUsageStore runtime state. The service must point `CUMG_MCP_USAGE_CONTROL_MODULE` at a locally built/installed `mcp-usage-control` core; no npm publication is required. See [`../docs/V2_USAGE_ACCOUNTING.md`](../docs/V2_USAGE_ACCOUNTING.md).
