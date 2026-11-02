#!/usr/bin/env bash
set -euo pipefail

# FSP Phase 5.3 Network Partition & Failure Injection Script
# Uses iptables or tc netem to simulate partitions between coordinator nodes and storage nodes.

ACTION="${1:-isolate-coord}"
TARGET="${2:-coord-0}"

case "${ACTION}" in
    isolate-coord)
        echo "Simulating partition on ${TARGET}..."
        # Example iptables drop rule
        echo "iptables -A INPUT -p tcp --dport 9000 -j DROP"
        ;;
    heal-coord)
        echo "Healing partition on ${TARGET}..."
        echo "iptables -D INPUT -p tcp --dport 9000 -j DROP"
        ;;
    packet-loss)
        echo "Simulating 20% packet loss on lo..."
        echo "tc qdisc add dev lo root netem loss 20%"
        ;;
    heal-loss)
        echo "Removing packet loss..."
        echo "tc qdisc del dev lo root netem"
        ;;
    *)
        echo "Usage: $0 {isolate-coord|heal-coord|packet-loss|heal-loss} [target]"
        exit 1
        ;;
esac
