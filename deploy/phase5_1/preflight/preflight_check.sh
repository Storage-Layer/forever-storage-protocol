#!/usr/bin/env bash
set -euo pipefail

CONFIG_PATH="${1:-deploy/phase5_1/staging/cluster_staging.toml}"

echo "=========================================================="
echo "FSP Phase 5.1 Deployment Preflight Topology & Sanity Check"
echo "Config Path: ${CONFIG_PATH}"
echo "=========================================================="

if [[ ! -f "${CONFIG_PATH}" ]]; then
    echo "ERROR: Configuration file not found at ${CONFIG_PATH}"
    exit 1
fi

# Run preflight validation CLI command
cargo run --release -- preflight --config "${CONFIG_PATH}"

echo "Preflight check PASSED cleanly."
