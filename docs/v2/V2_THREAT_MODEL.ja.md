# V2 threat model

> この日本語版は [`V2_THREAT_MODEL.md`](V2_THREAT_MODEL.md) の翻訳です。**英語版を canonical（正典）とし、解釈に差がある場合は英語版を優先します。**

Status: **V2-M1 trust-model baseline**。M0 の assumption を基礎として、accepted M1 transport/process control、post-M1 execution-safety hardening、residual deployment responsibility を反映しています。

この文書は delegated device capability control plane の security claim / non-claim を定義します。feature list より意図的に厳密です。ある component が、その control を迂回するために必要な key または execution surface を所有している場合、その component に対して control が有効だとはみなしません。

## Security objectives

Hub、Agent、backend が設計どおり動作している場合、V2 は次の property を提供する必要があります。

1. Hub が connection を受け入れる前に、Agent は現在 enrolled されている device key の possession を証明する。
2. Hub command を受け入れる前に、Agent は pin された Hub transport identity を authenticate する。
3. authenticated northbound client principal が grant を受け取れるのは、明示的に authorize された device/capability pair に対してのみ。M1 Agent-native operation は class-only authority ではなく exact `DeviceCapability` scope を使う。
4. short-lived grant は、ある semantic capability class または exact M1 device capability から別のものへ widen できない。
5. stale device generation、stale capability revision、consumed/revoked/expired grant、unknown signing key は fail closed する。
6. Hub / Agent key rotation は silent key replacement ではなく continuity proof を要求する。
7. 1 device が同時に実行する operation は最大1つであり、Hub admission と per-device queue は bounded のまま。
8. cancellation、disconnect、ambiguous completion が state-changing operation の automatic replay を引き起こさない。
9. normal audit evidence に raw screenshot、command argument、backend result、clipboard value、credential を含めない。
10. backend-specific name と response format は Agent adapter boundary で終端する。

## Trust boundaries

```text
authenticated MCP client principal
        |
        | northbound authn result + Hub authorization policy
        v
+---------------- Hub ----------------+
| client policy                        |
| grant signer client + public verifier|
| transport identity                   |
| admission / lease / audit state      |
+----------------+---------------------+
                 | typed exact-capability signing request
                 v
        +-------- external signer --------+
        | independent capability ceiling  |
        | grant-signing private key       |
        +----------------+----------------+
                         | signed GrantToken
                         v
+---------------- Hub ----------------+
        | signed, versioned Hub-Agent messages
        | + production confidentiality required
        v
+--------------- Agent ----------------+
| pinned Hub trust / device identity   |
| grant verifier set                   |
| single-operation execution gate      |
| backend adapter                      |
+--------------------------------------+
        |
        v
computer-use backend (Cua first)
        |
        v
operating system / desktop / user data
```

northbound client authentication、Hub transport identity、grant-signing authority、Agent device identity は別 credential です。ある credential の possession が、意図せず他 credential の possession を意味することはありません。

## Assets

high-value asset には次が含まれます。

- desktop session と、そこから見える / 操作できる data。
- Agent device private key。
- Hub transport private key。
- capability-grant signing private key（packaged production では `v2_hub` ではなく external signer が保持）。
- northbound client authentication state と authorization mapping。
- conflicting / replayed action を防ぐための operation/lease state。
- capability advertisement と generation/revision state。
- audit evidence と revocation state。

raw screenshot、raw backend response、command argument は、保持すると privacy impact が増えるため、意図的に通常の control-plane audit asset には含めません。

## Compromised component ごとの threat

### Malicious / compromised MCP client

client は別 device の選択、より強い capability の要求、prior grant の replay、Hub への flood、backend-specific parameter の悪用を試みる可能性があります。

Controls:

