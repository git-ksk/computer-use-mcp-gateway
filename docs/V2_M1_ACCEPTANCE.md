# V2-M1 final security and acceptance review

Status: **PASS (2026-08-12)** for the **single secure remote Agent** milestone.

This is a bounded milestone decision. It accepts the M1 architecture and implementation described below; it does not claim that an arbitrary internet deployment is secure without the documented authorization-server, firewall/reverse-proxy, OS-permission, secret-custody, and monitoring controls. V2-M2 multi-machine routing remains out of scope.

## Security invariants reviewed

| Invariant | Result | Evidence |
| --- | --- | --- |
| northbound identity is `AuthenticatedClientPrincipal` derived only after MCP/OAuth verification | PASS | RFC 9728 protected-resource metadata + RFC 7662 introspection tests and exact policy tests |
| northbound OAuth bearer token never crosses Hub -> Agent | PASS | bearer header is stripped before MCP handler dispatch; Hub/Agent transport types contain no OAuth credential field |
| authorization remains principal -> stable device -> exact `DeviceCapability` | PASS | existing policy/grant path retained; new GUI drag is a typed exact capability rather than a backend tool-name escape hatch |
| Hub -> Agent production carrier remains gRPC bidi over TLS | PASS | existing gRPC/TLS lifecycle/service E2E retained; raw TLS path remains regression-only |
| ordinary TLS renewal is not a custom protocol | PASS | ACME-compatible deploy hook validates certificate/key match, installs regular files atomically, then service manager restart reloads them |
| Hub, device, grant and TLS identities have separate lifecycles | PASS | signed Hub continuity, dual-signed device continuity, bounded grant-verifier overlap, independent ACME TLS renewal |
| replay/ambiguous operations fail closed | PASS | existing checkpoint/replay regression plus real-Cua cancellation leaves `DeviceIndeterminate` quarantine; no automatic replay |
| service overload is bounded without a second execution scheduler | PASS | Agent session start/concurrency shedding plus northbound HTTP rate/concurrency shedding compose with existing operation admission |
| observability uses a maintained standard and excludes sensitive payloads | PASS | OpenTelemetry/OTLP exporter configuration uses standard OTel environment variables; default events contain control metadata only |
| long-lived service supervision uses OS facilities | PASS | systemd Hub/Linux Agent templates and macOS LaunchAgent template; no custom daemon supervisor |
| generated secrets are protected from accidental repository inclusion | PASS | create-new secret files are 0600 on Unix; `.gitignore` rejects generated key/PKCS#12/secret material and `secrets/` directories |

## Production TLS and secret lifecycle

### TLS

The Hub still fails closed on symlinked/private-key inputs. ACME clients commonly maintain a symlinked `live/` tree, so `scripts/v2-install-renewed-tls.sh` is the explicit boundary between the standard ACME lifecycle and the Hub loader:

1. the ACME client owns issuance and renewal;
2. the deploy hook passes the renewed certificate/key paths to the script;
3. the script parses both objects, proves their public keys match, and writes new regular files into one destination directory;
4. certificate mode is 0644 and private-key mode is 0600;
5. replacement uses same-directory atomic rename;
6. the deploy hook restarts/try-restarts `cumg-v2-hub.service`, so the new TLS identity is loaded at process startup.

A mismatched certificate/private key is rejected before the existing deployed pair is replaced.

### Application identities

`v2_keyctl` creates private keys with create-new semantics and never emits private key bytes to stdout.

- **Hub identity:** replacement requires a continuity document signed by both old and new Hub identities with the exact next persisted Hub rotation epoch on the Agent.
- **Device identity:** replacement keeps the persisted stable device id and requires the existing dual-signature continuity statement before the Hub accepts the new verifier. Replaying a previously applied rotation cannot match the now-enrolled old verifier.
- **Grant signer:** the Agent may temporarily trust old and new verifiers. The overlap must last at least the enforced maximum grant lifetime (5 minutes); after that the configured verifier set is authoritative and the old verifier is retired on restart.
- **TLS server certificate:** independent from all three application identities and renewed through ACME.

For a Linux Hub, the packaged systemd unit uses `LoadCredentialEncrypted=` for Hub and grant application keys and `LoadCredential=` for the ACME-managed TLS key. Secret bytes are not environment-variable values. Replay/checkpoint JSON contains public trust/replay state, never private signing keys.

## Connection and rate limits

M1 adds bounded overload shedding at two service boundaries while retaining the existing operation admission controller as the only execution scheduler:

- Agent-facing gRPC: bounded active session permits and a bounded session-start sliding window; excess opens fail immediately with gRPC `RESOURCE_EXHAUSTED`;
- northbound MCP HTTP: bounded request concurrency and request-start rate; excess requests fail before OAuth introspection with HTTP 503 or 429 respectively;
- per-device command queue/lease/concurrency limits continue to decide whether actual device work is admitted.

These application limits do not pretend to stop raw TCP/TLS handshake floods. That belongs at the host firewall/security group and, where applicable, the reviewed standard reverse proxy/load balancer.

## OpenTelemetry / OTLP

Local structured logging remains available through `RUST_LOG`. OTLP export is opt-in:

