# Windows V2 persistence profile

This profile is for a Windows desktop/user deployment of the V2 Hub + Agent, including shell-only dogfood and a future interactive GUI/CUA Agent. It deliberately keeps the scheduled tasks at `RunLevel=Limited` under the enrolled interactive user.

## Why Task Scheduler instead of a Windows Service

A Windows Service runs in Session 0 and is therefore the wrong default lifecycle boundary for a future desktop Agent that must stay in the logged-in user's interactive session. A plain scheduled task is also insufficient for crash persistence: terminating the child process can leave the task `Ready` without Task Scheduler restarting the process even when `RestartOnFailure` is configured.

The reviewed Windows profile therefore makes `run-component.ps1` the scheduled-task action. The task launches Windows PowerShell with `-WindowStyle Hidden`, so the persistence wrapper does not leave visible console windows in the interactive desktop. The launcher starts exactly one configured child, redirects stdout/stderr to stable local log paths, writes a PID file, and restarts the child after abnormal or normal exit. The Hub starts immediately. The Agent waits for `127.0.0.1:7443` before each launch. The optional trusted proxy waits for the Hub northbound listener before each launch. Task Scheduler remains the user-session persistence boundary while the launcher supplies child-process supervision that Task Scheduler does not reliably provide for a force-terminated executable.

The launcher never logs the configured argv. Do not place secret bytes in JSON arguments; pass only secret **file paths** to Hub/Agent. For Caddy, keep the proxy token in a separately ACL-protected env file.

## Security boundaries

- Hub gRPC and northbound MCP should be bound to loopback for a same-host Windows dogfood deployment (`127.0.0.1:7443` and `127.0.0.1:8102` in the example).
- The trusted proxy binds only to `127.0.0.1:8103`.
- The Caddy example removes any incoming `X-CUMG-Trusted-Proxy-Token` and then sets the locally provisioned value before forwarding. Never pass through or append the client-supplied value.
- The installer replaces inherited ACLs below `DataRoot` with FullControl for only the current user SID, `SYSTEM`, and built-in `Administrators`.
- The scheduled tasks run as the current user, `Interactive` logon type, `Limited` run level. Do not elevate the Agent simply to make persistence work.
- The shell-only Agent example contains no CUA command or GUI capability configuration.

`#121` adds runtime ACL validation as a second fail-closed layer. Packaging ACLs are not a replacement for runtime validation.
## Runtime Windows ACL policy

Windows key/trust loading is fail-closed in addition to the packaging ACL applied above. The runtime reads the NTFS owner and DACL directly through the Win32 security APIs; it does not shell out to `icacls`.

Trusted principals are intentionally narrow:

- the current process identity (the account actually running the Hub or Agent),
- `NT AUTHORITY\SYSTEM`, and
- `BUILTIN\Administrators`.

The file/directory owner must be one of those principals. An unrelated owner is rejected even if the visible DACL looks restrictive because an owner can regain DACL control.

Allow ACEs inherited from a parent are evaluated exactly like explicit allow ACEs. For secret material, an unrelated principal with read **or** write access is rejected. For public trust material, unrelated read access is permitted but unrelated write/modify/delete/DACL-owner control is rejected. For the immediate parent directory, unrelated create/write/delete/DACL-owner control is rejected so an attacker cannot replace newly written trust material through a writable parent.

A null DACL, unsupported allow-ACE shape, or Windows security API failure is treated as unsafe. Files or parent directories carrying `FILE_ATTRIBUTE_REPARSE_POINT` are rejected, which covers junction/reparse paths in addition to ordinary symlinks. The existing Unix mode-bit checks remain unchanged.

## Prepare local configuration

Copy, do not edit in place:

