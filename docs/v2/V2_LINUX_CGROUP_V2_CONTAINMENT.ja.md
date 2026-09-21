# V2 Linux cgroup-v2 process containment

Issue #267 では bounded な `execute_process` / `shell` に対して、Linux限定・opt-inのcgroup v2 containment backendを追加します。portable Unixのprocess-group contractは変更しません。

## 有効化とfail-closed境界

明示設定した場合だけ有効です。

- `CUMG_V2_LINUX_CGROUP_V2_ROOT=/absolute/cgroup/path`
- `--linux-cgroup-v2-root /absolute/cgroup/path`

未設定なら従来のUnix process-group挙動のままです。cgroup v2がmountされているだけでstronger authorityを推論しません。

設定時は次を満たさなければAgent startupを拒否します。

- Agentはnon-root
- hostは通常のcgroup-v2 mountを `/sys/fs/cgroup` に1つだけ公開
- configured pathはabsolute / canonical / non-symlinkの `domain` cgroup
- **Agent process自身がそのconfigured cgroupのmember**
- startup時にchild cgroupが存在しない
- configured cgroup directory / `cgroup.procs` でoperation childを作成・移動可能
- fresh childの `cgroup.procs` / `cgroup.kill` がwrite可能
- configured cgroupのparent `cgroup.procs` はAgentからwrite不可
- unprivileged user namespace、cgroup namespace、mount namespaceが利用可能
- startup probeでchild cgroupへのentry、private cgroup view構築、exec、完全cleanupまで成功

Agent自身の事前配置は必須です。Linux cgroup v2 delegationではprocess migration時にsource/destinationのcommon ancestorへwrite authorityが必要です。したがってnon-root Agentがdelegated subtree外からchildを引き込みつつ、同時にoutside migration不可も証明することはできません。service manager/operatorがAgent起動前にdelegated rootへ配置します。

## 実行contract

configured rootはAgent serviceが所属するdedicated rootです。bounded process/shellは1 operation cgroupずつ直列実行します。

各operationでは:

1. fresh child cgroupを作成
2. childの `cgroup.procs` をpre-open
3. fork
4. post-fork / pre-exec hookでopen済みfdへ `0` をwriteし、childをoperation cgroupへ移動
5. requested codeを実行する前にnew user namespace / cgroup namespace / mount namespaceを作成
6. current Agent identityのUID/GIDを1対1 mapping
7. mount propagationをprivate化し、new cgroup namespaceのroot＝operation cgroupとなる **read-only cgroup2 view** を `/sys/fs/cgroup` へmount
8. `no_new_privs` を設定
9. その後だけrequested executable / fixed shellをexec

effectful user codeはoperation cgroupへ入る前には実行されません。private cgroup namespace + private read-only cgroup mountにより、同一UIDのcommand codeからouter Agent cgroup hierarchyへ戻るmigration pathを閉じます。既存Unix process-group wrapperもinner lifecycle primitiveとして維持します。

cancel / timeout / spawn後setup failure / top-level normal completionでは:

1. exact operation cgroupの `cgroup.kill` へ `1` をwrite
2. operation rootの `cgroup.events` が `populated 0` になるまで待機
3. descendant cgroupを除去し、最後にoperation cgroup自身も除去

これにより `setsid()` detached descendant、concurrent fork race、private operation root配下でのchild cgroup作成/移動もoperation termination domainから逃げません。

spawn後にterminal proofが失敗した場合は既存のIndeterminate / no-replay / quarantineへ入り、backend自体もpoisonしてprocess-groupへ黙ってfallbackしません。

## deployment要件

service manager/operatorがprovisionします。CUMG自身はhost cgroup hierarchyのmount、host-global delegation、privilege elevation、Agent自身のdelegationへの移動を行いません。

Agent binary起動前にservice managerがAgent processをdedicated delegated cgroupへ配置し、そのcgroupだけをAgentへdelegateしてください。parent migration boundaryはservice manager/root管理のままにします。

child側のouter cgroup hierarchyを隠すため、kernel policyでunprivileged user namespaceが利用可能である必要があります。host policyで無効ならstronger backendはunavailableとなり、明示設定時はstartup fail-closedです。

container / namespaced deploymentを自動的に同等とは扱いません。current implementationはreview済みのsingle `/sys/fs/cgroup` mount layoutを要求し、それ以外ではstronger modeを拒否します。

これはprocess lifecycle containmentでありgeneral sandboxではありません。network、通常filesystem access、credential、arbitrary syscallまでは隔離しません。namespace setupはcgroup execution boundaryを証明するためだけです。また `no_new_privs` を設定するため、このoptional backend内ではset-user-IDによるprivilege gainは利用できません。

## scope

- Linux bounded `execute_process` / `shell`: optional cgroup-v2 stronger cleanup
- macOS / other Unix: process-group contractのまま
- Windows: Job Object contractのまま
- managed job: 本Issueでは既存platform baselineのまま
- Playwright: 別途review済みcontainer sandboxが必要。cgroupは代替しない

## acceptance

Linux CIでwritable delegated rootとunwritable control rootをprovisionし、privileged test harnessがtest processをwritable rootへ配置してから通常runner identityへ戻します。実kernel上で次を検証します。

- `setsid()` detached descendantがoperation return時に消えている
- fork race後にoperation cgroupが残らない
- parent hierarchyへのmigrationが失敗
- 別processの `/proc/<pid>/root` 経由でもouter hierarchyへ到達できない
- private cgroup mountはcommand codeからread-only
- unwritable cgroup-v2 rootは明示的unavailable

References:

- Linux kernel cgroup v2 documentation: <https://docs.kernel.org/admin-guide/cgroup-v2.html>
- `cgroup_namespaces(7)`: <https://man7.org/linux/man-pages/man7/cgroup_namespaces.7.html>
- `user_namespaces(7)`: <https://man7.org/linux/man-pages/man7/user_namespaces.7.html>
