# V2 Playwright Sandbox

> この日本語版は [V2_PLAYWRIGHT_SANDBOX.md](V2_PLAYWRIGHT_SANDBOX.md) の翻訳です。英語版を canonical（正典）とします。

Issue #114 は generic process / browser authority を広げずに governed Playwright/E2E execution を追加します。Playwright config と test は Node.js code として実行されるため、安全な command allowlist ではなく external isolation provider 内の arbitrary code として扱います。

## Authority boundary

northbound surface は generic managed job と分離します。

- PlaywrightTestControl — Dangerous。playwright_test_start / playwright_test_stop に必須。
- PlaywrightTestObserve — Observe。playwright_test_status / playwright_test_output に必須。

ExecuteProcess、Shell、ManagedJobControl、ManagedJobObserve、browser authority、cwd/workspace root、class-only grant から推論しません。generic managed job は job_ public ref、Playwright test は別 registry の pwtest_ を使い、cross-registry lookup は fail closed します。

## Provider admission

initial provider は operator-configured Docker/Podman-compatible local container runtime です。CUMG は runtime の install/start、image pull、VM provision、fleet 構築を行いません。

configuration は all-or-nothing tuple です。

- absolute runtime executable path;
- ...@sha256:<64 lowercase hex> 形式の immutable digest-pinned image;
- 1個以上の Playwright-specific approved workspace root。

runtime path 自体が symlink の場合は拒否し、regular executable を必須とします。partial config は拒否します。provider 未設定時は generic process authority へ fallback せず Playwright capability 自体を advertise しません。

capability advertise 前に Agent は complete provider config を validate し、digest-pinned image を inspect し、io.cumg.playwright=v1 と stable device identity + Agent state directory 由来の exact owner label を両方持つ container を列挙します。その owned leftover だけを remove し、owned set が0であることを証明した後だけ old private artifact を prune して Playwright capability を advertise します。startup recovery のどこかで失敗すれば provider は fail closed です。別 Agent 所有 container は recovery label の対象にしません。

## Typed request

playwright_test_start は raw CLI/runtime args ではなく typed request を受け付けます。

- approved workspace;
- 最大32件、各256 bytes以下の relative test path;
- optional project 最大128 bytes;
- optional grep 最大512 bytes;
- optional workers 1〜16;
- hard lifetime 最大30分。

absolute test path、parent traversal、dash 始まりの option-looking path、NUL、arbitrary environment、raw reporter/output path、raw runtime args、host browser profile、CDP/connect endpoint、Docker socket、SSH agent、credential-store mount、arbitrary host path は拒否します。

## Fixed container contract

provider は shell を介さず direct argv で実行します。generated run profile は reviewed sandbox boundary に固定します。

- run --rm --init;
- CUMG-managed container name;
- --pull=never;
- CUMG owner labels;
- --network none;
- --read-only;
- --cap-drop ALL;
- --security-opt no-new-privileges;
- --pids-limit 512;
- --memory 2g;
- --cpus 2;
- --shm-size 1g;
- --user pwuser;
- /tmp と /home/pwuser の bounded writable tmpfs;
- approved workspace を /workspace へ read-only mount;
- Agent-private per-run directory を /artifacts へ writable mount;
- fixed HOME=/home/pwuser, CI=1, PLAYWRIGHT_BROWSERS_PATH=/ms-playwright;
- fixed executable /workspace/node_modules/.bin/playwright;
- fixed reporter=line;
- fixed output=/artifacts/test-results。

Playwright process policy の inherited-environment allowlist は empty です。host Agent/Gateway の HOME、user identity、SSH agent、credential、arbitrary env value を継承しません。

active Playwright job は最大2件です。output は managed-job rolling buffer primitive を使い、streamごとに最大4 MiB保持、1回のoutput readは最大64 KiBです。

## Network boundary

initial network profile は none のみです。同一 container 内 loopback は利用できるため、repository の project-local web server を同じ container で起動し localhost へ E2E test できます。