```powershell
$root = "$env:LOCALAPPDATA\computer-use-mcp-gateway"
Copy-Item packaging\windows\hub.config.example.json   "$root\v2-windows-shell\run\hub.json"
Copy-Item packaging\windows\agent.config.example.json "$root\v2-windows-shell\run\agent.json"
Copy-Item packaging\windows\proxy.config.example.json "$root\v2-windows-shell\run\proxy.json"
Copy-Item packaging\windows\Caddyfile.example          "$root\v2-windows-shell\run\Caddyfile"
```

Replace example principal/resource/device values with the reviewed deployment values. Store the real Caddy token in an ACL-protected `caddy.env` outside the repository. The launcher expands `%NAME%` environment references in configured paths and arguments.

## Install

```powershell
.\packaging\windows\install-user-tasks.ps1 `
  -DataRoot "$env:LOCALAPPDATA\computer-use-mcp-gateway" `
  -HubConfig "$env:LOCALAPPDATA\computer-use-mcp-gateway\v2-windows-shell\run\hub.json" `
  -AgentConfig "$env:LOCALAPPDATA\computer-use-mcp-gateway\v2-windows-shell\run\agent.json" `
  -ProxyConfig "$env:LOCALAPPDATA\computer-use-mcp-gateway\v2-windows-shell\run\proxy.json" `
  -Start
```

The canonical task names are `cumg-v2-windows-hub`, `cumg-v2-windows-agent`, and `cumg-v2-windows-proxy` unless `-TaskPrefix` is overridden.

## Logs and diagnostics

The stable paths are under the configured `logDirectory`:

- `<component>.supervisor.log` — lifecycle only; no argv or secret values.
- `archive\<component>.<UTC timestamp>.stdout.log` — one file per child run.
- `archive\<component>.<UTC timestamp>.stderr.log` — one file per child run.
- `<component>.pid`

A child exit should produce `event=child_exit`, followed by a new `event=child_start` after `restartDelaySeconds`. Each child run writes to a fresh timestamped stdout/stderr file so restart does not depend on renaming a recently closed Windows log handle.

## Disable or uninstall

Pass the same config paths so the helper can stop the scheduled-task launcher first, then terminate the launcher-owned child using its PID file before disabling/unregistering the task. This ordering avoids racing the supervisor restart loop and leaving an orphaned replacement child:

```powershell
$configs = @(
  "$env:LOCALAPPDATA\computer-use-mcp-gateway\v2-windows-shell\run\hub.json",
  "$env:LOCALAPPDATA\computer-use-mcp-gateway\v2-windows-shell\run\agent.json",
  "$env:LOCALAPPDATA\computer-use-mcp-gateway\v2-windows-shell\run\proxy.json"
)
.\packaging\windows\uninstall-user-tasks.ps1 -ConfigPaths $configs -DisableOnly
# Remove -DisableOnly to unregister the tasks.
```

## Windows test harness

The Rust test fixtures default to `python3`, matching the Linux CI environment. On Windows, set `CUMG_TEST_PYTHON` to a real Python 3 executable before running the full Rust suite; do not rely on the Microsoft Store/App Execution Alias stub.

```powershell
$env:CUMG_TEST_PYTHON = 'C:\path\to\python.exe'
cargo test --locked
```

Windows also keeps a process-local reservation alongside the OS file lock for the Hub state directory. This preserves the single-owner invariant when Windows permits a second lock attempt from the same process.
## Physical Windows acceptance

1. Confirm the unrelated WindowsMCP listener remains healthy before and after every step.
2. Kill only `v2_agent.exe`; verify the launcher starts a new PID and the Agent reconnects.
3. Kill only `v2_hub.exe`; verify the launcher starts a new PID and the Agent reconnects after Hub recovery.
4. Kill/restart the trusted proxy and verify its loopback listener returns.
5. Run a benign shell smoke through the authenticated external route.
6. Verify direct access to the Hub northbound listener is rejected and the trusted-proxy route succeeds.
7. Re-check the unrelated WindowsMCP listener.

Do not use `reset`, `clean`, forced checkout, or broad process-kill commands during acceptance.
