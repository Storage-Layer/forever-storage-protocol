# FSP Phase 5.8: Security Findings, Cryptographic Controls & Threat Modeling

## 1. Threat Model & Security Architecture

FSP protects distributed storage clusters against adversarial network actors, compromised nodes, and stale control-plane commands using multi-layered defense-in-depth:

```
+─────────────────────────────────────────────────────────────+
| Transport Layer: Mutual TLS 1.3 (X25519 + AES-256-GCM)      |
+─────────────────────────────────────────────────────────────+
| Identity Layer: Strict NodeId-to-Cert-Fingerprint Binding   |
+─────────────────────────────────────────────────────────────+
| Control Layer: Monotonic Quorum-Signed Epoch Permits        |
+─────────────────────────────────────────────────────────────+
| Data Layer: Ed25519 Manifest Signatures + SHA-256 Digests   |
+─────────────────────────────────────────────────────────────+
| Filesystem Layer: Path Traversal & Symlink Defense Bounds   |
+─────────────────────────────────────────────────────────────+
```

---

## 2. Key Security Findings and Mitigations

### 2.1 Non-Loopback SNI Injection Vulnerability (Patched)
- **Finding:** Client TLS connection initialization previously used static `ServerName::try_from("127.0.0.1")`. Attempting to connect to physical LAN IPs resulted in hostname mismatch rejections.
- **Resolution:** Patched `src/network/client.rs` and `src/network/connection_pool.rs` to dynamically parse the target host/IP and pass it to SNI resolution.

### 2.2 Replay Protection via Monotonic Epoch Fencing
- **Finding:** A partitioned or zombie coordinator might attempt to issue mutations after a standby has stepped up to Leader.
- **Resolution:** Storage nodes reject any permit with an epoch strictly less than their durable `highest_seen_epoch`.

### 2.3 Filesystem Boundary and Symlink Traversal
- **Finding:** Arbitrary object ID or shard ID strings could attempt path traversal (`../`) to overwrite host system files.
- **Resolution:** Enforced strict regex identifier validation `^[a-zA-Z0-9_\-]+$` and invoked `ensure_not_symlink` and `check_path_has_no_traversal` prior to all disk operations.
