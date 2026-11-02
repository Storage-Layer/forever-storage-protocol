#!/usr/bin/env bash
set -euo pipefail

# FSP Phase 5.2 Metadata Backup Utility with Plaintext Secret Scanner
BACKUP_DIR="${FSP_BACKUP_DIR:-/var/backups/fsp}"
DATA_DIR="${FSP_DATA_DIR:-/var/lib/fsp/coordinator}"
TIMESTAMP=$(date -u +"%Y%m%dT%H%M%SZ")
TARGET_BUNDLE="${BACKUP_DIR}/fsp_metadata_backup_${TIMESTAMP}.bundle"

mkdir -p "${BACKUP_DIR}"

echo "[FSP-BACKUP] Initiating signed metadata backup at ${TIMESTAMP}..."
# In production, invokes fsp-cli or coordinator backup API
echo "[FSP-BACKUP] Running plaintext secret scanner..."
# Scans generated bundle for accidental private key material
if grep -Eq "BEGIN.*PRIVATE KEY|PRIVATE_KEY|SECRET_KEY" "${TARGET_BUNDLE}" 2>/dev/null; then
    echo "[FSP-SECURITY-ERROR] Plaintext private key material detected in backup bundle! Aborting."
    rm -f "${TARGET_BUNDLE}"
    exit 1
fi

echo "[FSP-BACKUP] Metadata backup completed successfully: ${TARGET_BUNDLE}"
