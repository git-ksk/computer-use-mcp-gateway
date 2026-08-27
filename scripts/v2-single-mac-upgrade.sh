#!/bin/bash
set -euo pipefail

# Reviewed single-Mac V2 runtime upgrade helper.
# It deliberately does not mutate keys/trust, edit checkpoint JSON, or auto-restore
# an old binary over newer durable state after a failed post-start verification.

usage() {
  cat <<'USAGE'
usage: scripts/v2-single-mac-upgrade.sh [--preflight-only]

Environment overrides:
  CUMG_V2_INSTALL_ROOT       default: ~/Library/Application Support/computer-use-mcp-gateway
  CUMG_V2_RUN_ROOT           default: ~/Library/Caches/cumg-v2
  CUMG_V2_HUB_LABEL          default: com.github.git-ksk.cumg-v2-hub
  CUMG_V2_AGENT_LABEL        default: com.github.git-ksk.cumg-v2-agent
  CUMG_V2_SIGNER_LABEL       default: com.github.git-ksk.cumg-v2-grant-signer
  CUMG_V2_EXTERNAL_SIGNER    default: 1 (set 0 only for an explicitly reviewed legacy profile)
  CUMG_V2_EXPECTED_CUA_VERSION required; exact reviewed Cua version for post-upgrade v2_doctor
  CUMG_V2_MACOS_CODESIGN_FINGERPRINT preferred; exact 40-hex certificate fingerprint
  CUMG_V2_MACOS_CODESIGN_IDENTITY fallback; exact Apple code-signing identity name, must resolve uniquely
  CUMG_V2_MACOS_TEAM_ID       required; exact 10-character Apple Developer Team ID
  CUMG_V2_HANDOFF_SOURCE_ROOT required after first pinned cutover; reviewed Handoff checkout
  CUMG_V2_EXPECTED_HANDOFF_COMMIT required; exact reviewed mcp-execution-handoff commit
USAGE
}

PRELIGHT_ONLY=0
case "${1:-}" in
  "") ;;
  --preflight-only) PRELIGHT_ONLY=1 ;;
  -h|--help) usage; exit 0 ;;
  *) usage >&2; exit 64 ;;
esac

[[ "$(uname -s)" == "Darwin" ]] || { echo "REFUSED reason=macos_required" >&2; exit 2; }
command -v git >/dev/null || { echo "REFUSED reason=git_missing" >&2; exit 2; }
command -v cargo >/dev/null || { echo "REFUSED reason=cargo_missing" >&2; exit 2; }
command -v python3 >/dev/null || { echo "REFUSED reason=python3_missing" >&2; exit 2; }
command -v shasum >/dev/null || { echo "REFUSED reason=shasum_missing" >&2; exit 2; }
command -v launchctl >/dev/null || { echo "REFUSED reason=launchctl_missing" >&2; exit 2; }
command -v codesign >/dev/null || { echo "REFUSED reason=codesign_missing" >&2; exit 2; }
command -v security >/dev/null || { echo "REFUSED reason=security_missing" >&2; exit 2; }
command -v openssl >/dev/null || { echo "REFUSED reason=openssl_missing" >&2; exit 2; }
command -v plutil >/dev/null || { echo "REFUSED reason=plutil_missing" >&2; exit 2; }
command -v npm >/dev/null || { echo "REFUSED reason=npm_missing" >&2; exit 2; }
[[ -x /usr/libexec/PlistBuddy ]] || { echo "REFUSED reason=plistbuddy_missing" >&2; exit 2; }

REPO_ROOT="$(git rev-parse --show-toplevel 2>/dev/null)" || { echo "REFUSED reason=not_git_checkout" >&2; exit 2; }
cd "$REPO_ROOT"
[[ "$(git branch --show-current)" == "main" ]] || { echo "REFUSED reason=branch_not_main" >&2; exit 2; }
[[ -z "$(git status --porcelain=v1)" ]] || { echo "REFUSED reason=dirty_checkout" >&2; exit 2; }
git fetch --quiet origin main
HEAD="$(git rev-parse HEAD)"
ORIGIN_MAIN="$(git rev-parse origin/main)"
[[ "$HEAD" == "$ORIGIN_MAIN" ]] || { echo "REFUSED reason=main_diverged" >&2; exit 2; }

ROOT="${CUMG_V2_INSTALL_ROOT:-$HOME/Library/Application Support/computer-use-mcp-gateway}"
RUN_ROOT="${CUMG_V2_RUN_ROOT:-$HOME/Library/Caches/cumg-v2}"
BIN_DIR="$ROOT/bin"
HUB_STATE="$ROOT/v2/state/hub"
AGENT_STATE="$ROOT/v2/state/agent"
HUB_LABEL="${CUMG_V2_HUB_LABEL:-com.github.git-ksk.cumg-v2-hub}"
AGENT_LABEL="${CUMG_V2_AGENT_LABEL:-com.github.git-ksk.cumg-v2-agent}"
SIGNER_LABEL="${CUMG_V2_SIGNER_LABEL:-com.github.git-ksk.cumg-v2-grant-signer}"
EXTERNAL_SIGNER="${CUMG_V2_EXTERNAL_SIGNER:-1}"
EXPECTED_CUA_VERSION="${CUMG_V2_EXPECTED_CUA_VERSION:-}"
MACOS_CODESIGN_FINGERPRINT="${CUMG_V2_MACOS_CODESIGN_FINGERPRINT:-}"
MACOS_CODESIGN_IDENTITY="${CUMG_V2_MACOS_CODESIGN_IDENTITY:-}"
MACOS_TEAM_ID="${CUMG_V2_MACOS_TEAM_ID:-}"
EXPECTED_HANDOFF_COMMIT="${CUMG_V2_EXPECTED_HANDOFF_COMMIT:-}"
[[ -n "$EXPECTED_CUA_VERSION" ]] || { echo "REFUSED reason=expected_cua_version_required" >&2; exit 2; }
[[ -n "$MACOS_CODESIGN_FINGERPRINT" || -n "$MACOS_CODESIGN_IDENTITY" ]] || {
  echo "REFUSED reason=macos_codesign_selector_required" >&2; exit 2;
}
[[ -z "$MACOS_CODESIGN_FINGERPRINT" || "$MACOS_CODESIGN_FINGERPRINT" =~ ^[0-9A-Fa-f]{40}$ ]] || {
  echo "REFUSED reason=invalid_macos_codesign_fingerprint" >&2; exit 2;
}
[[ "$MACOS_TEAM_ID" =~ ^[A-Z0-9]{10}$ ]] || { echo "REFUSED reason=invalid_macos_team_id" >&2; exit 2; }
[[ "$EXPECTED_HANDOFF_COMMIT" =~ ^[0-9a-f]{40}$ ]] || { echo "REFUSED reason=invalid_expected_handoff_commit" >&2; exit 2; }
MACOS_CODESIGN_SELECTOR="$(python3 - "$MACOS_CODESIGN_IDENTITY" "$MACOS_CODESIGN_FINGERPRINT" "$MACOS_TEAM_ID" <<'PYIDENTITY'
import re, subprocess, sys
name, requested, expected_team = sys.argv[1:]
result = subprocess.run(
    ["security", "find-identity", "-v", "-p", "codesigning"],
    stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, text=True, check=False,
)
if result.returncode != 0:
    raise SystemExit(2)
