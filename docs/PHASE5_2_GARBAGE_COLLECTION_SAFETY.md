# Phase 5.2 Garbage Collection Reachability & Erasure Coding Redundancy Safety

## 1. Problem Statement
Garbage collection (GC) in an erasure-coded storage system is safety-critical. Deleting shards based solely on naive timestamps or unverified watermarks risks destroying:
1. Active, in-use committed data.
2. Historical versions required for audit, recovery, or regulatory retention.
3. Shards protected by offline disaster recovery checkpoints.
4. Shards involved in concurrent, in-flight transactions or adaptive repair.
5. Shards necessary to maintain the $k=12$ systematic Cauchy erasure-coding reconstruction threshold.

## 2. Invariants & Implementation
Phase 5.2 implements `NodeEpochFencingTracker::audit_gc_eligibility` with 7 sequential safety gates:

1. **Active Legal Hold Check:**
   If `active_legal_holds` contains the object, all GC and deletions are strictly blocked.
2. **In-Flight Transaction / Repair Check:**
   If `in_flight_transactions` contains active repairs or mutations for the object, deletion is rejected.
3. **Retention Watermark Check:**
   If `version <= min_retained_gen`, deletion is rejected.
4. **Recovery Checkpoint Protection:**
   If the `(object_id, generation)` pair is in `protected_checkpoints`, deletion is rejected.
5. **Active Committed Generation Check:**
   If `version >= cur_gen && cur_gen > 0`, deletion is rejected. Active committed state cannot be deleted.
6. **Retained Historical Version Set Check:**
   If the version is in `retained_historical_versions`, deletion is rejected.
7. **Surviving Redundancy Threshold Check:**
   If `surviving_shards <= 12`, deletion is rejected immediately. The cluster must never voluntarily drop below $k=12$ data redundancy.

## 3. Test Verification
Verified by:
- `test_11_gc_of_active_committed_shard_rejected`
- `test_12_gc_of_retained_historical_shard_rejected`
- `test_13_gc_of_checkpoint_protected_shard_rejected`
- `test_14_gc_blocked_by_active_legal_hold`
- `test_15_gc_rejected_when_surviving_shards_at_or_below_twelve`
- `test_16_stale_destructive_permit_replay_rejected`
