# V2-M1 service packaging and secret lifecycle

V2-M1 uses the operating system service manager instead of implementing a daemon supervisor. The Linux Hub is a hardened systemd system service; Linux Agents are systemd user services; macOS Agents are LaunchAgents so Cua and macOS privacy/TCC attribution remain in the interactive user session.

## TLS certificate lifecycle

Use an existing ACME client (Certbot, Caddy, lego, etc.) as the certificate authority/renewal mechanism. `v2_hub` intentionally rejects symlinked secret-key inputs, so do not point it directly at an ACME client's `live/` symlink tree. Run `scripts/v2-install-renewed-tls.sh` from the ACME deploy hook to validate the certificate/key pair and atomically copy regular files into `/etc/cumg-v2/tls/`, then `systemctl try-restart cumg-v2-hub.service`. Renewal logic remains owned by ACME, not this project.

The systemd Hub unit uses `LoadCredentialEncrypted=` for the long-lived Hub Ed25519 and grant-signing keys and `LoadCredential=` for the ACME-managed TLS private key. Provision the application keys into the systemd encrypted credential store with `systemd-creds encrypt --name=hub-secret ... /etc/credstore.encrypted/hub-secret` and the equivalent `grant-secret` command; keep the rotation/recovery copy in the operator's normal secret manager rather than in the repository. The service receives only `%d/...` credential file paths. Private key bytes are never placed in environment variables, checkpoints, logs, or OTLP attributes.

## Application-key lifecycle

`v2_keyctl` creates secrets with create-new semantics and never prints private key material.

On Linux Hub rotation, decrypt or retrieve the active old Hub key only in a protected offline/admin context, generate the dual-signed replacement, then encrypt the replacement into `/etc/credstore.encrypted/hub-secret` before restarting the service. Do not keep the temporary plaintext rotation input under the repository checkout.

- Hub identity rotation: `v2_keyctl rotate-hub --old-secret OLD --new-secret NEW --new-public NEW_PUB --rotation hub-rotation.json --epoch NEXT_EPOCH`. Stop the Agent during the cutover, deploy `NEW` to the Hub and `NEW_PUB` plus the signed rotation document to the Agent (`CUMG_V2_HUB_ROTATION_FILE`), then restart Hub and Agent. The Agent accepts the changed Hub key only if the persisted old key verifies the continuity document and the next epoch is exact.
- Device identity rotation: `v2_keyctl rotate-device --device-id STABLE_ID --old-secret OLD --new-secret NEW --new-public NEW_PUB --rotation device-rotation.json --epoch EPOCH`. Stop the Agent, deploy the new secret to it and the new public key plus `CUMG_V2_DEVICE_ROTATION_FILE` to the Hub, then restart Hub and Agent. The stable device id and existing replay/admission checkpoint are retained; old and new device keys must both sign the rotation.
- Grant-signing rotation: generate a new grant key, first add its public key to the Agent with `CUMG_V2_ADDITIONAL_GRANT_PUBLIC_KEY_FILES`, then switch the Hub signer. Keep both verifiers trusted for at least the maximum grant lifetime (5 minutes). After that, make the new verifier primary and remove the old verifier. The Agent's configured verifier set is authoritative on restart, so retired keys are removed from its persisted grant ledger.

TLS identity, Hub application identity, device identity, and capability-grant signing identity are deliberately separate lifecycles.

## Connection and request limits

`v2_hub` sheds excess Agent session starts with gRPC `RESOURCE_EXHAUSTED`; it does not create a second operation scheduler. Northbound MCP overload is shed before OAuth work with HTTP `429` (rate) or `503` (concurrency). These in-process limits complement the existing per-device operation admission controller.

Raw TCP/TLS handshake floods must be bounded at the standard deployment edge (cloud firewall/security group plus a reviewed TLS-capable reverse proxy/load balancer where applicable). The Hub does not grow a bespoke pre-TLS protocol or custom handshake queue. Keep northbound MCP loopback-only behind that edge.

## OpenTelemetry

Local structured tracing is always available through `RUST_LOG`. OTLP is opt-in only through standard endpoint variables: `OTEL_EXPORTER_OTLP_ENDPOINT` enables both traces and metrics; the signal-specific `OTEL_EXPORTER_OTLP_TRACES_ENDPOINT` and `OTEL_EXPORTER_OTLP_METRICS_ENDPOINT` enable only their respective signal. `opentelemetry-otlp` resolves the standard `OTEL_EXPORTER_OTLP_PROTOCOL`, signal-specific protocol, headers, and timeout variables; the packaged build uses the standard OTLP `grpc` transport. `OTEL_SDK_DISABLED=true` disables export. OTLP traces cover northbound HTTP requests (method + path only) and authenticated Agent session lifetimes; rejection counters cover rate/concurrency shedding. Default telemetry contains control-plane event names, rejection reasons, and opaque operation ids only; command payloads, argv, file contents, screenshots, clipboard contents, bearer tokens, and credentials are excluded.
