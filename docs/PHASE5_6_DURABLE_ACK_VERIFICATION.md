# FSP Phase 5.6: Durable Acknowledgement Verification & Contract Semantics

## 1. Executive Summary

Phase 5.6 performed an exhaustive audit and architectural hardening of the **Distributed PUT Acknowledgement Boundary**. Prior to this phase, an acknowledgement level of `Recoverable` communicated that at least $K=12$ nodes acknowledged a write, but lacked formal verification of node identity, did not guard against duplicate ACKs inflating the distinct shard count, and conflated minimum mathematical recoverability with full configured redundancy.

In Phase 5.6, the coordinator's transaction engine ([`src/coordinator/transactions.rs`](file:///home/rade/Project/Jobs/fsp/src/coordinator/transactions.rs)) was enhanced with strict cryptographic and topological verification of every incoming [`PutShardAck`](file:///home/rade/Project/Jobs/fsp/src/network/protocol.rs), and introduced the structured [`DurabilityContractReport`](file:///home/rade/Project/Jobs/fsp/src/coordinator/transactions.rs).

---

## 2. The Five-Tier Durability Hierarchy

FSP defines five explicit durability tiers in [`src/storage/backend.rs`](file:///home/rade/Project/Jobs/fsp/src/storage/backend.rs):

```rust
pub enum DurabilityLevel {
    Received,          // Bytes in process memory buffer; not written to disk
    Buffered,          // Written via write_all() into volatile OS page cache
    LocallyDurable,    // Flushed to non-volatile storage via sync_all() on file and directory
    QuorumCommitted,   // Manifest recorded in monotonic coordinator WAL
    Recoverable,       // Minimum K=12 distinct failure-domain shards confirmed LocallyDurable
}
```

---

## 3. Strict Verification Invariants on PutShardAck

When a storage node responds to a `PutShard` RPC, the coordinator now strictly verifies:

1. **Node Identity Binding:** The responding node ID must match the assigned candidate:
   $$\text{ack.node\_id} == \text{target.node\_id}$$
   Mismatched node IDs (e.g. forged or rogue responses) are rejected and logged as errors.
2. **Shard Index Matching:**
   $$\text{ack.shard\_id} == \text{planned\_shard\_id}$$
   Cross-shard substitutions are rejected.
3. **Object ID & Version Generation:**
   $$\text{ack.object\_id} == \text{object\_id} \quad \wedge \quad \text{ack.version} == \text{shard\_version}$$
   Stale or mismatched generations are discarded.
4. **Local Durability Enforcement:**
   $$\text{ack.durability\_level} \ge \text{DurabilityLevel::LocallyDurable}$$
   Responses indicating `Received` or `Buffered` are rejected from the durable quorum count.
5. **Deduplicated Shard Tracking:** The coordinator tracks `acknowledged_shards: BTreeSet<usize>`. Duplicate ACKs from identical retries or misbehaving nodes cannot inflate the verified distinct shard count.

---

## 4. Distinguishing Minimum Recoverability from Full Protection

A core finding of Phase 5.6 is that **12 durable shards do not equal 20 durable shards**:

- **Minimum Recoverability ($K=12$):** Allows reconstructing the original object under current operational conditions. However, the system has **zero remaining fault tolerance margin**. A single additional shard loss causes permanent unrecoverability.
- **Full Redundancy ($N=20$):** All 12 data shards and 8 parity shards are locally durable across distinct nodes, providing tolerance against up to 8 simultaneous node losses.

The new [`DurabilityContractReport`](file:///home/rade/Project/Jobs/fsp/src/coordinator/transactions.rs) explicitly reports these separated dimensions:
```rust
pub struct DurabilityContractReport {
    pub durability_level: DurabilityLevel,
    pub is_recoverable: bool,                  // >= 12 distinct durable shards
    pub distinct_durable_shards: usize,        // Exact count of verified shards
    pub total_shards: usize,                   // 20
    pub min_shards: usize,                     // 12
    pub full_redundancy_achieved: bool,        // true iff distinct == 20
    pub failure_domain_policy_satisfied: bool, // Anti-affinity satisfied
    pub metadata_quorum_committed: bool,       // WAL synced with barrier
}
```

All 10 contract criteria were validated under automated tests in [`tests/phase5_6_validation.rs`](file:///home/rade/Project/Jobs/fsp/tests/phase5_6_validation.rs) (Tests 1, 4, 6, 8, and 9).
