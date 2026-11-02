#!/usr/bin/env bash
set -euo pipefail

# FSP Phase 5.3 Offline Emergency Read Tool
# Reconstructs object directly from surviving storage node shards using RS(20,12)

OBJECT_ID="${1:?Usage: $0 <object_id> <output_file> [token_file]}"
OUTPUT_FILE="${2:?Usage: $0 <object_id> <output_file> [token_file]}"
TOKEN_FILE="${3:-/var/lib/fsp/recovery/token.sig}"
STORAGE_ROOTS="${STORAGE_ROOTS:-/var/lib/fsp/nodes}"

echo "=== FSP Phase 5.3 Secure Emergency Read ==="
echo "Target Object: ${OBJECT_ID}"
echo "Output Path:   ${OUTPUT_FILE}"
echo "Token File:    ${TOKEN_FILE}"

if command -v fsp-sim >/dev/null 2>&1; then
    BIN="fsp-sim"
elif [ -f "./target/release/fsp-sim" ]; then
    BIN="./target/release/fsp-sim"
else
    BIN="cargo run --release --bin fsp-sim --"
fi

echo "Invoking offline emergency reconstruction..."
${BIN} emergency-read \
    --object-id "${OBJECT_ID}" \
    --output "${OUTPUT_FILE}" \
    --token "${TOKEN_FILE}" \
    --storage-roots "${STORAGE_ROOTS}"

echo "Emergency reconstruction complete. Verifying SHA-256..."
sha256sum "${OUTPUT_FILE}"