pattern = re.compile(r'^\s*\d+\)\s+([0-9A-Fa-f]{40})\s+"([^"]+)"(?:\s+\(([^)]*)\))?\s*$')
valid = []
for line in result.stdout.splitlines():
    match = pattern.match(line)
    if not match:
        continue
    fingerprint, common_name, invalid_reason = match.groups()
    if invalid_reason is None:
        valid.append((fingerprint.upper(), common_name))
if requested:
    matches = [(fp, cn) for fp, cn in valid if fp == requested.upper() and (not name or cn == name)]
else:
    matches = [(fp, cn) for fp, cn in valid if cn == name]
if len(matches) != 1:
    raise SystemExit(3)
selector = matches[0][0]
certs = subprocess.run(
    ["security", "find-certificate", "-a", "-p"],
    stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, check=False,
).stdout
pem_pattern = re.compile(br'-----BEGIN CERTIFICATE-----.*?-----END CERTIFICATE-----', re.S)
matched_teams = []
for pem in pem_pattern.findall(certs):
    detail = subprocess.run(
        ["openssl", "x509", "-noout", "-fingerprint", "-sha1", "-subject", "-nameopt", "RFC2253"],
        input=pem, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, text=False, check=False,
    )
    if detail.returncode != 0:
        continue
    text = detail.stdout.decode("utf-8", "replace")
    fingerprint_match = re.search(r'Fingerprint=([0-9A-Fa-f:]+)', text)
    if not fingerprint_match or fingerprint_match.group(1).replace(":", "").upper() != selector:
        continue
    subject = next((line.split("=", 1)[1] for line in text.splitlines() if line.startswith("subject=")), "")
    team_match = re.search(r'(?:^|,)OU=([^,]+)', subject)
    if team_match:
        matched_teams.append(team_match.group(1))
if matched_teams != [expected_team]:
    raise SystemExit(4)
print(selector)
PYIDENTITY
)" || {
  echo "REFUSED reason=macos_codesign_identity_unavailable_ambiguous_or_team_mismatch" >&2; exit 2;
}
DOMAIN="gui/$(id -u)"
HUB_PLIST="$HOME/Library/LaunchAgents/$HUB_LABEL.plist"
AGENT_PLIST="$HOME/Library/LaunchAgents/$AGENT_LABEL.plist"
SIGNER_PLIST="$HOME/Library/LaunchAgents/$SIGNER_LABEL.plist"
LAUNCHCTL_BIN="$(command -v launchctl)"
LAUNCHD_TOPOLOGY_GUARD="$REPO_ROOT/scripts/v2_launchd_topology_guard.py"
[[ -f "$LAUNCHD_TOPOLOGY_GUARD" && ! -L "$LAUNCHD_TOPOLOGY_GUARD" ]] || {
  echo "REFUSED reason=launchd_topology_guard_missing_or_unsafe" >&2; exit 2;
}
python3 "$LAUNCHD_TOPOLOGY_GUARD" check \
  --domain "$DOMAIN" --hub-label "$HUB_LABEL" --agent-label "$AGENT_LABEL" \
  --launchctl "$LAUNCHCTL_BIN" || exit 2

MAINTENANCE_JOB_GUARD="$REPO_ROOT/scripts/v2_launchd_maintenance_job.py"
[[ -f "$MAINTENANCE_JOB_GUARD" && ! -L "$MAINTENANCE_JOB_GUARD" ]] || {
  echo "REFUSED reason=maintenance_job_guard_missing_or_unsafe" >&2; exit 2;
}
MAINTENANCE_JOB_LABEL="${CUMG_V2_MAINTENANCE_JOB_LABEL:-}"
MAINTENANCE_GUARD_ARGS=(
  --domain "$DOMAIN" --launchctl "$LAUNCHCTL_BIN" assert-clear
)
if [[ -n "$MAINTENANCE_JOB_LABEL" ]]; then
  case "$MAINTENANCE_JOB_LABEL" in
    com.github.git-ksk.cumg-v2-maintenance.*) ;;
    *) echo "REFUSED reason=invalid_current_maintenance_job_label" >&2; exit 2 ;;
  esac
  CURRENT_MAINTENANCE_JOB="$(launchctl print "$DOMAIN/$MAINTENANCE_JOB_LABEL" 2>/dev/null)" || {
    echo "REFUSED reason=current_maintenance_job_not_loaded" >&2; exit 2
  }
  CURRENT_MAINTENANCE_PID="$(printf '%s\n' "$CURRENT_MAINTENANCE_JOB" | awk '/^[[:space:]]*pid = / {print $3; exit}')"
  [[ "$CURRENT_MAINTENANCE_PID" == "$$" ]] || {
    echo "REFUSED reason=current_maintenance_job_pid_mismatch" >&2; exit 2
  }
  MAINTENANCE_GUARD_ARGS+=(--exclude-label "$MAINTENANCE_JOB_LABEL")
fi
python3 "$MAINTENANCE_JOB_GUARD" "${MAINTENANCE_GUARD_ARGS[@]}" || exit 2

