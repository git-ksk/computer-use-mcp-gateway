## Summary

<!-- What changes, and why does it belong in CUMG? -->

## Change class

- [ ] A — editorial / documentation-only
- [ ] B — normal implementation / maintenance
- [ ] C — public-contract / security-boundary / execution-safety
- [ ] D — requires privileged / physical-desktop acceptance

## Validation

- [ ] Required CI / deterministic checks are green or any limitation is documented.
- [ ] `git diff --check` is clean.
- [ ] Behavior changes include relevant tests.
- [ ] Security/schema/capability/compatibility changes update the relevant canonical docs.
- [ ] Paired Japanese docs are synchronized when normative meaning changes.
- [ ] No secret, credential, private endpoint, raw desktop payload, or sensitive provider error data is included.
- [ ] No ambiguous state-changing operation is made automatically retryable/replayable without a proven idempotency contract.

## Compatibility / release impact

- [ ] No release-version impact.
- [ ] PATCH-compatible change.
- [ ] MINOR-level public-contract expansion or pre-1.0 incompatibility.
- [ ] Breaking/migration notes are included where required.

## Roadmap impact

- [ ] Maintenance only; no roadmap change required.
- [ ] Updates an accepted roadmap item.
- [ ] Proposes a future-minor candidate; admission evidence is documented.
- [ ] Changes the product/non-goal boundary; rationale and governance review are included.
