#!/usr/bin/env bash
# FSP Phase 5.8: Netlab Network Restore Script
# Removes all netem queuing disciplines and brings all interfaces UP.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../../.." && pwd)"
RUNTIME_DIR="$REPO_ROOT/results/phase5_8/netlab/runtime"
PID_FILE="$RUNTIME_DIR/supervisor.pid"

if [ ! -f "$PID_FILE" ] || ! kill -0 "$(cat "$PID_FILE")" 2>/dev/null; then
    echo "Supervisor is not running."
    exit 0
fi

SUP_PID="$(cat "$PID_FILE")"

echo "=== Restoring all network interfaces in netlab ==="
nsenter -t "$SUP_PID" --preserve-credentials -U -m -n bash -c '
for iface in $(ip link show | grep -o "v-[a-z0-9]*-br"); do
    tc qdisc del dev "$iface" root 2>/dev/null || true
    ip link set dev "$iface" up
done
ip link set dev fsp-br0 up
'
echo "All netem filters cleared and all links set to UP."