- Hub は client-supplied device identity を authorization として信頼せず、既に authenticate 済みの principal identity を利用する。
- M0 は `principal -> device -> capability class` をサポートし、M1 Agent-native operation はさらに exact `principal -> device -> DeviceCapability` authorization をサポートして Agent boundary で class-only grant を拒否する。
- client は Agent device key や Hub transport key を受け取らない。
- grant は signed、device-bound、one-shot で、Agent が最長5分の lifetime を強制する。M1 Agent-native grant は exact device capability にも bind される。
- global admission と per-device queue は bounded。
- Hub-Agent protocol は任意の Cua tool name/argument ではなく typed semantic command を公開する。

M1 northbound boundary は新しい OAuth/OIDC server を作るのではなく、MCP Authorization protected-resource side を実装します。`v2_hub` は RFC 9728 metadata を公開し、header bearer presentation を要求し、RFC 7662 introspection または local signed OIDC/JWT verification のどちらかで token を検証してから verified identity を `AuthenticatedClientPrincipal` へ縮約します。trusted-proxy adapter は別の fixed-principal option のままです。OIDC/JWT mode は configured HTTPS JWKS endpoint をpinし、redirect と symmetric/unexpected algorithm を拒否し、exact configured `iss`/`aud` と `sub`/`exp` を必須にし、存在する `nbf` を検証し、JWKS/cache/unknown-`kid` refresh をboundします。required OAuth scope は MCP resource への entry を gate しますが、delegated device access については別の local principal -> device -> exact `DeviceCapability` policy が authoritative のままです。bearer header は rmcp handler dispatch 前に除去され、typed Hub-to-Agent command/grant path には bearer field がありません。

optional #221 semantic-constraint layerは、exact capability authorization後かつconstrained CUMG commandがfinal normalized dispatchable representationになった後に行う第2のnarrow-only Hub admission checkです。初期contractは`TypeText`のexact UTF-8 byte ceilingと`BrowserNavigate`のnormalized requested-origin allowlistです。private authorized-command wrapperがevaluate済みcommandをbindし、provider adapter testでconstrained text/URL fieldが変更されずmaterializeされることを要求します。operator snapshotはHub runtimeごとにexact `(revision, digest)` pairとしてimmutableです。allow operationがpersistするのはrevision/digest/kind/rule IDだけで、constrained text/URLは保存しません。stale snapshotはdispatch前にcancelし`Indeterminate`にはしません。requested-origin policyはredirect confinementではありません。[`V2_SEMANTIC_AUTHORIZATION.ja.md`](V2_SEMANTIC_AUTHORIZATION.ja.md)を参照してください。

Residual risk: authorization-server/introspection または JWKS availability、upstream signing-key compromise、cache lifetime 内でまだ保持される旧key、issuer/audience/algorithm ceiling のoperator misconfiguration は deployment trust dependency のままです。unknown key は fail-open せず、必要なJWKS refreshを完了できない場合はvalidationをfail closedします。public HTTPS termination と transport-edge rate limiting は loopback northbound listener の外側にある deployment responsibility です。packaged production では compromised `v2_hub` は grant-signing private key を保持せず、external signer が unavailable / reject の間は新しい grant を mint できません。ただし live signer は independent signer policy に明示的に allow された exact capability の request を受理できるため、OAuth + key isolation だけで fully compromised Hub が無害になるわけではありません。

### Compromised Hub

fully compromised Hub は引き続き **high-severity trust failure** です。Hub は northbound authorization/admission と active Hub transport identity を制御するためです。ただし packaged production では grant-signing private key を Hub process から除去します。`v2_hub` が送れるのは bounded typed signing request だけで、別 Unix-socket service が stable device / exact capability、TTL、issue-time skew の独立 ceiling を検証し、grant ID と canonical payload を自身で生成して sign します。Hub は返却tokenを pinned grant public key で再検証します。signer unavailable / deny 時は Agent dispatch 前に operation を cancel し、in-process fallback はありません。

それでも blast radius を抑える / recovery を助ける control:

