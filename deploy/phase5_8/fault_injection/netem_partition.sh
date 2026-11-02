#!/usr/bin/env bash
# FSP Phase 5.8 Linux Traffic Control (tc/netem) Fault Injection Helper
set -euo pipefail

ACTION="${1:-status}"
IFACE="${2:-enp2s0}"

case "$ACTION" in
    status)
        echo "=== TC/Netem status on interface $IFACE ==="
        tc qdisc show dev "$IFACE" || true
        ;;
    add_latency)
        LATENCY="${3:-25ms}"
        JITTER="${4:-5ms}"
        echo "Adding $LATENCY ± $JITTER delay to $IFACE..."
        sudo tc qdisc add dev "$IFACE" root netem delay "$LATENCY" "$JITTER"
        ;;
    add_loss)
        PERCENT="${3:-5%}"
        echo "Adding $PERCENT packet loss to $IFACE..."
        sudo tc qdisc add dev "$IFACE" root netem loss "$PERCENT"
        ;;
    add_corruption)
        PERCENT="${3:-0.1%}"
        echo "Adding $PERCENT packet corruption to $IFACE..."
        sudo tc qdisc add dev "$IFACE" root netem corrupt "$PERCENT"
        ;;
    clear)
        echo "Clearing netem qdisc on $IFACE..."
        sudo tc qdisc del dev "$IFACE" root 2>/dev/null || true
        ;;
    *)
        echo "Usage: $0 {status|add_latency|add_loss|add_corruption|clear} [iface]"
        exit 1
        ;;
esac
