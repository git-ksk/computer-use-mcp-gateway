# V2 typed semantic authorization

> English is canonical. [日本語版 / Japanese translation](V2_SEMANTIC_AUTHORIZATION.ja.md)

Status: **implemented for the `0.4.0` candidate by #221**.

## Purpose

CUMG already authorizes the exact tuple:

```text
AuthenticatedClientPrincipal -> stable device -> exact DeviceCapability
```

Typed semantic authorization adds a second, **narrow-only** decision at the same northbound execution boundary. It does not grant a capability. It can only reject a finalized command that already passed exact capability authorization.

The initial constraints are deliberately small and backend-neutral:

- `TypeText`: maximum UTF-8 byte length of the exact finalized text;
- `BrowserNavigate`: allowlist of normalized **requested origins**.

CUMG does not expose a regex/JSON-expression/OPA-like escape hatch in this contract.

## Decision boundary

Ordinary northbound execution follows this order:

```text
verified principal
    -> exact principal/device/DeviceCapability authorization
    -> parse + resolve + normalize CUMG semantic command
    -> typed semantic constraint evaluation
    -> private AuthorizedSemanticCommand
    -> Hub admission + durable bounded decision metadata
    -> stale snapshot fence immediately before durable dispatch
    -> provider materialization / Agent dispatch
```

`AuthorizedSemanticCommand` is private to the northbound implementation and owns the exact `DeviceCommand` that was evaluated. The ordinary northbound execution function consumes that wrapper rather than a raw `DeviceCommand`, so constrained fields are not reconstructed from caller arguments after allow.

Tool discovery is advisory only. Exact capability authorization is repeated at the finalized-command seam and remains mandatory even when a semantic constraint permits the subject.

## Operator policy file

Configure the optional policy with:

```text
CUMG_V2_SEMANTIC_CONSTRAINT_POLICY_FILE=/private/path/semantic-constraints.json
```

`v2_hub` loads the file through the existing trusted/private-file boundary. Maximum file size is 64 KiB. JSON is strict: unknown fields, unknown rule kinds, duplicate rules for one capability, malformed values, revision `0`, invalid rule IDs, invalid origins, or bounds above the protocol ceiling fail startup/configuration closed.

Example:

```json
{
  "revision": 12,
  "rules": [
    {
      "kind": "type_text_max_utf8_bytes",
      "rule_id": "interactive-text-small",
      "max_utf8_bytes": 4096
    },
    {
      "kind": "browser_navigate_requested_origins",
      "rule_id": "browser-prod-origins",
      "allowed_origins": [
        "https://example.com",
        "https://admin.example.com:8443"
      ]
    }
  ]
}
```

The policy file is optional. When it is absent, the existing exact capability policy remains the authorization contract. When it is present, only capabilities with an admitted typed rule gain an additional semantic ceiling; other capabilities keep exact-capability authorization only. There is currently no caller-, Agent-, or session-supplied semantic policy input.

A future narrower deployment/session layer is acceptable only as an intersection with this operator ceiling. It must not widen it.

## Snapshot identity and changes

A valid policy is canonicalized and assigned a SHA-256 snapshot digest. The Hub installs exactly one `(revision, digest)` identity for its runtime.

- installing the exact same `(revision, digest)` is idempotent;
- the same revision with different contents is rejected;
- a different revision in the running Hub is rejected;
- changing the policy requires a reviewed Hub restart/revision transition;
- there is no Agent- or caller-facing hot-reload/widening endpoint.

The digest identifies reviewed policy content; it is not a fingerprint of text, URLs, or other constrained request values.

## Constraint semantics

### `TypeText` byte ceiling

The constraint evaluates the exact finalized `DeviceCommand::TypeText` / `TypeTextAdvanced` UTF-8 bytes. The limit must be within the existing protocol maximum. The allowed string flows unchanged into the current Cua adapter's `text` argument; authorization is not performed on one string and execution on a transformed string.

### `BrowserNavigate` requested-origin allowlist

Configured origins must be absolute HTTP(S) origins without credentials, path, query, or fragment. CUMG normalizes the requested navigation URL using a typed URL parser and compares its serialized origin with the allowlist. Default ports therefore normalize consistently.

This is **requested-origin authorization**, not redirect confinement. The current `NavigationCompleted` contract proves the requested URL transition but does not prove or enforce the final post-redirect origin. CUMG does not claim that cross-origin redirects are confined. Such a claim would require a separately enforceable backend contract that can prevent or attest redirect outcomes.

`about:` navigation is not covered by the requested-origin rule. If a deployment configures an origin constraint for `BrowserNavigate`, a navigation without a sound HTTP(S) origin fails closed as an unsupported semantic subject.

## Durable audit and privacy

An allowed constrained operation persists only bounded decision metadata:

- policy revision;
- 64-hex-character snapshot digest;
- fixed constraint kind;
- bounded operator rule ID.

Raw typed text, requested URLs, policy contents, backend/private refs, credentials, tokens, screenshots, clipboard data, and provider payloads are not stored in semantic-authorization evidence.

Denied calls return a fixed `semantic_constraint_denied` category plus bounded revision/snapshot/reason metadata. They do not create an execution operation and do not become `Indeterminate`, because provider dispatch has not occurred.

## Stale-decision fence

The operation admission record binds the exact `(revision, digest)` that allowed the finalized command. Immediately before the durable dispatch boundary, the Hub compares that identity with the active immutable snapshot.

If they differ, the not-yet-dispatched operation is cancelled with `semantic_constraint_snapshot_stale`:

- no provider dispatch occurs;
- `dispatched_at` remains absent;
- no dispatch binding is created;
- no quarantine or `Indeterminate` state is created;
- the bounded original decision evidence remains available for audit.

The production runtime does not hot-reload snapshots, but the stale fence is still enforced defensively and is covered by a forced-divergence test.

## Durable schema

#221 raises execution-safety durable schema from v11 to **v12** to persist semantic-constraint admission evidence in active/terminal operation state and bounded recovery archives.

Older snapshots without semantic evidence remain migratable. A v12 snapshot that contains semantic-constraint evidence cannot be downgraded to v11 or earlier, because doing so would silently discard the admission record that explains which reviewed snapshot authorized the command.

## Security boundary and non-claims

Semantic constraints narrow what an authenticated caller may cause an uncompromised Hub to admit. They do **not**:

- replace exact `DeviceCapability` authorization;
- make the external grant signer argument-aware;
- make the Agent independently enforce text/origin policy;
- make a fully compromised Hub harmless;
- replace Handoff, local-user recovery, quarantine, or no-auto-replay;
- create a filesystem/process/browser sandbox;
- provide redirect confinement;
- provide a generic policy language.

The external signer remains an independent exact device/capability/TTL ceiling. Independent semantic enforcement against a compromised Hub would require a separately reviewed protocol change that binds signed semantic subjects at another authority boundary.

## Evidence

Automated coverage includes:

- final UTF-8 byte evaluation and requested-origin normalization;
- strict malformed/unknown/duplicate policy rejection;
- exact capability authorization remaining mandatory at the finalized-command seam;
- privacy-bounded denial output without raw constrained values;
- exact revision+digest snapshot immutability;
- forced stale-snapshot cancellation before dispatch with no `Indeterminate`/quarantine;
- execution-safety v12 persistence/migration checks;
- TypeText and BrowserNavigate provider-materialization preservation tests;
- existing northbound, recovery, cancellation, ambiguity, and no-replay regression suites.
