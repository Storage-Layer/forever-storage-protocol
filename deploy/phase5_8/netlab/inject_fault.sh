#!/usr/bin/env bash
# FSP Phase 5.8: Netlab Fault Injection Script
# Provides tc netem, link up/down, process killing/restarting, and network profile application.
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

run_in_supervisor() {
    nsenter -t "$SUP_PID" --preserve-credentials -U -m -n bash -c "$1"
}

CMD="${1:-help}"

case "$CMD" in
    netem-delay)
        IFACE="$2"
        DELAY="$3"
        JITTER="${4:-0ms}"
        echo "Injecting netem delay $DELAY ($JITTER jitter) on $IFACE"
        run_in_supervisor "tc qdisc del dev $IFACE root 2>/dev/null || true; tc qdisc add dev $IFACE root netem delay $DELAY $JITTER"
        ;;

    netem-loss)
        IFACE="$2"
        LOSS="$3"
        echo "Injecting netem loss $LOSS on $IFACE"
        run_in_supervisor "tc qdisc del dev $IFACE root 2>/dev/null || true; tc qdisc add dev $IFACE root netem loss $LOSS"
        ;;

    link-down)
        IFACE="$2"
        echo "Setting link DOWN on $IFACE"
        run_in_supervisor "ip link set dev $IFACE down"
        ;;

    link-up)
        IFACE="$2"
        echo "Setting link UP on $IFACE"
        run_in_supervisor "ip link set dev $IFACE up"
        ;;

    kill-node)
        NODE_ID="$2"
        padded=$(printf "%02d" "$NODE_ID")
        pid_file="$RUNTIME_DIR/nodes/node_$padded.pid"
        if [ -f "$pid_file" ]; then
            pid=$(cat "$pid_file")
            echo "SIGKILL to Node $padded (PID $pid)"
            kill -9 "$pid" 2>/dev/null || true
            rm -f "$pid_file" "$RUNTIME_DIR/nodes/node_$padded.ready"
        else
            echo "Node $padded not running"
        fi
        ;;

    restart-node)
        NODE_ID="$2"
        padded=$(printf "%02d" "$NODE_ID")
        idx=$((NODE_ID + 1))
        ns=$(printf "fsp-n%02d" "$idx")
        ip="10.240.58.$((101 + NODE_ID))"
        node_root="$RUNTIME_DIR/nodes/node_$padded"
        ready_file="$RUNTIME_DIR/nodes/node_$padded.ready"
        pid_file="$RUNTIME_DIR/nodes/node_$padded.pid"
        log_file="$RUNTIME_DIR/nodes/node_$padded.log"

        echo "Restarting Node $padded in $ns ($ip:9100)..."
        rm -f "$ready_file"
        setsid nsenter -t "$SUP_PID" --preserve-credentials -U -m -n ip netns exec "$ns" \
            "$REPO_ROOT/target/release/fsp-node" \
                --node-id "$NODE_ID" \
                --listen "$ip:9100" \
                --storage-root "$node_root" \
                --ca-cert "$RUNTIME_DIR/pki/ca.crt" \
                --node-cert "$RUNTIME_DIR/pki/node-$padded.crt" \
                --node-key "$RUNTIME_DIR/pki/node-$padded.key" \
                --coord-cert "$RUNTIME_DIR/pki/coordinator.crt" \
                --observer-cert "$RUNTIME_DIR/pki/observer.crt" \
                --ready-file "$ready_file" </dev/null >>"$log_file" 2>&1 &
        echo $! > "$pid_file"

        timeout=15
        while [ ! -f "$ready_file" ] && [ "$timeout" -gt 0 ]; do
            sleep 0.2
            timeout=$((timeout - 1))
        done
        if [ ! -f "$ready_file" ]; then
            echo "ERROR: Node $padded failed to restart!"
            exit 1
        fi
        echo "Node $padded successfully restarted."
        ;;

    kill-coordinator)
        COORD_PID="$RUNTIME_DIR/coordinator.pid"
        if [ -f "$COORD_PID" ]; then
            pid=$(cat "$COORD_PID")
            echo "SIGKILL to Coordinator (PID $pid)"
            kill -9 "$pid" 2>/dev/null || true
            rm -f "$COORD_PID" "$RUNTIME_DIR/coordinator.ready"
        fi
        ;;

    profile-baseline)
        echo "Applying BASELINE profile (no artificial delay/loss on any interface)"
        run_in_supervisor '
        for iface in $(ip link show | grep -o "v-[a-z0-9]*-br"); do
            tc qdisc del dev "$iface" root 2>/dev/null || true
            ip link set dev "$iface" up
        done
        '
        ;;

    profile-lan)
        echo "Applying LAN profile (0.5ms delay on all storage interfaces)"
        run_in_supervisor '
        for iface in $(ip link show | grep -o "v-[a-z0-9]*-br"); do
            tc qdisc del dev "$iface" root 2>/dev/null || true
            tc qdisc add dev "$iface" root netem delay 0.5ms
            ip link set dev "$iface" up
        done
        '
        ;;

    profile-wan)
        echo "Applying WAN profile (20ms delay on storage node interfaces)"
        run_in_supervisor '
        for iface in $(ip link show | grep -o "v-n[0-9]*-br"); do
            tc qdisc del dev "$iface" root 2>/dev/null || true
            tc qdisc add dev "$iface" root netem delay 20ms
            ip link set dev "$iface" up
        done
        '
        ;;

    profile-degraded-wan)
        echo "Applying DEGRADED WAN profile (35ms delay, 5ms jitter, 0.5% loss on storage node interfaces)"
        run_in_supervisor '
        for iface in $(ip link show | grep -o "v-n[0-9]*-br"); do
            tc qdisc del dev "$iface" root 2>/dev/null || true
            tc qdisc add dev "$iface" root netem delay 35ms 5ms loss 0.5%
            ip link set dev "$iface" up
        done
        '
        ;;

    *)
        echo "Usage: $0 {netem-delay|netem-loss|link-down|link-up|kill-node|restart-node|kill-coordinator|profile-baseline|profile-lan|profile-wan|profile-degraded-wan}"
        exit 1
        ;;
esac