- packaged production では transport identity と grant-signing private key custody を別 process / service identity に分離する。
- signer は arbitrary bytes を sign せず、grant ID / canonical payload を自身で生成し、policy にない capability を独立に reject する。
- signer は TTL を bounded にし、自身の clock-skew window 外の issue time を reject する。
- Agent trust change は signed key-rotation continuity を要求する。
- grant は scoped / short-lived のまま。
- Agent は grant signature、device generation、capability revision、single-operation execution を独立して検証する。
- backend/Agent policy ceiling は Hub / signer grant より厳しくできる。
- content-minimizing audit evidence により raw desktop data を保存せず investigation を支援できる。

Non-claim: external key custody は per-operation user approval ではありません。malicious Hub が Hub transport key を保持し、healthy signer に到達できるなら、independently administered signer policy が意図的に allow した capability は request できます。Hub と signer の両方が compromise されれば強い trust failure に戻ります。dangerous capability に human/hardware approval が必要な deployment は key isolation だけで実現したとみなさず signer boundary に別 approval authority を追加すべきです。[`V2_GRANT_SIGNING.md`](V2_GRANT_SIGNING.md) を参照してください。

### Compromised Agent

compromised Agent は、その OS account が利用可能な desktop capability を悪用したり、backend result について虚偽を報告したり、protocol 外で local data を漏えいさせたりできます。

Controls:

- Hub は future session 向けに enrolled device identity を revoke できる。
- device-key rotation は old-key / new-key の continuity proof を要求する。
- Hub audit は result が enrolled Agent identity によって sign されたことを記録できる。
- stale/reconnected generation は Hub で fail closed する。

Non-claim: Hub は compromised Agent が truthful desktop state を報告したことを cryptographically prove できず、protocol 外の local action も防止できません。Agent host は high-trust machine のままで、least-privilege OS/backend policy を使うべきです。

### Compromised backend

malicious computer-use backend は Agent semantic adapter より下で動作し、requested semantics を無視したり、追加の local action を実行したり、result を捏造したりできます。

Controls:

- backend-specific behavior は adapter contract の背後に隔離する。
- adapter conformance で normalized capability advertisement と result type を検証する。
- capability advertisement は explicit / versioned。
- Agent は実用上可能な限り narrow な backend policy / OS permission を適用する。

Non-claim: adapter 単体で malicious backend を sandbox することはできません。backend provenance、version pinning、OS isolation は deployment control として引き続き必要です。

### Network attacker

network attacker は peer impersonation、message modification、prior handshake/command replay、sensitive metadata observation、connection termination を試みる可能性があります。

M0 で既に証明済みの controls:

- fresh Agent / Hub nonce が各 authentication transcript を bind する。
- Agent は pin された Hub identity を verify する。
- Hub は enrolled device identity を verify する。
- session acceptance、command、cancellation、result、cancellation acknowledgement は signed かつ connection-bound。
- oversized frame は declared payload allocation より前に拒否する。
- signed Hub time を Agent の monotonic elapsed time に anchor し grant-expiry evaluation に使う。
- dispatch 後の connection loss は durable `Indeterminate` state + exact-operation desktop quarantine となり、automatic replay しない。
- authoritative Hub operation record は issuer/subject ownership と device generation にも bind されるため、competing principal や stale Agent generation は operation を settle できない。
- reconnect/liveness だけでは quarantine を clear できない。transient ambiguity を self-reconcile できるのは、Hub の durable な operation/device/original-generation/capability-revision/capability/dispatch-fence binding に exact match する authenticated terminal-evidence report だけで、その terminal transition も persistence-gated / auditable。その他は explicit operator resolution が必要。

post-M1 P0 execution-safety の詳細と residual recovery assumption は [`V2_P0_EXECUTION_SAFETY.md`](V2_P0_EXECUTION_SAFETY.md) にあります。