[[ -x "$BIN_DIR/v2_maint" ]] || { echo "REFUSED reason=installed_maint_missing" >&2; exit 2; }
[[ -d "$HUB_STATE" && -d "$AGENT_STATE" ]] || { echo "REFUSED reason=state_directory_missing" >&2; exit 2; }
[[ -f "$HUB_PLIST" && -f "$AGENT_PLIST" ]] || { echo "REFUSED reason=launchd_profile_missing" >&2; exit 2; }
HANDOFF_ENV_FILE="$(/usr/libexec/PlistBuddy -c 'Print :EnvironmentVariables:CUMG_V2_HANDOFF_RUNTIME_ENV_FILE' "$AGENT_PLIST" 2>/dev/null || true)"
[[ "$HANDOFF_ENV_FILE" == /* && -f "$HANDOFF_ENV_FILE" && ! -L "$HANDOFF_ENV_FILE" ]] || {
  echo "REFUSED reason=agent_handoff_runtime_env_missing_or_unsafe" >&2; exit 2;
}
HANDOFF_HELPERS="$(python3 - "$HANDOFF_ENV_FILE" <<'PYHELPERS'
import pathlib, sys
path = pathlib.Path(sys.argv[1])
keys = {
    "CUMG_V2_HANDOFF_WEBRTC_HOST_EXECUTABLE": "com.github.git-ksk.cumg-v2-handoff-webrtc-host",
    "CUMG_V2_HANDOFF_NATIVE_HOST_EXECUTABLE": "com.github.git-ksk.cumg-v2-handoff-native-host",
    "CUMG_V2_HANDOFF_NATIVE_REVOKE_EXECUTABLE": "com.github.git-ksk.cumg-v2-handoff-native-revoke",
}
found = []
for raw in path.read_text(encoding="utf-8").splitlines():
    line = raw.strip()
    if not line or line.startswith("#") or "=" not in line:
        continue
    key, value = line.split("=", 1)
    key, value = key.strip(), value.strip()
    if key not in keys:
        continue
    if len(value) >= 2 and value[0] == value[-1] and value[0] in "\"'":
        value = value[1:-1]
    if not value or "\x00" in value or "\n" in value or "\r" in value:
        raise SystemExit(2)
    found.append((key, keys[key], value))
if not found:
    raise SystemExit(3)
for key, identifier, value in found:
    print(f"{key}\t{identifier}\t{value}")
PYHELPERS
)" || { echo "REFUSED reason=handoff_host_executable_missing" >&2; exit 2; }
while IFS=$'\t' read -r _key _identifier helper; do
  [[ -n "$helper" ]] || continue
  [[ "$helper" == "$ROOT"/v2/handoff/* && -f "$helper" && -x "$helper" && ! -L "$helper" ]] || {
    echo "REFUSED reason=handoff_host_executable_unsafe" >&2; exit 2;
  }
done <<< "$HANDOFF_HELPERS"
CONFIGURED_HANDOFF_ROOT="$(python3 - "$HANDOFF_ENV_FILE" <<'PYHANDOFFROOT'
import pathlib, sys
path = pathlib.Path(sys.argv[1])
value = None
for raw in path.read_text(encoding="utf-8").splitlines():
    line = raw.strip()
    if not line or line.startswith("#") or "=" not in line:
        continue
    key, candidate = line.split("=", 1)
    if key.strip() != "CUMG_V2_HANDOFF_ROOT":
        continue
    candidate = candidate.strip()
    if len(candidate) >= 2 and candidate[0] == candidate[-1] and candidate[0] in "\"'":
        candidate = candidate[1:-1]
    if value is not None or not candidate or "\x00" in candidate or "\n" in candidate or "\r" in candidate:
        raise SystemExit(2)
    value = candidate
if value is None:
    raise SystemExit(3)
print(value)
PYHANDOFFROOT
)" || { echo "REFUSED reason=handoff_source_root_missing" >&2; exit 2; }
HANDOFF_SOURCE_ROOT="${CUMG_V2_HANDOFF_SOURCE_ROOT:-$CONFIGURED_HANDOFF_ROOT}"
[[ "$HANDOFF_SOURCE_ROOT" == /* && -d "$HANDOFF_SOURCE_ROOT" && ! -L "$HANDOFF_SOURCE_ROOT" ]] || {
  echo "REFUSED reason=handoff_source_root_unsafe" >&2; exit 2;
}
[[ -f "$HANDOFF_SOURCE_ROOT/dist/index.js" && ! -L "$HANDOFF_SOURCE_ROOT/dist" ]] || {
  echo "REFUSED reason=handoff_dist_missing_or_unsafe" >&2; exit 2;
}
[[ -f "$HANDOFF_SOURCE_ROOT/package.json" && ! -L "$HANDOFF_SOURCE_ROOT/package.json" \
    && -f "$HANDOFF_SOURCE_ROOT/package-lock.json" && ! -L "$HANDOFF_SOURCE_ROOT/package-lock.json" ]] || {
  echo "REFUSED reason=handoff_package_lock_missing_or_unsafe" >&2; exit 2;
}
[[ "$(git -C "$HANDOFF_SOURCE_ROOT" branch --show-current)" == "main" ]] || {
  echo "REFUSED reason=handoff_branch_not_main" >&2; exit 2;
}
[[ -z "$(git -C "$HANDOFF_SOURCE_ROOT" status --porcelain=v1)" ]] || {
  echo "REFUSED reason=handoff_dirty_checkout" >&2; exit 2;
}
git -C "$HANDOFF_SOURCE_ROOT" fetch --quiet origin main
HANDOFF_HEAD="$(git -C "$HANDOFF_SOURCE_ROOT" rev-parse HEAD)"
HANDOFF_ORIGIN_MAIN="$(git -C "$HANDOFF_SOURCE_ROOT" rev-parse origin/main)"
[[ "$HANDOFF_HEAD" == "$HANDOFF_ORIGIN_MAIN" && "$HANDOFF_HEAD" == "$EXPECTED_HANDOFF_COMMIT" ]] || {
  echo "REFUSED reason=handoff_source_commit_mismatch" >&2; exit 2;
}
HANDOFF_CONTROL_SOCKET="$(/usr/libexec/PlistBuddy -c 'Print :EnvironmentVariables:CUMG_V2_HANDOFF_CONTROL_SOCKET' "$HUB_PLIST" 2>/dev/null || true)"
[[ "$HANDOFF_CONTROL_SOCKET" == /* ]] || { echo "REFUSED reason=handoff_control_socket_missing" >&2; exit 2; }
python3 - "$HANDOFF_CONTROL_SOCKET" <<'PYHANDOFFSTATUS' || {
import json, socket, sys
pathname = sys.argv[1]
client = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
client.settimeout(2)
try:
    client.connect(pathname)
    client.sendall(b'{"action":"status"}\n')
    data = b""
    while not data.endswith(b"\n") and len(data) <= 8192:
        chunk = client.recv(8192 - len(data) + 1)
        if not chunk:
            break
        data += chunk
finally:
    client.close()
if len(data) > 8192 or not data.endswith(b"\n"):
    raise SystemExit(2)
response = json.loads(data)
status = response.get("status") if response.get("ok") is True else None
if not isinstance(status, dict):
    raise SystemExit(3)
if (status.get("active") is not None or status.get("recovery_required") is not False
        or status.get("resume_requested") is not False or status.get("faulted") is not False):
    raise SystemExit(4)
PYHANDOFFSTATUS
  echo "REFUSED reason=handoff_not_idle_or_status_unavailable" >&2; exit 2;
}
if [[ "$EXTERNAL_SIGNER" == "1" ]]; then
  [[ -f "$SIGNER_PLIST" ]] || { echo "REFUSED reason=grant_signer_launchd_profile_missing" >&2; exit 2; }
  [[ -d "$RUN_ROOT" ]] || { echo "REFUSED reason=grant_signer_run_directory_missing" >&2; exit 2; }
elif [[ "$EXTERNAL_SIGNER" != "0" ]]; then
  echo "REFUSED reason=invalid_external_signer_flag" >&2
  exit 2
fi

QUARANTINE_JSON="$("$BIN_DIR/v2_maint" inspect-quarantine --state-dir "$HUB_STATE")" || {
  echo "REFUSED reason=quarantine_inspection_failed" >&2; exit 2;
}
QUARANTINE_COUNT="$(printf '%s' "$QUARANTINE_JSON" | python3 -c 'import json,sys; print(len(json.load(sys.stdin).get("quarantines", [])))')"
[[ "$QUARANTINE_COUNT" == "0" ]] || { echo "REFUSED reason=live_quarantine count=$QUARANTINE_COUNT" >&2; exit 2; }

for label in "$HUB_LABEL" "$AGENT_LABEL"; do
  launchctl print "$DOMAIN/$label" >/dev/null 2>&1 || { echo "REFUSED reason=service_not_loaded label=$label" >&2; exit 2; }
done
if [[ "$EXTERNAL_SIGNER" == "1" ]]; then
  launchctl print "$DOMAIN/$SIGNER_LABEL" >/dev/null 2>&1 || { echo "REFUSED reason=service_not_loaded label=$SIGNER_LABEL" >&2; exit 2; }
fi

verify_stable_codesign() {
  local pathname="$1" identifier="$2" expr designated
  expr="identifier \"$identifier\" and anchor apple generic and certificate leaf[subject.OU] = \"$MACOS_TEAM_ID\""
  codesign --verify --strict "$pathname" >/dev/null
  codesign -v -R="$expr" "$pathname" >/dev/null
  designated="$(codesign -dr - "$pathname" 2>&1)"
  [[ "$designated" == *"identifier \"$identifier\""* \
      && "$designated" == *"certificate leaf[subject.OU] = \"$MACOS_TEAM_ID\""* \
      && "$designated" != *"cdhash"* ]]
}

stable_codesign() {
  local pathname="$1" identifier="$2" expr requirement
  expr="identifier \"$identifier\" and anchor apple generic and certificate leaf[subject.OU] = \"$MACOS_TEAM_ID\""
  requirement="=designated => $expr"
  codesign --force --sign "$MACOS_CODESIGN_SELECTOR" --identifier "$identifier" \
    --requirements "$requirement" "$pathname" >/dev/null
  verify_stable_codesign "$pathname" "$identifier"
}

echo "PREFLIGHT_OK source_commit=$HEAD quarantine=0 handoff=agent_owned stable_tcc_signing=required"
[[ "$PRELIGHT_ONLY" == "1" ]] && exit 0

cargo build --release --locked \
  --bin v2_hub --bin v2_agent --bin v2_maint --bin v2_doctor --bin v2_recover --bin v2_grant_signer
for name in v2_hub v2_agent v2_maint v2_doctor v2_recover v2_grant_signer; do
  [[ -x "target/release/$name" ]] || { echo "REFUSED reason=build_output_missing binary=$name" >&2; exit 2; }
done
stable_codesign "target/release/v2_agent" "com.github.git-ksk.cumg-v2-agent" || {
  echo "REFUSED reason=agent_stable_codesign_failed" >&2; exit 2;
}
stable_codesign "target/release/v2_recover" "com.github.git-ksk.cumg-v2-recover" || {
  echo "REFUSED reason=recovery_cli_stable_codesign_failed" >&2; exit 2;
}

HANDOFF_RUNTIME_COMMAND="$(/usr/libexec/PlistBuddy -c 'Print :EnvironmentVariables:CUMG_V2_HANDOFF_RUNTIME_COMMAND' "$AGENT_PLIST" 2>/dev/null || true)"
[[ "$HANDOFF_RUNTIME_COMMAND" == /* && -x "$HANDOFF_RUNTIME_COMMAND" ]] || {
  echo "REFUSED reason=handoff_runtime_command_missing_or_unsafe" >&2; exit 2;
}
HANDOFF_RUNTIME_PREFLIGHT="$REPO_ROOT/scripts/v2_handoff_runtime_preflight.py"
[[ -f "$HANDOFF_RUNTIME_PREFLIGHT" && ! -L "$HANDOFF_RUNTIME_PREFLIGHT" ]] || {
  echo "REFUSED reason=handoff_runtime_preflight_missing_or_unsafe" >&2; exit 2
}
HANDOFF_RUNTIME_COMMAND_RESOLVED="$(python3 "$HANDOFF_RUNTIME_PREFLIGHT" resolve-executable --path "$HANDOFF_RUNTIME_COMMAND")" || {
  echo "REFUSED reason=handoff_runtime_command_resolution_failed" >&2; exit 2
}

install_handoff_runtime_dependencies() {
  local runtime_root="$1" bin_dir
  (
    cd "$runtime_root"
    npm ci --omit=dev --ignore-scripts --no-audit --no-fund >/dev/null
  ) || return 2
  # npm creates convenience command symlinks under node_modules/.bin. The managed
  # Handoff runtime imports packages directly and never executes these shims, while
  # runtime generations are intentionally symlink-free for manifest/cleanup safety.
  bin_dir="$runtime_root/node_modules/.bin"
  if [[ -L "$bin_dir" ]]; then
    return 3
  fi
  if [[ -e "$bin_dir" ]]; then
    [[ -d "$bin_dir" ]] || return 4
    rm -rf "$bin_dir"
  fi
  python3 - "$runtime_root/node_modules" <<'PYRUNTIMEDEPS'
import os, pathlib, sys
root = pathlib.Path(sys.argv[1])
if root.is_symlink() or not root.is_dir():
    raise SystemExit(2)
for base, directories, files in os.walk(root, topdown=True, followlinks=False):
    base = pathlib.Path(base)
    for name in [*directories, *files]:
        if (base / name).is_symlink():
            raise SystemExit(3)
PYRUNTIMEDEPS
}

HANDOFF_DIR="$ROOT/v2/handoff"
[[ -d "$HANDOFF_DIR" && ! -L "$HANDOFF_DIR" ]] || { echo "REFUSED reason=handoff_directory_unsafe" >&2; exit 2; }
CURRENT_HANDOFF_SCRIPT="$(/usr/libexec/PlistBuddy -c 'Print :EnvironmentVariables:CUMG_V2_HANDOFF_RUNTIME_SCRIPT' "$AGENT_PLIST" 2>/dev/null || true)"
[[ "$CURRENT_HANDOFF_SCRIPT" == "$HANDOFF_DIR"/runtime-*/* && -f "$CURRENT_HANDOFF_SCRIPT" && ! -L "$CURRENT_HANDOFF_SCRIPT" ]] || {
  echo "REFUSED reason=current_handoff_runtime_script_unsafe" >&2; exit 2;
}
CURRENT_HANDOFF_RUNTIME="$(dirname "$CURRENT_HANDOFF_SCRIPT")"
[[ "$(basename "$CURRENT_HANDOFF_RUNTIME")" =~ ^runtime-[0-9a-f]{7,40}(-[0-9a-f]{7,40})?$ && ! -L "$CURRENT_HANDOFF_RUNTIME" ]] || {
  echo "REFUSED reason=current_handoff_runtime_generation_unsafe" >&2; exit 2;
}
NEW_RUNTIME_NAME="runtime-${HEAD:0:12}-${HANDOFF_HEAD:0:12}"
NEW_HANDOFF_RUNTIME="$HANDOFF_DIR/$NEW_RUNTIME_NAME"
NEW_HANDOFF_RUNTIME_CREATED=0
NEW_HANDOFF_HELPERS=""

