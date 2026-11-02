# FSP Phase 5.7: Network Fault Tolerance & Partition Resilience

**Milestone:** Phase 5.7  
**Artifact Path:** `docs/PHASE5_7_NETWORK_FAULTS.md`  
**Machine-Readable Proof:** [`results/phase5_7/network_faults.json`](file:///home/rade/Project/Jobs/fsp/results/phase5_7/network_faults.json)  
**Tests:** `tests/phase5_6_validation.rs` (Tests 03, 12, 21)  

---

## 1. Network Fault Scenarios Evaluated

Under real-world distributed conditions, networks exhibit packet loss, transient disconnections, symmetric/asymmetric partitions, and malicious or unauthenticated traffic. FSP implements explicit defenses against each class of fault.

---

## 2. Tested Fault Conditions & Empirical Outcomes

### Scenario 1: Unauthorized Node Rejection & TLS Handshake Abort
- **Test:** `test_p5_6_03_tls_identity_and_unauthorized_node_rejection`
- **Mechanism:** A malicious client or rogue node attempts to connect without presenting an authentic certificate signed by the cluster root CA, or presents a certificate with mismatched SANs.
- **Observed Behavior:** `rustls` terminates the TLS handshake immediately during certificate verification (`BadCertificate`). No data bytes or RPC messages are parsed.
- **Verdict:** **PASS (Zero unauthorized access)**.

### Scenario 2: Network Timeout & Idempotent Transaction Replay
- **Test:** `test_p5_6_12_client_timeout_and_idempotent_transaction_replay`
- **Mechanism:** A client initiates a write transaction, but the network drops the commit acknowledgment. The client times out and retransmits the exact same transaction ID (`tx_id`).
- **Observed Behavior:** The coordinator checks its persistent journal. Detecting that `tx_id` has already committed, it returns the previously committed manifest without re-executing stripe encoding or allocating duplicate storage blocks.
- **Verdict:** **PASS (Strict idempotency)**.

### Scenario 3: Correlated Partition & Quorum Loss Fail-Closed
- **Test:** `test_p5_6_21_quorum_loss_mutation_fails_closed_safely`
- **Mechanism:** A catastrophic network partition isolates more than 8 storage nodes, leaving fewer than $K=12$ reachable nodes. A client attempts to execute a PUT mutation.
- **Observed Behavior:** The coordinator probes nodes during the preparation phase. Upon discovering fewer than 12 reachable nodes, it aborts the transaction immediately with `StorageError::InsufficientDurableQuorum`. No half-written stripes or unrecoverable objects are committed.
- **Verdict:** **PASS (Fails closed; split-brain prevention)**.

---

## 3. Network Resilience Summary

| Fault Condition | Mitigation Mechanism | Protocol Outcome | Verified In Test |
| :--- | :--- | :--- | :--- |
| **Untrusted Peer** | Mutual TLS 1.3 with CA enforcement | Handshake dropped | Test 03 |
| **Lost Commit ACK** | Idempotent transaction journaling | Cached response replayed | Test 12 |
| **Quorum Loss (< 12 nodes)** | Fast-abort before 2PC commit phase | Fails closed; 0 bytes corrupted | Test 21 |
| **Corrupt Shard on Wire** | Per-shard SHA-256 validation | Discarded; alternate node read | Test 19, 25 |
