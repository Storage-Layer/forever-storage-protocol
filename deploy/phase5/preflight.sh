#!/usr/bin/env bash
set -euo pipefail

# FSP Phase 5 Preflight Validation Script
CONFIG_PATH="${1:-deploy/phase5/cluster_5host.toml}"

echo "=== FSP Phase 5 Physical Deployment Preflight Validation ==="
echo "Validating configuration: ${CONFIG_PATH}"

if [ ! -f "${CONFIG_PATH}" ]; then
    echo "ERROR: Configuration file not found at ${CONFIG_PATH}"
    exit 1
fi

cargo run --release -- preflight --config "${CONFIG_PATH}"
cargo run --release -- topology validate --config "${CONFIG_PATH}"
cargo run --release -- cluster status --config "${CONFIG_PATH}"

echo "=== Preflight Validation Passed Successfully ==="