if [[ -e "$NEW_HANDOFF_RUNTIME" || -L "$NEW_HANDOFF_RUNTIME" ]]; then
  [[ -d "$NEW_HANDOFF_RUNTIME" && ! -L "$NEW_HANDOFF_RUNTIME" ]] || {
    echo "REFUSED reason=handoff_runtime_generation_existing_path_unsafe" >&2; exit 2;
  }
  python3 "$HANDOFF_RUNTIME_PREFLIGHT" verify-generation \
    --runtime-root "$NEW_HANDOFF_RUNTIME" \
    --expected-cumg-commit "$HEAD" \
    --expected-handoff-commit "$HANDOFF_HEAD" || {
    echo "REFUSED reason=handoff_runtime_generation_existing_validation_failed" >&2; exit 2;
  }
  python3 "$HANDOFF_RUNTIME_PREFLIGHT" verify-import \
    --runtime-command "$HANDOFF_RUNTIME_COMMAND_RESOLVED" \
    --entrypoint "$NEW_HANDOFF_RUNTIME/handoff-root/dist/index.js" \
    --require-export ExecutionHandoffState \
    --require-export InheritedFdNativeRuntimeProvider \
    --require-export SignedFileHandoffCheckpointStore \
    --require-export SpawnedWebRtcRuntimeProvider \
    --require-export TakeoverBroker \
    --require-export WindowHandoffAdapter \
    --require-export TerminalHandoffAdapter \
    --require-export claimHandoffOwner \
    --require-export createHandoffOwner || {
    echo "REFUSED reason=existing_handoff_runtime_import_failed" >&2; exit 2;
  }
  [[ -f "$NEW_HANDOFF_RUNTIME/handoff-root/node_modules/werift/package.json" ]] || {
    echo "REFUSED reason=existing_handoff_runtime_not_self_contained" >&2; exit 2;
  }
  while IFS=$'\t' read -r key identifier helper; do
    [[ -n "$helper" ]] || continue
    case "$key" in
      CUMG_V2_HANDOFF_WEBRTC_HOST_EXECUTABLE) reusable="$NEW_HANDOFF_RUNTIME/takeover-webrtc-host" ;;
      CUMG_V2_HANDOFF_NATIVE_HOST_EXECUTABLE) reusable="$NEW_HANDOFF_RUNTIME/takeover-native-host" ;;
      CUMG_V2_HANDOFF_NATIVE_REVOKE_EXECUTABLE) reusable="$NEW_HANDOFF_RUNTIME/takeover-native-revoke" ;;
      *) echo "REFUSED reason=unsupported_handoff_helper_key" >&2; exit 2 ;;
    esac
    [[ -f "$reusable" && -x "$reusable" && ! -L "$reusable" ]] || {
      echo "REFUSED reason=existing_handoff_host_executable_unsafe" >&2; exit 2;
    }
    verify_stable_codesign "$reusable" "$identifier" || {
      echo "REFUSED reason=existing_handoff_host_codesign_invalid" >&2; exit 2;
    }
    NEW_HANDOFF_HELPERS+="${key}"$'\t'"${identifier}"$'\t'"${helper}"$'\t'"${reusable}"$'\n'
  done <<< "$HANDOFF_HELPERS"
  echo "RUNTIME_REUSE_OK runtime_generation=$NEW_RUNTIME_NAME"
