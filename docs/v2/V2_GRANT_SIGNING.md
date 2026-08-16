# V2 external grant-signing boundary

The packaged Linux V2 Hub uses a separate `v2_grant_signer` process for capability-grant signing. This removes the Ed25519 grant private key from the `v2_hub` process while keeping the existing in-process `GrantAuthority` backend available for explicitly single-host/development deployments.

## Production boundary

```text
northbound request
      |
      v
+---------------- v2_hub ----------------+
| OAuth/trusted-proxy auth                |
| principal -> device -> exact capability |
| operation admission / quarantine        |
| Hub transport Ed25519 key               |
| grant signer: Unix client + public key  |
+-------------------+---------------------+
                    |
                    | bounded typed request
                    | device_id + exact capability
                    | issued_at + short TTL
                    v
+------------- v2_grant_signer -----------+
| separate service UID                    |
| independent exact-capability ceiling    |
| signer clock-skew / TTL validation       |
| grant ID generation + canonical signing |
| grant Ed25519 private key                |
+-------------------+---------------------+
                    |
                    v
              signed GrantToken
```

The Hub never sends arbitrary bytes to be signed. The Unix protocol is length-prefixed and bounded to 16 KiB. A request contains only the protocol version, stable device ID, exact `DeviceCapability`, requested issue time, and TTL. The signer generates the grant ID, constructs the canonical `GrantPayload`, enforces its own policy, and signs with the key that exists only in the signer process.

The Hub pins the signer public verifier. Every returned token must match the requested device/capability/timestamps and pass Ed25519 verification before it can be attached to a Hub-to-Agent command. An invalid/mismatched response fails closed.

## Independent signer policy

`GrantSigningPolicyDocument` is intentionally separate from the Hub northbound policy. Its current schema is:

```json
{
  "schema_version": 1,
  "device_id": "dev_...",
  "allowed_device_capabilities": ["screenshot", "pointer_click", "type_text"],
  "max_grant_lifetime_ms": 30000,
  "max_clock_skew_ms": 15000
}
```

The signer denies a request unless the device ID and exact capability are allowlisted, the TTL is non-zero and no greater than both the signer policy and protocol maximum, and the Hub-supplied issue time is within the signer-controlled clock-skew window. The signer therefore cannot be used to sign arbitrary payload bytes, future-dated grants outside the bounded skew, or capabilities omitted from its independent ceiling.

The signer policy should normally be the **intersection ceiling** of what the deployment intends to expose. In particular, operators can omit `shell`, `execute_process`, file-transfer, process-termination, or other dangerous capabilities even if a compromised Hub policy attempted to authorize them.

## Failure semantics

External signing has no in-process fallback. If the signer socket is absent, times out, rejects policy, or returns an invalid token, the Hub cancels that operation **before dispatch**, returns `grant_signing_unavailable`, emits `v2_grant_signing_failed` / `cumg.v2.grant_signing_failed`, and leaves the Agent session and desktop quarantine state unchanged. The signer emits payload-safe `v2_external_grant_signed` / `v2_external_grant_rejected` events and counters.

The packaged systemd split is a structural key-custody boundary:

- `cumg-v2-hub.service` receives the Hub transport key, TLS key, signer socket path, and grant **public** key. It does not receive `grant-secret`.
- `cumg-v2-grant-signer.service` runs as separate user `cumg-v2-signer`, receives `grant-secret` through `LoadCredentialEncrypted=`, exposes only an `AF_UNIX` socket, and has no network address family.
- the socket lives in the signer-owned runtime directory and is mode 0660; the signer policy must be root/operator-owned and non-group/other-writable.

`tests/v2_grant_signer_packaging.rs` locks this packaging invariant. `external_grant_signer_executes_without_hub_key_custody_and_has_no_fallback` runs the real signer binary and Hub/Agent gRPC/TLS path, proves an externally signed command executes, kills the signer, and proves the next operation is cancelled before dispatch with no local signing fallback or quarantine.

## Security claim and non-claim

External mode materially narrows a Hub-process compromise: the grant private key is not resident in Hub memory and cannot be extracted from the Hub process, signer absence prevents new signatures, and signer policy can independently deny exact capabilities or malformed/future-dated requests.

It does **not** make a fully compromised Hub harmless. A malicious Hub that still controls the Hub transport key and can reach a live signer may request any capability the signer policy intentionally allows. Therefore the signer policy is a second ceiling, not proof of user intent. Deployments needing per-operation human approval or hardware-backed authorization for dangerous capabilities should add that approval authority at the signer boundary; it is not silently claimed by this implementation.

## In-process fallback

For a consciously single-host/development deployment, configure only `CUMG_V2_GRANT_SECRET_FILE`. For external mode, omit that variable and configure both `CUMG_V2_GRANT_SIGNER_SOCKET` and `CUMG_V2_GRANT_PUBLIC_KEY_FILE` (plus optional `CUMG_V2_GRANT_SIGNER_TIMEOUT_SECS`). `v2_hub` rejects mixed or incomplete configurations.
