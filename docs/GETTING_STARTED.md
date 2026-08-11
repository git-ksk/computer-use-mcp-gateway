# Getting started

This guide takes a new user from a clean machine to a locally reachable `computer-use-mcp-gateway` endpoint. Get the local path working first. Add a remote tunnel only after local health and MCP discovery work.

## What you need

- Git
- Rust 1.88 or newer
- an interactive desktop session
- Cua Driver

This pre-alpha repository is currently distributed from source, so Git, Rust, and the platform's normal native build tools are part of the user setup rather than only contributor dependencies.

The repository currently tests against **Cua Driver 0.19.3** in CI. Newer Cua releases may work, but 0.19.3 is the reproducible compatibility target until the repository pins and tests another version.

The gateway is localhost-first and deny-by-default. The examples below expose only a small non-mutating inspection surface. Inspection can still reveal sensitive window titles, application names, screen dimensions, or accessibility text, so review the allowlist before using it on a personal desktop.

## 1. Install Git and native build prerequisites

Official Git platform installers are listed at:

```text
https://git-scm.com/install/
```

### macOS

Apple's Command Line Tools provide Git and the native linker/toolchain needed by Rust builds:

```bash
xcode-select --install
```

After installation:

```bash
git --version
```

### Windows

Install Git for Windows. One official Git page option is `winget`:

```powershell
winget install --id Git.Git -e --source winget
```

Open a new PowerShell window and verify:

```powershell
git --version
```

The Rust step below also needs the Microsoft C++ build/linker prerequisites. `rustup-init` can offer to install them; alternatively install Visual Studio Build Tools with **Desktop development with C++**.

### Linux

Use your distribution's package manager. On Debian/Ubuntu-like systems, this also installs the basic native build toolchain and `curl` used by the setup commands below:

```bash
sudo apt update
sudo apt install git curl build-essential
```

Then verify:

```bash
git --version
```

For Fedora, Arch, openSUSE, and other distributions, use the Git package listed by your distribution or the official Git install page above.

## 2. Install Rust 1.88+

Rust's official installer is `rustup`:

```text
https://www.rust-lang.org/tools/install
```

### macOS / Linux

Install Rust with the official rustup bootstrap command:

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
```

Open a new terminal, then verify:

```bash
rustc --version
cargo --version
```

The reported Rust version must be 1.88 or newer. If you already use rustup but your active toolchain is older, update it before building this repository:

```bash
rustup update stable
```

### Windows

Use the Windows `rustup-init` installer from the official Rust install page above. Rust's default Windows MSVC toolchain needs the Microsoft C++ linker/libraries; accept the prerequisite installation when offered or install Visual Studio Build Tools with **Desktop development with C++**.

Open a new PowerShell window after installation and verify:

```powershell
rustc --version
cargo --version
```

The reported Rust version must be 1.88 or newer. If an existing rustup installation is older:

```powershell
rustup update stable
```

## 3. Install Cua Driver 0.19.3

### macOS

Requirements: macOS 14 or later on Apple Silicon or Intel.

```bash
CUA_DRIVER_RS_VERSION=0.19.3 /bin/bash -c "$(curl -fsSL https://cua.ai/driver/install.sh)"
```

Open a new terminal if `cua-driver` is not immediately on `PATH`, then verify the installation:

```bash
cua-driver --version
cua-driver doctor
```

Cua's macOS permissions must belong to `CuaDriver.app`, not to an arbitrary terminal-launched helper process. Start the application-backed daemon, request the grants, and verify them:

```bash
open -n -g -a CuaDriver --args serve
cua-driver permissions grant
cua-driver permissions status
```

Grant **Accessibility** and **Screen & System Audio Recording** to CuaDriver in System Settings when macOS opens the relevant panes. A changed TCC grant may require CuaDriver to relaunch before it becomes effective.

If `CuaDriver.app` does **not** appear automatically under **System Settings → Privacy & Security → Screen & System Audio Recording**, use this fallback only for that case: click `+`, select `/Applications/CuaDriver.app`, enable it, relaunch CuaDriver if necessary, then verify again with `cua-driver permissions status`. Manual addition is not required when macOS already lists CuaDriver normally.

### Windows

Requirements: Windows 10/11 or Windows Server with an interactive desktop session and PowerShell.

```powershell
$env:CUA_DRIVER_RS_VERSION = "0.19.3"
irm https://cua.ai/driver/install.ps1 | iex
cua-driver autostart kick
```

Open a new PowerShell window if the updated user `Path` is not visible yet, then verify:

```powershell
cua-driver --version
cua-driver doctor
```

Computer-use actions need an interactive desktop session. A non-interactive service or SSH-only session is not enough for normal click/type/window automation.

### Linux

The tested Cua target expects an x86_64 Linux desktop session. X11/XWayland is the conservative path; accessibility-tree tools also need AT-SPI 2. On Debian/Ubuntu-like minimal installations:

```bash
sudo apt update
sudo apt install libxi6 at-spi2-core
CUA_DRIVER_RS_VERSION=0.19.3 /bin/bash -c "$(curl -fsSL https://cua.ai/driver/install.sh)"
```

Then verify:

```bash
cua-driver --version
cua-driver doctor
```

A headless server does not provide a desktop to drive. Use a real desktop session before expecting screenshot/window/input tools to work.

### Optional telemetry setting

Cua Driver documents product telemetry as enabled by default. Disable it if that is your preference:

```bash
cua-driver telemetry disable
```

## 4. Verify Cua before adding the gateway

Confirm that the backend can answer a harmless read operation:

```bash
cua-driver call list_apps
```

If this fails, fix the Cua installation/permissions first. The gateway cannot repair missing OS permissions or an unusable desktop session.

## 5. Clone and build the gateway

```bash
git clone https://github.com/git-ksk/computer-use-mcp-gateway.git
cd computer-use-mcp-gateway
cargo build --locked
```

`Cargo.lock` is committed and normal builds use `--locked` so the dependency graph is reproducible.

## 6. Start the gateway locally

The shortest cross-platform path is to pass the initial allowlist as a CLI option instead of loading a shell-specific `.env` file.

### macOS / Linux

```bash
cargo run --locked -- \
  --allow-tools list_apps,list_windows,get_accessibility_tree,get_screen_size
