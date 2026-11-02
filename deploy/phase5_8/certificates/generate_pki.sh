#!/usr/bin/env bash
# FSP Phase 5.8 PKI & Mutual TLS Certificate Generation Script
set -euo pipefail

PKI_DIR="${1:-/etc/fsp/pki}"
mkdir -p "$PKI_DIR"
chmod 700 "$PKI_DIR"

echo "=== FSP Phase 5.8: Generating Cluster PKI at $PKI_DIR ==="

# Root CA
if [ ! -f "$PKI_DIR/ca.key" ]; then
    openssl ecparam -name prime256v1 -genkey -noout -out "$PKI_DIR/ca.key"
    chmod 600 "$PKI_DIR/ca.key"
    openssl req -new -x509 -key "$PKI_DIR/ca.key" -out "$PKI_DIR/ca.crt" -days 3650 \
        -subj "/CN=FSP-Root-CA-Phase5-8"
fi

# Function to issue leaf certificate
issue_cert() {
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

# Coordinator
issue_cert "coordinator" "DNS:coordinator.fsp.internal,DNS:localhost,IP:127.0.0.1,IP:10.77.77.2,IP:192.168.1.103"

# Observer
issue_cert "observer" "DNS:observer.fsp.internal,DNS:localhost,IP:127.0.0.1"

# 20 Storage Nodes
for i in $(seq 0 19); do
    padded=$(printf "%02d" "$i")
    issue_cert "node-$padded" "DNS:node-$padded.fsp.internal,DNS:localhost,IP:127.0.0.1,IP:10.77.77.2,IP:192.168.1.103,IP:10.77.77.3,IP:192.168.1.104"
done

echo "=== Generated Root CA and 22 client/server certificates successfully ==="