else
  STAGE_RUNTIME="$HANDOFF_DIR/.stage-${NEW_RUNTIME_NAME}-$$"
  [[ ! -e "$STAGE_RUNTIME" ]] || { echo "REFUSED reason=handoff_runtime_stage_exists" >&2; exit 2; }
  umask 077
  mkdir -p "$STAGE_RUNTIME/handoff-root"
  chmod 700 "$STAGE_RUNTIME" "$STAGE_RUNTIME/handoff-root"
  cp "$REPO_ROOT/scripts/v2_handoff_runtime.mjs" "$STAGE_RUNTIME/v2_handoff_runtime.mjs"
  chmod 700 "$STAGE_RUNTIME/v2_handoff_runtime.mjs"
  cp -R "$HANDOFF_SOURCE_ROOT/dist" "$STAGE_RUNTIME/handoff-root/dist"
  cp "$HANDOFF_SOURCE_ROOT/package.json" "$HANDOFF_SOURCE_ROOT/package-lock.json" "$STAGE_RUNTIME/handoff-root/"
  install_handoff_runtime_dependencies "$STAGE_RUNTIME/handoff-root" || {
    rm -rf "$STAGE_RUNTIME"
    echo "REFUSED reason=handoff_runtime_dependencies_install_or_symlink_validation_failed" >&2
    exit 2
  }
  python3 "$HANDOFF_RUNTIME_PREFLIGHT" verify-import \
    --runtime-command "$HANDOFF_RUNTIME_COMMAND_RESOLVED" \
    --entrypoint "$STAGE_RUNTIME/handoff-root/dist/index.js" \
    --require-export ExecutionHandoffState \
    --require-export InheritedFdNativeRuntimeProvider \
    --require-export SignedFileHandoffCheckpointStore \
    --require-export SpawnedWebRtcRuntimeProvider \
    --require-export TakeoverBroker \
    --require-export WindowHandoffAdapter \
    --require-export TerminalHandoffAdapter \
    --require-export claimHandoffOwner \
    --require-export createHandoffOwner || {
    rm -rf "$STAGE_RUNTIME"
    echo "REFUSED reason=staged_handoff_runtime_import_failed" >&2
    exit 2
  }
  while IFS=$'\t' read -r key identifier helper; do
    [[ -n "$helper" ]] || continue
    case "$key" in
      CUMG_V2_HANDOFF_WEBRTC_HOST_EXECUTABLE) staged="$STAGE_RUNTIME/takeover-webrtc-host" ;;
      CUMG_V2_HANDOFF_NATIVE_HOST_EXECUTABLE) staged="$STAGE_RUNTIME/takeover-native-host" ;;
      CUMG_V2_HANDOFF_NATIVE_REVOKE_EXECUTABLE) staged="$STAGE_RUNTIME/takeover-native-revoke" ;;
      *) echo "REFUSED reason=unsupported_handoff_helper_key" >&2; rm -rf "$STAGE_RUNTIME"; exit 2 ;;
    esac
    cp "$helper" "$staged"
    chmod 700 "$staged"
    if ! stable_codesign "$staged" "$identifier"; then
      rm -rf "$STAGE_RUNTIME"
      echo "REFUSED reason=staged_handoff_host_stable_codesign_failed" >&2
      exit 2
    fi
    NEW_HANDOFF_HELPERS+="${key}"$'\t'"${identifier}"$'\t'"${helper}"$'\t'"${staged}"$'\n'
  done <<< "$HANDOFF_HELPERS"
  python3 - "$HEAD" "$HANDOFF_HEAD" "$STAGE_RUNTIME" > "$STAGE_RUNTIME/runtime-generation-manifest.json" <<'PYRUNTIMEGEN'