external-origin networking は intentionally deferred です。URL filtering や Playwright CLI argument validation を network sandbox と表現しません。future external-network profile には CUMG より下で enforce できる provider-level policy が必要です。

## Lifecycle and terminal proof

container runtime client process の終了は provider container stop の証明ではありません。そのため Playwright terminal state は managed-job process-domain proof に provider-aware proof を追加します。

explicit stop では、managed-job state に stop intent を先に記録し、provider container rm -f を要求し、attached runtime client を reap/terminate し、provider cleanup を再度要求してから、exact CUMG-managed name を provider container ps -a で照会します。exact container absence を証明できた場合だけ stopped を返します。

natural completion / hard expiry も cleanup + provider absence を証明してから terminal success を公開します。stop-intent-first により explicit stop が race で ordinary completion と誤判定されるのを防ぎます。

provider absence を証明できない場合、provider termination は unproven です。effectful operation は PlaywrightProviderOutcomeUnproven として existing Indeterminate / reconnect / quarantine semantics に入り、test を auto-replay しません。asynchronous provider ambiguity も Agent fail-closed safety boundary を有効化します。

## Restart recovery

Agent crash 時は host runtime-client process が消えても provider container が残る可能性があります。そのため capability advertise 前に owner-scoped startup recovery を実行します。ordinary process-group / Windows Job Object cleanup を external container の十分な証拠とは扱いません。

--rm 単体も十分ではありません。cleanup 後に provider query で owned container absence を証明します。

## Artifacts

artifact は Agent state directory 配下の Agent-private Playwright root に置きます。parent は private で、container から writable なのは per-run directory だけです。

northbound API は host path を公開しません。status は bounded artifact_count / artifact_total_bytes だけを公開します。inspection は symlink を拒否し、file/directory/total-byte traversal を bound します。

artifact cleanup は provider terminality を証明できた場合だけ実行します。cleanup が ambiguous な場合は、evidence や active mount を壊さないよう artifact を保持します。

## Privacy and telemetry

default telemetry に raw test path、grep value、workspace/artifact host path、provider container identifier、pwtest_ public ref、raw output body を記録しません。existing audit policy の fixed capability/reason category と bounded operational metadata は維持できます。

## Platform claim and non-claims

sandbox boundary は CUMG host process supervision ではなく configured container runtime が提供します。

- macOS/Windows: selected runtime が内部で VM を使う場合も external provider infrastructure として扱います。
- Linux: Playwright feature には container isolation 自体が必要です。optional Linux cgroup-v2 containment は host descendant cleanup を強化しますが filesystem/network sandbox ではなく provider の代替ではありません。
- macOS sandbox-exec / SBPL は product contract にしません。

compromised provider runtime、kernel、Agent、workspace dependency について selected reviewed execution provider の guarantee を超える containment は CUMG claim に含めません。

## Schema compatibility

#114 の current live v0.6 values:

- CONTROL_SCHEMA_VERSION = 12;
- capability schema 8;
- HUB_AGENT_SCHEMA_VERSION = 6;
- persisted device registry schema 8。

restore 用 historical registry/capability pairing は 2/2、3/3、4..=6/4、7/5、released-v0.5 8/6、historical #106 v0.6 8/7 です。current live pairing は 8/8。historical advertisement は restore 時に捨て、dispatch 前に fresh current advertisement を必須とします。

## Acceptance focus

merge/release acceptance 前に少なくとも、provider absent / partial config の fail closed、digest/runtime/workspace validation、fixed isolated argv / no host env inheritance、exact capability split と job_/pwtest_ namespace separation、explicit stop / natural completion / hard-expiry の provider absence proof、provider-proof failure が terminal success ではなく Indeterminate になること、capability advertise 前の startup orphan recovery、artifact symlink/traversal rejection、privacy-bounded status、response loss/provider ambiguity 後に auto replay しないこと、compiled surface の Linux/macOS/Windows compile/CI compatibility を確認します。

external-origin networking は initial profile の scope 外です。
