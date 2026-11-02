#!/usr/bin/env bash
# FSP Phase 5.8 Durability Audit Tool
set -euo pipefail

DATA_DIR="${1:-/tmp/fsp_cluster_phase5_8}"

echo "=========================================================="
echo "FSP Phase 5.8: Storage Durability & Container Audit"
echo "Target directory: $DATA_DIR"
echo "=========================================================="

if [ ! -d "$DATA_DIR" ]; then
    echo "Directory $DATA_DIR does not exist. No active cluster storage found."
    exit 0
fi

echo "--- 1. Node Storage Markers ---"
find "$DATA_DIR" -name ".fsp-node-marker" -exec ls -la {} + || echo "No markers found."

echo "--- 2. Shard Containers (*.fspc) ---"
CONTAINERS=$(find "$DATA_DIR" -name "*.fspc" || true)
if [ -n "$CONTAINERS" ]; then
    echo "$CONTAINERS" | wc -l | xargs echo "Total container files:"
else
    echo "No .fspc files found."
fi

echo "--- 3. Uncommitted Staging / Temporary Files (*.tmp) ---"
TMP_FILES=$(find "$DATA_DIR" -name "*.tmp" || true)
if [ -n "$TMP_FILES" ]; then
    echo "[WARNING] Found orphaned temporary files:"
    echo "$TMP_FILES"
else
    echo "No orphaned temporary files found."
fi

echo "Audit complete."