現在の M1 evidence には、pinned certificate trust/domain validation を伴う TLS-protected gRPC bidirectional streaming と、独立した Ed25519 application authentication が含まれます。以前の raw-TLS transport は regression/reference implementation として残り、TLS 1.3-only + dedicated ALPN です。operator-facing `v2_hub` daemon と `v2_agent` は end-to-end TLS/gRPC test でカバーされています。public-edge firewall/reverse-proxy control、raw transport handshake shedding、external authorization-service availability、credential/certificate custody は deployment responsibility のままです。これらを理由に accepted application-level safety model を弱めてはいけません。

production hardening では authenticated Agent session に hard maximum lifetime も設定します。Hub は期限前の bounded drain window で new admission を止め、通常は already-admitted work が settle してから stream を閉じ、fresh nonce-bound handshake / generation を要求します。hard deadline が先に到達した場合も fail closed のままで、unresolved dispatched work は replay せず `Indeterminate` + quarantine になり得ます。

### Replay / stale-state attacker

Controls:

- grant ID は1回だけ consume され、revoke 可能。
- unknown/retired grant-signing key は fail closed。
- device generation は reconnect / credential rotation で変化する。
- capability revision はすべての command/result で check する。
- operation ID は silent reuse できない。completed/cancelled Agent replay tombstone は authenticated device generation によって bounded され、Hub `Indeterminate` operation は explicit resolution まで durable に残る。
- handshake proof replay は fresh nonce に対して失敗する。

completed/cancelled replay tombstone は authenticated device generation によって bounded されます。fresh generation により old signed command は execution gate 到達前に stale になるため、old terminal ID は prune できます。`Indeterminate` Hub operation は意図的にこの pruning 対象外であり、同じ already-dispatched operation の exact authoritative terminal evidence による self-reconciliation、または explicit operator resolution が durable commit されるまで device を quarantine し続けます。どちらの path も operation を replay しません。

### Denial of service

Controls:

- bounded wire frame。
- bounded Hub global active operation。
- bounded per-device queue。
- single active Agent operation。
- existing V1 northbound concurrency control は V2 Hub admission layer とは conceptually separate。

Residual risk: connection count、TLS handshake、upstream identity provider、host CPU/memory など上記 bound 外の resource exhaustion には deployment-level rate limiting / observability が必要です。

## Cancellation と ambiguous outcome

state-changing computer use は、transport response を失ったという理由だけで安全に retry できるものではありません。

Rules:

- dispatch 前の cancellation は Agent execution を防止する。
- dispatch 後の cancellation は signed Hub->Agent request と signed Agent acknowledgement である。
- Agent terminal replay tombstone は generation-bounded であり、fresh authenticated generation が old signed command を stale にした後にのみ prune できる。Hub `Indeterminate` operation は durable のままで、explicit resolution まで device を quarantine する。
- dispatch 後の Hub connection loss は operation を `indeterminate` にする。
- `indeterminate`、`completed`、`cancelled` operation ID は re-admit できない。
- `indeterminate` operation は Hub admission で device を quarantine するため、explicit resolution まで別 operation も拒否される。
- reconnect は既存の generation-bound operation lease を transfer しない。

Agent-native process cancellation は GUI-backend cancellation より強い guarantee を持ちます。Unix process group と Windows Job Object は supervised process-control domain に残る ordinary descendant を terminate し、top-level process completion 時にも cleanup します。ただし Unix では OS-wide sandbox ではなく、`setsid()` などで意図的に別 session/process group へ detach した descendant は現在の process-group cleanup guarantee の外です。この detachment を supported persistence mechanism として依存してはいけません。一方 Cua MCP adapter は cancellation を exact in-flight downstream request ID に propagate しますが、それを desktop side effect が止まった proof とは扱いません。そのため propagated cancellation / timeout は `indeterminate` disposition と device quarantine に map されます。

