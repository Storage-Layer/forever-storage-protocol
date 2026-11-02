# Phase 5.4 Security Findings & Threat Analysis

## 1. Threat Model & Security Posture

The Phase 5.4 security assessment evaluated FSP against adversarial models including split-brain network partitions, unauthorized node intrusion, replay of captured coordinator permits, and ungraceful crash corruption.

## 2. Security Findings Matrix

| Finding ID | Vulnerability / Threat Area | Severity | Mitigation Implemented | Verified Status |
|---|---|---|---|---|
| **SEC-5.4-01** | Unauthorized Node Connection | High | Mutual TLS (mTLS) with Ed25519/ECDSA certificates; nodes reject connections lacking valid CA signature. | **VERIFIED HARDENED** |
| **SEC-5.4-02** | Stale Permit Replay (Split-Brain) | Critical | Monotonic coordinator incarnation epochs; storage nodes reject permits with stale incarnation numbers. | **VERIFIED HARDENED** |
| **SEC-5.4-03** | Expired Permit Replay | Medium | Time-bounded lease windows (default 15s) with millisecond timestamps and unique nonces. | **VERIFIED HARDENED** |
| **SEC-5.4-04** | GC / Repair Race Deletion | High | Exclusive mutex between GC tombstones and active object repair; prevents premature shard unlinking. | **VERIFIED HARDENED** |
| **SEC-5.4-05** | Silent Shard Data Corruption | High | Full CRC32C header/footer checksums + BLAKE3 object hash verification on every read. | **VERIFIED HARDENED** |
| **SEC-5.4-06** | Incomplete WAL Crash Consistency | High | Atomic manifest updates and synchronous fsync on commit records before acknowledging writes. | **VERIFIED HARDENED** |

## 3. Cryptographic Key Management

- **Root CA:** Ephemeral root CA generated for test clusters; in production, external PKI or Vault should provision CA.
- **Node Keys:** Each node generates its private key locally; keys are never transmitted over the wire.
- **Permit Signatures:** Signed using coordinator private keys and validated by storage nodes against coordinator public keys distributed at initialization.
