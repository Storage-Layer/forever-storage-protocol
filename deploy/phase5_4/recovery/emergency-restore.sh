#!/usr/bin/env bash
set -euo pipefail

BACKUP_BUNDLE="${1:-/var/backups/fsp/latest.bundle}"
TARGET_COORD_DIR="${2:-/var/lib/fsp/coordinator_restored}"
TARGET_INCARNATION="${3:-fsp-cluster-prod:2:$(date +%s)}"

echo "=== FSP Phase 5.4 Disaster Recovery & Emergency Restore ==="
echo "Backup Bundle: ${BACKUP_BUNDLE}"
echo "Target Dir: ${TARGET_COORD_DIR}"
echo "Target Incarnation: ${TARGET_INCARNATION}"

mkdir -p "${TARGET_COORD_DIR}"

fsp-sim restore \
  --bundle "${BACKUP_BUNDLE}" \
  --target-dir "${TARGET_COORD_DIR}" \
  --target-incarnation "${TARGET_INCARNATION}" \
  --strict-metadata-only

echo "Restore successfully completed."
