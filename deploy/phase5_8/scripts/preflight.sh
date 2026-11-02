#!/usr/bin/env bash
# FSP Phase 5.8 Preflight Environmental and Hardware Audit
set -euo pipefail

echo "=========================================================="
echo "FSP Phase 5.8: Physical Environment & Readiness Preflight"
echo "=========================================================="

echo "--- 1. Operating System & Kernel ---"
uname -s -r -v -m
if [ -f /etc/os-release ]; then
    grep -E '^(NAME|VERSION)=' /etc/os-release
fi

echo "--- 2. CPU Architecture ---"
lscpu | grep -E '(Model name|Architecture|CPU\(s\):|Thread\(s\) per core|Socket\(s\):)'

echo "--- 3. Memory ---"
free -h

echo "--- 4. Network Interfaces & IPs ---"
ip -brief addr show

echo "--- 5. Binaries ---"
BIN_NODE="target/release/fsp-node"
BIN_SIM="target/release/fsp-sim"

if [ -f "$BIN_NODE" ]; then
    echo "fsp-node: FOUND (SHA-256: $(sha256sum "$BIN_NODE" | awk '{print $1}'))"
else
    echo "fsp-node: NOT BUILT (run cargo build --release)"
fi

if [ -f "$BIN_SIM" ]; then
    echo "fsp-sim: FOUND (SHA-256: $(sha256sum "$BIN_SIM" | awk '{print $1}'))"
else
    echo "fsp-sim: NOT BUILT (run cargo build --release)"
fi

echo "--- 6. Secondary Host Gate Status ---"
if [ -z "${FSP_REMOTE_HOST:-}" ]; then
    echo "FSP_REMOTE_HOST not defined in environment."
    echo "Status: BLOCKED: No authorized independent physical host available."
else
    echo "Target remote host: $FSP_REMOTE_HOST"
fi

echo "=========================================================="
echo "Preflight check completed."
echo "=========================================================="