import hashlib, json, pathlib, sys
cumg, handoff, root = sys.argv[1:]
root = pathlib.Path(root)
files = []
for path in sorted(p for p in root.rglob("*") if p.is_file() and p.name != "runtime-generation-manifest.json"):
    if path.is_symlink():
        raise SystemExit(2)
    digest = hashlib.sha256(path.read_bytes()).hexdigest()
    files.append({"path": path.relative_to(root).as_posix(), "sha256": digest})
print(json.dumps({
    "schema_version": 1,
    "cumg_source_commit": cumg,
    "handoff_source_commit": handoff,
    "files": files,
}, indent=2))
PYRUNTIMEGEN
  chmod 600 "$STAGE_RUNTIME/runtime-generation-manifest.json"
  mv "$STAGE_RUNTIME" "$NEW_HANDOFF_RUNTIME"
  NEW_HANDOFF_RUNTIME_CREATED=1
  # Rewrite staged helper destinations after the atomic directory rename.
  NEW_HANDOFF_HELPERS="${NEW_HANDOFF_HELPERS//$STAGE_RUNTIME/$NEW_HANDOFF_RUNTIME}"
fi

STAMP="$(date '+%Y%m%dT%H%M%S%z')"
ROLLBACK="$ROOT/rollback/runtime-upgrade-$STAMP"
umask 077
mkdir -p "$ROLLBACK/bin" "$ROLLBACK/state" "$ROLLBACK/launchd" "$ROLLBACK/handoff"
chmod 700 "$ROLLBACK" "$ROLLBACK/bin" "$ROLLBACK/state" "$ROLLBACK/launchd" "$ROLLBACK/handoff"
for name in v2_hub v2_agent v2_maint v2_doctor v2_recover v2_grant_signer; do
  [[ -f "$BIN_DIR/$name" ]] && cp -p "$BIN_DIR/$name" "$ROLLBACK/bin/$name"
done
cp -p "$HUB_PLIST" "$ROLLBACK/launchd/"
cp -p "$AGENT_PLIST" "$ROLLBACK/launchd/"
[[ "$EXTERNAL_SIGNER" == "1" ]] && cp -p "$SIGNER_PLIST" "$ROLLBACK/launchd/"
[[ -f "$ROOT/runtime-manifest.json" ]] && cp -p "$ROOT/runtime-manifest.json" "$ROLLBACK/runtime-manifest.json"
HELPER_INDEX=0
: > "$ROLLBACK/handoff/paths.tsv"
while IFS=$'\t' read -r key identifier helper; do
  [[ -n "$helper" ]] || continue
  HELPER_INDEX=$((HELPER_INDEX + 1))
  archived="$ROLLBACK/handoff/helper-$HELPER_INDEX"
  cp -p "$helper" "$archived"
  printf '%s\t%s\t%s\t%s\n' "$key" "$identifier" "$helper" "$archived" >> "$ROLLBACK/handoff/paths.tsv"
done <<< "$HANDOFF_HELPERS"
cp -p "$HANDOFF_ENV_FILE" "$ROLLBACK/handoff/managed-runtime.env"
chmod 600 "$ROLLBACK/handoff/managed-runtime.env"
cp -R "$CURRENT_HANDOFF_RUNTIME" "$ROLLBACK/handoff/runtime-generation"
if [[ ! -f "$ROLLBACK/handoff/runtime-generation/handoff-root/dist/index.js" \
      || ! -f "$ROLLBACK/handoff/runtime-generation/handoff-root/node_modules/werift/package.json" ]]; then
  mkdir -p "$ROLLBACK/handoff/runtime-generation/handoff-root"
  rm -rf "$ROLLBACK/handoff/runtime-generation/handoff-root/dist" "$ROLLBACK/handoff/runtime-generation/handoff-root/node_modules"
  cp -R "$HANDOFF_SOURCE_ROOT/dist" "$ROLLBACK/handoff/runtime-generation/handoff-root/dist"
  cp "$HANDOFF_SOURCE_ROOT/package.json" "$HANDOFF_SOURCE_ROOT/package-lock.json" "$ROLLBACK/handoff/runtime-generation/handoff-root/"
  install_handoff_runtime_dependencies "$ROLLBACK/handoff/runtime-generation/handoff-root" || {
    echo "REFUSED reason=rollback_handoff_dependencies_install_or_symlink_validation_failed rollback=$ROLLBACK" >&2
    exit 2
  }
fi
[[ -f "$ROLLBACK/handoff/runtime-generation/handoff-root/node_modules/werift/package.json" ]] || {
  echo "REFUSED reason=rollback_handoff_runtime_not_self_contained rollback=$ROLLBACK" >&2
  exit 2
}
python3 - "$HEAD" "$HANDOFF_HEAD" "$ROLLBACK/handoff/runtime-generation" > "$ROLLBACK/handoff/runtime-generation/runtime-generation-manifest.json" <<'PYARCHIVE'
import hashlib, json, pathlib, sys
cumg, handoff, root = sys.argv[1:]
root = pathlib.Path(root)
files = []
for path in sorted(p for p in root.rglob("*") if p.is_file() and p.name != "runtime-generation-manifest.json"):
    if path.is_symlink():
        raise SystemExit(2)
    files.append({
        "path": path.relative_to(root).as_posix(),
        "sha256": hashlib.sha256(path.read_bytes()).hexdigest(),
    })
print(json.dumps({
    "schema_version": 1,
    "archive_complete": True,
    "archived_by_source_commit": cumg,
    "handoff_source_commit": handoff,
    "files": files,
}, indent=2))
PYARCHIVE
printf '%s' "$NEW_HANDOFF_HELPERS" > "$ROLLBACK/handoff/new-paths.tsv"
chmod 600 "$ROLLBACK/handoff/new-paths.tsv"
printf '%s\n' "$HEAD" > "$ROLLBACK/replacement-source-commit.txt"

hub_pid() {
  launchctl print "$DOMAIN/$HUB_LABEL" 2>/dev/null | awk '/^[[:space:]]*pid = / {print $3; exit}'
}

