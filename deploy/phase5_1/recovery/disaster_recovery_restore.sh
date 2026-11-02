#!/usr/bin/env bash
set -euo pipefail

if [[ $# -lt 2 ]]; then
    echo "Usage: $0 <backup-bundle-file> <target-coordinator-dir> [--force]"
    exit 1
fi

BUNDLE_FILE="$1"
TARGET_DIR="$2"
FORCE_FLAG="${3:-}"

OPERATOR_PUBKEY="/etc/fsp/keys/dr_operator.pub"

if [[ ! -f "${BUNDLE_FILE}" ]]; then
    echo "ERROR: Backup bundle does not exist: ${BUNDLE_FILE}"
    exit 1
fi

echo "=========================================================="
echo "FSP DISASTER RECOVERY RESTORE PROCEDURE"
echo "Target Directory: ${TARGET_DIR}"
echo "Bundle:           ${BUNDLE_FILE}"
echo "=========================================================="

# Stop coordinator service if running
echo "Ensuring local coordinator service is stopped..."
systemctl stop fsp-coordinator 2>/dev/null || true

# Execute restore
if [[ "${FORCE_FLAG}" == "--force" ]]; then
    /opt/fsp/bin/fsp storage restore-metadata \
        --bundle "${BUNDLE_FILE}" \
        --target-dir "${TARGET_DIR}" \
        --operator-key "${OPERATOR_PUBKEY}" \
        --force
else
    /opt/fsp/bin/fsp storage restore-metadata \
        --bundle "${BUNDLE_FILE}" \
        --target-dir "${TARGET_DIR}" \
        --operator-key "${OPERATOR_PUBKEY}"
fi

echo "Restore completed successfully. Re-starting coordinator..."
systemctl start fsp-coordinator
echo "FSP Coordinator online and synchronized."
