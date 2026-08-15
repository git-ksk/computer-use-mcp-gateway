# プロジェクト運営方針

> この日本語版は [`PROJECT_GOVERNANCE.md`](PROJECT_GOVERNANCE.md) の翻訳です。**英語版を canonical（正典）とし、解釈に差がある場合は英語版を優先します。**

## 対象範囲

この文書は `computer-use-mcp-gateway`（CUMG）の保守、review、merge、release の進め方を定義します。runtime security model 自体は置き換えません。security model は [`SECURITY.ja.md`](SECURITY.ja.md)、[`v2/V2_THREAT_MODEL.ja.md`](v2/V2_THREAT_MODEL.ja.md)、product boundary は [`v2/V2_POSITIONING.ja.md`](v2/V2_POSITIONING.ja.md) が canonical な参照先です。

## Maintainer model

CUMG は現在 maintainer-led model で運営します。

- maintainer が scope、security boundary、compatibility、release timing、merge の最終判断を行います。
- external review / design discussion は歓迎しますが、合意がないことだけを理由に無期限で判断を止めません。
- documented execution-safety invariant を黙って弱めたり、acceptance evidence を捏造したり、未実装 capability を実装済みとして扱ったりしてはいけません。
- security-significant な意見の相違は code、test、acceptance result、安全に保持できる protocol trace、authoritative upstream documentation などの evidence で解決します。

maintainer が増える場合、mandatory multi-maintainer approval rule を導入する前にこの文書を更新します。

## Project invariant

明示的に review された replacement が同等以上の safety property を証明しない限り、変更は次を維持します。

1. ambiguous な state-changing work を automatic retry / replay しない;
2. post-dispatch uncertainty は authoritative な CUMG `Indeterminate` / quarantine / explicit-resolution path に収束させる;
3. 適用される exact principal、device、capability、operation identity、revision、generation fence を authoritative とする;
4. raw backend/provider authority を generic escape hatch として northbound に公開しない;
5. secret、credential、private endpoint、raw desktop payload、sensitive provider error を通常の log、telemetry、example、acceptance artifact に含めない;
6. compatibility claim は version が近いという推測ではなく reproducible validation で裏付ける。

## Change class と gate

### A. Editorial / documentation-only

必須: documentation validation、`git diff --check`、source/config/workflow change が accidental に混入していないことの確認。capability status、schema meaning、security semantics、compatibility、release policy を変える documentation change は normative なので、より強い gate を適用します。

### B. Normal implementation / maintenance

必須: normal CI、behavior change に対応する relevant deterministic test、intentional な `Cargo.lock` update、user-visible behavior / compatibility change に対応する documentation。

### C. Public-contract / security-boundary / execution-safety

B に加えて、failure / downgrade behavior の explicit review、relevant threat-model/security/status/parity/architecture docs の更新、targeted regression coverage、claim に必要な acceptance evidence を要求します。ambiguity を safe completion、safe cancellation、safe retry、replay permission と推測せず fail closed します。

### D. Privileged / physical-desktop acceptance

real desktop、TCC、GUI、provider、privileged-host behavior に依存する claim は reviewed trusted-host acceptance path を必要とします。untrusted pull-request code を privileged self-hosted desktop runner で実行してはいけません。

## Pull request と `main`

- `main` を release source of truth とします。
- 通常の change は pull request 経由で `main` に入れます。direct push は通常 workflow に含めません。
- 1 PR は1つの coherent change または密接に結び付いた change set を表すようにします。
- required CI check が green で、review thread が resolve されるまで merge しません。
- maintainer が1人の間は external approval を mandatory にしません。maintainer 自身が独立して満たせない形式的な governance requirement を避けるためです。
- `main` に PR ごとの logical commit を1つ残すため、通常は squash merge を使います。
- intentionally long-lived な branch を除き、merge 後に branch を削除します。

repository を安全に復旧するため通常 flow が使えない emergency repair の場合だけ例外を許します。復旧後は状態を記録し、直ちに通常の PR path に戻します。

## Documentation と localization

- English documentation を canonical とします。
- paired English/Japanese document の normative meaning を変える場合は、同じ PR で両方更新します。security semantics、schema version、capability status、compatibility、release policy を含みます。
- editorial follow-up は別でも構いませんが、reciprocal link と heading structure は対応を維持します。
- historical/archive document は historical status が明示されていれば過去時点の wording を保持できます。

## Dependency と upstream compatibility

- reproducibility が重要な behavior-sensitive upstream component は pin します。
- major dependency update は blind refresh ではなく reviewed change として扱います。
- Cua Driver compatibility target の変更は [`TESTING.md`](TESTING.md) に定義した validation を必要とします。
- external command、API、release-specific behavior は authoritative upstream documentation を優先し、stale repository instruction を local consistency のためだけに残しません。

## Feature admission

技術的に実装可能であることだけでは feature を採用しません。custom generic infrastructure を追加する前に、maintained standard / OSS が CUMG の invariant を弱めず代替できないか review します。

proposed feature は、何の問題を解決するか、public/security contract を変えるか、どの failure state が新たに生じるか、それをどう represent するか、どの evidence で change を close するか、なぜ external maintained component ではなく CUMG に属するかを説明できる必要があります。

現在の GO / NO-GO boundary は [`ROADMAP.md`](ROADMAP.md) と [`v2/V2_POSITIONING.ja.md`](v2/V2_POSITIONING.ja.md) にあります。

## Release

version selection、support、deprecation、release mechanics は [`VERSIONING.ja.md`](VERSIONING.ja.md) に定義します。release claim は evidence-backed のまま維持し、version number が大きいこと自体を evidence としません。
