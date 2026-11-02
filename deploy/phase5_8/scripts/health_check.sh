#!/usr/bin/env bash
# FSP Phase 5.8 Cluster Health Verification
set -euo pipefail

COORD_PORT="${1:-9000}"

echo "Checking FSP Coordinator health on port $COORD_PORT..."
if nc -z 127.0.0.1 "$COORD_PORT" 2>/dev/null; then
    echo "Coordinator is listening on 127.0.0.1:$COORD_PORT"
else
    echo "Coordinator port $COORD_PORT unreachable."
fi
