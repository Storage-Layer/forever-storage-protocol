#!/usr/bin/env bash
# FSP Phase 5.7: PKI Generation Script with Physical SANs
# Generates Cluster CA, Coordinator Certificates, and Storage Node Certificates
# including Subject Alternative Names for physical LAN IP addresses and hostnames.

set -euo pipefail

OUT_DIR="${1:-./deploy/phase5_7/certificates/pki}"
mkdir -p "${OUT_DIR}"

echo "[PKI] Generating FSP Phase 5.7 TLS 1.3 PKI in ${OUT_DIR}..."

# 1. Generate Root CA
openssl req -x509 -newkey rsa:4096 -days 3650 -nodes \
    -keyout "${OUT_DIR}/ca.key" -out "${OUT_DIR}/ca.crt" \
    -subj "/CN=FSP-Cluster-CA-Phase57/O=Forever Storage Protocol"

# 2. Coordinator Certificate with SANs
cat > "${OUT_DIR}/coord_ext.cnf" <<EOF
[v3_req]
basicConstraints = CA:FALSE
keyUsage = nonRepudiation, digitalSignature, keyEncipherment
subjectAltName = @alt_names

[alt_names]
DNS.1 = localhost
DNS.2 = fsp-host-a.lan
DNS.3 = fsp-host-b.lan
DNS.4 = coordinator.fsp.local
IP.1 = 127.0.0.1
IP.2 = 192.168.1.10
IP.3 = 192.168.1.11
IP.4 = 10.10.1.11
EOF

openssl req -newkey rsa:2048 -nodes \
    -keyout "${OUT_DIR}/coordinator.key" -out "${OUT_DIR}/coordinator.csr" \
    -subj "/CN=fsp-coordinator/O=Forever Storage Protocol"

openssl x509 -req -in "${OUT_DIR}/coordinator.csr" \
    -CA "${OUT_DIR}/ca.crt" -CAkey "${OUT_DIR}/ca.key" -CAcreateserial \
    -out "${OUT_DIR}/coordinator.crt" -days 365 -extfile "${OUT_DIR}/coord_ext.cnf" -extensions v3_req

# 3. Storage Node Certificates (0..19)
for nid in $(seq 0 19); do
    cat > "${OUT_DIR}/node_${nid}_ext.cnf" <<EOF
[v3_req]
basicConstraints = CA:FALSE
keyUsage = nonRepudiation, digitalSignature, keyEncipherment
subjectAltName = @alt_names

[alt_names]
DNS.1 = localhost
DNS.2 = node-${nid}.fsp.local
DNS.3 = fsp-host-a.lan
DNS.4 = fsp-host-b.lan
IP.1 = 127.0.0.1
IP.2 = 192.168.1.10
IP.3 = 192.168.1.11
EOF

    openssl req -newkey rsa:2048 -nodes \
        -keyout "${OUT_DIR}/node_${nid}.key" -out "${OUT_DIR}/node_${nid}.csr" \
        -subj "/CN=fsp-node-${nid}/O=Forever Storage Protocol"

    openssl x509 -req -in "${OUT_DIR}/node_${nid}.csr" \
        -CA "${OUT_DIR}/ca.crt" -CAkey "${OUT_DIR}/ca.key" -CAcreateserial \
        -out "${OUT_DIR}/node_${nid}.crt" -days 365 -extfile "${OUT_DIR}/node_${nid}_ext.cnf" -extensions v3_req
    rm -f "${OUT_DIR}/node_${nid}.csr" "${OUT_DIR}/node_${nid}_ext.cnf"
done

rm -f "${OUT_DIR}/coordinator.csr" "${OUT_DIR}/coord_ext.cnf"
echo "[PKI] Successfully generated Cluster CA and 21 certificates (coordinator + 20 storage nodes)."
