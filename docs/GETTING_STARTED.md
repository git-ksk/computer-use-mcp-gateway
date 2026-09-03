# Getting started

> English is the canonical documentation. [日本語版 / Japanese translation](GETTING_STARTED.ja.md)

This guide takes a new user from a clean machine to the recommended **V2 Hub + V2 Agent** runtime. V1 remains available as `v1_gateway` for regression/reference, but `cargo run` now starts the V2 Hub and intentionally requires explicit trust/TLS material.

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


`v0.4.0` has not shipped yet. CI may expose verified **release-candidate** archives for packaging review, but those unsigned/unnotarized artifacts are not an official installer or a broader platform-support claim. See [`VERSIONING.md`](VERSIONING.md#release-candidate-artifacts) for the bounded manifest/checksum/fresh-extraction contract.

## 6. Provision the V2 identities and TLS boundary

V2 deliberately does not have the old V1 one-line startup because the Hub/Agent trust split is part of the safety model. Create separate Hub, grant, and device identities with `v2_keyctl`; keep secret files outside the repository:

```bash
cargo run --locked --bin v2_keyctl -- generate-hub /secure/cumg/hub.key /secure/cumg/hub.pub
cargo run --locked --bin v2_keyctl -- generate-grant /secure/cumg/grant.key /secure/cumg/grant.pub
cargo run --locked --bin v2_keyctl -- generate-device /secure/cumg/device.key /secure/cumg/device.pub
```

Use a normal TLS certificate lifecycle for the Hub gRPC endpoint. For Linux service deployment, follow [`../packaging/README.md`](../packaging/README.md) and [`DEPLOYMENT.md`](DEPLOYMENT.md); they define the key placement, TLS root, state directories, and continuity rules. Do not collapse Hub, grant, device, and TLS keys into one credential.

## 7. Start the V2 Hub

The default binary is now the V2 Hub. `v2_hub` remains as an explicit equivalent binary for service packaging:

```bash
cargo run --locked -- --help
# equivalent explicit entrypoint:
cargo run --locked --bin v2_hub -- --help
```

Configure the required Hub/grant/device-public/TLS/state paths plus the Agent-facing gRPC bind. To expose northbound MCP, configure the loopback `CUMG_V2_MCP_BIND`, canonical public HTTPS resource, and exact principal -> device -> `DeviceCapability` policy from [`DEPLOYMENT.md`](DEPLOYMENT.md). Choose exactly one authentication adapter: OAuth RFC 7662 introspection, signed OIDC/JWT for multi-principal deployments, or the packaged trusted-proxy fixed-principal mode for an explicitly single-principal authenticated tunnel. OIDC/JWT mode requires the configured issuer, exact audience, pinned HTTPS JWKS URI, asymmetric algorithm allowlist, and required scopes described in [`DEPLOYMENT.md`](DEPLOYMENT.md). All adapters reduce identity to the same `AuthenticatedClientPrincipal` before CUMG authorization; the modes are mutually exclusive.

The northbound MCP is not a raw Cua proxy. The current typed V2 contract is grouped into Agent-native process/shell execution plus bounded filesystem observation, Desktop semantic capabilities including same-context native element targeting/actions, and Browser semantic capabilities plus bounded upload/download transfer when exact policy and the live `CapabilityAdvertisement` permit them. Discovery is filtered by exact authorization and the Agent's live advertisement; an offline Agent exposes no semantic device tools. See [`V2_GUI_SEMANTIC_CAPABILITIES.md`](v2/V2_GUI_SEMANTIC_CAPABILITIES.md), [`V2_BROWSER_SEMANTIC_CAPABILITIES.md`](v2/V2_BROWSER_SEMANTIC_CAPABILITIES.md), and [`V2_CUA_PARITY_MATRIX.md`](v2/V2_CUA_PARITY_MATRIX.md).

## 8. Start the V2 Agent on the controlled desktop

The desktop runs a separate outbound Agent:

```bash
cargo run --locked --bin v2_agent -- --help
```

Configure the Hub endpoint/domain, stable device ID, device secret, Hub/grant public keys, TLS root, state directory, process/shell cwd roots, and the separate read-only filesystem roots. `CUMG_V2_ALLOWED_CWD_ROOTS` governs only process/shell working directories; `CUMG_V2_ALLOWED_FILE_ROOTS` governs only `ReadFile`/`ListDirectory`. There is no implicit cwd-to-file fallback. On upgrade from an older configuration, explicitly copy the old cwd root list into the new file-root setting if identical read behavior is required, verify startup, then narrow file roots independently. Missing/empty file roots fail Agent startup rather than silently broadening read authority. To use Cua for the GUI capabilities, configure:

```text
CUMG_V2_CUA_COMMAND=cua-driver
CUMG_V2_CUA_ARGS=mcp
CUMG_V2_CUA_BACKEND_VERSION=0.19.3
```

Cua stays behind the Agent over MCP stdio. Set `CUMG_V2_CUA_BACKEND_VERSION` to the exact reviewed compatibility target in production. When set to a concrete value, the Agent verifies the Cua MCP handshake `serverInfo.version` on every connection and reconnect and fails closed on drift. The `external` default is an explicit unpinned mode for custom deployments, not the recommended production setting for the reviewed Cua path. On macOS, keep the Agent/Cua in the logged-in user session and do not bypass TCC prompts or move GUI automation into a headless system daemon.

## 9. Verify before remote exposure

Before exposing the northbound MCP resource, verify all of these independently:

- `cua-driver --version` matches the configured `CUMG_V2_CUA_BACKEND_VERSION`;
- `cua-driver call list_apps` works on the desktop;
- the V2 Agent connects with the expected stable device and a fresh generation;
- northbound OAuth produces only the intended issuer+subject principal;
- `tools/list` contains only capabilities granted by the exact policy;
- a harmless V2 Cua-backed operation such as `list_apps` or `get_screen_size` succeeds;
- reconnect/liveness alone does not clear unresolved CUMG quarantine; only exact signed terminal evidence for the same prior dispatch may self-reconcile without replay.

Then use [`DEPLOYMENT.md`](DEPLOYMENT.md) for the reviewed reverse-proxy/TLS path. Keep the northbound MCP listener loopback-only.

## Legacy V1 local regression path

For regression/reference only, the former single-process instructions remain available through the explicit `v1_gateway` binary:

```bash
cargo run --locked --bin v1_gateway -- \
  --allow-tools list_apps,list_windows,get_accessibility_tree,get_screen_size
```

Its default endpoints remain `http://127.0.0.1:8100/mcp` and `/healthz`. V1's dynamic 54-tool Cua surface and exact-name allow/deny model are intentionally not copied into the V2 exact-capability contract.

## If something fails

See [`TROUBLESHOOTING.md`](TROUBLESHOOTING.md). It covers PATH problems, macOS permissions, Linux display/runtime dependencies, Windows interactive sessions, empty tool lists, Host/Origin 403s, reverse-proxy authentication, and backend timeouts.
