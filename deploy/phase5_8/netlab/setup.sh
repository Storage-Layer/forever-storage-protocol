#!/usr/bin/env bash
# FSP Phase 5.8: Virtual Distributed Network (Netlab) Setup Script
# Creates 23 isolated network namespaces, veth interfaces, and private Linux bridge fsp-br0.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../../.." && pwd)"
NETLAB_DIR="$REPO_ROOT/deploy/phase5_8/netlab"
RUNTIME_DIR="$REPO_ROOT/results/phase5_8/netlab/runtime"
PID_FILE="$RUNTIME_DIR/supervisor.pid"

mkdir -p "$RUNTIME_DIR/pki"

echo "=== 1. Checking / Starting Supervisor Namespace ==="
if [ -f "$PID_FILE" ] && kill -0 "$(cat "$PID_FILE")" 2>/dev/null; then
    echo "Supervisor is already running with PID $(cat "$PID_FILE")"
else
    echo "Starting unshare supervisor process..."
    setsid unshare -r -m -n bash -c "echo \$\$ > '$PID_FILE'; mount -t tmpfs none /run; mkdir -p /run/netns; exec sleep 86400" </dev/null >/dev/null 2>&1 &
    sleep 0.5
    echo "Supervisor started with PID $(cat "$PID_FILE")"
fi

SUP_PID="$(cat "$PID_FILE")"

run_in_netlab() {
    nsenter -t "$SUP_PID" --preserve-credentials -U -m -n bash -c "$1"
}

echo "=== 2. Provisioning Bridge and 23 Namespaces inside Netlab ==="
SETUP_CMDS='
mount -t tmpfs none /run
mkdir -p /run/netns

# Create Linux bridge
if ! ip link show fsp-br0 >/dev/null 2>&1; then
    ip link add fsp-br0 type bridge
    ip link set fsp-br0 up
    ip addr add 10.240.58.1/24 dev fsp-br0
fi

# 3 Coordinator Namespaces (fsp-c01..03)
for i in 1 2 3; do
    ns="fsp-c0$i"
    ip netns add "$ns" 2>/dev/null || true
    if ! ip link show "v-c0$i-br" >/dev/null 2>&1; then
        ip link add "veth-c0$i" type veth peer name "v-c0$i-br"
        ip link set "veth-c0$i" netns "$ns"
        ip link set "v-c0$i-br" master fsp-br0
        ip link set "v-c0$i-br" up
        ip netns exec "$ns" ip link set lo up
        ip netns exec "$ns" ip addr add "10.240.58.1$i/24" dev "veth-c0$i"
        ip netns exec "$ns" ip link set "veth-c0$i" up
    fi
done

# 20 Storage Node Namespaces (fsp-n01..20)
for i in $(seq 1 20); do
    padded=$(printf "%02d" "$i")
    ns="fsp-n$padded"
    ip netns add "$ns" 2>/dev/null || true
    if ! ip link show "v-n$padded-br" >/dev/null 2>&1; then
        ip link add "veth-n$padded" type veth peer name "v-n$padded-br"
        ip link set "veth-n$padded" netns "$ns"
        ip link set "v-n$padded-br" master fsp-br0
        ip link set "v-n$padded-br" up
        ip netns exec "$ns" ip link set lo up
        ip netns exec "$ns" ip addr add "10.240.58.$((100 + i))/24" dev "veth-n$padded"
        ip netns exec "$ns" ip link set "veth-n$padded" up
    fi
done
'

run_in_netlab "$SETUP_CMDS"

echo "=== 3. Generating Netlab Cluster PKI ==="
PKI_DIR="$RUNTIME_DIR/pki"
if [ ! -f "$PKI_DIR/ca.crt" ]; then
    echo "Generating Root CA..."
    openssl ecparam -name prime256v1 -genkey -noout -out "$PKI_DIR/ca.key"
    chmod 600 "$PKI_DIR/ca.key"
    openssl req -new -x509 -key "$PKI_DIR/ca.key" -out "$PKI_DIR/ca.crt" -days 365 \
        -subj "/CN=FSP-Netlab-Root-CA"
fi

issue_netlab_cert() {
    local name="$1"
    local san="$2"
    local key="$PKI_DIR/$name.key"
    local csr="$PKI_DIR/$name.csr"
    local crt="$PKI_DIR/$name.crt"
    local ext="$PKI_DIR/$name.ext"

    openssl ecparam -name prime256v1 -genkey -noout -out "$key"
    chmod 600 "$key"
    openssl req -new -key "$key" -out "$csr" -subj "/CN=$name"

    cat > "$ext" <<EOF
basicConstraints=CA:FALSE
keyUsage=critical,digitalSignature,keyEncipherment
extendedKeyUsage=serverAuth,clientAuth
subjectAltName=$san
EOF

    openssl x509 -req -in "$csr" -CA "$PKI_DIR/ca.crt" -CAkey "$PKI_DIR/ca.key" \
        -CAcreateserial -out "$crt" -days 365 -extfile "$ext"
    rm -f "$csr" "$ext"
}

# Coordinator certs
issue_netlab_cert "coordinator" "DNS:coordinator-primary.fsp.internal,DNS:localhost,IP:10.240.58.11,IP:127.0.0.1"
issue_netlab_cert "coordinator-standby" "DNS:coordinator-standby.fsp.internal,DNS:localhost,IP:10.240.58.12,IP:127.0.0.1"
issue_netlab_cert "coordinator-witness" "DNS:coordinator-witness.fsp.internal,DNS:localhost,IP:10.240.58.13,IP:127.0.0.1"
issue_netlab_cert "observer" "DNS:observer.fsp.internal,DNS:localhost,IP:127.0.0.1"

# 20 Node certs
for i in $(seq 0 19); do
    padded=$(printf "%02d" "$i")
    ip="10.240.58.$((101 + i))"
    issue_netlab_cert "node-$padded" "DNS:node-$padded.fsp.internal,DNS:localhost,IP:$ip,IP:127.0.0.1"
done

echo "=== Netlab setup complete: 23 namespaces, bridge fsp-br0, and cluster PKI ready ==="
