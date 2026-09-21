# V2 Linux cgroup-v2 process containment

Issue #267 では bounded な `execute_process` / `shell` に対して、Linux限定・opt-inのcgroup v2 containment backendを追加します。portable Unixのprocess-group contractは変更しません。

## 有効化とfail-closed境界

明示設定した場合だけ有効です。

- `CUMG_V2_LINUX_CGROUP_V2_ROOT=/absolute/cgroup/path`
- `--linux-cgroup-v2-root /absolute/cgroup/path`

未設定なら従来のUnix process-group挙動のままです。cgroup v2がmountされているだけでauthorityを推論せず、stronger backendへ自動fallbackもしません。

設定時は次を満たさなければAgent startupを拒否します。

- Agentはnon-root
- pathはabsolute / canonical / non-symlinkでcgroup-v2 filesystem上
- emptyな`domain` cgroupでchild cgroupも存在しない
- Agentが`cgroup.procs` / `cgroup.kill`へwrite可能
- child cgroupを作成し、その`cgroup.procs` / `cgroup.kill`へwrite可能
- parent cgroupの`cgroup.procs`はAgentからwrite不可

最後の条件は意図的です。cgroup v2のdelegation containmentでは、non-root delegateeがcommon ancestorの`cgroup.procs`へwriteできなければdelegated subtree外へのmigrationはできません。CUMGは、それより広いmigration authorityを持つ構成をstronger guaranteeとして扱いません。

## 実行contract

configured subtreeはCUMG専用とし、bounded process/shell operationを1つずつ直列実行します。

各operationでは:

1. fresh child cgroupを作成
2. childの`cgroup.procs`をpre-open
3. post-fork / pre-exec hookで、そのopen済みfdへ`0`を書いてchild自身を移動
4. その後だけrequested executable / fixed shellをexec

したがってuser codeはcgroup domainへ入る前には実行されません。既存Unix process-group wrapperもinner lifecycle primitiveとして維持します。

cancel / timeout / spawn後setup failure / top-level normal completionでは:

1. dedicated root の`cgroup.kill`へ`1`をwrite
2. root `cgroup.events`が`populated 0`になるまで待機
3. descendant cgroup directoryを除去してからsubtreeを解放

root-level killなのは意図的です。同一UID workloadがdelegated subtree内部で別cgroupへmigrationしてもroot termination domainからは逃げません。delegation boundary外へのmigrationは拒否される必要があります。kernelの`cgroup.kill`はconcurrent forkを扱い、migration raceからも保護されます。

spawn後にterminal proofが失敗した場合は既存のIndeterminate / no-replay / quarantineへ入り、backend自体もpoisonしてprocess-groupへ黙ってfallbackしません。

## deployment要件

cgroup rootはoperator / service managerが事前に供給します。CUMG自身はcgroup2 mount、host-global delegation、privilege elevation、delegation repairを行いません。

同じsystemd unitへ単に`Delegate=yes`を付けるだけでは不十分です。Agent自身がCUMGのkill対象subtreeに居てはいけず、configured subtreeのparent migration authorityもAgentから利用できてはいけません。service manager/rootが管理するparentの下に、別のdedicated・初期empty execution subtreeをprovisionしてください。

container内Agentでも同じです。writable cgroup namespace viewだけでは不十分で、configured subtreeがreview済みdelegation boundaryで、parent migration pathが見えない/書けない必要があります。read-only / partial cgroup mountではstronger backendはunavailableです。

これはprocess lifecycle containmentであり、filesystem / network / credential / syscall / executable authorityを制限するsandboxではありません。

## scope

- Linux bounded `execute_process` / `shell`: optional cgroup-v2 stronger cleanup
- macOS / other Unix: process-group contractのまま
- Windows: Job Object contractのまま
- managed job: 本Issueでは既存platform baselineのまま
- Playwright: 別途review済みcontainer sandboxが必要。cgroupは代替しない

## acceptance

Linux CIでwritable delegated rootとunwritable control rootを実際にprovisionし、kernel上で次を検証します。

- `setsid()` detached descendantがoperation return時に消えている
- fork race後もrecursive `populated 0`
- delegation boundary外migrationが失敗
- dedicated root内migrationでもroot-level terminationから逃げられない
- unwritable cgroup-v2 subtreeは明示的unavailable

References:

- Linux kernel cgroup v2 documentation: <https://docs.kernel.org/admin-guide/cgroup-v2.html>
- `cgroup_namespaces(7)`: <https://man7.org/linux/man-pages/man7/cgroup_namespaces.7.html>
