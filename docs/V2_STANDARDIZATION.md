# V2 standard-first boundaries

Status: **accepted design direction (2026-08-12)**. This document records the long-term boundary between standards the project should adopt and custom control semantics that remain part of the product.

## Decision

V2 follows a **standard-first, custom-semantics-only-where-needed** rule.

Do not build custom infrastructure merely because the current single-device implementation can. Prefer maintained standards for transport, northbound authentication, observability, certificate lifecycle, and operating-system service management. Keep project-specific protocol semantics only where the relevant standards do not define the safety property this control plane needs.

The target layering is:

```text
MCP Client
   |
   | MCP Authorization / OAuth
   v
Hub
   |
   +-- authenticated principal + local policy
   +-- admission / replay / indeterminate state
   +-- short-lived exact device-capability grant
   |
   | gRPC bidirectional streaming
   | TLS; workload identity remains replaceable
   v
Agent
   |
   +-- exact grant enforcement
   +-- replay barrier / generation checks
   +-- bounded execution and cancellation
   +-- backend-neutral capability surface
```

## Standards to prefer

### Northbound MCP authentication

Use the MCP Authorization model and its OAuth-based protected-resource semantics rather than inventing a separate public authentication protocol.

The Hub should adapt the validated northbound identity into `AuthenticatedClientPrincipal`, then apply the existing local principal -> device -> exact capability authorization policy.

The OAuth access token is **not** a Hub-to-Agent credential and must not be forwarded to the Agent. Northbound client authorization and southbound Agent grants are separate trust domains.

Long-term boundary:

```text
OAuth / MCP Authorization
        |
        v
AuthenticatedClientPrincipal
        |
        v
Hub authorization policy
        |
        v
short-lived exact DeviceCapability grant
```

The internal exact-capability grant remains necessary because OAuth scopes do not define this project's generation, replay, one-shot grant, per-device operation, or ambiguous-execution semantics.

### Hub-Agent transport

Keep gRPC bidirectional streaming over TLS as the production transport candidate. Keep the application command/grant semantics transport-neutral.

The raw TLS + signed-JSON implementation remains useful as a regression/reference transport, not as the long-term production framing standard.

Use gRPC's standard status model where it matches the failure being represented, including authentication, authorization, resource exhaustion, cancellation, and availability failures. Do not invent duplicate transport error vocabularies when a gRPC status already expresses the same boundary.

### TLS certificate lifecycle

Prefer standard certificate automation such as ACME for publicly terminated Hub TLS where applicable. Do not build a bespoke certificate-renewal protocol merely to rotate ordinary server certificates.

Application identity and grant-signing rotation remain separate from TLS certificate renewal.

### Workload identity / SPIFFE

Do **not** require SPIRE for the current single-Hub/small-Agent M1 deployment. Adding SPIRE Server/Agent infrastructure now may cost more operational complexity than it removes.

However, the identity design must not become permanently coupled to provisioned Ed25519 files. Introduce or preserve abstraction boundaries so a future identity verifier/provider can support alternatives such as SPIFFE X.509-SVID without rewriting operation, grant, replay, or execution semantics.

Conceptually, identity should be replaceable behind interfaces equivalent to:

```text
AgentIdentity / AgentIdentityVerifier
    +-- provisioned Ed25519
    +-- future SPIFFE/X.509 identity

GrantIssuer / GrantVerifier
    +-- local Ed25519 signer
    +-- future KMS/HSM-backed signer
```

SPIFFE/SPIRE becomes worth serious adoption when V2-M2 introduces enough machines, environments, or trust domains that manual/provisioned workload identity becomes an operational liability.

### Observability

Prefer OpenTelemetry/OTLP as the long-term logs/metrics/traces integration model instead of growing a project-specific telemetry protocol.

Use standard semantic attributes where possible and add `cumg.*` attributes only for concepts that are genuinely specific to this control plane, such as capability, device generation, operation state, or indeterminate quarantine.

Never include raw shell commands, argv, file contents, screenshots, clipboard contents, credentials, or other sensitive operation payloads in default telemetry.

### Service management

Use operating-system service managers rather than implementing a custom supervisor:

- macOS: launchd / LaunchAgent where the Agent needs the interactive user session;
- Linux: systemd;
- other platforms: the platform-native service mechanism when added.

The service package should own restart policy, state/secret directory permissions, environment/config loading, log routing, and upgrade/restart procedures without changing Agent protocol semantics.

### Rate and connection limits

Use standard HTTP/gRPC failure semantics, but keep the actual admission policy local to the Hub.

Token-bucket/rate-window details are implementation choices. Existing Hub concurrency, queue, lease, and load-shedding semantics remain authoritative for whether work is admitted. Rate limits must compose with them rather than create a second conflicting scheduler.

## Custom semantics to keep

The following are intentionally project-owned unless a mature standard later provides equivalent semantics **without weakening the current safety guarantees**:

