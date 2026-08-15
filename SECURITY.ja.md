# セキュリティ報告ポリシー

> この日本語版は [`SECURITY.md`](SECURITY.md) の翻訳です。**英語版を canonical（正典）とし、解釈に差がある場合は英語版を優先します。**

この文書は `computer-use-mcp-gateway`（CUMG）の脆弱性報告方法を定義します。runtime security model と invariant は [`docs/SECURITY.ja.md`](docs/SECURITY.ja.md) と [`docs/v2/V2_THREAT_MODEL.ja.md`](docs/v2/V2_THREAT_MODEL.ja.md) に分離して記載しています。

## Supported versions

1.0 より前は、[`docs/VERSIONING.ja.md`](docs/VERSIONING.ja.md) の定義どおり、latest released minor line のみを actively supported line とします。

| Version | Security support |
| --- | --- |
| `0.2.x` | Supported |
| `< 0.2` | Not actively supported |

より新しい minor release が存在する場合、この表の更新が追いついていなくても、その latest minor line を actively supported line として扱います。

## 脆弱性の報告

**脆弱性の疑いがある内容を public issue に投稿しないでください。**

repository の **Security → Advisories → Report a vulnerability** から GitHub Private Vulnerability Reporting を使用してください。初期報告を repository administrator / security manager に限定して非公開で扱えます。

可能な範囲で次を含めてください。

- affected CUMG version、tag、commit;
- affected platform と deployment shape;
- 関係する security boundary / capability;
- 不要な sensitive data を含めない最小 reproduction step または proof of concept;
- expected behavior と observed behavior;
- 想定される impact と prerequisite;
- Cua Driver など upstream dependency にも同じ問題が存在するか。

問題の実証だけを目的として、実 credential、access token、private endpoint、無関係な desktop content、第三者の personal data を含めないでください。

## Security issue の scope

例:

- authentication / authorization bypass;
- capability escalation / scope confusion;
- unsafe reuse や duplicate mutation を許可し得る replay、stale-generation、settlement、quarantine、explicit-resolution failure;
- secret、credential、sensitive desktop payload、raw provider error の漏えい;
- path traversal、symlink escape、staging boundary escape、unsafe file-transfer behavior;
- explicit grant の外側で発生する remote code execution / command execution;
- CUMG 固有の supply-chain / release-integrity weakness;
- documented security boundary の fail-open condition。

通常の setup problem、expected policy refusal、feature request、非security bug は public issue form を使用してください。

root cause が明確に upstream project にあり、CUMG が impact を弱めたり別の形で露出したりしていない場合は upstream へ報告してください。CUMG によって impact が変わるか不明な場合は、まず private CUMG reporting channel を使用してください。

## Handling と disclosure

report は best-effort で triage し、固定の response / remediation SLA は約束しません。maintainer は coordinated disclosure を優先し、practical な範囲で再現し、fix / mitigation が利用可能になる前に exploit-sensitive detail を公開せず、必要に応じて GitHub Security Advisory / CVE workflow を使用します。

security fix でも [`docs/PROJECT_GOVERNANCE.ja.md`](docs/PROJECT_GOVERNANCE.ja.md) と [`docs/VERSIONING.ja.md`](docs/VERSIONING.ja.md) の execution-safety invariant / release rule を維持します。compatibility を維持すること自体が vulnerability を残す security emergency では break を許可しますが、exploit-sensitive detail を不必要に早く公開せず、その break を文書化します。
