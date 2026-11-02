#!/usr/bin/env bash
set -euo pipefail

# FSP Phase 5.3 Mutual TLS 1.3 Certificate Authority & Node Certificate Provisioner
CERT_DIR="${1:-./deploy/phase5_3/certificates}"
mkdir -p "${CERT_DIR}"

echo "=== FSP Phase 5.3 Certificate Generation ==="
# 1. Generate Root CA
openssl req -x509 -newkey rsa:4096 -days 3650 -nodes \
    -keyout "${CERT_DIR}/ca-key.pem" \
    -out "${CERT_DIR}/ca-cert.pem" \
    -subj "/CN=fsp-cluster-ca/O=Forever Storage Protocol/OU=Phase 5.3"

# 2. Generate Coordinator Certificates
for id in coord-0 coord-1 coord-2; do
    openssl req -newkey rsa:2048 -nodes \
        -keyout "${CERT_DIR}/${id}-key.pem" \
        -out "${CERT_DIR}/${id}-req.pem" \
        -subj "/CN=${id}/O=Forever Storage Protocol/OU=Coordinators"
    openssl x509 -req -in "${CERT_DIR}/${id}-req.pem" -days 365 \
        -CA "${CERT_DIR}/ca-cert.pem" -CAkey "${CERT_DIR}/ca-key.pem" -CAcreateserial \
        -out "${CERT_DIR}/${id}-cert.pem"
    rm -f "${CERT_DIR}/${id}-req.pem"
done

# 3. Generate Storage Node Certificates (20 nodes)
for i in $(seq 0 19); do
    id="node-${i}"
    openssl req -newkey rsa:2048 -nodes \
        -keyout "${CERT_DIR}/${id}-key.pem" \
        -out "${CERT_DIR}/${id}-req.pem" \
        -subj "/CN=${id}/O=Forever Storage Protocol/OU=StorageNodes"
    openssl x509 -req -in "${CERT_DIR}/${id}-req.pem" -days 365 \
        -CA "${CERT_DIR}/ca-cert.pem" -CAkey "${CERT_DIR}/ca-key.pem" -CAcreateserial \
        -out "${CERT_DIR}/${id}-cert.pem"
    rm -f "${CERT_DIR}/${id}-req.pem"
done

echo "Generated TLS 1.3 mutual authentication certificates for CA, 3 coordinators, and 20 storage nodes."
