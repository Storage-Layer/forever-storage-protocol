#!/usr/bin/env bash
# FSP Phase 5.8: Netlab Metrics Collection Script
# Reads kernel RX/TX counters from ip -s link, disk usage per node, and ping latencies.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../../.." && pwd)"
RUNTIME_DIR="$REPO_ROOT/results/phase5_8/netlab/runtime"
PID_FILE="$RUNTIME_DIR/supervisor.pid"

if [ ! -f "$PID_FILE" ] || ! kill -0 "$(cat "$PID_FILE")" 2>/dev/null; then
    echo "Supervisor is not running."
    exit 1
fi

SUP_PID="$(cat "$PID_FILE")"

echo "=== Virtual Network Interface Metrics ==="
RAW_JSON="$(nsenter -t "$SUP_PID" --preserve-credentials -U -m -n ip -s -j link show)"
python3 -c '
import sys, json
raw = json.loads(sys.argv[1])
out = []
for item in raw:
    stats = item.get("stats64", {})
    rx = stats.get("rx", {})
    tx = stats.get("tx", {})
    out.append({
        "ifname": item.get("ifname"),
        "operstate": item.get("operstate"),
        "rx_bytes": rx.get("bytes", 0),
        "rx_packets": rx.get("packets", 0),
        "tx_bytes": tx.get("bytes", 0),
        "tx_packets": tx.get("packets", 0),
        "rx_dropped": rx.get("dropped", 0),
        "tx_dropped": tx.get("dropped", 0)
    })
print(json.dumps(out, indent=2))
' "$RAW_JSON" > "$REPO_ROOT/results/phase5_8/netlab/virtual_network_metrics.json"

echo "Saved virtual_network_metrics.json"
