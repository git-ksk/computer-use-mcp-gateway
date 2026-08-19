from pathlib import Path
import re


def read(path):
    return Path(path).read_text()


def write(path, text):
    Path(path).write_text(text)


def sub(path, pattern, repl, count=1, flags=0):
    text = read(path)
    new, n = re.subn(pattern, repl, text, count=count, flags=flags)
    if n != count:
        raise SystemExit(f"{path}: expected {count} replacement(s), got {n}: {pattern}")
    write(path, new)


sub(
    "README.md",
    r"The Hub owns admission, authorization, operation state, replay barriers, and durable `indeterminate` quarantine\. The Agent owns the authenticated device session and local execution boundary\. Optional usage accounting is separate accounting authority and cannot authorize execution, clear quarantine, or permit replay\.",
    "The Hub owns admission, authorization, operation state, replay barriers, and durable `indeterminate` quarantine. The Agent owns the authenticated device session and local execution boundary. Deployment-specific quota, billing, or usage controls belong outside CUMG and do not gain execution, replay, quarantine, or recovery authority.",
)
sub(
    "README.ja.md",
    r"Hub は admission、authorization、operation state、replay barrier、永続的な `indeterminate` quarantine を所有します。Agent は認証済み device session とローカル実行境界を所有します。任意の usage accounting は独立した accounting authority であり、実行を認可したり、quarantine を解除したり、replay を許可したりする権限はありません。",
    "Hub は admission、authorization、operation state、replay barrier、永続的な `indeterminate` quarantine を所有します。Agent は認証済み device session とローカル実行境界を所有します。quota、billing、usage control は deployment 側で外付けする責務であり、CUMG の execution、replay、quarantine、recovery authority を得ることはありません。",
)

sub(
    "docs/ARCHITECTURE.md",
    r"## V2 runtime and optional accounting seam\n\nThe actual northbound runtime is now the V2 Hub\. The default binary and the explicit `v2_hub` binary share that entrypoint; `v2_agent` remains a separate outbound desktop process\. The old single-process V1 entrypoint is preserved as `v1_gateway`\.\n\nThe usage seam is deliberately outside the authoritative execution controller:.*?The bridge is CUMG-owned, loopback-only, and sends no bearer token or tool payload\. The initial implementation uses `mcp-usage-control` `MemoryUsageStore`; see \[`V2_USAGE_ACCOUNTING\.md`\]\(v2/V2_USAGE_ACCOUNTING\.md\)\.\n",
    "## V2 runtime\n\nThe actual northbound runtime is now the V2 Hub. The default binary and the explicit `v2_hub` binary share that entrypoint; `v2_agent` remains a separate outbound desktop process. The old single-process V1 entrypoint is preserved as `v1_gateway`.\n\nQuota, billing, and usage accounting are deployment-layer concerns outside the CUMG core. A reverse proxy, MCP edge, or other operator-controlled component may enforce them before requests reach the Hub, but that component cannot alter CUMG operation identity, authorization, generation fencing, durable execution state, quarantine, replay admission, or recovery.\n",
    flags=re.S,
)
sub(
    "docs/ARCHITECTURE.ja.md",
    r"## V2 runtime と optional accounting seam\n\n実際の northbound runtime は現在 V2 Hub です。default binary と explicit `v2_hub` binary は同じ entrypoint を共有し、`v2_agent` は別の outbound desktop process のままです。旧 single-process V1 entrypoint は `v1_gateway` として保持されています。\n\nusage seam は authoritative execution controller の外側に意図的に置きます。.*?bridge は CUMG-owned / loopback-only で、bearer token / tool payload を送りません。initial implementation は `mcp-usage-control` の `MemoryUsageStore` を使います。\[`V2_USAGE_ACCOUNTING\.md`\]\(v2/V2_USAGE_ACCOUNTING\.md\) を参照してください。\n",
    "## V2 runtime\n\n実際の northbound runtime は現在 V2 Hub です。default binary と explicit `v2_hub` binary は同じ entrypoint を共有し、`v2_agent` は別の outbound desktop process のままです。旧 single-process V1 entrypoint は `v1_gateway` として保持されています。\n\nquota、billing、usage accounting は CUMG core の外側にある deployment-layer の責務です。reverse proxy、MCP edge、その他 operator-controlled component で Hub 到達前に制御できますが、その component は CUMG の operation identity、authorization、generation fencing、durable execution state、quarantine、replay admission、recovery を変更できません。\n",
    flags=re.S,
)