同じ rule は、mutating request を dispatch した後に backend が generic tool error を返した場合、malformed/unprovable completion を返した場合、response channel を失った場合にも適用します。これらは non-execution を証明しません。adapter/Agent boundary では `BackendOutcomeIndeterminate` と分類し、Hub は reason `BackendOutcomeUnproven` を持つ durable `Indeterminate` として persist し、その desktop に queue 済みの work を cancel し、reuse 前に explicit かつ persistence-gated な resolution を要求します。read-only command は definite backend error を返せる場合があります。backend-level non-execution proof がないことを、successful cancellation、safe retry、replay permission と解釈してはいけません。

Agent-native process/shell の definite な validation / spawn failure は、raw host detail を公開しなくても remediation できます。signed Agent->Hub result が運ぶのは closed な `DeviceErrorCode` だけで、northbound は `working_directory_denied`、`working_directory_invalid`、`invalid_timeout`、`invalid_program`、`program_denied`、`too_many_arguments`、`environment_key_denied`、`invalid_environment`、`too_many_environment_entries`、`process_spawn_failed` など review 済み category を固定 message に map します。requested/allowed path、program/argv、environment key/value、raw OS error は返しません。Shell は inner process error の safe category を保持し、generic wrapper code へ潰しません。意図的に分類していない executor/configuration failure は `internal_failure` / generic fail-closed error のままです。native runtime timeout / cancellation は proven process-termination outcome（`timed_out` / `cancelled`）のままで、dispatch 後の outcome が prove できなければ `Indeterminate` + quarantine のままです。この error taxonomy は no-replay / quarantine semantics を弱めません。

## Browser transfer data boundary

Browser transfer は意図的に filesystem access より narrow です。upload northbound traffic は bounded byte と path-safe logical name を運び、context/generation/revision-bound one-shot ref を mint します。その backend value は Agent-private staging handle です。Agent は hardened state directory 配下に実 file を作り、symlink/directory/replacement/size violation を拒否し、southbound Cua call 直前に canonical regular file であることを再度 prove します。raw host path は northbound caller から受け取らず、返しもしません。

download は caller から destination path / root capability を受け取りません。Agent は private per-operation canonical root を作り、exact `BrowserDownload` authorization + fresh click-capable page ref を Cua の reviewed MCP-host approval mechanism に map し、opaque id が single component でない completion や、その exact root 直下の regular file ではない object を拒否します。bounded read 前に reported length、actual length、caller maximum、global 16 MiB ceiling がすべて一致する必要があります。logical-name collision では explicit overwrite が必要で、replacement は new object が安全に finalize した後にのみ行います。

transfer ref と staging は context/generation/revision lifecycle とともに消滅します。definite pre-dispatch validation/refusal failure は即 cleanup します。provider dispatch 後の cancellation、timeout、generic backend error、response loss、unprovable completion では、teardown まで Agent-private staging のみを残し、通常の indeterminate quarantine に入ります。これにより in-flight backend read/write との race を避け、no-auto-replay を維持します。この threat model は compromised Agent/Cua process を sandbox できるとは主張しません。uncompromised V2 boundary が generic host filesystem authority を northbound に公開しないことを主張します。

## Key rotation

### Enrollment と TLS trust-anchor lifecycle

fresh fixed-device enrollment は意図的に offline です。`v2_keyctl prepare-agent-enrollment` は private staging directory 配下に create-new device secret と exact Hub/grant/TLS trust input を生成し、non-secret manifest / device ID だけを出力します。Agent 側 artifact は operator-authenticated provisioning channel で転送し、Hub には device public key だけを登録します。mutable runtime discovery や network enrollment oracle は追加しません。

TLS trust は Hub Ed25519 application identity とも独立です。compromised TLS server key の所持だけでは signed Hub handshake を満たせませんが、confidentiality assumption は無効になります。private pinned-root model では CUMG は意図的に CRL/OCSP を実装しません。そのため old compromised leaf は explicit な root/server-identity maintenance cutover と Agent root reprovisioning によって trust boundary から外します。dedicated TLS regression は old root が replacement chain を拒否し、replacement root が受理することを確認します。expiry check は stable operational alert を出しますが trust を自動変更しません。

### Agent credential rotation

