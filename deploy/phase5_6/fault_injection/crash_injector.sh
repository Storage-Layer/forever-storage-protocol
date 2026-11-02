#!/usr/bin/env bash
set -euo pipefail

# Injects deterministic crash points (SIGKILL) to storage nodes or coordinators
TARGET_PID="${1:-}"
SIGNAL="${2:-9}"

if [ -z "${TARGET_PID}" ]; then
  echo "Usage: $0 <PID> [SIGNAL_NUM]"
  exit 1
fi

echo "Injecting fault signal ${SIGNAL} to PID ${TARGET_PID}..."
kill "-${SIGNAL}" "${TARGET_PID}" 2>/dev/null || true
echo "Fault injected."
