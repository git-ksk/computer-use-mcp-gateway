# V2 Unix process containment 調査

Status: **Issue #96 の調査完了。current portable contract は変更しない**。

## 結論

CUMG は既存 Unix `execute_process` / `shell` に対して OS-wide descendant containment を claim しません。また command text filter、recursive PID scan、process-name matching、best-effort lineage polling でその保証を擬似的に作りません。

current portable contract は次のままです。

- Unix operation ごとに専用 process group を作る。
- cancellation、timeout、ordinary top-level completion では、そのgroup内に残る descendant を terminate する。
- 別session/process groupへの deliberate detachment は保証外。
- persistent work は `nohup`、`setsid`、daemonize 等で bounded operation から逃がさず、別exact capabilityを持つ managed-job/service lifecycle を使う。

これは exact `Dangerous` process/shell capability を既にauthorizeされたcallerに対するsandboxではなく、lifecycle boundaryです。

## macOS evidence

2026-09-03 に trusted macOS で、CUMG と同じ process-group model に対する既知escapeを再現しました。

1. top-level shellを新process-group leaderとして起動。
2. childが`setsid()`を呼び、そのまま生存。
3. top-level shellを終了。
4. original process groupへ`SIGKILL`。
5. detached childが生存していることを確認し、そのtest childはPID指定でcleanup。

観測:

```text
top_level_rc=0 ... detached_alive_after_killpg=true
```

これはmacOS `setsid(2)` contractと一致します。成功したcallerは**new sessionかつnew process groupのleader**になります。

current macOS SDKには`proc_listchildpids()`とPID metadataがありますが、descendant containerではなくsnapshot/relationship inspection APIです。parent exit後はchildがreparentされ得るため、recursive child enumerationだけではrace-free kill guaranteeになりません。`EVFILT_PROC`には`NOTE_FORK`が残っていますが、historical `NOTE_TRACK` / `NOTE_CHILD` fork-lineage tracking flagはSDK上でmacOS 10.5以降unsupportedと明記されています。したがってこれらをpollしてgapが閉じたとclaimしません。

意図的にpersistentなmacOS background workにはAppleのsupported lifecycleである`launchd` / Service Managementがあります。これは#106のproduct directionと整合し、persistent jobはbounded shell operationからのescapeではなく別explicit lifecycleとして扱います。

## Linux evidence と stronger optional direction

Linux cgroup v2は、CUMG Agentにproperly delegatedなcgroup subtreeが与えられる場合、より強いprimitiveを提供します。kernel delegation modelではresource restrictionがhierarchicalで、POSIX session/process-group membershipが変わってもdelegated subtree側でdescendantをcontainできます。そのためcgroup-backed execution domainはdeliberate `setsid()` escapeに対して`killpg`より強くできます。

ただしcgroup v2をcurrent Unix baselineのportable drop-in replacementにはしません。

- delegation / writable cgroup ownershipはservice manager/deploymentで明示provisionが必要。
- cgroup v2 mountが存在するだけで`/sys/fs/cgroup` writableとは仮定しない。
- systemd policy、container、cgroup namespaceでdelegated subtreeが変わり得る。
- stale/escaped processがoperation subtree外へmigrateできないことを実装evidenceでproveする必要がある。
- cancellation/timeoutでproven terminal resultを返す前にterminal proofが必要。

よってLinux cgroup containmentはexisting process-group backendの保証を黙って広げず、Linux-specific implementation issueへ分離します。

## 採用しないapproach

次はauthoritative containment mechanismとして採用しません。

- shell textの`setsid`、`nohup`、`daemon`、`launchctl`、`systemctl`等のfilter。
- current child PIDをrecursive scanしてvisibleなものをkill。
- executable name / command line matching。
- broad same-user process kill。
- daemonize/reparent後もnormal parent/child relationが残るという仮定。
- signal delivery成功を「全side effect/process停止済み」のproofにすること。

いずれもincomplete、race-prone、over-broad、またはproofなしでsecurity boundaryを変更します。

## planned workへの影響

### #106 managed long-running jobs

意図的にpersistentなdeveloper processは#106がownerです。managed jobにはdistinct exact capability、stable job identity、explicit start/status/output/stop lifecycle、bounded concurrency/storage、platform-specific termination proofが必要です。macOSではreviewed service/helper lifecycle、Linuxではcgroup-backed execution domainを候補にできます。

### #114 Playwright / E2E sandbox

#114はordinary process-group boundaryをsandboxとして依存してはいけません。Browser test executionはseparately reviewed OS/container/VM isolation boundary、ephemeral profile/home、bounded filesystem/network authority、explicit cleanup semanticsが必要です。

## support claim

この調査でreleased Unix support claimは変更しません。ordinary descendantに対するcurrent guaranteeはtruthfulかつtestedのままです。将来platform-specific stronger backendを追加する場合、deployment prerequisiteとregression/physical evidenceを明示した後だけstronger guaranteeをadvertiseします。

## References

- Apple `setsid(2)`: <https://developer.apple.com/library/archive/documentation/System/Conceptual/ManPages_iPhoneOS/man2/setsid.2.html>
- Apple Service Management: <https://developer.apple.com/documentation/servicemanagement/>
- Apple launchd daemon/agent guidance: <https://developer.apple.com/library/archive/documentation/MacOSX/Conceptual/BPSystemStartup/Chapters/CreatingLaunchdJobs.html>
- Linux cgroup v2: <https://docs.kernel.org/admin-guide/cgroup-v2.html>