for path in ["docs/GETTING_STARTED.md", "docs/GETTING_STARTED.ja.md"]:
    text = read(path)
    new, n = re.subn(r"\n## 9\..*?\n## 10\. ", "\n## 9. ", text, count=1, flags=re.S)
    if n != 1:
        raise SystemExit(f"{path}: could not remove usage setup section")
    new = re.sub(r"(?m)^- .*optional usage accounting.*\n", "", new)
    new = re.sub(r"(?m)^- .*usage accounting.*有効.*\n", "", new)
    write(path, new)

for path in ["docs/SECURITY.md", "docs/SECURITY.ja.md"]:
    text = read(path)
    new, n = re.subn(r"\n### Optional MCPUsage security boundary\n.*?(?=\n## )", "\n", text, count=1, flags=re.S)
    if n != 1:
        raise SystemExit(f"{path}: could not remove MCPUsage security section")
    write(path, new)

path = "docs/TESTING.md"
text = read(path)
text = re.sub(r"\nV2 usage integration adds deterministic tests.*?rejection of accidental payload fields\.\n", "\n", text, count=1, flags=re.S)
text, n = re.subn(r"\n### Optional MCPUsage sidecar test\n.*?(?=\n### |\n## )", "\n", text, count=1, flags=re.S)
if n != 1:
    raise SystemExit("docs/TESTING.md: could not remove sidecar test section")
write(path, text)

path = "docs/DEPLOYMENT.md"
text = read(path)
text, n = re.subn(r"\n#### Optional MemoryUsageStore sidecar\n.*?(?=\n### TLS renewal)", "\n", text, count=1, flags=re.S)
if n != 1:
    text, n = re.subn(r"\nUsage accounting is disabled by default\. To enable it, install `packaging/systemd/cumg-v2-usage-sidecar\.service`.*?Do not use this Memory store as a financial ledger\. See \[`V2_USAGE_ACCOUNTING\.md`\]\(V2_USAGE_ACCOUNTING\.md\)\.\n", "\n", text, count=1, flags=re.S)
if n != 1:
    raise SystemExit("docs/DEPLOYMENT.md: could not remove sidecar deployment section")
write(path, text)

path = "packaging/README.md"
text = read(path)
text, n = re.subn(r"\n## Optional MCPUsage Memory sidecar\n.*?\Z", "\n", text, count=1, flags=re.S)
if n != 1:
    raise SystemExit("packaging/README.md: could not remove sidecar section")
write(path, text)

path = ".env.example"
text = read(path)
text, n = re.subn(r"\n# Optional V2 runtime/session usage accounting\..*?# Sidecar configuration lives in packaging/systemd/usage\.env\.example\.\n?", "\n", text, count=1, flags=re.S)
if n != 1:
    raise SystemExit(".env.example: could not remove usage env block")
write(path, text)

path = "packaging/systemd/hub.env.example"
text = read(path)
text = re.sub(r"(?m)^# .*usage.*\n", "", text)
text = re.sub(r"(?m)^# CUMG_V2_USAGE_.*\n", "", text)
write(path, text)

for path in [
    "src/v2_usage.rs",
    "docs/v2/V2_USAGE_ACCOUNTING.md",
    "packaging/systemd/cumg-v2-usage-sidecar.service",
    "packaging/systemd/cumg-v2-hub-usage.conf.example",
    "packaging/systemd/usage.env.example",
    "integrations/mcp-usage-control-sidecar/README.md",
    "integrations/mcp-usage-control-sidecar/server.mjs",
    "integrations/mcp-usage-control-sidecar/server.test.mjs",
]:
    p = Path(path)
    if p.exists():
        p.unlink()
