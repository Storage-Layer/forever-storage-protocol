# FSP Phase 5.7: Durability Contract & Acknowledgment Audit

**Milestone:** Phase 5.7  
**Artifact Path:** `docs/PHASE5_7_DURABLE_ACK_AUDIT.md`  
**Audited Code:** [`src/coordinator/transactions.rs`](file:///home/rade/Project/Jobs/fsp/src/coordinator/transactions.rs)  
**Machine-Readable Proof:** [`results/phase5_7/durable_ack.json`](file:///home/rade/Project/Jobs/fsp/results/phase5_7/durable_ack.json)  

---

## 1. The Durability Acknowledgment Hazard

In distributed storage protocols, a common vulnerability is premature commit:
1. **The Duplicate ACK Attack/Bug:** A rogue or buggy storage node sends multiple duplicate ACKs for the same shard index (or an in-flight network replay occurs). If the coordinator merely counts total received ACK messages, it could record `count = 12` and commit the object when only 1 or 2 distinct shards were actually written to disk. If those nodes crash, data is permanently lost.
2. **Node/Shard Identity Spoofing:** A node sends an ACK claiming to have persisted shard $i$, but the ACK payload references shard $j$ or originates from an unauthorized node.
3. **Volatile ACK Acceptance:** A storage node replies with an ACK as soon as the bytes enter its RAM/socket buffer, before calling `fsync` or writing to persistent disk. If power fails immediately, unwritten data vanishes.

---

## 2. Hardened Architecture in `src/coordinator/transactions.rs`

In Phase 5.6 and 5.7, the transaction coordinator enforces a strict `DurabilityContractReport` contract.

### Invariant 1: Node Identity Binding
```rust
// src/coordinator/transactions.rs:727
if ack.node_id != target.node_id {
    eprintln!(
        "[SECURITY] Rejected ACK: node_id {} does not match target {}",
        ack.node_id.0, target.node_id.0
    );
    continue;
}
```
Cross-node ACK spoofing is immediately rejected.

### Invariant 2: Shard Identity Binding
```rust
// src/coordinator/transactions.rs:736
if ack.shard_id != planned_shard_id {
    eprintln!(
        "[INTEGRITY] Rejected ACK: shard_id {} does not match planned {}",
        ack.shard_id, planned_shard_id
    );
    continue;
}
```
Mismatched shard allocations are discarded.

### Invariant 3: Durability Level Floor
```rust
// src/coordinator/transactions.rs:745
if ack.durability_level < DurabilityLevel::LocallyDurable {
    eprintln!(
        "[DURABILITY] Rejected ACK: level {:?} < LocallyDurable",
        ack.durability_level
    );
    continue;
}
```
Only ACKs that certify completion of `write_all` + `sync_all` (or equivalent disk sync barrier) are accepted. Volatile/in-memory ACKs are strictly rejected.

### Invariant 4: Distinct Shard Deduplication via `BTreeSet<usize>`
```rust
// src/coordinator/transactions.rs:757
let mut acknowledged_shards: BTreeSet<usize> = BTreeSet::new();
...
if acknowledged_shards.insert(ack.shard_id) {
    distinct_durable_shards += 1;
}
```
Because `acknowledged_shards` is a set of unique shard indices, receiving 10 duplicate ACKs for shard 0 only increments `distinct_durable_shards` once.

### Invariant 5: Quorum Commit Gating
```rust
// src/coordinator/transactions.rs:764
if distinct_durable_shards < options.commit_policy.required_shards() {
    return Err(StorageError::InsufficientDurableQuorum {
        required: options.commit_policy.required_shards(),
        actual: distinct_durable_shards,
    });
}
```
For RS(20,12), an object cannot be committed unless at least $K=12$ unique shards have been confirmed locally durable on distinct storage nodes.

---

## 3. Unit Test Validation Evidence

These invariants are validated in `tests/phase5_6_validation.rs`:
- `test_p5_6_04_durable_shard_ack_with_distinct_indices_and_hashes` validates that ACKs carry distinct indices and SHA-256 digests.
- `test_p5_6_08_duplicate_shard_ack_does_not_inflate_durable_count` injects 5 duplicate ACKs for shard 0 and verifies that durable count remains 1.
- `test_p5_6_09_mismatched_shard_or_node_identity_rejection` tests rejection of tampered node and shard IDs.

All tests pass 100% deterministically.