# Hub first: close admission and allow its own bounded drain while Agent remains connected.
OLD_HUB_PID="$(hub_pid || true)"
[[ -n "$OLD_HUB_PID" ]] || { echo "REFUSED reason=hub_pid_unavailable rollback=$ROLLBACK" >&2; exit 2; }
kill -TERM "$OLD_HUB_PID"
DRAIN_DEADLINE=$(( $(date +%s) + 45 ))
while kill -0 "$OLD_HUB_PID" 2>/dev/null; do
  if (( $(date +%s) >= DRAIN_DEADLINE )); then
    echo "REFUSED reason=hub_drain_timeout rollback=$ROLLBACK" >&2
    exit 2
  fi
  sleep 1
done
launchctl bootout "$DOMAIN/$HUB_LABEL" >/dev/null 2>&1 || true
launchctl bootout "$DOMAIN/$AGENT_LABEL" >/dev/null 2>&1 || true
if [[ "$EXTERNAL_SIGNER" == "1" ]]; then
  launchctl bootout "$DOMAIN/$SIGNER_LABEL" >/dev/null 2>&1 || true
fi
if ! python3 "$LAUNCHD_TOPOLOGY_GUARD" retire-alternates \
  --domain "$DOMAIN" --hub-label "$HUB_LABEL" --agent-label "$AGENT_LABEL" \
  --launchctl "$LAUNCHCTL_BIN"; then
  echo "REFUSED reason=alternate_launchd_retirement_failed rollback=$ROLLBACK services_stopped=1" >&2
  exit 2
fi

# Capture the old authoritative state only after Hub admission is closed and drain completed.
cp -R "$HUB_STATE" "$ROLLBACK/state/hub"
cp -R "$AGENT_STATE" "$ROLLBACK/state/agent"
STOPPED_QUARANTINE_JSON="$("$BIN_DIR/v2_maint" inspect-quarantine --state-dir "$HUB_STATE")" || {
  echo "REFUSED reason=stopped_quarantine_inspection_failed rollback=$ROLLBACK" >&2
  exit 2
}
STOPPED_QUARANTINE_COUNT="$(printf '%s' "$STOPPED_QUARANTINE_JSON" | python3 -c 'import json,sys; print(len(json.load(sys.stdin).get("quarantines", [])))')"
if [[ "$STOPPED_QUARANTINE_COUNT" != "0" ]]; then
  [[ "$EXTERNAL_SIGNER" == "1" ]] && launchctl bootstrap "$DOMAIN" "$SIGNER_PLIST" >/dev/null 2>&1 || true
  launchctl bootstrap "$DOMAIN" "$HUB_PLIST" >/dev/null 2>&1 || true
  sleep 1
  launchctl bootstrap "$DOMAIN" "$AGENT_PLIST" >/dev/null 2>&1 || true
  echo "REFUSED reason=quarantine_created_during_drain count=$STOPPED_QUARANTINE_COUNT rollback=$ROLLBACK" >&2
  exit 2
fi

restore_preinstall_profile() {
  cp -p "$ROLLBACK/launchd/$(basename "$AGENT_PLIST")" "$AGENT_PLIST" 2>/dev/null || true
  cp -p "$ROLLBACK/handoff/managed-runtime.env" "$HANDOFF_ENV_FILE" 2>/dev/null || true
  chmod 600 "$HANDOFF_ENV_FILE" 2>/dev/null || true
  while IFS=$'\t' read -r _key _identifier helper archived; do
    [[ -n "$helper" && -f "$archived" ]] && cp -p "$archived" "$helper" || true
  done < "$ROLLBACK/handoff/paths.tsv"
  if [[ "$EXTERNAL_SIGNER" == "1" ]]; then
    launchctl bootstrap "$DOMAIN" "$SIGNER_PLIST" >/dev/null 2>&1 || true
    sleep 1
  fi
  launchctl bootstrap "$DOMAIN" "$HUB_PLIST" >/dev/null 2>&1 || true
  sleep 1
  launchctl bootstrap "$DOMAIN" "$AGENT_PLIST" >/dev/null 2>&1 || true
  if [[ "$NEW_HANDOFF_RUNTIME_CREATED" == "1" && "$NEW_HANDOFF_RUNTIME" == "$HANDOFF_DIR"/runtime-* \
      && -d "$NEW_HANDOFF_RUNTIME" && ! -L "$NEW_HANDOFF_RUNTIME" ]]; then
    rm -rf "$NEW_HANDOFF_RUNTIME" || true
  fi
}

if ! python3 - "$HANDOFF_ENV_FILE" "$NEW_HANDOFF_RUNTIME/handoff-root" "$ROLLBACK/handoff/new-paths.tsv" <<'PYENVREWRITE'
import os, pathlib, sys, tempfile
path = pathlib.Path(sys.argv[1])
new_root = sys.argv[2]
mapping_file = pathlib.Path(sys.argv[3])
if path.is_symlink() or not path.is_file() or not pathlib.Path(new_root).is_absolute():
    raise SystemExit(2)
replacements = {"CUMG_V2_HANDOFF_ROOT": new_root}
for raw in mapping_file.read_text(encoding="utf-8").splitlines():
    columns = raw.split("\t")
    if len(columns) != 4:
        raise SystemExit(3)
    key, _identifier, _old, new = columns
    if key in replacements or not pathlib.Path(new).is_absolute():
        raise SystemExit(4)
    replacements[key] = new
lines = path.read_text(encoding="utf-8").splitlines()
seen = set()
out = []
for raw in lines:
    stripped = raw.strip()
    if stripped and not stripped.startswith("#") and "=" in stripped:
        key = stripped.split("=", 1)[0].strip()
        if key in replacements:
            if key in seen:
                raise SystemExit(5)
            out.append(f"{key}={replacements[key]}")
            seen.add(key)
            continue
    out.append(raw)
if seen != set(replacements):
    raise SystemExit(6)
