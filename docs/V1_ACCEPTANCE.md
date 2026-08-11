# V1 acceptance

Automated/code-local V1 closeout is covered by normal CI. Two acceptance checks intentionally remain outside hosted CI because they require a trusted real desktop and the operator's authenticated remote environment.

Do not run either check on an unrestricted daily-use workstation or with production secrets committed to this repository.

## 1. Dedicated macOS desktop E2E

Use a dedicated test Mac, not a normal personal workstation.

Prerequisites:

- the machine is logged into an interactive macOS GUI session;
- CuaDriver is installed and working;
- CuaDriver has Accessibility and Screen Recording permissions;
- a GitHub Actions self-hosted runner is installed only on this dedicated machine;
- the runner has the `cua-desktop-e2e` label;
- the runner is not permitted to execute untrusted pull-request code.

Run `.github/workflows/desktop-e2e.yml` manually from `main` using `workflow_dispatch`.

Acceptance evidence:

- [ ] workflow runs from trusted `main`;
- [ ] it lands on the dedicated `cua-desktop-e2e` runner;
- [ ] TextEdit is launched fresh;
- [ ] screenshot evidence is obtained;
- [ ] the editor is clicked;
- [ ] a unique marker is typed;
- [ ] the marker is independently observed through accessibility state;
- [ ] TextEdit is cleaned up;
- [ ] the workflow completes successfully.

Do not upload screenshots or artifacts containing unrelated desktop data merely to prove the run. The workflow result plus its non-sensitive logs are sufficient unless debugging is required.

## 2. Cloudflare Access/Tunnel + ChatGPT remote MCP dogfood

First make sure the local gateway still works. Then follow [`DEPLOYMENT.md`](DEPLOYMENT.md) and [`CLIENTS.md`](CLIENTS.md).

Security requirements:

- keep `CUMG_BIND` on loopback;
- terminate remote HTTPS at the authenticated proxy/tunnel;
- enforce Cloudflare Access before requests reach the gateway;
- keep Host and Origin validation enabled;
- configure only the exact expected Host/Origin values or deliberate Host rewrite;
- expose only reviewed tools through the gateway allowlist;
- keep tunnel credentials, Access tokens, real private hostnames, and client secrets out of the repository and issue/PR logs.

Acceptance evidence:

- [ ] the remote MCP endpoint is HTTPS and requires authentication;
- [ ] an unauthenticated request is rejected before reaching the gateway;
- [ ] the authenticated endpoint reaches `/mcp` while the gateway itself remains on loopback;
- [ ] ChatGPT can connect to the remote MCP endpoint and discover only the expected policy-filtered tools;
- [ ] a harmless `observe` operation succeeds on the dedicated/test desktop;
- [ ] if an interaction is tested, it uses an explicitly reviewed low-risk tool and the intended target only;
- [ ] unexpected Host/Origin values remain rejected;
- [ ] no credentials or desktop content are exposed in normal gateway logs.

## Closing V1

Once both sections pass, update [`ROADMAP.md`](ROADMAP.md) to check the two remaining V1 acceptance items and record V1 as closed.

If one of these checks is deliberately waived rather than executed, record that decision explicitly instead of silently marking it complete. V2-M0 should not begin merely because V1 implementation is feature-complete; its separate GO/NO-GO gate still applies.
