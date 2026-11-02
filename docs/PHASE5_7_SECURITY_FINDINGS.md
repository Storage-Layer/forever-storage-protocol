# FSP Phase 5.7: Security Audit & Adversarial Verification

**Milestone:** Phase 5.7  
**Artifact Path:** `docs/PHASE5_7_SECURITY_FINDINGS.md`  
**Machine-Readable Proof:** [`results/phase5_7/security_findings.json`](file:///home/rade/Project/Jobs/fsp/results/phase5_7/security_findings.json)  

---

## 1. Security Threat Model

The FSP architecture enforces zero-trust communication across all network and local boundaries. The security model considers:
- Untrusted network peers attempting unauthorized read or write access.
- Malicious or compromised nodes attempting to spoof ACKs or forge manifests.
- Local privilege escalation or denial-of-service via process supervisor PID reuse.
- Split-brain mutations caused by asymmetric network partitions.

---

## 2. Security Findings & Defenses

### Finding 1: Mutual TLS 1.3 Handshake Enforcement
- **Implementation:** Custom `rustls` server and client certificate verifiers (`rustls::client::danger::ServerCertVerifier`).
- **Enforcement:** All connections mandate valid TLS 1.3 certificates signed by the cluster root CA. Untrusted certificates, expired certificates, and mismatched Subject Alternative Names (SANs) are aborted at the TLS layer before parsing protocol frames.
- **Audit Result:** **SECURE (Verified in Test 03)**.

### Finding 2: Ed25519 Manifest & Transaction Signature Verification
- **Implementation:** [`src/security/signatures.rs`](file:///home/rade/Project/Jobs/fsp/src/security/signatures.rs).
- **Enforcement:** Coordinators digitally sign all manifests and journal state records using Ed25519 keypairs. Clients independently verify the cryptographic signature before trusting object stripe layouts. Tampered manifests are detected and rejected.
- **Audit Result:** **SECURE**.

### Finding 3: Durability ACK Spoofing & Inflation Prevention
- **Implementation:** [`src/coordinator/transactions.rs:727-760`](file:///home/rade/Project/Jobs/fsp/src/coordinator/transactions.rs).
- **Enforcement:** The coordinator binds each ACK to `(ack.node_id, ack.shard_id)` and records distinct shard indices in `BTreeSet<usize>`. Duplicate ACKs from rogue nodes cannot artificially inflate the commit threshold.
- **Audit Result:** **SECURE (Verified in Tests 04, 08, 09)**.

### Finding 4: Race-Free Process Termination via Linux `pidfd`
- **Implementation:** [`src/cluster/launcher.rs:677`](file:///home/rade/Project/Jobs/fsp/src/cluster/launcher.rs).
- **Enforcement:** Eliminates TOCTOU PID reuse races by pinning the exact kernel task struct via Linux `pidfd_open` (syscall 434) and delivering SIGKILL via `pidfd_send_signal` (syscall 424), cross-referenced with `START_TIME` in `/proc/{pid}/stat`.
- **Audit Result:** **SECURE (Verified in Tests 14, 15)**.

### Finding 5: Fail-Closed Quorum Enforcement
- **Implementation:** [`src/coordinator/transactions.rs`](file:///home/rade/Project/Jobs/fsp/src/coordinator/transactions.rs).
- **Enforcement:** If a network partition isolates more than 8 storage nodes, write mutations fail closed immediately, preventing inconsistent split-brain writes.
- **Audit Result:** **SECURE (Verified in Test 21)**.