tmp = path.with_name(path.name + f".new.{os.getpid()}")
fd = os.open(tmp, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
try:
    payload = ("\n".join(out) + "\n").encode("utf-8")
    os.write(fd, payload)
    os.fsync(fd)
finally:
    os.close(fd)
os.replace(tmp, path)
PYENVREWRITE
then
  restore_preinstall_profile
  echo "REFUSED reason=handoff_runtime_env_update_failed rollback=$ROLLBACK" >&2
  exit 2
fi
if ! plutil -replace EnvironmentVariables.CUMG_V2_HANDOFF_RUNTIME_SCRIPT -string "$NEW_HANDOFF_RUNTIME/v2_handoff_runtime.mjs" "$AGENT_PLIST"; then
  restore_preinstall_profile
  echo "REFUSED reason=agent_handoff_runtime_script_update_failed rollback=$ROLLBACK" >&2
  exit 2
fi

install_atomic() {
  local source="$1" destination="$2" tmp
  mkdir -p "$(dirname "$destination")"
  tmp="$destination.new.$$"
  cp "$source" "$tmp"
  chmod 700 "$tmp"
  mv -f "$tmp" "$destination"
}
for name in v2_hub v2_agent v2_maint v2_doctor v2_recover v2_grant_signer; do
  install_atomic "target/release/$name" "$BIN_DIR/$name"
done

PACKAGE_VERSION="$(cargo metadata --no-deps --format-version 1 | python3 -c 'import json,sys; d=json.load(sys.stdin); print(next(p["version"] for p in d["packages"] if p["name"]=="computer-use-mcp-gateway"))')"
HUB_AGENT_SCHEMA_VERSION="$(python3 - "$REPO_ROOT/src/v2_m0_transport.rs" <<'PYSCHEMA'
import pathlib, re, sys
text = pathlib.Path(sys.argv[1]).read_text(encoding="utf-8")
match = re.search(r'pub const HUB_AGENT_SCHEMA_VERSION: u16 = ([0-9]+);', text)
if not match:
    raise SystemExit(2)
print(match.group(1))
PYSCHEMA
)" || { echo "REFUSED reason=hub_agent_schema_unavailable" >&2; exit 2; }
MANIFEST_TMP="$ROOT/runtime-manifest.json.new.$$"
python3 - "$HEAD" "$PACKAGE_VERSION" "$HUB_AGENT_SCHEMA_VERSION" "$BIN_DIR" > "$MANIFEST_TMP" <<'PY'
import hashlib, json, pathlib, sys
commit, version, hub_agent_schema, bindir = sys.argv[1:]
bindir = pathlib.Path(bindir)
names = ["v2_hub", "v2_agent", "v2_maint", "v2_doctor", "v2_recover", "v2_grant_signer"]
items = []
for name in names:
    h = hashlib.sha256()
    with (bindir / name).open("rb") as f:
        for chunk in iter(lambda: f.read(1024 * 1024), b""):
            h.update(chunk)
    items.append({"name": name, "sha256": h.hexdigest()})
print(json.dumps({
    "schema_version": 3,
    "hub_agent_schema_version": int(hub_agent_schema),
    "source_commit": commit,
    "package_version": version,
    "binaries": items,
}, indent=2))
PY
chmod 600 "$MANIFEST_TMP"
mv -f "$MANIFEST_TMP" "$ROOT/runtime-manifest.json"

fail_poststart() {
  local reason="$1"
  echo "POSTSTART_FAILED reason=$reason rollback=$ROLLBACK" >&2
  if launchctl print "$DOMAIN/$HUB_LABEL" >/dev/null 2>&1; then
    local pid
    pid="$(hub_pid || true)"
    [[ -n "$pid" ]] && kill -TERM "$pid" 2>/dev/null || true
    sleep 1
    launchctl bootout "$DOMAIN/$HUB_LABEL" >/dev/null 2>&1 || true
  fi
  launchctl bootout "$DOMAIN/$AGENT_LABEL" >/dev/null 2>&1 || true
  [[ "$EXTERNAL_SIGNER" == "1" ]] && launchctl bootout "$DOMAIN/$SIGNER_LABEL" >/dev/null 2>&1 || true
  echo "Services stopped fail-closed. Do not mix old binaries with new state; inspect rollback asset before explicit recovery." >&2
  exit 3
}

if [[ "$EXTERNAL_SIGNER" == "1" ]]; then
  launchctl bootstrap "$DOMAIN" "$SIGNER_PLIST" || fail_poststart "grant_signer_bootstrap"
  sleep 1
fi
launchctl bootstrap "$DOMAIN" "$HUB_PLIST" || fail_poststart "hub_bootstrap"
sleep 1
launchctl bootstrap "$DOMAIN" "$AGENT_PLIST" || fail_poststart "agent_bootstrap"
sleep 2
if ! python3 "$LAUNCHD_TOPOLOGY_GUARD" check \
  --domain "$DOMAIN" --hub-label "$HUB_LABEL" --agent-label "$AGENT_LABEL" \
  --launchctl "$LAUNCHCTL_BIN"; then
  fail_poststart "conflicting_launchd_topology"
fi

DOCTOR_ARGS=(
  --hub-state-dir "$HUB_STATE"
  --agent-state-dir "$AGENT_STATE"
  --runtime-manifest "$ROOT/runtime-manifest.json"
  --binary-dir "$BIN_DIR"
  --hub-launchd-label "$HUB_LABEL"
  --agent-launchd-label "$AGENT_LABEL"
  --handoff-control-socket "$HANDOFF_CONTROL_SOCKET"
  --json
)
if [[ "$EXTERNAL_SIGNER" == "1" ]]; then
  DOCTOR_ARGS+=(--grant-signer-launchd-label "$SIGNER_LABEL" --grant-signer-socket "$RUN_ROOT/grant-signer.sock")
else
  # Use a deliberately missing optional service only if the current doctor CLI grows a signer-mode flag.
  # The legacy profile is not the reviewed default and therefore skips automatic doctor success.
  echo "POSTSTART_FAILED reason=legacy_in_process_signer_requires_manual_doctor rollback=$ROLLBACK" >&2
  exit 3
fi
DOCTOR_ARGS+=(--tls-server-certificate "$ROOT/v2/trust/tls-server.pem" --tls-root-certificate "$ROOT/v2/trust/tls-root.der")
if [[ -x "$HOME/.local/bin/cua-driver" ]]; then
  DOCTOR_ARGS+=(--cua-command "$HOME/.local/bin/cua-driver")
  DOCTOR_ARGS+=(--expected-cua-version "$EXPECTED_CUA_VERSION")
fi

DOCTOR_OUTPUT=""
DOCTOR_OK=0
for _attempt in $(seq 1 15); do
  if DOCTOR_OUTPUT="$("$BIN_DIR/v2_doctor" "${DOCTOR_ARGS[@]}" 2>/dev/null)"; then
    DOCTOR_OK=1
    break
  fi
  sleep 1
done
[[ "$DOCTOR_OK" == "1" ]] || fail_poststart "doctor"
printf '%s\n' "$DOCTOR_OUTPUT"
if ! python3 "$REPO_ROOT/scripts/v2_handoff_runtime_cleanup.py" \
  --install-root "$ROOT" \
  --agent-plist "$AGENT_PLIST" \
  --rollback-root "$ROOT/rollback" \
  --runtime-manifest "$ROOT/runtime-manifest.json" \
  --expected-source-commit "$HEAD" \
  --keep-recent 2 \
  --health-confirmed \
  --apply; then
  echo "CLEANUP_DEFERRED reason=safety_refusal" >&2
  exit 4
fi
echo "UPGRADE_OK source_commit=$HEAD handoff_source_commit=$HANDOFF_HEAD runtime_generation=$NEW_RUNTIME_NAME rollback=$ROLLBACK"
