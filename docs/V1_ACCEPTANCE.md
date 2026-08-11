# V1 acceptance

V1 acceptance was completed on 2026-08-11. Normal CI covers automated/code-local closeout; operator-controlled acceptance covered both a trusted real macOS desktop and the authenticated Cloudflare Access/Tunnel + ChatGPT remote path.

The desktop acceptance is a **product-level real-desktop check**, not a requirement to operate a permanent GitHub self-hosted runner. The same fixture may be run either directly by a trusted operator on a logged-in TCC-granted Mac or through the manual self-hosted workflow. The repository keeps `.github/workflows/desktop-e2e.yml` as a repeatable automation path for teams that maintain a dedicated runner.

## 1. Trusted macOS desktop E2E — completed 2026-08-11

The acceptance fixture is `scripts/cua_desktop_e2e.py`. It is guarded by `CUMG_DESKTOP_E2E_ACK=1` because it performs real GUI actions.

Accepted execution modes:

- direct operator-controlled run on a trusted, logged-in macOS desktop with CuaDriver Accessibility and Screen Recording permissions; or
- `.github/workflows/desktop-e2e.yml` via `workflow_dispatch` on a dedicated `cua-desktop-e2e` self-hosted runner.

The 2026-08-11 V1 closeout used the first mode. No self-hosted runner was registered or claimed as part of this evidence.

Acceptance evidence:

- [x] the gateway started on loopback with a narrow six-tool E2E allowlist;
- [x] a fresh TextEdit instance opened a unique temporary text fixture;
- [x] the exact visible fixture window was selected after bounded window-readiness polling;
- [x] `get_window_state` returned screenshot evidence;
- [x] the editor was clicked using window-local screenshot coordinates derived from the observed AX frame and window bounds;
- [x] a unique `CUMG_DESKTOP_E2E_<timestamp>` marker was typed using the current Cua `element_token` / snapshot contract;
- [x] a fresh accessibility snapshot independently contained the marker;
- [x] TextEdit and the temporary fixture were cleaned up;
- [x] the script returned `PASS desktop E2E: gateway -> Cua -> TextEdit -> screenshot -> click -> type -> AX verify`.

The closeout run also hardened the fixture against three real macOS/Cua conditions discovered during execution: asynchronous TextEdit window creation, multiple helper/hidden windows in one TextEdit PID, and Cua 0.17+ refusal of bare `element_index` writes.

Do not upload screenshots or artifacts containing unrelated desktop data merely to prove a future rerun. Non-sensitive logs and the deterministic AX readback are sufficient unless debugging is required.

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

Both operator-controlled acceptance sections passed on 2026-08-11, so V1 is closed. No acceptance item was waived.

V2-M0 must still pass its separate competitor-gap / trust-model GO/NO-GO gate before major V2 implementation begins. V1 closure is not itself evidence that V2 should proceed.
