#!/usr/bin/env bash
set -euo pipefail

BACKUP_DIR="/var/backups/fsp/metadata"
COORD_DATA_DIR="/var/lib/fsp/coordinator"
TIMESTAMP=$(date -u +"%Y%m%d_%H%M%SZ")
BUNDLE_FILE="${BACKUP_DIR}/fsp_metadata_backup_${TIMESTAMP}.bundle"
OPERATOR_KEY="/etc/fsp/keys/dr_operator.key"

mkdir -p "${BACKUP_DIR}"

echo "[$(date -u)] Starting FSP metadata backup bundle generation..."

# Generate atomic signed metadata bundle
/opt/fsp/bin/fsp storage backup-metadata \
    --data-dir "${COORD_DATA_DIR}" \
    --operator-key "${OPERATOR_KEY}" \
    --output "${BUNDLE_FILE}"

echo "[$(date -u)] Metadata backup successfully saved to ${BUNDLE_FILE}"

# Keep last 192 backups (48 hours of 15-minute backups)
ls -tp "${BACKUP_DIR}"/fsp_metadata_backup_*.bundle | grep -v '/$' | tail -n +193 | xargs -I {} rm -- {} 2>/dev/null || true