```

### Windows PowerShell

```powershell
cargo run --locked -- --allow-tools "list_apps,list_windows,get_accessibility_tree,get_screen_size"
```

Keep this terminal open. The default endpoints are:

```text
MCP     http://127.0.0.1:8100/mcp
Health  http://127.0.0.1:8100/healthz
```

For persistent configuration, copy `.env.example` and set environment variables using the mechanism appropriate for your OS or process manager. The CLI also exposes the same core settings; run:

```bash
cargo run --locked -- --help
```

## 7. Check local readiness

### macOS / Linux

```bash
curl --fail http://127.0.0.1:8100/healthz
```

### Windows PowerShell

```powershell
Invoke-RestMethod http://127.0.0.1:8100/healthz
```

A ready gateway returns the equivalent of:

```json
{"status":"ok","backend":"ready"}
```

A successful `/healthz` response means the gateway considers the backend connection ready. It does not prove that every desktop permission and every Cua tool is usable.

## 8. Connect an MCP client

Use MCP **Streamable HTTP** and point the client at:

```text
http://127.0.0.1:8100/mcp
```

For Codex CLI, the Codex IDE extension, and ChatGPT desktop's MCP-server settings, see [`CLIENTS.md`](CLIENTS.md). ChatGPT web cannot use this localhost URL directly; remote access requires an authenticated HTTPS endpoint or another product-supported secure tunnel mechanism.

## 9. Add capabilities deliberately

The gateway exposes no tools when `CUMG_ALLOW_TOOLS` is empty. Add only the tools your workflow needs. For example, a local test that needs interaction might explicitly include selected input tools rather than opening the entire backend surface.

`CUMG_ALLOW_TOOLS=*` means **every discovered backend tool**. Do not use it merely to make setup easier, especially on a remotely reachable gateway.

For argument-level restrictions, use Cua's native policy engine as a second layer. Start from [`../examples/cua-policy.yaml`](../examples/cua-policy.yaml) and configure `CUA_DRIVER_POLICY_FILE` after reviewing it for your machine.

## 10. Make it remote only after local success

Once all of these are true:

- `cua-driver doctor` is satisfactory;
- `cua-driver call list_apps` works;
- `/healthz` reports `backend: ready`;
- a local MCP client can list the expected policy-filtered tools;

then follow [`DEPLOYMENT.md`](DEPLOYMENT.md) to place an authenticated TLS reverse proxy/tunnel in front of the still-loopback-bound gateway.

Do not expose `127.0.0.1:8100` by changing it to `0.0.0.0` just to make remote access work.

## If something fails

See [`TROUBLESHOOTING.md`](TROUBLESHOOTING.md). It covers PATH problems, macOS permissions, Linux display/runtime dependencies, Windows interactive sessions, empty tool lists, Host/Origin 403s, reverse-proxy authentication, and backend timeouts.
