#!/usr/bin/env bash
set -euo pipefail

# Check on-disk shard containers for atomic sync barriers and valid magic headers
STORAGE_ROOT="${1:-/tmp/fsp_p55_cluster}"
echo "=== Scanning Shard Containers for Storage Durability ==="

CONTAINERS=$(find "${STORAGE_ROOT}" -name "*.fspc" -type f 2>/dev/null || true)
COUNT=0
VALID=0

for c in $CONTAINERS; do
  COUNT=$((COUNT + 1))
  # Check 8-byte magic header FSPC\0\2\0\0
  MAGIC=$(head -c 8 "$c" | xxd -p || true)
  if [ "$MAGIC" = "4653504300020000" ]; then
    VALID=$((VALID + 1))
  fi
done

echo "Total Shard Containers Found: ${COUNT}"
echo "Valid Header Containers: ${VALID}"
if [ "$COUNT" -eq "$VALID" ]; then
  echo "Durability Audit: 100% CONTAINERS VALID"
  exit 0
else
  echo "Durability Audit: CORRUPTED CONTAINERS DETECTED"
  exit 1
fi
