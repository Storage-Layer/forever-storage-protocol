# FSP Phase 5.8: Mutual TLS 1.3 Validation & Network Transport Hardening

## 1. Transport Security Architecture

FSP enforces mandatory Mutual Transport Layer Security (mTLS) for all cross-node and coordinator-to-node communications:
- **TLS Version:** TLS 1.3 exclusively (`rustls` with `ring` cryptographic provider).
- **Cipher Suites:** `TLS_AES_256_GCM_SHA384` and `TLS_CHACHA20_POLY1305_SHA256`.
- **Key Exchange:** Ephemeral Elliptic Curve Diffie-Hellman over Curve25519 (`X25519`) and NIST P-256 (`secp256r1`).
- **Certificate Standard:** X.509 v3 with ECDSA (`prime256v1`) signatures.

---

## 2. Non-Loopback Network Transport Patching

Prior to Phase 5.8, client TLS connection initialization contained hardcoded server name indications:
```rust
// Historical code in src/network/client.rs and connection_pool.rs:
ServerName::try_from("127.0.0.1")
ServerName::try_from("localhost")
```
Connecting to physical LAN IPs (e.g., `10.77.77.2`) would fail certificate SAN validation against certificates issued for those interfaces.

### Codebase Correction:
Both `src/network/client.rs` and `src/network/connection_pool.rs` were patched to dynamically extract the host component from the target socket address:
```rust
let host_part = addr.split(':').next().unwrap_or("127.0.0.1");
let server_name = rustls::pki_types::ServerName::try_from(host_part)
    .unwrap_or_else(|_| rustls::pki_types::ServerName::try_from("127.0.0.1").unwrap());
```
This enables transparent, cryptographically valid mTLS handshakes across any physical LAN IP or DNS hostname.

---

## 3. Remote Subject Alternative Names (SAN) & Certificate Binding

Leaf certificates are generated with Subject Alternative Names encompassing:
- Physical LAN IP: `IP:10.77.77.2`
- Wireless LAN IP: `IP:192.168.1.103`
- Secondary Target IP: `IP:10.77.77.3`
- Loopback IP: `IP:127.0.0.1`
- Cluster Internal DNS: `DNS:host-a.fsp.internal`, `DNS:node-XX.fsp.internal`

### Strict Mutual Binding:
`ClusterTrustPolicy` validates that incoming peer certificates not only chain to the cluster Root CA, but also match the registered SHA-256 fingerprint bound to that specific `NodeId` or `CoordinatorId`. Rogue certificates issued by other CAs or valid cluster certificates presented with mismatched identities are strictly rejected.

---

## 4. Execution Verdict

- **Local mTLS & SAN Suite:** Fully verified (`tests/phase5_8_validation.rs::test_p5_8_04`, `test_p5_8_05`, `test_p5_8_07`, `test_p5_8_09`).
- **Cross-Host Physical Wire Handshake:** Designated `NOT EXECUTED` (no authorized secondary physical host).