logical device ID は stable のままです。replacement には currently enrolled device key と proposed new device key の両方で sign された rotation statement が必要です。rotation は current capability session を無効化し、reconnect より前に generation state を進めます。

packaged rotation runbook では continuity document を one-shot として扱います。Agent を停止し、offline/admin context で dual-signed replacement を生成し、Hub が new verifier を verify/persist してから Agent を new secret で起動します。fresh authenticated generation の成功後は rotation-file setting を外し、その document を再利用せず persisted trust だけで subsequent Hub restart が成功しなければなりません。詳細は [`../../packaging/README.md`](../../packaging/README.md) を参照してください。

current Agent key が continuity proof に使える状態ではなく lost した場合、recovery は explicit administrative re-enrollment flow でなければなりません。ordinary in-band rotation として表現してはいけません。

### Hub transport identity rotation

Agent は pinned Hub verifying key から開始します。in-band replacement ごとに monotonically increasing rotation epoch と old/new Hub key 双方の signature が必要です。currently trusted key から chain しない key は拒否します。

### Grant-signing key rotation

grant token は signing key を識別します。Agent は bounded overlap の間、old/new grant verifier を一時的に trust できます。old verifier を retire すると、その key で sign された newly presented grant は nominal TTL 内でも fail closed します。

## Clock model

grant issue/expiry time は Hub が生成します。signed session acceptance が Hub wall-clock time を運びます。verify 後、Agent は local monotonic clock でその anchor を進め、connection 中の grant validation に derived time を使います。

これにより routine wall-clock skew や backward wall-clock adjustment が grant を黙って延長することを防ぎます。fully compromised Agent は自身の execution environment を falsify できるため、それは上記 compromised-Agent non-claim の範囲です。

## Privacy と audit

normal audit event には device ID、generation、grant ID、operation ID、semantic capability class、policy outcome、reason、timing metadata など stable identifier を含められます。

raw screenshot、raw backend output、raw command argument、clipboard value、typed text、credential、full accessibility tree は含めるべきではありません。意図的にそれらを含む Debug capture は別の high-sensitivity mode であり、暗黙に enable してはいけません。

schema v3 の reconciliation metadata は evidence を4種類に明確に分離します。**correlation evidence** は bounded な workflow/client label と、明示的に canonicalize された shell/process contract の keyed request fingerprint であり、特定できるのは「どの higher-level step / candidate request を議論しているか」までです。**observational evidence** は独立した read-only postcondition/state check で、capability contract が authority を明示しない限り advisory です。**authoritative terminal evidence** は operation state machine を settle できる既存の signed/verified execution evidence です。**operator resolution** は exact quarantined operation に十分な evidence がある場合だけ適用する explicit / persistence-gated decision です。correlation/fingerprint 一致だけでは terminal evidence、replay permission、side effect 発生の proof にはなりません。fingerprint key/value は normal audit asset ではなく、key rotation 時は request を different と再解釈せず comparison を unavailable に degrade させます。

schema v4 は、このうち **authoritative terminal evidence** class だけを self-reconciliation に使用します。Hub は send 前に stable device ID、original device generation、exact `DeviceCapability`、capability revision、one-shot grant ID からなる original dispatch binding を persist します。Agent の durable journal は request/result payload を一切含まず最大64件で、normal execution path が definite terminal result に到達した後だけ entry を作れます。fresh authenticated session では journal を new session nonce に対して署名します。Hub は current device signature / reporting generation を検証したうえで、older quarantined record との全項目 exact equality を要求します。current/stale wrong generation、wrong device、wrong operation、wrong capability/revision/fence、unsupported evidence/state pair、missing journal entry は quarantine を clear できません。`auto_reconciling`、`auto_resolved`、`operator_required`、`unrecoverable_evidence_gap` を explicit な durable/audit state として扱います。Hub は candidate terminal checkpoint を write してから live quarantine removal を可視化します。したがって compromised Agent に generic な「clear quarantine」primitive は増えず、online 時にその exact dispatched operation へ元々持っていた result authority を別 operation/fence へ拡張できません。correlation fingerprint から execution evidence を捏造することもできません。

