#!/usr/bin/env bash
set -euo pipefail

# Orphan Process Sweeper: Cleans lingering FSP subprocesses whose parent launcher has terminated
CLUSTER_ROOT="${1:-/tmp/fsp_cluster}"
PIDS_FILE="${CLUSTER_ROOT}/cluster.pids"

if [ ! -f "${PIDS_FILE}" ]; then
  exit 0
fi

LAUNCHER_PID=$(grep "LAUNCHER_PID=" "${PIDS_FILE}" | cut -d'=' -f2 || echo "")

if [ -n "${LAUNCHER_PID}" ] && ! kill -0 "${LAUNCHER_PID}" 2>/dev/null; then
  echo "Launcher parent ${LAUNCHER_PID} is DEAD. Sweeping orphaned child processes..."
  CHILD_PIDS=$(grep "PID=" "${PIDS_FILE}" | awk -F'PID=' '{print $2}')
  for pid in ${CHILD_PIDS}; do
    if kill -0 "${pid}" 2>/dev/null; then
      echo "Killing orphaned daemon PID ${pid} with SIGKILL..."
      kill -9 "${pid}" 2>/dev/null || true
    fi
  done
  rm -f "${PIDS_FILE}"
  echo "Orphan cleanup complete."
fi
