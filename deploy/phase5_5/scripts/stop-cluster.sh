#!/usr/bin/env bash
set -euo pipefail

BASE_DIR="${FSP_BASE_DIR:-/tmp/fsp_p55_cluster}"
echo "=== Stopping FSP Cluster ==="

if [ -d "${BASE_DIR}/run" ]; then
  for pidfile in "${BASE_DIR}/run/"*.pid; do
    if [ -f "$pidfile" ]; then
      pid=$(cat "$pidfile")
      if kill -0 "$pid" 2>/dev/null; then
        kill -15 "$pid" 2>/dev/null || true
      fi
      rm -f "$pidfile"
    fi
  done
fi

pkill -f "target/release/fsp-node" || true
echo "FSP Cluster stopped cleanly."
