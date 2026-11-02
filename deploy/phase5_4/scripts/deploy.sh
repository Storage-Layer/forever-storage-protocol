#!/usr/bin/env bash
set -euo pipefail

# deploy.sh: Automated build, directory preparation, and binary installation
TARGET_HOST="${1:-localhost}"
BIN_DIR="${2:-/usr/local/bin}"

echo "=== FSP Deployment Script ==="
echo "Building release binaries..."
cargo build --release --bin fsp-node --bin fsp-sim

if [ "$TARGET_HOST" = "localhost" ]; then
  echo "Installing binaries locally to ${BIN_DIR}..."
  mkdir -p "${BIN_DIR}"
  cp target/release/fsp-node "${BIN_DIR}/"
  cp target/release/fsp-sim "${BIN_DIR}/"
  echo "Local installation complete."
else
  echo "Deploying to remote target ${TARGET_HOST}..."
  scp target/release/fsp-node "${TARGET_HOST}:${BIN_DIR}/"
  scp target/release/fsp-sim "${TARGET_HOST}:${BIN_DIR}/"
  echo "Remote deployment to ${TARGET_HOST} complete."
fi
