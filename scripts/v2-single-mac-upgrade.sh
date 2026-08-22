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
[[ -n "$EXPECTED_CUA_VERSION" ]] || { echo "REFUSED reason=expected_cua_version_required" >&2; exit 2; }
DOMAIN="gui/$(id -u)"
HUB_PLIST="$HOME/Library/LaunchAgents/$HUB_LABEL.plist"
AGENT_PLIST="$HOME/Library/LaunchAgents/$AGENT_LABEL.plist"
SIGNER_PLIST="$HOME/Library/LaunchAgents/$SIGNER_LABEL.plist"

[[ -x "$BIN_DIR/v2_maint" ]] || { echo "REFUSED reason=installed_maint_missing" >&2; exit 2; }
[[ -d "$HUB_STATE" && -d "$AGENT_STATE" ]] || { echo "REFUSED reason=state_directory_missing" >&2; exit 2; }
[[ -f "$HUB_PLIST" && -f "$AGENT_PLIST" ]] || { echo "REFUSED reason=launchd_profile_missing" >&2; exit 2; }
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

echo "PREFLIGHT_OK source_commit=$HEAD quarantine=0"
[[ "$PRELIGHT_ONLY" == "1" ]] && exit 0

cargo build --release --locked \
  --bin v2_hub --bin v2_agent --bin v2_maint --bin v2_doctor --bin v2_grant_signer
for name in v2_hub v2_agent v2_maint v2_doctor v2_grant_signer; do
  [[ -x "target/release/$name" ]] || { echo "REFUSED reason=build_output_missing binary=$name" >&2; exit 2; }
done

STAMP="$(date '+%Y%m%dT%H%M%S%z')"
ROLLBACK="$ROOT/rollback/runtime-upgrade-$STAMP"
umask 077
mkdir -p "$ROLLBACK/bin" "$ROLLBACK/state" "$ROLLBACK/launchd"
chmod 700 "$ROLLBACK" "$ROLLBACK/bin" "$ROLLBACK/state" "$ROLLBACK/launchd"
for name in v2_hub v2_agent v2_maint v2_doctor v2_grant_signer; do
  [[ -f "$BIN_DIR/$name" ]] && cp -p "$BIN_DIR/$name" "$ROLLBACK/bin/$name"
done
cp -p "$HUB_PLIST" "$ROLLBACK/launchd/"
cp -p "$AGENT_PLIST" "$ROLLBACK/launchd/"
[[ "$EXTERNAL_SIGNER" == "1" ]] && cp -p "$SIGNER_PLIST" "$ROLLBACK/launchd/"
[[ -f "$ROOT/runtime-manifest.json" ]] && cp -p "$ROOT/runtime-manifest.json" "$ROLLBACK/runtime-manifest.json"
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

install_atomic() {
  local source="$1" destination="$2" tmp
  mkdir -p "$(dirname "$destination")"
  tmp="$destination.new.$$"
  cp "$source" "$tmp"
  chmod 700 "$tmp"
  mv -f "$tmp" "$destination"
}
for name in v2_hub v2_agent v2_maint v2_doctor v2_grant_signer; do
  install_atomic "target/release/$name" "$BIN_DIR/$name"
done

PACKAGE_VERSION="$(cargo metadata --no-deps --format-version 1 | python3 -c 'import json,sys; d=json.load(sys.stdin); print(next(p["version"] for p in d["packages"] if p["name"]=="computer-use-mcp-gateway"))')"
MANIFEST_TMP="$ROOT/runtime-manifest.json.new.$$"
python3 - "$HEAD" "$PACKAGE_VERSION" "$BIN_DIR" > "$MANIFEST_TMP" <<'PY'
import hashlib, json, pathlib, sys
commit, version, bindir = sys.argv[1:]
bindir = pathlib.Path(bindir)
names = ["v2_hub", "v2_agent", "v2_maint", "v2_doctor", "v2_grant_signer"]
items = []
for name in names:
    h = hashlib.sha256()
    with (bindir / name).open("rb") as f:
        for chunk in iter(lambda: f.read(1024 * 1024), b""):
            h.update(chunk)
    items.append({"name": name, "sha256": h.hexdigest()})
print(json.dumps({"schema_version": 1, "source_commit": commit, "package_version": version, "binaries": items}, indent=2))
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

DOCTOR_ARGS=(
  --hub-state-dir "$HUB_STATE"
  --agent-state-dir "$AGENT_STATE"
  --runtime-manifest "$ROOT/runtime-manifest.json"
  --binary-dir "$BIN_DIR"
  --hub-launchd-label "$HUB_LABEL"
  --agent-launchd-label "$AGENT_LABEL"
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
echo "UPGRADE_OK source_commit=$HEAD rollback=$ROLLBACK"
