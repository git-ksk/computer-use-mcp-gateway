# V1 acceptance

Automated/code-local V1 closeout is covered by normal CI. The authenticated Cloudflare Access/Tunnel + ChatGPT remote dogfood was completed on 2026-08-11. One acceptance check remains outside hosted CI because it requires a trusted dedicated real desktop.

Do not run the remaining dedicated-desktop check on an unrestricted daily-use workstation or with production secrets committed to this repository.

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

## 2. Cloudflare Access/Tunnel + ChatGPT remote MCP dogfood — completed 2026-08-11

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

- [x] the remote MCP endpoint is HTTPS and requires authentication;
- [x] an unauthenticated request is rejected before reaching the gateway;
- [x] the authenticated endpoint reaches `/mcp` while the gateway itself remains on loopback;
- [x] ChatGPT can connect to the remote MCP endpoint and discover only the expected policy-filtered tools;
- [x] a harmless `observe` operation succeeds on the dedicated/test desktop;
- [x] no interaction was required for this acceptance run; the ChatGPT acceptance path remained observe-only;
- [x] unexpected Host/Origin values remain rejected;
- [x] no credentials or desktop content are exposed in normal gateway logs.

Recorded acceptance evidence (2026-08-11):

- Cloudflare Tunnel `n8n-gcp` was healthy and routed `cua-gateway.cloud-sokuho.com` to `http://127.0.0.1:8101`; Access validation was required at the tunnel origin request and the gateway listener remained loopback-only.
- The Access application `Computer Use MCP Gateway` required authentication and used the operator-only allow policy. Unauthenticated requests to both `/mcp` and `/healthz` returned HTTP 401 before reaching the origin.
- ChatGPT successfully called the authenticated remote MCP endpoint after a gateway restart; `get_screen_size` returned `1920x1080` at scale `1.0`. During the original read-only allowlist dogfood, ChatGPT discovered exactly the four expected exposed tools. A later operator change to `--allow-tools *` expanded the live gateway to 54 tools; the existing ChatGPT conversation retained its earlier tool-schema cache until reconnect/re-discovery, so that later cache state is not used as the policy-filtering acceptance assertion.
- Production-loopback probes with a malicious `Origin` and malicious `Host` both returned HTTP 403.
- Gateway log review found tool name/class/policy/outcome/duration metadata and backend handshake instructions, but no authorization tokens, credentials, tool arguments/results, screenshot/image payloads, or desktop content.

## Closing V1

The remote-access section is complete. Once the dedicated macOS desktop E2E section passes, update [`ROADMAP.md`](ROADMAP.md) to check the final V1 acceptance item and record V1 as closed.

If one of these checks is deliberately waived rather than executed, record that decision explicitly instead of silently marking it complete. V2-M0 should not begin merely because V1 implementation is feature-complete; its separate GO/NO-GO gate still applies.
