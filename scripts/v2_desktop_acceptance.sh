#!/usr/bin/env bash
set -euo pipefail

if [[ "$(uname -s)" != "Darwin" ]]; then
  echo "V2 physical desktop acceptance currently requires macOS." >&2
  exit 1
fi

if [[ "${CUMG_DESKTOP_E2E_ACK:-}" != "1" ]]; then
  echo "Refusing physical desktop automation without CUMG_DESKTOP_E2E_ACK=1." >&2
  exit 1
fi
if [[ "${CUMG_V2_CUA_CANCEL_E2E_ACK:-}" != "1" ]]; then
  echo "Refusing V2 ambiguity acceptance without CUMG_V2_CUA_CANCEL_E2E_ACK=1." >&2
  exit 1
fi
if [[ "${CUMG_V2_NATIVE_ELEMENT_E2E_ACK:-}" != "1" ]]; then
  echo "Refusing native element-action acceptance without CUMG_V2_NATIVE_ELEMENT_E2E_ACK=1." >&2
  exit 1
fi

: "${CUMG_V2_CUA_COMMAND:=$(command -v cua-driver || true)}"
if [[ -z "$CUMG_V2_CUA_COMMAND" ]]; then
  echo "cua-driver was not found on PATH; set CUMG_V2_CUA_COMMAND explicitly." >&2
  exit 1
fi

export CUMG_DESKTOP_E2E_ACK
export CUMG_V2_CUA_CANCEL_E2E_ACK
export CUMG_V2_NATIVE_ELEMENT_E2E_ACK
export CUMG_V2_CUA_COMMAND
export PATH="$(dirname "$CUMG_V2_CUA_COMMAND"):$PATH"

"$CUMG_V2_CUA_COMMAND" permissions status
cargo +1.88.0 build --locked --bin v1_gateway
python3 scripts/cua_desktop_e2e.py

cargo +1.88.0 test --locked --lib \
  v2_m1_backend::tests::real_cua_native_element_action_acceptance \
  -- --ignored --exact --nocapture

cargo +1.88.0 test --locked --test v2_p1_real_cua_e2e \
  real_cua_indeterminate_survives_restart_without_replay_and_requires_resolution \
  -- --ignored --exact --nocapture

echo "PASS local physical desktop acceptance"
