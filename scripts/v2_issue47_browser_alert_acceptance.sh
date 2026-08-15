#!/usr/bin/env bash
set -euo pipefail

if [[ "$(uname -s)" != "Darwin" ]]; then
  echo "Issue #47 physical browser acceptance currently requires macOS." >&2
  exit 1
fi
if [[ "${CUMG_V2_ISSUE47_E2E_ACK:-}" != "1" ]]; then
  echo "Refusing real browser automation without CUMG_V2_ISSUE47_E2E_ACK=1." >&2
  exit 1
fi

: "${CUMG_V2_CUA_COMMAND:=$(command -v cua-driver || true)}"
: "${CUMG_V2_ISSUE47_CHROME_COMMAND:=/Applications/Google Chrome.app/Contents/MacOS/Google Chrome}"
if [[ -z "$CUMG_V2_CUA_COMMAND" || ! -x "$CUMG_V2_CUA_COMMAND" ]]; then
  echo "cua-driver was not found; set CUMG_V2_CUA_COMMAND explicitly." >&2
  exit 1
fi
if [[ ! -x "$CUMG_V2_ISSUE47_CHROME_COMMAND" ]]; then
  echo "Google Chrome was not found; set CUMG_V2_ISSUE47_CHROME_COMMAND explicitly." >&2
  exit 1
fi

root="$(mktemp -d "${TMPDIR:-/private/tmp}/cumg-issue47.XXXXXX")"
http_pid=""
chrome_pid=""
cleanup() {
  if [[ -n "$chrome_pid" ]]; then kill "$chrome_pid" 2>/dev/null || true; fi
  # Chrome helper processes can outlive the browser parent and recreate profile
  # files while cleanup runs. Match only this unique disposable user-data-dir.
  pkill -TERM -f -- "--user-data-dir=$root/profile" 2>/dev/null || true
  sleep 0.2
  pkill -KILL -f -- "--user-data-dir=$root/profile" 2>/dev/null || true
  if [[ -n "$http_pid" ]]; then kill "$http_pid" 2>/dev/null || true; fi
  rm -rf "$root" 2>/dev/null || true
  return 0
}
trap cleanup EXIT INT TERM
mkdir -p "$root/profile" "$root/www"
cat > "$root/www/index.html" <<'HTML'
<!doctype html><meta charset="utf-8"><title>CUMG issue47</title>
<button id="plain" onclick="document.getElementById('status').textContent='CLICK_OK'">plain</button>
<button id="alert" onclick="alert('GATEWAY_ALERT_OK')">alert</button>
<div id="status">READY</div>
HTML

read -r http_port devtools_port < <(python3 - <<'PY'
import socket
ports=[]
for _ in range(2):
    s=socket.socket()
    s.bind(("127.0.0.1", 0))
    ports.append(s.getsockname()[1])
    s.close()
print(*ports)
PY
)

python3 -m http.server "$http_port" --bind 127.0.0.1 --directory "$root/www" >"$root/http.log" 2>&1 &
http_pid=$!
"$CUMG_V2_ISSUE47_CHROME_COMMAND" \
  --user-data-dir="$root/profile" \
  --remote-debugging-port="$devtools_port" \
  --remote-allow-origins='*' \
  --no-first-run \
  --no-default-browser-check \
  "http://127.0.0.1:$http_port/" >"$root/chrome.log" 2>&1 &
chrome_pid=$!

for _ in $(seq 1 50); do
  if curl -fsS "http://127.0.0.1:$devtools_port/json/version" >/dev/null 2>&1; then
    break
  fi
  sleep 0.1
done
curl -fsS "http://127.0.0.1:$devtools_port/json/version" >/dev/null

export CUMG_V2_ISSUE47_E2E_ACK
export CUMG_V2_CUA_COMMAND
export CUMG_V2_ISSUE47_BROWSER_PID="$chrome_pid"

cargo +1.88.0 test --locked \
  v2_m1_backend::tests::real_cua_browser_alert_backend_error_is_indeterminate \
  -- --ignored --exact --nocapture

echo "PASS issue #47 real-Cua browser alert acceptance"