schema v5 は別の threat、すなわち terminal truth を回復不能な legacy ambiguity により service が永久停止する問題を扱います。retirement primitive は terminal-result authority を与えません。offline local maintenance、review 済み capability allowlist（`Scroll` / `MovePointer`）、exact quarantined operation、original dispatch より strictly newer な durable registry generation に限定します。old operation は `Indeterminate` かつ permanent replay tombstone のままで、解除するのは admission quarantine だけです。forged/stale generation、auto-reconciling operation、dispatch marker 欠落、duplicate/conflicting transition、allowlist 外/high-impact capability は fail closed します。これにより retirement が generic な「success とみなす」「cancelled とみなす」または replay bypass になることを防ぎます。 `Scroll` / `MovePointer` allowlist は、すべての application で副作用がないことや idempotent であることの証明ではなく、review 済み operational-risk policy です。application 固有の state change が起こり得る点は unknown outcome の一部として明示的に受容します。strictly newer generation により stale interaction context/ref は失効し、pre-ambiguity の queued work も既に cancel されるため、retirement が古い dependent action chain を resume することはありません。retirement policy は versioned value を明示指定し、initial allowlist は `Scroll` / `MovePointer` に限定します。schema v5-v10 は permanent replay tombstone と64件の detailed history cap を結合しています。schema v11 はこれらを分離し、detailed history と full retired operation detail は64件 bounded のまま、lossless exact-ID replay-deny index を独立して4096件上限とします。index exhaustion 時も quarantine を維持して fail closed します。

## V2-M1 acceptance と residual deployment responsibility

V2-M1 implementation gate は 2026-08-12 に pass しました。M1 code には verified northbound principal construction、production key/certificate lifecycle procedure、bounded service connection/rate shedding、bounded replay pruning、real-Cua cancellation quarantine、OpenTelemetry/OTLP integration、OS service packaging が含まれます。詳細は [`V2_M1_ACCEPTANCE.md`](acceptance/V2_M1_ACCEPTANCE.md) を参照してください。

threat model は deployment に引き続き次の external responsibility を要求します。

- public authorization server/introspection endpoint と Hub の configured issuer/resource/audience を1つの trust boundary として review する。
- raw TCP/TLS handshake flood は host firewall/security group と、使用する場合は reviewed reverse proxy/load balancer で制限する。application-layer limit は transport accept 後から始まる。
- application-key recovery material と systemd/macOS credential file を repository 外で保護する。old Hub/device key の loss を ordinary continuity rotation に silently convert しない。
- `ExecuteProcess` / `Shell` は exact `Dangerous` capability であり filesystem sandbox ではない。cwd/root check は arbitrary process argv / shell syntax を制限しない。
- `ReadFile` / `ListDirectory` は別の explicit file-root policy を使う。file root は process cwd root より狭くでき、cwd から暗黙推測せず、canonicalization / symlink-escape denial を維持し、設定欠落時は Agent startup を fail closed する。
- macOS GUI automation は operator-controlled Cua/TCC trust boundary に依存する。compromised Agent / desktop backend は上記 non-compromise guarantee の対象外。
- default telemetry は payload-free のままにする。collector/proxy body logging や high-sensitivity debug capture の enable は別 sensitive-data boundary を作る。
- `indeterminate` operation は persistence-gated な settlement が存在するまで quarantined のままにする。network recovery、backend reconnect、service restart だけでは settlement ではなく、replay permission にもならない。automatic な例外は同じ prior dispatch binding に対する exact signed terminal evidence だけで、missing/mismatch evidence は operator-required / fail-closed のまま。

これらは deployment assumption / residual risk であり、missing V2-M1 protocol feature ではありません。multi-machine identity、fleet attestation、additional native GUI backend は later milestone に意図的に defer されています。
