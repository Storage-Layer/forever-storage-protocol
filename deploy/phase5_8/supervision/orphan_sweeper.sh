#!/usr/bin/env bash
# FSP Phase 5.8 Process Supervision and Orphan Sweeper
set -euo pipefail

echo "=== FSP Orphan Process Sweeper ==="
PATTERNS=("fsp-node" "fsp-coordinator")

for pat in "${PATTERNS[@]}"; do
    PIDS=$(pgrep -f "$pat" || true)
    if [ -n "$PIDS" ]; then
        echo "Found active instances for pattern '$pat': $PIDS"
        for pid in $PIDS; do
            # Check PPID (if PPID == 1, it is an orphaned daemon)
            ppid=$(ps -o ppid= -p "$pid" | tr -d ' ' || echo "0")
            if [ "$ppid" -eq 1 ]; then
                echo "Terminating orphan PID $pid (parent is init)..."
                kill -TERM "$pid" 2>/dev/null || true
                sleep 1
                kill -KILL "$pid" 2>/dev/null || true
            else
                echo "PID $pid is managed by active parent $ppid."
            fi
        done
    else
        echo "No instances matching '$pat' found."
    fi
done

echo "Sweeper run complete."
