# Security policy

> English is the canonical security-reporting policy. [日本語版 / Japanese translation](SECURITY.ja.md)

This file explains how to report vulnerabilities in `computer-use-mcp-gateway` (CUMG). The runtime security model and invariants are documented separately in [`docs/SECURITY.md`](docs/SECURITY.md) and [`docs/v2/V2_THREAT_MODEL.md`](docs/v2/V2_THREAT_MODEL.md).

## Supported versions

Before 1.0, only the latest released minor line is actively supported, as defined in [`docs/VERSIONING.md`](docs/VERSIONING.md).

| Version | Security support |
| --- | --- |
| `0.2.x` | Supported |
| `< 0.2` | Not actively supported |

If a newer minor release exists, treat that latest minor line as the actively supported line even if this table has not yet been updated.

## Reporting a vulnerability

**Do not open a public issue for a suspected vulnerability.**

Use GitHub Private Vulnerability Reporting from the repository **Security → Advisories → Report a vulnerability** flow. This keeps the initial report private to repository administrators and security managers.

Include, when available:

- affected CUMG version, tag, or commit;
- affected platform and deployment shape;
- the security boundary or capability involved;
- minimal reproduction steps or a proof of concept that avoids unnecessary sensitive data;
- expected versus observed behavior;
- likely impact and prerequisites;
- whether the issue also appears to exist in an upstream dependency such as Cua Driver.

Never include real credentials, access tokens, private endpoints, unrelated desktop contents, or third-party personal data merely to demonstrate an issue.

## Security issues in scope

Examples include:

- authentication or authorization bypass;
- capability escalation or scope confusion;
- replay, stale-generation, settlement, quarantine, or explicit-resolution failures that could permit unsafe reuse or duplicate mutation;
- secret, credential, sensitive desktop payload, or raw provider-error disclosure;
- path traversal, symlink escape, staging-boundary escape, or unsafe file-transfer behavior;
- remote code execution or command execution outside an explicitly granted capability;
- supply-chain or release-integrity weaknesses specific to CUMG;
- a fail-open condition in a documented security boundary.

Ordinary setup problems, expected policy refusals, feature requests, and non-security bugs belong in the public issue forms instead.

If the root cause is clearly an upstream project and CUMG does not weaken or expose it differently, report it to that upstream project. If you are unsure whether CUMG changes the impact, use the private CUMG reporting channel first.

## Handling and disclosure

Reports are triaged on a best-effort basis; this project does not promise a fixed response or remediation SLA. The maintainer will prefer coordinated disclosure, reproduce the issue where practical, avoid publishing exploit-sensitive details before a fix or mitigation is available, and use a GitHub Security Advisory/CVE workflow when appropriate.

Security fixes must preserve the execution-safety invariants and release rules in [`docs/PROJECT_GOVERNANCE.md`](docs/PROJECT_GOVERNANCE.md) and [`docs/VERSIONING.md`](docs/VERSIONING.md). Compatibility may be broken in a security emergency when preserving it would preserve the vulnerability; such a break must be documented without prematurely disclosing exploit-sensitive details.
