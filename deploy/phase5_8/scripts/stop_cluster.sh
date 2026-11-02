#!/usr/bin/env bash
# FSP Phase 5.8 Cluster Shutdown Script
set -euo pipefail

echo "Stopping FSP daemons..."
pkill -TERM -f "fsp-node" 2>/dev/null || true
pkill -TERM -f "fsp-coordinator" 2>/dev/null || true
sleep 1
pkill -KILL -f "fsp-node" 2>/dev/null || true
pkill -KILL -f "fsp-coordinator" 2>/dev/null || true
echo "All FSP daemons stopped."