- `OTEL_EXPORTER_OTLP_ENDPOINT` enables both traces and metrics;
- `OTEL_EXPORTER_OTLP_TRACES_ENDPOINT` enables traces only;
- `OTEL_EXPORTER_OTLP_METRICS_ENDPOINT` enables metrics only;
- `opentelemetry-otlp` resolves the standard generic/signal-specific protocol, headers and timeout variables;
- the build uses the standard OTLP `grpc` transport transports;
- `OTEL_SDK_DISABLED=true` disables export.

Trace spans cover the bounded northbound HTTP method field and authenticated Agent session lifetimes; overload metrics contain bounded rejection reasons. Default events and metrics do not carry raw shell commands, argv, file contents, screenshots, clipboard values, bearer tokens or private credentials. An external collector/proxy must preserve that privacy boundary rather than enabling body/payload logging around it.

## Service packaging

- `packaging/systemd/cumg-v2-hub.service` — hardened Linux system Hub service with systemd credentials, `StateDirectory=`, restrictive umask, no ambient capabilities, and restart-on-failure.
- `packaging/systemd/cumg-v2-agent.service` — Linux user service. It deliberately does not add filesystem namespaces that would silently alter the operator's explicitly configured Agent filesystem/process capability semantics.
- `packaging/launchd/com.github.git-ksk.cumg-v2-agent.plist` — macOS LaunchAgent so Cua and TCC attribution remain in the logged-in interactive user session.

The templates own supervision/config/log routing; they do not change principal, grant, generation, replay, or execution semantics.

## Real Cua cancellation acceptance

The ignored/manual test `tests/v2_m1_cua_cancellation_e2e.rs` was run on the operator-controlled macOS desktop against the installed **Cua Driver 0.19.3** with Accessibility and Screen Recording permission available.

Post-M1 P0 hardening extends the same acceptance test without weakening the historical M1 gate: the TextEdit fixture is now launched through the Agent-native shell path under an explicit operation owner, the real Cua action uses that same desktop execution boundary, quarantine is checked against a competing principal, and an explicit auditable resolution is required before Cua reuse. See [`V2_P0_EXECUTION_SAFETY.md`](V2_P0_EXECUTION_SAFETY.md).

The test runs the real M1 Hub and Agent over TLS-protected gRPC, performs a real Cua `ScreenGeometry`, then starts a 10-second no-displacement desktop drag and cancels it after 500 ms. Required evidence:

1. Agent propagates MCP cancellation to the live downstream Cua request;
2. cancellation acknowledgement is `IndeterminateAfterPropagation`, not success;
3. originating Hub operation resolves as `DeviceIndeterminate`;
4. the device remains quarantined and the next operation fails with the same indeterminate operation id;
5. no automatic replay occurs.

Result on 2026-08-12: **PASS**.

## Acceptance command set

The final gate uses the repository-pinned Rust toolchain explicitly:

```bash
cargo +1.88.0 fmt --check
cargo +1.88.0 check --locked --all-targets
cargo +1.88.0 test --locked --all-targets
cargo +1.88.0 clippy --locked --all-targets
python3 -m py_compile \
  scripts/cua_gateway_smoke.py \
  scripts/cua_desktop_e2e.py \
  scripts/mock_mcp_backend.py \
  scripts/v1_quality_gate.py \
  scripts/v1_conformance.py
python3 scripts/check_docs.py
plutil -lint packaging/launchd/com.github.git-ksk.cumg-v2-agent.plist
sh -n scripts/v2-install-renewed-tls.sh
CUMG_V2_CUA_CANCEL_E2E_ACK=1 \
CUMG_V2_CUA_COMMAND="$HOME/.local/bin/cua-driver" \
  cargo +1.88.0 test --locked \
  --test v2_m1_cua_cancellation_e2e -- --ignored --nocapture
```

The TLS lifecycle fixture additionally generates local throwaway certificate pairs, proves valid-pair install modes/regular-file behavior, and requires a mismatched key to fail without replacing the deployed key. The `v2_keyctl` fixture generates/rotates throwaway Hub/device/grant material outside the repository and verifies 0600 private-key modes.

## Accepted residuals / non-claims

The following are deliberate post-M1 or deployment boundaries, not hidden blockers:

- a real deployment must configure and operate its authorization server/introspection endpoint, external TLS/network edge, firewall/security group, collector and secret manager correctly;
- systemd unit semantic verification is a Linux packaging/CI concern; the macOS acceptance host can only statically review that template. The template follows systemd's credential/service model and should be validated with `systemd-analyze verify` on the target distribution during package installation/release CI;
- `ExecuteProcess` and `Shell` remain exact `Dangerous` capabilities, not a filesystem sandbox;
- a compromised Agent/backend can still act with the privileges of that local execution boundary; M1 limits delegation and ambiguity but does not claim to secure a fully compromised endpoint;
- macOS TCC approval remains an operator-controlled local trust boundary;
- SPIFFE/SPIRE, fleet workload identity, multi-machine routing and native GUI backends remain V2-M2/later decisions.

## Decision

**GO: V2-M1 is accepted.** Do not reopen custom transport/auth/telemetry/supervisor work that is now owned by gRPC/TLS, MCP Authorization/OAuth, OpenTelemetry/OTLP or the OS service manager unless the standard cannot preserve an existing safety property. Continue to V2-M2 only as a separate milestone that preserves the M1 principal -> device -> exact grant, replay, generation and indeterminate-quarantine invariants.
