#!/usr/bin/env bash
set -euo pipefail
if [[ "$(uname -s)" != "Darwin" ]]; then
  echo "macOS Secure Enclave recovery helper can only be built on macOS" >&2
  exit 2
fi
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
OUT="${1:-$ROOT/target/release/v2_recovery_enclave_helper}"
mkdir -p "$(dirname "$OUT")"
xcrun swiftc -O -whole-module-optimization \
  "$ROOT/native/macos-recovery-helper.swift" \
  -o "$OUT"
chmod 700 "$OUT"
"$OUT" --version