- exact `DeviceCapability` authorization and one-shot short-lived grants;
- device generation and capability revision checks;
- explicit operation IDs;
- per-device lease / admission ownership;
- replay rejection and bounded replay tombstones;
- restart conversion of ambiguous in-flight work into fail-closed state;
- `indeterminate` operation state and device quarantine;
- explicit resolution of ambiguous execution outcomes;
- no automatic replay of possibly state-changing work;
- cancellation semantics that distinguish requested cancellation from proven non-execution / proven termination;
- backend-neutral typed command/result semantics;
- policy evidence without logging sensitive operation payloads.

These are not replacements for OAuth, gRPC, TLS, or SPIFFE. They sit above or beside those standards and encode the delegated-device safety properties that those standards do not provide.

## Identity decision

The existing independently signed Ed25519 application messages remain acceptable for M1. Do not remove them merely because TLS authenticates the carrier: they currently provide transport-independent application identity and bind session/command/result/cancellation semantics.

At the same time, do not make Ed25519 file provisioning a permanent architectural requirement. Separate:

1. **identity semantics** — who is the Hub/Agent and what message/session is authenticated;
2. **credential implementation** — Ed25519 file, OS key store, KMS/HSM, SPIFFE SVID, or another reviewed mechanism.

Future migration may replace the credential implementation without replacing operation-state semantics.

## What must not be collapsed together

Do not collapse these boundaries merely to reduce code:

- MCP/OAuth client authorization and Hub-to-Agent grants;
- TLS certificate identity and application operation authorization;
- gRPC cancellation and proof that a real desktop side effect stopped;
- rate limiting and operation admission/lease ownership;
- observability identifiers and raw operation payload logging.

In particular, **never forward a northbound OAuth bearer token to an Agent** as a substitute for an Agent-scoped capability grant.

## Real Cua cancellation

Transport-level cancellation can use standard MCP/gRPC mechanisms, but real desktop cancellation acceptance remains backend-specific.

A cancellation request is not proof that a click, drag, keystroke sequence, or other desktop side effect did not execute. If the backend cannot provide sufficient evidence, retain the current `indeterminate` outcome and device quarantine rather than converting uncertainty into success.

This is an intentional custom safety semantic, not missing standardization work.

## Migration policy

Existing custom implementation should be classified into three buckets during future work:

1. **Keep** — product-specific safety semantics listed above.
2. **Adapt** — current credential/identity implementations that should sit behind replaceable interfaces.
3. **Retire** — custom infrastructure superseded by a maintained standard, once equivalent behavior is proven by tests.

Do not perform rewrites solely for architectural fashion. A standardization migration must preserve or improve the existing security property, retain regression evidence, and avoid combining unrelated protocol changes in one step.

## M1 implementation order after Shell

The preferred remaining M1 order is:

1. northbound MCP Authorization integration using the standard OAuth-based MCP boundary;
2. production TLS/secret lifecycle, using standard certificate automation where applicable and keeping application identity replaceable;
3. Hub connection/rate limits plus OpenTelemetry-oriented observability;
4. launchd/systemd packaging around the existing long-lived runtimes;
5. real-Cua cancellation acceptance;
6. V2-M1 acceptance review before any V2-M2 multi-machine expansion.

## V2-M1 completion against this policy

V2-M1 was accepted on 2026-08-12 without collapsing the boundaries above:

- northbound authentication uses MCP/OAuth protected-resource semantics and still reduces identity to `AuthenticatedClientPrincipal`; bearer tokens are not forwarded southbound;
- Hub↔Agent remains gRPC bidirectional streaming over TLS with independently signed application messages;
- ordinary server-certificate renewal is ACME-driven, while Hub/device/grant identities retain separate signed rotation lifecycles;
- overload shedding uses standard gRPC/HTTP failures and leaves the existing operation admission/lease controller authoritative;
- observability uses OpenTelemetry/OTLP standard configuration rather than a project-specific telemetry transport;
- launchd/systemd own service supervision;
- real-Cua cancellation still resolves to `indeterminate` + quarantine when the backend cannot prove non-execution.

Acceptance evidence is in [`V2_M1_ACCEPTANCE.md`](V2_M1_ACCEPTANCE.md). Future work should treat this document as the architectural boundary, not as a reason to replace proven custom safety semantics with nominally standard but weaker mechanisms.

## V2-M2 trigger for SPIFFE reconsideration

Re-evaluate SPIFFE/SPIRE before or during V2-M2 when one or more of these become true:

- machine count makes manual identity provisioning operationally significant;
- multiple environments/trust domains require automated workload attestation;
- short-lived workload credentials are required operationally;
- certificate/key rotation becomes a fleet-management problem;
- external deployments need interoperable workload identity rather than repository-specific provisioning.

Until then, maintain SPIFFE-compatible architectural seams without requiring SPIRE operationally.

## Review rule

When adding a new V2 subsystem, ask in this order:

1. Is there a maintained protocol/platform standard that already owns this concern?
2. Does adopting it preserve the project's current security property?
3. If yes, use or adapt the standard instead of creating a parallel protocol.
4. If no, document exactly which delegated-device semantic is missing and keep the custom surface as narrow and transport-neutral as possible.

This rule applies to both M1 completion and later M2/M3 work.
