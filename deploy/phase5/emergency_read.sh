#!/usr/bin/env bash
set -euo pipefail

# FSP Phase 5 Emergency Offline Recovery Tool
# Reconstructs data directly from surviving storage node roots without coordinator quorum.
OBJECT_ID="${1:-}"
OUTPUT_PATH="${2:-}"
CONFIG_PATH="${3:-deploy/phase5/cluster_5host.toml}"

if [ -z "${OBJECT_ID}" ] || [ -z "${OUTPUT_PATH}" ]; then
    echo "Usage: $0 <object-id> <output-file> [config-path]"
    exit 1
fi

echo "=== FSP Emergency Offline Shard Recovery ==="
echo "Recovering Object: ${OBJECT_ID}"
echo "Writing Output to: ${OUTPUT_PATH}"

cargo run --release -- emergency-read --id "${OBJECT_ID}" --config "${CONFIG_PATH}" --output "${OUTPUT_PATH}"
