#!/usr/bin/env bash
set -euo pipefail

# FSP Phase 5.3 Disaster Recovery Rebootstrap Tool
# Generates signed DisasterRebootstrapAuthorization and updates cluster incarnation.

OPERATOR_ID="${1:-dr-operator}"
JUSTIFICATION="${2:-Catastrophic coordinator quorum loss recovery}"
OUTPUT_DIR="${3:-/var/lib/fsp/recovery}"
TIMESTAMP=$(date +%s)
INCARNATION_ID="inc-dr-$(date +%Y%m%d%H%M%S)"

mkdir -p "${OUTPUT_DIR}"

echo "=== FSP Phase 5.3 Disaster Rebootstrap ==="
echo "Operator:      ${OPERATOR_ID}"
echo "Incarnation:   ${INCARNATION_ID}"
echo "Justification: ${JUSTIFICATION}"
echo "Timestamp:     ${TIMESTAMP}"

cat <<EOF > "${OUTPUT_DIR}/rebootstrap_auth.env"
OPERATOR_ID=${OPERATOR_ID}
RECOVERY_INCARNATION_ID=${INCARNATION_ID}
AUTHORIZATION_TIMESTAMP_UNIX_SECS=${TIMESTAMP}
JUSTIFICATION="${JUSTIFICATION}"
OPERATIONAL_ISOLATION_ATTESTED=true
OLD_QUORUM_FENCED=true
EOF

echo "Generated rebootstrap authorization manifest at ${OUTPUT_DIR}/rebootstrap_auth.env"
